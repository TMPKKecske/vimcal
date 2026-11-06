use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, Duration, Local, LocalResult, NaiveDate, TimeZone, Timelike, Utc};
use serde::{Deserialize, Serialize};
use std::{fs, path::{Path, PathBuf}};

pub const SLOT_MINUTES: i64 = 15;
pub const SLOTS_PER_DAY: usize = 96;
pub const ALL_DAY_ROWS: usize = 3;
pub const DEFAULT_CALENDAR_ID: &str = "default";

fn default_calendar_id() -> String { DEFAULT_CALENDAR_ID.to_string() }
fn default_visible() -> bool { true }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Calendar {
    pub id: String,
    pub name: String,
    /// User-editable color in #RRGGBB format.
    pub color: String,
    #[serde(default = "default_visible")]
    pub visible: bool,
}

impl Calendar {
    pub fn default_calendar() -> Self {
        Self { id: default_calendar_id(), name: "Personal".into(), color: "#51D4C7".into(), visible: true }
    }
}

pub fn valid_color(value: &str) -> bool {
    value.len() == 7 && value.starts_with('#') && value[1..].bytes().all(|c| c.is_ascii_hexdigit())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CalEvent {
    pub id: String,
    #[serde(default = "default_calendar_id")]
    pub calendar_id: String,
    pub title: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    #[serde(default)]
    pub all_day: bool,
    #[serde(default)]
    pub location: String,
    #[serde(default)]
    pub imported: bool,
    /// Native repeating-series definition. Only the master event is persisted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recurrence: Option<Recurrence>,
    /// Links imported ICS occurrences, or transient generated instances, to a series.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub series_id: Option<String>,
    /// Generated instance index: transient, never written to disk.
    #[serde(skip)]
    pub occurrence_index: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Recurrence {
    pub every_days: u32,
    /// Number of occurrences including the starting occurrence; None = forever.
    pub count: Option<u32>,
    #[serde(default)]
    pub excluded: Vec<u32>,
}

/// Expand a native recurrence only into a bounded date window. A forever-series
/// occupies one JSON record, not infinitely many events in memory/disk.
pub fn expand_in_window(master: &CalEvent, from: NaiveDate, through: NaiveDate) -> Vec<CalEvent> {
    let Some(rule) = &master.recurrence else { return vec![master.clone()]; };
    if rule.every_days == 0 { return Vec::new(); }
    let local_start = master.start.with_timezone(&Local);
    let first_day = local_start.date_naive();
    let span_days = if master.all_day { master.all_day_days().max(1) }
        else { ((master.end - master.start).num_days() + 2).max(1) };
    let interval = rule.every_days as i64;
    let offset = (from - first_day).num_days().saturating_sub(span_days);
    let first_index = offset.div_euclid(interval).max(0) as u32;
    let max_index = rule.count.unwrap_or(u32::MAX);
    let mut events = Vec::new();
    for index in first_index..max_index {
        let Some(days) = (index as i64).checked_mul(interval) else { break; };
        let Some(date) = first_day.checked_add_signed(Duration::days(days)) else { break; };
        if date > through { break; }
        if rule.excluded.contains(&index) { continue; }
        let (start, end) = if master.all_day {
            all_day_bounds(date, master.all_day_days().max(1))
        } else {
            let naive = date.and_time(local_start.time());
            let local = Local.from_local_datetime(&naive).earliest()
                .unwrap_or_else(|| local_time(date, local_start.time().hour() as i64 * 60
                    + local_start.time().minute() as i64));
            let start = local.with_timezone(&Utc);
            (start, start + (master.end - master.start))
        };
        if end.with_timezone(&Local).date_naive() < from { continue; }
        let mut event = master.clone();
        event.id = format!("{}#{}", master.id, index);
        event.series_id = Some(master.id.clone());
        event.occurrence_index = Some(index);
        event.recurrence = None;
        event.start = start;
        event.end = end;
        events.push(event);
    }
    events
}


impl CalEvent {
    pub fn duration_minutes(&self) -> i64 { (self.end - self.start).num_minutes() }

    /// All-day event ends are exclusive. Using local dates also handles 23/25-hour DST days.
    pub fn all_day_days(&self) -> i64 {
        (self.end.with_timezone(&Local).date_naive()
            - self.start.with_timezone(&Local).date_naive()).num_days()
    }

    pub fn intersects_day(&self, date: NaiveDate) -> bool {
        let start = local_time(date, 0).with_timezone(&Utc);
        let next = local_time(date + Duration::days(1), 0).with_timezone(&Utc);
        self.start < next && self.end > start
    }

    pub fn intersects_slot(&self, date: NaiveDate, slot: usize) -> bool {
        if self.all_day { return false; }
        let start = local_time(date, (slot as i64) * SLOT_MINUTES).with_timezone(&Utc);
        let end = local_time(date, ((slot + 1) as i64) * SLOT_MINUTES).with_timezone(&Utc);
        self.start < end && self.end > start
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CalendarStore {
    pub calendars: Vec<Calendar>,
    pub events: Vec<CalEvent>,
    pub active_calendar_id: String,
}

impl Default for CalendarStore {
    fn default() -> Self {
        Self { calendars: vec![Calendar::default_calendar()], events: Vec::new(),
            active_calendar_id: default_calendar_id() }
    }
}

impl CalendarStore {
    pub fn normalize(&mut self) {
        if self.calendars.is_empty() { self.calendars.push(Calendar::default_calendar()); }
        if !self.calendars.iter().any(|c| c.id == self.active_calendar_id) {
            self.active_calendar_id = self.calendars[0].id.clone();
        }
        for event in &mut self.events {
            if !self.calendars.iter().any(|c| c.id == event.calendar_id) {
                event.calendar_id = self.active_calendar_id.clone();
            }
        }
        self.events.retain(|e| e.end > e.start && e.recurrence.as_ref()
            .is_none_or(|r| r.every_days > 0 && r.count != Some(0)));
        self.events.sort_by_key(|e| e.start);
    }

    pub fn calendar(&self, id: &str) -> Option<&Calendar> {
        self.calendars.iter().find(|c| c.id == id)
    }

    pub fn is_visible(&self, event: &CalEvent) -> bool {
        self.calendar(&event.calendar_id).is_some_and(|c| c.visible)
    }
}

pub fn local_time(date: NaiveDate, minute: i64) -> DateTime<Local> {
    let day = date + Duration::days(minute.div_euclid(1440));
    let minute = minute.rem_euclid(1440) as u32;
    let naive = day.and_hms_opt(minute / 60, minute % 60, 0).expect("valid calendar minute");
    match Local.from_local_datetime(&naive) {
        LocalResult::Single(dt) | LocalResult::Ambiguous(dt, _) => dt,
        LocalResult::None => Local.from_utc_datetime(&naive),
    }
}

pub fn all_day_bounds(date: NaiveDate, days: i64) -> (DateTime<Utc>, DateTime<Utc>) {
    (local_time(date, 0).with_timezone(&Utc),
        local_time(date + Duration::days(days), 0).with_timezone(&Utc))
}

pub fn week_start(date: NaiveDate) -> NaiveDate {
    date - Duration::days(date.weekday().num_days_from_monday() as i64)
}

pub fn default_data_path() -> PathBuf {
    if let Ok(value) = std::env::var("VIMCAL_DATA") { return PathBuf::from(value); }
    if cfg!(windows) {
        if let Ok(root) = std::env::var("APPDATA") {
            return PathBuf::from(root).join("vimcal").join("events.json");
        }
    }
    if let Ok(root) = std::env::var("XDG_DATA_HOME") {
        return PathBuf::from(root).join("vimcal").join("events.json");
    }
    let root = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(root).join(".local/share/vimcal/events.json")
}

pub fn load_data(path: &Path) -> Result<CalendarStore> {
    if !path.exists() { return Ok(CalendarStore::default()); }
    let body = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    // Backward compatibility: v0.1 stored a bare list of events without calendars.
    let value: serde_json::Value = serde_json::from_str(&body).context("invalid calendar JSON")?;
    let mut store = if value.is_array() {
        CalendarStore { events: serde_json::from_value(value)?, ..CalendarStore::default() }
    } else {
        serde_json::from_value(value).context("invalid multi-calendar JSON")?
    };
    store.normalize();
    Ok(store)
}

pub fn save_data(path: &Path, store: &CalendarStore) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(store)?)?;
    if cfg!(windows) && path.exists() { fs::remove_file(path)?; }
    fs::rename(&tmp, path).with_context(|| format!("saving {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monday_is_week_start() {
        assert_eq!(week_start(NaiveDate::from_ymd_opt(2026, 11, 4).unwrap()),
                   NaiveDate::from_ymd_opt(2026, 11, 2).unwrap());
    }

    #[test]
    fn slot_overlap_is_half_open() {
        let date = Local::now().date_naive();
        let start = local_time(date, 9 * 60).with_timezone(&Utc);
        let event = CalEvent {
            id: "a".into(), calendar_id: default_calendar_id(), title: "x".into(), start,
            end: start + Duration::minutes(30), all_day: false,
            location: String::new(), imported: false,
            recurrence: None, series_id: None, occurrence_index: None,
        };
        assert!(event.intersects_slot(date, 36));
        assert!(event.intersects_slot(date, 37));
        assert!(!event.intersects_slot(date, 38));
    }

    #[test]
    fn migrates_legacy_json_array() {
        let date = NaiveDate::from_ymd_opt(2026, 11, 6).unwrap();
        let (start, end) = all_day_bounds(date, 1);
        let original = CalEvent { id: "one".into(), calendar_id: default_calendar_id(),
            title: "Legacy".into(), start, end, all_day: true,
            location: String::new(), imported: false,
            recurrence: None, series_id: None, occurrence_index: None };
        let mut legacy = serde_json::to_value(vec![original]).unwrap();
        legacy[0].as_object_mut().unwrap().remove("calendar_id");
        let path = std::env::temp_dir()
            .join(format!("vimcal-legacy-{}.json", uuid::Uuid::new_v4()));
        fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();
        let store = load_data(&path).unwrap();
        assert_eq!(store.events[0].calendar_id, DEFAULT_CALENDAR_ID);
        assert_eq!(store.calendars.len(), 1);
        assert_eq!(store.events[0].title, "Legacy");
        save_data(&path, &store).unwrap();
        assert!(serde_json::from_str::<serde_json::Value>(&fs::read_to_string(&path).unwrap())
            .unwrap().is_object());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn all_day_day_count_uses_local_calendar_days() {
        let date = NaiveDate::from_ymd_opt(2026, 10, 24).unwrap();
        let (start, end) = all_day_bounds(date, 3);
        let event = CalEvent { id: "x".into(), calendar_id: default_calendar_id(),
            title: "Holiday".into(), start, end, all_day: true,
            location: String::new(), imported: false,
            recurrence: None, series_id: None, occurrence_index: None };
        assert_eq!(event.all_day_days(), 3);
        assert!(event.intersects_day(date + Duration::days(2)));
        assert!(!event.intersects_day(date + Duration::days(3)));
    }

    #[test]
    fn hex_color_validation() {
        assert!(valid_color("#aAbbCC"));
        assert!(!valid_color("red"));
        assert!(!valid_color("#12345g"));
    }
}
