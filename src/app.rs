use crate::{ics, model::{self, CalEvent, Calendar, CalendarStore, Recurrence, SLOT_MINUTES, SLOTS_PER_DAY}, vim_input::{VimInput, InputMode}};
use chrono::{Duration, Local, NaiveDate, NaiveTime, TimeZone, Timelike, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::{mem, path::{Path, PathBuf}};
use uuid::Uuid;

/// 08:00 in the quarter-hour grid (32 slots after midnight).
const START_OF_WORKDAY_SLOT: usize = 8 * 60 / SLOT_MINUTES as usize;

#[derive(Clone, Debug)]
pub struct Editor {
    pub id: Option<String>,
    pub title: String,
    pub date: String,
    pub time: String,
    /// Minutes for timed events, days for all-day events.
    pub duration: String,
    pub all_day: bool,
    pub calendar_id: String,
    pub field: usize,
    pub error: String,
    pub recurring: bool,
    pub every_days: String,
    pub forever: bool,
    pub repeat_count: String,
    pub vim: VimInput,
}

impl Editor {
    fn new(date: NaiveDate, slot: usize, all_day: bool, calendar_id: String) -> Self {
        Self {
            id: None, title: String::new(), date: date.format("%Y-%m-%d").to_string(),
            time: format!("{:02}:{:02}", slot / 4, slot % 4 * 15),
            duration: if all_day { "1" } else { "60" }.into(),
            all_day, calendar_id, field: 0, error: String::new(),
            recurring: false, every_days: "1".into(), forever: false,
            repeat_count: "5".into(), vim: VimInput::new(""),
        }
    }

    fn from_event(event: &CalEvent) -> Self {
        let local = event.start.with_timezone(&Local);
        Self {
            id: Some(event.id.clone()), title: event.title.clone(),
            date: local.format("%Y-%m-%d").to_string(),
            time: local.format("%H:%M").to_string(),
            duration: if event.all_day { event.all_day_days().to_string() }
                      else { event.duration_minutes().to_string() },
            all_day: event.all_day, calendar_id: event.calendar_id.clone(),
            field: 0, error: String::new(),
            recurring: event.recurrence.is_some(),
            every_days: event.recurrence.as_ref().map(|r| r.every_days.to_string())
                .unwrap_or_else(|| "1".into()),
            forever: event.recurrence.as_ref().is_some_and(|r| r.count.is_none()),
            repeat_count: event.recurrence.as_ref().and_then(|r| r.count)
                .unwrap_or(5).to_string(),
            vim: VimInput::new(&event.title),
        }
    }

    fn field_value(&self) -> Option<&str> {
        match self.field {
            0 => Some(&self.title), 1 => Some(&self.date),
            3 if !self.all_day => Some(&self.time),
            4 => Some(&self.duration), 7 => Some(&self.every_days),
            9 => Some(&self.repeat_count), _ => None,
        }
    }
    fn switch_field(&mut self, delta: isize) {
        self.field = (self.field as isize + delta).rem_euclid(10) as usize;
        let value = self.field_value().unwrap_or("").to_owned();
        self.vim.switch_field(&value);
        self.error.clear();
    }
    fn handle_text(&mut self, key: KeyEvent) -> bool {
        match self.field {
            0 => self.vim.handle(&mut self.title, key),
            1 => self.vim.handle(&mut self.date, key),
            3 if !self.all_day => self.vim.handle(&mut self.time, key),
            4 => self.vim.handle(&mut self.duration, key),
            7 if self.recurring => self.vim.handle(&mut self.every_days, key),
            9 if self.recurring && !self.forever => self.vim.handle(&mut self.repeat_count, key),
            _ => false,
        }
    }

    fn toggle_all_day(&mut self) {
        self.all_day = !self.all_day;
        self.duration = if self.all_day { "1" } else { "60" }.into();
    }

    /// Normal-mode h/l adjusts integer fields instead of moving their caret.
    /// Insert mode continues to let users type numbers or use arrow keys to edit.
    fn adjust_number(&mut self, delta: i64) {
        let (value, step, min, max) = match self.field {
            4 => (&mut self.duration, if self.all_day { 1 } else { 15 },
                if self.all_day { 1 } else { 15 },
                if self.all_day { 365 } else { 14 * 1440 }),
            7 => {
                self.recurring = true;
                (&mut self.every_days, 1, 1, 3650)
            }
            9 => {
                self.recurring = true;
                self.forever = false;
                (&mut self.repeat_count, 1, 1, 1_000_000)
            }
            _ => return,
        };
        let current = value.trim().parse::<i64>().unwrap_or(min);
        *value = current.saturating_add(delta.saturating_mul(step))
            .clamp(min, max).to_string();
        self.vim.switch_field(value);
        self.error.clear();
    }
}

#[derive(Clone, Debug)]
pub struct ImportEditor { pub path: String, pub calendar_id: String, pub field: usize, pub vim: VimInput }

#[derive(Clone, Debug)]
pub struct SearchEditor { pub query: String, pub selected: usize, pub vim: VimInput }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View { Week, Month }

#[derive(Clone, Debug)]
pub struct CalendarEditor {
    pub id: Option<String>, pub name: String, pub color: String,
    pub field: usize, pub error: String, pub vim: VimInput,
}

#[derive(Clone, Debug)]
pub enum Mode {
    Normal,
    Visual { anchor_date: NaiveDate, anchor_slot: usize },
    Edit(Editor),
    Import(ImportEditor),
    Search(SearchEditor),
    Delete(String),
    CalendarList { selected: usize },
    CalendarEdit(CalendarEditor),
    CalendarDelete { selected: usize, id: String },
    Help,
}

pub struct App {
    pub data: CalendarStore,
    pub data_path: PathBuf,
    pub cursor_date: NaiveDate,
    pub cursor_slot: usize,
    pub scroll_slot: usize,
    /// The first `g` of the `gg` normal-mode command.
    pending_g: bool,
    pub mode: Mode,
    pub view: View,
    pub focus_id: Option<String>,
    pub status: String,
    /// All nonrepeating entries plus generated instances near cursor (cache).
    pub shown_events: Vec<CalEvent>,
    cache_center: NaiveDate,
}

impl App {
    pub fn new(data_path: PathBuf) -> anyhow::Result<Self> {
        let data = model::load_data(&data_path)?;
        let now = Local::now();
        let slot = START_OF_WORKDAY_SLOT;
        let mut app = Self {
            data, data_path,
            cursor_date: now.date_naive(), cursor_slot: slot,
            scroll_slot: slot.saturating_sub(5), pending_g: false,
            mode: Mode::Normal, view: View::Week,
            focus_id: None, status: "NORMAL · m month/week · / search · c calendars · ? help".into(),
            shown_events: Vec::new(), cache_center: now.date_naive(),
        };
        app.refresh_occurrences();
        Ok(app)
    }

    pub fn ensure_visible(&mut self, screen_height: u16) {
        self.ensure_occurrences();
        if self.view == View::Month { return; }
        // Header/footer 6, grid frame 2, weekday header 1 and 3 all-day rows.
        let rows = screen_height.saturating_sub(12).max(1) as usize;
        if self.cursor_slot < self.scroll_slot + 2 {
            self.scroll_slot = self.cursor_slot.saturating_sub(2);
        } else if self.cursor_slot >= self.scroll_slot + rows.saturating_sub(2) {
            self.scroll_slot = (self.cursor_slot + 3).saturating_sub(rows);
        }
        self.scroll_slot = self.scroll_slot.min(SLOTS_PER_DAY.saturating_sub(rows));
    }

    fn refresh_occurrences(&mut self) {
        self.cache_center = self.cursor_date;
        let from = self.cursor_date - Duration::days(120);
        let through = self.cursor_date + Duration::days(120);
        self.shown_events = self.data.events.iter().flat_map(|e| {
            model::expand_in_window(e, from, through)
        }).collect();
        self.shown_events.sort_by_key(|e| e.start);
    }

    fn ensure_occurrences(&mut self) {
        if (self.cursor_date - self.cache_center).num_days().abs() > 28 {
            self.refresh_occurrences();
        }
    }

    pub fn visible_events(&self) -> impl Iterator<Item = &CalEvent> + '_ {
        self.shown_events.iter().filter(|e| self.data.is_visible(e))
    }

    pub fn selected_event(&self) -> Option<&CalEvent> {
        // Month selection is date-based; the week grid is slot-based.
        if let Some(id) = &self.focus_id {
            if let Some(event) = self.visible_events().find(|e| &e.id == id &&
                (if self.view == View::Month || e.all_day {
                    e.intersects_day(self.cursor_date)
                } else {
                    e.intersects_slot(self.cursor_date, self.cursor_slot)
                })) {
                return Some(event);
            }
        }
        if self.view == View::Month {
            return self.visible_events().find(|e| e.intersects_day(self.cursor_date));
        }
        self.visible_events()
            .find(|e| e.intersects_slot(self.cursor_date, self.cursor_slot))
            .or_else(|| self.visible_events().find(|e| {
                e.all_day && self.cursor_slot == 0 && e.intersects_day(self.cursor_date)
            }))
    }

    /// Match the title, location, or calendar name. Hidden calendars stay hidden
    /// in search too, so opening a result always makes the event visible.
    pub fn search_results(&self, query: &str) -> Vec<&CalEvent> {
        let term = query.trim().to_lowercase();
        if term.is_empty() { return Vec::new(); }
        let mut found: Vec<&CalEvent> = self.visible_events().filter(|event| {
            event.title.to_lowercase().contains(&term)
                || event.location.to_lowercase().contains(&term)
                || self.data.calendar(&event.calendar_id)
                    .is_some_and(|cal| cal.name.to_lowercase().contains(&term))
        }).collect();
        found.sort_by_key(|event| {
            ((event.start.with_timezone(&Local).date_naive() - self.cursor_date).num_days().abs(),
             event.start, event.id.clone())
        });
        found
    }

    fn selected_index(&self) -> Option<usize> {
        self.selected_event().and_then(|e| self.data.events.iter().position(|item| item.id == e.id
            || e.series_id.as_deref() == Some(item.id.as_str())))
    }

    fn persist(&mut self, message: &str) {
        self.data.events.sort_by_key(|e| e.start);
        self.refresh_occurrences();
        match model::save_data(&self.data_path, &self.data) {
            Ok(()) => self.status = message.to_owned(),
            Err(e) => self.status = format!("SAVE FAILED: {e:#}"),
        }
    }

    pub fn select_or_create_calendar(&mut self, name: &str) -> anyhow::Result<()> {
        anyhow::ensure!(!name.trim().is_empty(), "calendar name cannot be empty");
        if let Some(found) = self.data.calendars.iter().find(|c| c.name.eq_ignore_ascii_case(name.trim())) {
            let id = found.id.clone();
            self.data.active_calendar_id = id.clone();
            if let Some(found) = self.data.calendars.iter_mut().find(|c| c.id == id) {
                found.visible = true;
            }
        } else {
            let id = Uuid::new_v4().to_string();
            self.data.calendars.push(Calendar {
                id: id.clone(), name: name.trim().into(), color: "#8E9CF7".into(), visible: true,
            });
            self.data.active_calendar_id = id;
        }
        self.persist("Calendar selected");
        Ok(())
    }

    pub fn import_path(&mut self, raw_path: &str) {
        let calendar_id = self.data.active_calendar_id.clone();
        self.import_path_to(raw_path, &calendar_id);
    }

    pub fn import_path_to(&mut self, raw_path: &str, calendar_id: &str) {
        if self.data.calendar(calendar_id).is_none() {
            self.status = "Import failed: destination calendar no longer exists".into();
            return;
        }
        let expanded = if let Some(suffix) = raw_path.trim().strip_prefix("~/") {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(suffix)
        } else { PathBuf::from(raw_path.trim()) };
        match ics::import_file(&expanded) {
            Err(e) => self.status = format!("IMPORT FAILED: {e:#}"),
            Ok(import) => {
                let mut added = 0;
                let mut updated = 0;
                for mut event in import.events {
                    let original_id = event.id.clone();
                    // UID identities are namespaced per calendar: the same file may be
                    // imported into both Personal and Work without overwriting either.
                    event.id = format!("{calendar_id}::{original_id}");
                    event.calendar_id = calendar_id.into();
                    if let Some(series) = event.series_id.as_mut() {
                        *series = format!("{calendar_id}::{}", series.as_str());
                    }
                    if let Some(existing) = self.data.events.iter_mut().find(|e| {
                        e.calendar_id == calendar_id && (
                            e.id == event.id ||
                            // Match pre-multi-calendar imports without duplicating them.
                            e.id == original_id
                        )
                    }) {
                        event.id = existing.id.clone();
                        *existing = event;
                        updated += 1;
                    } else {
                        self.data.events.push(event);
                        added += 1;
                    }
                }
                let name = self.data.calendar(calendar_id).map(|c| c.name.clone()).unwrap_or_default();
                let limit_note = if import.limited { " · recurrence capped" } else { "" };
                self.persist(&format!(
                    "Imported into {name}: {added} new / {updated} updated · {} skipped{limit_note}",
                    import.skipped,
                ));
            }
        }
    }

    fn place_cursor_on(&mut self, event: &CalEvent) {
        let start = event.start.with_timezone(&Local);
        self.cursor_date = start.date_naive();
        self.cursor_slot = if event.all_day { 0 }
                           else { (start.hour() as usize * 60 + start.minute() as usize) / 15 };
        self.focus_id = Some(if event.recurrence.is_some() {
            format!("{}#0", event.id)
        } else { event.id.clone() });
    }

    fn create_editor(&mut self, all_day: bool) {
        self.mode = Mode::Edit(Editor::new(self.cursor_date, self.cursor_slot,
            all_day, self.data.active_calendar_id.clone()));
    }

    fn edit_editor(&mut self) {
        if let Some(event) = self.selected_event() {
            // Editing a generated occurrence opens its master: changes apply to
            // the whole series; deleting offers an occurrence-specific choice.
            let master = event.series_id.as_ref().and_then(|id|
                self.data.events.iter().find(|e| &e.id == id))
                .unwrap_or(event);
            self.mode = Mode::Edit(Editor::from_event(master));
        } else { self.create_editor(false); }
    }

    fn create_visual_editor(&mut self, anchor_date: NaiveDate, anchor_slot: usize) {
        let anchor = model::local_time(anchor_date, anchor_slot as i64 * SLOT_MINUTES).with_timezone(&Utc);
        let cursor = model::local_time(self.cursor_date, self.cursor_slot as i64 * SLOT_MINUTES).with_timezone(&Utc);
        let first = anchor.min(cursor).with_timezone(&Local);
        let last = anchor.max(cursor).with_timezone(&Local);
        let duration = (last - first).num_minutes() + SLOT_MINUTES;
        let start_slot = (first.hour() as usize * 60 + first.minute() as usize) / 15;
        let mut editor = Editor::new(first.date_naive(), start_slot, false,
            self.data.active_calendar_id.clone());
        editor.duration = duration.to_string();
        self.mode = Mode::Edit(editor);
    }

    fn adjust_duration(&mut self, delta: i64) {
        match self.selected_index() {
            Some(i) => {
                if self.data.events[i].all_day {
                    let days = self.data.events[i].all_day_days() + delta.signum();
                    if !(1..=365).contains(&days) {
                        self.status = "All-day duration must be 1–365 days".into(); return;
                    }
                    let start_date = self.data.events[i].start.with_timezone(&Local).date_naive();
                    self.data.events[i].end = model::all_day_bounds(start_date, days).1;
                    let selected = self.data.events[i].clone();
                    self.place_cursor_on(&selected);
                    self.persist(&format!("All-day duration: {days} day(s)"));
                } else {
                    let minutes = self.data.events[i].duration_minutes() + delta;
                    if !(15..=14 * 1440).contains(&minutes) {
                        self.status = "Duration must be 15 minutes to 14 days".into(); return;
                    }
                    let start = self.data.events[i].start;
                    self.data.events[i].end = start + Duration::minutes(minutes);
                    let selected = self.data.events[i].clone();
                    self.place_cursor_on(&selected);
                    self.persist(&format!("Duration: {minutes} minutes"));
                }
            }
            None => self.status = "Move the cursor onto an event (Tab to reach all-day), then +/-".into(),
        }
    }

    fn move_event(&mut self, delta: i64) {
        match self.selected_index() {
            Some(i) => {
                if self.data.events[i].all_day {
                    let days = delta.signum();
                    let old = &self.data.events[i];
                    let date = old.start.with_timezone(&Local).date_naive() + Duration::days(days);
                    let bounds = model::all_day_bounds(date, old.all_day_days());
                    self.data.events[i].start = bounds.0;
                    self.data.events[i].end = bounds.1;
                } else {
                    self.data.events[i].start += Duration::minutes(delta);
                    self.data.events[i].end += Duration::minutes(delta);
                }
                let moved = self.data.events[i].clone();
                self.place_cursor_on(&moved);
                self.persist("Event moved");
            }
            None => self.status = "Move the cursor onto an event to move it".into(),
        }
    }

    fn move_cursor(&mut self, key: KeyCode) -> bool {
        match key {
            KeyCode::Char('h') | KeyCode::Left => self.cursor_date -= Duration::days(1),
            KeyCode::Char('l') | KeyCode::Right => self.cursor_date += Duration::days(1),
            KeyCode::Char('j') | KeyCode::Down if self.view == View::Month => {
                self.cursor_date += Duration::days(7)
            }
            KeyCode::Char('k') | KeyCode::Up if self.view == View::Month => {
                self.cursor_date -= Duration::days(7)
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.cursor_slot = (self.cursor_slot + 1).min(SLOTS_PER_DAY - 1)
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.cursor_slot = self.cursor_slot.saturating_sub(1)
            }
            _ => return false,
        }
        self.focus_id = None;
        true
    }

    fn jump_event(&mut self, backwards: bool) {
        let mut events: Vec<CalEvent> = self.visible_events()
            .filter(|e| e.intersects_day(self.cursor_date)).cloned().collect();
        events.sort_by_key(|e| (e.start, e.id.clone()));
        if events.is_empty() { self.status = "No visible events on this day".into(); return; }
        let selected = self.selected_event().map(|e| e.id.clone());
        let idx = selected.and_then(|id| events.iter().position(|e| e.id == id));
        let next = match (idx, backwards) {
            (Some(i), true) => (i + events.len() - 1) % events.len(),
            (Some(i), false) => (i + 1) % events.len(),
            (None, true) => events.len() - 1,
            (None, false) => 0,
        };
        let chosen = &events[next];
        self.focus_id = Some(chosen.id.clone());
        let start = chosen.start.with_timezone(&Local);
        self.cursor_slot = if chosen.all_day || start.date_naive() != self.cursor_date { 0 }
            else { (start.hour() as usize * 60 + start.minute() as usize) / 15 };
    }

    fn cycle_calendar(&self, id: &str, delta: i32) -> String {
        let count = self.data.calendars.len();
        let index = self.data.calendars.iter().position(|c| c.id == id).unwrap_or(0);
        self.data.calendars[((index as i32 + delta).rem_euclid(count as i32)) as usize].id.clone()
    }

    fn handle_normal(&mut self, key: KeyEvent) -> bool {
        // Interpret "gg" as one Vim-style command. A lone "g" waits for its
        // second key; any other key cancels the prefix and acts normally.
        if self.pending_g {
            self.pending_g = false;
            if key.code == KeyCode::Char('g') {
                self.cursor_slot = START_OF_WORKDAY_SLOT;
                self.scroll_slot = START_OF_WORKDAY_SLOT.saturating_sub(5);
                self.focus_id = None;
                // A time-slot cursor is only visible in the weekly grid.
                self.view = View::Week;
                self.status = "WEEK · cursor at 08:00 · t today".into();
                return false;
            }
        }
        if key.code == KeyCode::Char('g') {
            self.pending_g = true;
            self.status = "g … press g again for 08:00 · t today".into();
            return false;
        }
        if self.move_cursor(key.code) { self.ensure_occurrences(); return false; }
        match key.code {
            KeyCode::Char('q') => return true,
            KeyCode::Esc => { self.status = "NORMAL · m month/week · / search · gg 08:00 · ? help".into(); }
            KeyCode::Char('a') => self.create_editor(false),
            KeyCode::Char('d') => self.create_editor(true),
            KeyCode::Char('e') | KeyCode::Enter => self.edit_editor(),
            KeyCode::Char('v') if self.view == View::Week => self.mode = Mode::Visual {
                anchor_date: self.cursor_date, anchor_slot: self.cursor_slot,
            },
            KeyCode::Char('v') => self.status = "Switch to week view (m) to select time slots".into(),
            KeyCode::Char('/') => self.mode = Mode::Search(SearchEditor {
                query: String::new(), selected: 0, vim: VimInput::new(""),
            }),
            KeyCode::Char('m') => {
                self.view = if self.view == View::Week { View::Month } else { View::Week };
                self.status = if self.view == View::Month {
                    "MONTH · h/l day · j/k week · / search · m week".into()
                } else {
                    "WEEK · h/l day · j/k 15 min · / search · m month".into()
                };
            }
            KeyCode::Char('i') => self.mode = Mode::Import(ImportEditor {
                path: String::new(), calendar_id: self.data.active_calendar_id.clone(),
                field: 0, vim: VimInput::new(""),
            }),
            KeyCode::Char('c') => self.mode = Mode::CalendarList {
                selected: self.data.calendars.iter().position(|c| c.id == self.data.active_calendar_id).unwrap_or(0),
            },
            KeyCode::Char('x') | KeyCode::Delete => {
                if let Some(event) = self.selected_event() {
                    self.mode = Mode::Delete(event.id.clone());
                } else { self.status = "No event under cursor".into(); }
            }
            KeyCode::Char('+') | KeyCode::Char('=') => self.adjust_duration(15),
            KeyCode::Char('-') => self.adjust_duration(-15),
            KeyCode::Char('<') => self.move_event(-15),
            KeyCode::Char('>') => self.move_event(15),
            KeyCode::Char('H') => self.move_event(-1440),
            KeyCode::Char('L') => self.move_event(1440),
            KeyCode::Char('t') => {
                let now = Local::now();
                self.cursor_date = now.date_naive();
                self.cursor_slot = (now.hour() * 60 + now.minute()) as usize / 15;
                self.focus_id = None;
            }
            KeyCode::Char('0') => { self.cursor_slot = 0; self.focus_id = None; }
            KeyCode::Char('G') => { self.cursor_slot = SLOTS_PER_DAY - 1; self.focus_id = None; }
            KeyCode::Tab => self.jump_event(false),
            KeyCode::BackTab => self.jump_event(true),
            KeyCode::Char('?') => self.mode = Mode::Help,
            _ => {}
        }
        false
    }

    fn save_editor(&mut self, mut editor: Editor) {
        let parsed = (|| -> anyhow::Result<CalEvent> {
            let title = editor.title.trim();
            anyhow::ensure!(!title.is_empty(), "Title cannot be empty");
            let date = NaiveDate::parse_from_str(&editor.date, "%Y-%m-%d")?;
            anyhow::ensure!(self.data.calendar(&editor.calendar_id).is_some(), "Calendar not found");
            let duration: i64 = editor.duration.trim().parse()?;
            let (start, end) = if editor.all_day {
                anyhow::ensure!((1..=365).contains(&duration), "All-day duration: 1–365 days");
                model::all_day_bounds(date, duration)
            } else {
                anyhow::ensure!((15..=14 * 1440).contains(&duration), "Duration: 15–20160 min");
                let time = NaiveTime::parse_from_str(&editor.time, "%H:%M")?;
                let naive = date.and_time(time);
                let local = Local.from_local_datetime(&naive).earliest()
                    .ok_or_else(|| anyhow::anyhow!("This local time does not exist (DST transition)"))?;
                let start = local.with_timezone(&Utc);
                (start, start + Duration::minutes(duration))
            };
            let previous = editor.id.as_ref()
                .and_then(|id| self.data.events.iter().find(|e| &e.id == id));
            let recurrence = if editor.recurring {
                let every_days: u32 = editor.every_days.trim().parse()?;
                anyhow::ensure!((1..=3650).contains(&every_days), "Repeat interval must be 1–3650 days");
                let count = if editor.forever { None } else {
                    let n: u32 = editor.repeat_count.trim().parse()?;
                    anyhow::ensure!((1..=1_000_000).contains(&n), "Occurrence count must be 1–1000000");
                    Some(n)
                };
                Some(Recurrence { every_days, count,
                    excluded: previous.and_then(|e| e.recurrence.as_ref())
                        .map(|r| r.excluded.clone()).unwrap_or_default() })
            } else { None };
            // Once an imported event is moved to another calendar, it becomes an
            // independent local event. Otherwise re-importing its former calendar
            // could silently move the event back or cause a UID collision.
            let detached = previous.is_some_and(|e|
                e.imported && (e.calendar_id != editor.calendar_id || editor.recurring));
            Ok(CalEvent {
                id: if detached { Uuid::new_v4().to_string() }
                    else { editor.id.clone().unwrap_or_else(|| Uuid::new_v4().to_string()) },
                calendar_id: editor.calendar_id.clone(), title: title.into(), start, end,
                all_day: editor.all_day,
                location: previous.map(|e| e.location.clone()).unwrap_or_default(),
                imported: previous.is_some_and(|e| e.imported) && !detached,
                recurrence, series_id: if detached { None }
                    else { previous.and_then(|e| e.series_id.clone()) },
                occurrence_index: None,
            })
        })();
        match parsed {
            Ok(event) => {
                if let Some(old) = self.data.events.iter_mut()
                    .find(|e| editor.id.as_ref().is_some_and(|id| &e.id == id)) {
                    *old = event.clone();
                } else { self.data.events.push(event.clone()); }
                self.place_cursor_on(&event);
                self.persist("Event saved · +/- resize · </> move · Tab cycle events");
                self.mode = Mode::Normal;
            }
            Err(e) => { editor.error = format!("{e:#}"); self.mode = Mode::Edit(editor); }
        }
    }

    fn handle_editor(&mut self, mut editor: Editor, key: KeyEvent) {
        let normal = editor.vim.mode == InputMode::Normal;
        match key.code {
            // First Esc leaves INSERT; the next Esc closes the form without saving.
            KeyCode::Esc if normal => {
                self.status = "Event editing cancelled".into();
                self.mode = Mode::Normal;
            }
            KeyCode::Esc => {
                editor.handle_text(key);
                self.mode = Mode::Edit(editor);
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.status = "Event editing cancelled".into();
                self.mode = Mode::Normal;
            }
            KeyCode::Enter => self.save_editor(editor),
            KeyCode::Tab | KeyCode::Down | KeyCode::Char('j') if
                key.code != KeyCode::Char('j') || normal => {
                editor.switch_field(1);
                self.mode = Mode::Edit(editor);
            }
            KeyCode::BackTab | KeyCode::Up | KeyCode::Char('k') if
                key.code != KeyCode::Char('k') || normal => {
                editor.switch_field(-1);
                self.mode = Mode::Edit(editor);
            }
            KeyCode::Char(' ') | KeyCode::Left | KeyCode::Right |
                KeyCode::Char('h') | KeyCode::Char('l')
                if matches!(editor.field, 2 | 5 | 6 | 8) => {
                let delta = if matches!(key.code, KeyCode::Left | KeyCode::Char('h')) { -1 } else { 1 };
                match editor.field {
                    2 => editor.toggle_all_day(),
                    5 => editor.calendar_id = self.cycle_calendar(&editor.calendar_id, delta),
                    6 => editor.recurring = !editor.recurring,
                    8 => { editor.recurring = true; editor.forever = !editor.forever; }
                    _ => {}
                }
                self.mode = Mode::Edit(editor);
            }
            KeyCode::Char('h') | KeyCode::Char('l') if normal &&
                matches!(editor.field, 4 | 7 | 9) => {
                editor.adjust_number(if key.code == KeyCode::Char('h') { -1 } else { 1 });
                self.mode = Mode::Edit(editor);
            }
            _ => { editor.handle_text(key); self.mode = Mode::Edit(editor); }
        }
    }

    fn handle_import(&mut self, mut dialog: ImportEditor, key: KeyEvent) {
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => self.mode = Mode::Normal,
            KeyCode::Enter => {
                self.import_path_to(&dialog.path, &dialog.calendar_id);
                self.mode = Mode::Normal;
            }
            KeyCode::Tab | KeyCode::Down | KeyCode::BackTab | KeyCode::Up => {
                dialog.field = 1 - dialog.field;
                dialog.vim.switch_field(&dialog.path);
                self.mode = Mode::Import(dialog);
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') if dialog.field == 1 => {
                let delta = if key.code == KeyCode::Left { -1 } else { 1 };
                dialog.calendar_id = self.cycle_calendar(&dialog.calendar_id, delta);
                self.mode = Mode::Import(dialog);
            }
            _ => {
                if dialog.field == 0 { dialog.vim.handle(&mut dialog.path, key); }
                self.mode = Mode::Import(dialog);
            }
        }
    }

    fn handle_search(&mut self, mut dialog: SearchEditor, key: KeyEvent) {
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => self.mode = Mode::Normal,
            KeyCode::Enter => {
                let selected = self.search_results(&dialog.query)
                    .get(dialog.selected).map(|event| (*event).clone());
                if let Some(event) = selected {
                    self.place_cursor_on(&event);
                    self.status = format!("Found: {} · Tab to cycle events", event.title);
                    self.mode = Mode::Normal;
                } else {
                    self.status = "No matching visible events".into();
                    self.mode = Mode::Search(dialog);
                }
            }
            KeyCode::Down | KeyCode::Tab => {
                let count = self.search_results(&dialog.query).len();
                if count > 0 { dialog.selected = (dialog.selected + 1) % count; }
                self.mode = Mode::Search(dialog);
            }
            KeyCode::Up | KeyCode::BackTab => {
                let count = self.search_results(&dialog.query).len();
                if count > 0 { dialog.selected = (dialog.selected + count - 1) % count; }
                self.mode = Mode::Search(dialog);
            }
            _ => {
                let before = dialog.query.clone();
                dialog.vim.handle(&mut dialog.query, key);
                if dialog.query != before { dialog.selected = 0; }
                self.mode = Mode::Search(dialog);
            }
        }
    }

    fn handle_calendar_list(&mut self, mut selected: usize, key: KeyEvent) {
        let count = self.data.calendars.len();
        selected = selected.min(count - 1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('c') => self.mode = Mode::Normal,
            KeyCode::Char('j') | KeyCode::Down => self.mode = Mode::CalendarList { selected: (selected + 1) % count },
            KeyCode::Char('k') | KeyCode::Up => self.mode = Mode::CalendarList { selected: (selected + count - 1) % count },
            KeyCode::Enter => {
                self.data.active_calendar_id = self.data.calendars[selected].id.clone();
                self.data.calendars[selected].visible = true;
                self.persist("Active calendar changed");
                self.mode = Mode::Normal;
            }
            KeyCode::Char(' ') => {
                let next = !self.data.calendars[selected].visible;
                if !next && self.data.calendars.iter().filter(|c| c.visible).count() == 1 {
                    self.status = "At least one calendar must remain visible".into();
                } else {
                    self.data.calendars[selected].visible = next;
                    self.focus_id = None;
                    if !next && self.data.active_calendar_id == self.data.calendars[selected].id {
                        self.data.active_calendar_id = self.data.calendars.iter().find(|c| c.visible).unwrap().id.clone();
                    }
                    self.persist("Calendar visibility updated");
                }
                self.mode = Mode::CalendarList { selected };
            }
            KeyCode::Char('a') => self.mode = Mode::CalendarEdit(CalendarEditor {
                id: None, name: String::new(), color: "#8E9CF7".into(), field: 0,
                error: String::new(), vim: VimInput::new(""),
            }),
            KeyCode::Char('e') => {
                let cal = &self.data.calendars[selected];
                self.mode = Mode::CalendarEdit(CalendarEditor {
                    id: Some(cal.id.clone()), name: cal.name.clone(), color: cal.color.clone(),
                    field: 0, error: String::new(), vim: VimInput::new(&cal.name),
                });
            }
            KeyCode::Char('x') | KeyCode::Delete => {
                if count == 1 {
                    self.status = "Cannot delete the last calendar".into();
                    self.mode = Mode::CalendarList { selected };
                } else {
                    self.mode = Mode::CalendarDelete {
                        selected, id: self.data.calendars[selected].id.clone(),
                    };
                }
            }
            _ => self.mode = Mode::CalendarList { selected },
        }
    }

    fn save_calendar_editor(&mut self, mut editor: CalendarEditor) {
        let name = editor.name.trim();
        let color = editor.color.trim();
        if name.is_empty() { editor.error = "Calendar name is required".into(); }
        else if !model::valid_color(color) { editor.error = "Color must be #RRGGBB (e.g. #51D4C7)".into(); }
        else if self.data.calendars.iter().any(|c|
            c.name.eq_ignore_ascii_case(name) && Some(&c.id) != editor.id.as_ref()) {
            editor.error = "A calendar with that name already exists".into();
        } else {
            if let Some(id) = &editor.id {
                if let Some(cal) = self.data.calendars.iter_mut().find(|c| &c.id == id) {
                    cal.name = name.into(); cal.color = color.to_ascii_uppercase();
                }
            } else {
                let id = Uuid::new_v4().to_string();
                self.data.calendars.push(Calendar {
                    id: id.clone(), name: name.into(), color: color.to_ascii_uppercase(), visible: true,
                });
                self.data.active_calendar_id = id;
            }
            let selected = editor.id.as_ref()
                .and_then(|id| self.data.calendars.iter().position(|c| &c.id == id))
                .unwrap_or(self.data.calendars.len().saturating_sub(1));
            self.persist("Calendar saved");
            self.mode = Mode::CalendarList { selected };
            return;
        }
        self.mode = Mode::CalendarEdit(editor);
    }

    fn handle_calendar_editor(&mut self, mut editor: CalendarEditor, key: KeyEvent) {
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.mode = Mode::CalendarList { selected: 0 };
            }
            KeyCode::Enter => self.save_calendar_editor(editor),
            KeyCode::Tab | KeyCode::Down | KeyCode::BackTab | KeyCode::Up => {
                editor.field = 1 - editor.field;
                editor.error.clear();
                let text = if editor.field == 0 { &editor.name } else { &editor.color };
                editor.vim.switch_field(text);
                self.mode = Mode::CalendarEdit(editor);
            }
            _ => {
                if editor.field == 0 { editor.vim.handle(&mut editor.name, key); }
                else {
                    if editor.id.is_none() && editor.color == "#8E9CF7"
                        && editor.vim.mode == crate::vim_input::InputMode::Insert
                        && matches!(key.code, KeyCode::Char(_))
                        && !key.modifiers.contains(KeyModifiers::CONTROL) {
                        editor.color.clear(); editor.vim.switch_field("");
                    }
                    editor.vim.handle(&mut editor.color, key);
                }
                self.mode = Mode::CalendarEdit(editor);
            }
        }
    }

    fn delete_event(&mut self, id: &str, all: bool) {
        let Some(selected) = self.shown_events.iter().find(|e| e.id == id).cloned() else {
            self.status = "Event no longer selected".into(); return;
        };
        if let (Some(series_id), Some(index)) = (&selected.series_id, selected.occurrence_index) {
            // Native recurrence: one master with excluded indices.
            if all {
                self.data.events.retain(|e| e.id != *series_id);
                self.persist("Entire recurring series deleted");
            } else if let Some(master) = self.data.events.iter_mut().find(|e| e.id == *series_id) {
                if let Some(rule) = &mut master.recurrence {
                    if !rule.excluded.contains(&index) { rule.excluded.push(index); }
                }
                self.persist("Only this occurrence deleted");
            }
        } else if all && selected.series_id.is_some() {
            // Imported ICS recurring events: the importer has already expanded
            // the occurrences, but links them by the same UID (per calendar).
            self.data.events.retain(|e|
                e.series_id.as_deref() != selected.series_id.as_deref());
            self.persist("All imported recurring occurrences deleted");
        } else {
            self.data.events.retain(|e| e.id != id);
            self.persist("Event deleted");
        }
        self.focus_id = None;
    }

    fn delete_calendar(&mut self, id: &str) {
        let target = self.data.calendars.iter().find(|c| c.id != id).unwrap().id.clone();
        let reassigned = self.data.events.iter().filter(|e| e.calendar_id == id).count();
        for event in &mut self.data.events {
            if event.calendar_id == id {
                event.calendar_id = target.clone();
                if event.imported {
                    event.id = Uuid::new_v4().to_string();
                    event.imported = false;
                    event.series_id = None;
                }
            }
        }
        self.data.calendars.retain(|c| c.id != id);
        if self.data.active_calendar_id == id { self.data.active_calendar_id = target; }
        if !self.data.calendars.iter().any(|c| c.visible) {
            if let Some(first) = self.data.calendars.first_mut() { first.visible = true; }
        }
        if self.data.calendar(&self.data.active_calendar_id).is_some_and(|c| !c.visible) {
            self.data.active_calendar_id = self.data.calendars.iter().find(|c| c.visible).unwrap().id.clone();
        }
        self.focus_id = None;
        self.persist(&format!("Calendar deleted · {reassigned} event(s) moved to another calendar"));
    }

    pub fn on_key(&mut self, key: KeyEvent) -> bool {
        if key.kind == KeyEventKind::Release { return false; }
        self.ensure_occurrences();
        let previous = mem::replace(&mut self.mode, Mode::Normal);
        match previous {
            Mode::Normal => return self.handle_normal(key),
            Mode::Visual { anchor_date, anchor_slot } => {
                if matches!(key.code, KeyCode::Esc | KeyCode::Char('v')) {
                    self.status = "Selection cancelled".into();
                } else if matches!(key.code, KeyCode::Enter | KeyCode::Char('a')) {
                    self.create_visual_editor(anchor_date, anchor_slot);
                } else {
                    self.move_cursor(key.code);
                    self.mode = Mode::Visual { anchor_date, anchor_slot };
                }
            }
            Mode::Edit(editor) => self.handle_editor(editor, key),
            Mode::Import(dialog) => self.handle_import(dialog, key),
            Mode::Search(dialog) => self.handle_search(dialog, key),
            Mode::Delete(id) => {
                match key.code {
                    KeyCode::Char('o') | KeyCode::Char('y') => self.delete_event(&id, false),
                    KeyCode::Char('a') | KeyCode::Char('A') => self.delete_event(&id, true),
                    _ => self.status = "Deletion cancelled".into(),
                }
            }
            Mode::CalendarList { selected } => self.handle_calendar_list(selected, key),
            Mode::CalendarEdit(editor) => self.handle_calendar_editor(editor, key),
            Mode::CalendarDelete { selected, id } => {
                if matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
                    self.delete_calendar(&id);
                    self.mode = Mode::CalendarList { selected: selected.min(self.data.calendars.len() - 1) };
                } else {
                    self.status = "Calendar deletion cancelled".into();
                    self.mode = Mode::CalendarList { selected };
                }
            }
            Mode::Help => self.mode = Mode::Normal,
        }
        self.ensure_occurrences();
        false
    }

    pub fn event_count_for(&self, date: NaiveDate) -> usize {
        self.visible_events().filter(|e| e.intersects_day(date)).count()
    }

    pub fn data_location(&self) -> &Path { &self.data_path }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_app() -> App {
        let path = std::env::temp_dir().join(format!("vimcal-test-{}.json", Uuid::new_v4()));
        App::new(path).unwrap()
    }

    #[test]
    fn opens_at_eight_am_regardless_of_wall_clock() {
        let app = fresh_app();
        assert_eq!(app.cursor_slot, START_OF_WORKDAY_SLOT);
        assert_eq!(app.scroll_slot, START_OF_WORKDAY_SLOT.saturating_sub(5));
        assert_eq!(app.cursor_date, Local::now().date_naive());
    }

    #[test]
    fn double_g_goes_to_eight_on_selected_day() {
        let mut app = fresh_app();
        let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        let selected_day = NaiveDate::from_ymd_opt(2026, 12, 2).unwrap();
        app.cursor_date = selected_day;
        app.cursor_slot = 72; // 18:00
        app.scroll_slot = 67;
        app.on_key(key('g'));
        assert_eq!(app.cursor_slot, 72, "first g must not move the cursor");
        app.on_key(key('g'));
        assert_eq!(app.cursor_date, selected_day, "gg keeps the selected date");
        assert_eq!(app.cursor_slot, START_OF_WORKDAY_SLOT);
        assert_eq!(app.scroll_slot, START_OF_WORKDAY_SLOT.saturating_sub(5));
    }

    #[test]
    fn g_prefix_can_be_cancelled_by_another_key() {
        let mut app = fresh_app();
        let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        app.cursor_slot = 48;
        app.on_key(key('g'));
        app.on_key(key('j'));
        assert_eq!(app.cursor_slot, 49);
        app.on_key(key('g'));
        assert_eq!(app.cursor_slot, 49, "a new g starts a new prefix");
        app.on_key(key('g'));
        assert_eq!(app.cursor_slot, START_OF_WORKDAY_SLOT);
    }

    #[test]
    fn double_g_from_month_switches_to_week_at_eight() {
        let mut app = fresh_app();
        let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        app.view = View::Month;
        app.cursor_slot = 64;
        app.on_key(key('g'));
        app.on_key(key('g'));
        assert_eq!(app.view, View::Week);
        assert_eq!(app.cursor_slot, START_OF_WORKDAY_SLOT);
    }

    #[test]
    fn creates_and_resizes_all_day() {
        let mut app = fresh_app();
        let date = NaiveDate::from_ymd_opt(2026, 11, 6).unwrap();
        app.cursor_date = date;
        app.create_editor(true);
        let Mode::Edit(mut editor) = app.mode.clone() else { panic!("editor missing"); };
        editor.title = "Conference".into();
        editor.duration = "2".into();
        app.save_editor(editor);
        assert_eq!(app.data.events.len(), 1);
        assert!(app.data.events[0].all_day);
        assert_eq!(app.data.events[0].all_day_days(), 2);
        app.adjust_duration(15);
        assert_eq!(app.data.events[0].all_day_days(), 3);
        app.move_event(15);
        assert_eq!(app.data.events[0].start.with_timezone(&Local).date_naive(),
                   date + Duration::days(1));
        std::fs::remove_file(&app.data_path).unwrap();
    }

    #[test]
    fn month_mode_moves_by_week_and_old_week_shortcuts_are_inactive() {
        let mut app = fresh_app();
        let start = NaiveDate::from_ymd_opt(2026, 9, 30).unwrap();
        app.cursor_date = start;
        let slot = app.cursor_slot;
        app.on_key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE));
        assert_eq!(app.view, View::Month);
        app.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        assert_eq!(app.cursor_date, start + Duration::days(7));
        assert_eq!(app.cursor_slot, slot);
        app.on_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
        assert_eq!(app.cursor_date, start + Duration::days(6));
        app.on_key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE));
        assert_eq!(app.view, View::Week);
        let date = app.cursor_date;
        app.on_key(KeyEvent::new(KeyCode::Char('['), KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::NONE));
        assert_eq!(app.cursor_date, date, "week-jump shortcuts must be removed");
    }

    #[test]
    fn search_finds_visible_events_and_jumps_to_match() {
        let mut app = fresh_app();
        let date = NaiveDate::from_ymd_opt(2026, 11, 6).unwrap();
        app.cursor_date = date;
        let start = model::local_time(date, 9 * 60).with_timezone(&Utc);
        app.data.events.push(CalEvent {
            id: "search-event".into(), calendar_id: model::DEFAULT_CALENDAR_ID.into(),
            title: "Rust Conference".into(), start,
            end: start + Duration::minutes(60), all_day: false,
            location: "Main Hall".into(), imported: false,
            recurrence: None, series_id: None, occurrence_index: None,
        });
        app.refresh_occurrences();
        assert_eq!(app.search_results("CONFERENCE").len(), 1);
        assert_eq!(app.search_results("hall").len(), 1);
        assert_eq!(app.search_results("personal").len(), 1);
        assert!(app.search_results("zzz").is_empty());
        app.on_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        for ch in "rust".chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        assert!(matches!(app.mode, Mode::Search(_)));
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(app.focus_id.as_deref(), Some("search-event"));
        assert_eq!(app.cursor_slot, 36);
        app.data.calendars[0].visible = false;
        assert_eq!(app.search_results("rust").len(), 0);
    }

    #[test]
    fn escape_does_not_quit_and_editor_can_insert_in_middle() {
        let mut app = fresh_app();
        assert!(!app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(!app.on_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)));
        for ch in "Meetng".chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        for _ in 0..2 { app.on_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)); }
        app.on_key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        let Mode::Edit(editor) = &app.mode else { panic!("event editor expected"); };
        assert_eq!(editor.title, "Meeting");
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        let Mode::Edit(editor) = &app.mode else { panic!("Esc must not exit the editor"); };
        assert_eq!(editor.vim.mode, crate::vim_input::InputMode::Normal);
        app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(matches!(app.mode, Mode::Normal));
        assert!(app.on_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)));
    }

    #[test]
    fn event_editor_escape_twice_closes_but_q_stays_main_quit() {
        let mut app = fresh_app();
        let key = |c| KeyEvent::new(c, KeyModifiers::NONE);
        app.on_key(key(KeyCode::Char('a')));
        assert!(matches!(app.mode, Mode::Edit(_)));
        app.on_key(key(KeyCode::Esc));
        assert!(matches!(&app.mode, Mode::Edit(e) if e.vim.mode == InputMode::Normal));
        app.on_key(key(KeyCode::Esc));
        assert!(matches!(app.mode, Mode::Normal));
        assert!(!app.on_key(key(KeyCode::Esc)), "Esc on the calendar never quits");
        assert!(app.on_key(key(KeyCode::Char('q'))), "only q quits from calendar");
    }

    #[test]
    fn normal_mode_jk_navigate_editor_fields_and_hl_adjust_options() {
        let mut app = fresh_app();
        let key = |c| KeyEvent::new(c, KeyModifiers::NONE);
        app.on_key(key(KeyCode::Char('a')));
        app.on_key(key(KeyCode::Esc)); // Enter NORMAL input.
        app.on_key(key(KeyCode::Char('j')));
        assert!(matches!(&app.mode, Mode::Edit(e) if e.field == 1));
        app.on_key(key(KeyCode::Char('k')));
        assert!(matches!(&app.mode, Mode::Edit(e) if e.field == 0));
        // h/l must still move within strings in NORMAL.
        if let Mode::Edit(ref mut editor) = app.mode {
            editor.title = "Hello".into();
            editor.vim.switch_field("Hello");
        }
        app.on_key(key(KeyCode::Char('0')));
        app.on_key(key(KeyCode::Char('l')));
        assert!(matches!(&app.mode, Mode::Edit(e) if e.vim.cursor == 1 && e.title == "Hello"));
        app.on_key(key(KeyCode::Char('j'))); // Date
        app.on_key(key(KeyCode::Char('j'))); // All day
        app.on_key(key(KeyCode::Char('l')));
        assert!(matches!(&app.mode, Mode::Edit(e) if e.all_day));
        app.on_key(key(KeyCode::Char('j'))); // Time (disabled)
        app.on_key(key(KeyCode::Char('j'))); // Duration (days)
        app.on_key(key(KeyCode::Char('l')));
        assert!(matches!(&app.mode, Mode::Edit(e) if e.duration == "2"));
        app.on_key(key(KeyCode::Char('h')));
        assert!(matches!(&app.mode, Mode::Edit(e) if e.duration == "1"));
        app.on_key(key(KeyCode::Char('j'))); // Calendar
        app.on_key(key(KeyCode::Char('j'))); // Repeat
        app.on_key(key(KeyCode::Char('l')));
        assert!(matches!(&app.mode, Mode::Edit(e) if e.recurring));
        app.on_key(key(KeyCode::Char('j'))); // Every N days
        app.on_key(key(KeyCode::Char('l')));
        assert!(matches!(&app.mode, Mode::Edit(e) if e.every_days == "2"));
        app.on_key(key(KeyCode::Char('j'))); // Repeat ends
        app.on_key(key(KeyCode::Char('l')));
        assert!(matches!(&app.mode, Mode::Edit(e) if e.forever));
        app.on_key(key(KeyCode::Char('j'))); // Occurrences
        app.on_key(key(KeyCode::Char('l')));
        assert!(matches!(&app.mode, Mode::Edit(e) if !e.forever && e.repeat_count == "6"));
    }

    #[test]
    fn insert_mode_keeps_jk_as_text_and_tab_still_navigates() {
        let mut app = fresh_app();
        let key = |c| KeyEvent::new(c, KeyModifiers::NONE);
        app.on_key(key(KeyCode::Char('a')));
        for c in "jkl".chars() { app.on_key(key(KeyCode::Char(c))); }
        assert!(matches!(&app.mode, Mode::Edit(e) if e.field == 0 && e.title == "jkl"));
        app.on_key(key(KeyCode::Tab));
        assert!(matches!(&app.mode, Mode::Edit(e) if e.field == 1));
    }

    #[test]
    fn recurring_count_and_single_occurrence_deletion() {
        let mut app = fresh_app();
        let date = NaiveDate::from_ymd_opt(2026, 11, 6).unwrap();
        app.cursor_date = date;
        app.create_editor(false);
        let Mode::Edit(mut editor) = app.mode.clone() else { panic!("editor missing"); };
        editor.title = "Language practice".into();
        editor.recurring = true;
        editor.every_days = "2".into();
        editor.forever = false;
        editor.repeat_count = "4".into();
        app.save_editor(editor);
        assert_eq!(app.data.events.len(), 1, "only the master should be saved");
        let master_id = app.data.events[0].id.clone();
        let items: Vec<_> = app.visible_events().filter(|e|
            e.series_id.as_deref() == Some(master_id.as_str())).collect();
        assert_eq!(items.len(), 4);
        assert_eq!(items[3].start.with_timezone(&Local).date_naive(),
            date + Duration::days(6));
        app.delete_event(&format!("{master_id}#2"), false);
        assert_eq!(app.data.events.len(), 1);
        assert_eq!(app.data.events[0].recurrence.as_ref().unwrap().excluded, vec![2]);
        assert_eq!(app.visible_events().filter(|e|
            e.series_id.as_deref() == Some(master_id.as_str())).count(), 3);
        app.delete_event(&format!("{master_id}#1"), true);
        assert!(app.data.events.is_empty());
        std::fs::remove_file(&app.data_path).unwrap();
    }

    #[test]
    fn forever_recurrence_persists_as_one_entry_and_keeps_future_days() {
        let mut app = fresh_app();
        let date = NaiveDate::from_ymd_opt(2026, 11, 6).unwrap();
        app.cursor_date = date;
        app.create_editor(true);
        let Mode::Edit(mut editor) = app.mode.clone() else { panic!("editor missing"); };
        editor.title = "Weekly rest".into();
        editor.recurring = true;
        editor.every_days = "7".into();
        editor.forever = true;
        app.save_editor(editor);
        assert_eq!(app.data.events.len(), 1);
        assert!(app.data.events[0].recurrence.as_ref().unwrap().count.is_none());
        assert!(app.visible_events().filter(|e| e.title == "Weekly rest").count() > 10);
        let loaded = model::load_data(&app.data_path).unwrap();
        assert_eq!(loaded.events.len(), 1);
        assert_eq!(loaded.events[0].recurrence.as_ref().unwrap().every_days, 7);
        app.cursor_date += Duration::days(3 * 365);
        app.refresh_occurrences();
        assert!(app.visible_events().any(|e| e.title == "Weekly rest" &&
            e.start.with_timezone(&Local).date_naive() > date + Duration::days(1000)));
        std::fs::remove_file(&app.data_path).unwrap();
    }

    #[test]
    fn imported_ics_recurring_events_can_delete_one_or_all() {
        let mut app = fresh_app();
        let path = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/demo.ics"));
        app.import_path(path.to_str().unwrap());
        let first = app.data.events.iter().find(|e| e.series_id.is_some())
            .expect("demo must include recurring .ics events");
        let group = first.series_id.clone().unwrap();
        let first_id = first.id.clone();
        let original = app.data.events.iter().filter(|e|
            e.series_id.as_deref() == Some(group.as_str())).count();
        assert!(original > 1);
        app.delete_event(&first_id, false);
        assert_eq!(app.data.events.iter().filter(|e|
            e.series_id.as_deref() == Some(group.as_str())).count(), original - 1);
        let next = app.data.events.iter().find(|e|
            e.series_id.as_deref() == Some(group.as_str())).unwrap().id.clone();
        app.delete_event(&next, true);
        assert_eq!(app.data.events.iter().filter(|e|
            e.series_id.as_deref() == Some(group.as_str())).count(), 0);
        std::fs::remove_file(&app.data_path).unwrap();
    }

    #[test]
    fn imports_same_ics_to_two_calendars_without_collision() {
        let mut app = fresh_app();
        app.select_or_create_calendar("Work").unwrap();
        let work = app.data.active_calendar_id.clone();
        let path = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/demo.ics"));
        app.import_path_to(path.to_str().unwrap(), &work);
        let count = app.data.events.len();
        app.import_path_to(path.to_str().unwrap(), model::DEFAULT_CALENDAR_ID);
        assert_eq!(app.data.events.len(), count * 2);
        app.import_path_to(path.to_str().unwrap(), &work);
        assert_eq!(app.data.events.len(), count * 2);
        app.data.calendars.iter_mut().find(|c| c.id == work).unwrap().visible = false;
        assert_eq!(app.visible_events().count(), count);
        std::fs::remove_file(&app.data_path).unwrap();
    }
}
