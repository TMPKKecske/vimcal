//! RFC 5545 iCalendar export. One VCALENDAR per Vimcal calendar preserves
//! calendar names and colors while keeping imported files compatible with our importer.
use crate::model::{CalEvent, Calendar, CalendarStore};
use chrono::{DateTime, Duration, Local, NaiveDate, Utc};

/// RFC 5545 TEXT escaping, including embedded newlines and delimiters.
fn escape_text(text: &str) -> String {
    let mut escaped = String::new();
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    for ch in normalized.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            ',' => escaped.push_str("\\,"),
            ';' => escaped.push_str("\\;"),
            '\0' => {},
            _ => escaped.push(ch),
        }
    }
    escaped
}

/// Fold lines at 75 *bytes*, not characters. All lines, including continuation
/// lines (which begin with one space), use CRLF as required by RFC 5545.
fn add_line(output: &mut String, line: &str) {
    let mut width = 0;
    for ch in line.chars() {
        let bytes = ch.len_utf8();
        if width + bytes > 75 {
            output.push_str("\r\n ");
            width = 1;
        }
        output.push(ch);
        width += bytes;
    }
    output.push_str("\r\n");
}

fn stamp(date: DateTime<Utc>) -> String { date.format("%Y%m%dT%H%M%SZ").to_string() }
fn local_stamp(date: DateTime<Utc>) -> String {
    date.with_timezone(&Local).format("%Y%m%dT%H%M%S").to_string()
}
fn date_stamp(date: NaiveDate) -> String { date.format("%Y%m%d").to_string() }

/// Unique, deterministic UID even if two calendars contain events with the
/// same imported UID. Hex encoding prevents CRLF/property injection.
fn event_uid(calendar: &Calendar, event: &CalEvent) -> String {
    let key = format!("{}:{}", calendar.id, event.id);
    use std::fmt::Write as _;
    let mut encoded = String::with_capacity(key.len() * 2);
    for byte in key.bytes() { let _ = write!(&mut encoded, "{byte:02x}"); }
    format!("vimcal-{encoded}@local")
}

fn add_event(output: &mut String, cal: &Calendar, event: &CalEvent, generated_at: &str) {
    add_line(output, "BEGIN:VEVENT");
    add_line(output, &format!("UID:{}", event_uid(cal, event)));
    add_line(output, &format!("DTSTAMP:{generated_at}"));
    add_line(output, &format!("SUMMARY:{}", escape_text(&event.title)));
    if !event.location.is_empty() {
        add_line(output, &format!("LOCATION:{}", escape_text(&event.location)));
    }
    let rule = event.recurrence.as_ref();
    if event.all_day {
        // DTEND for DATE events is exclusive (the following day for a one-day event).
        let start = event.start.with_timezone(&Local).date_naive();
        let end = event.end.with_timezone(&Local).date_naive();
        add_line(output, &format!("DTSTART;VALUE=DATE:{}", date_stamp(start)));
        add_line(output, &format!("DTEND;VALUE=DATE:{}", date_stamp(end)));
    } else if rule.is_some() {
        // Floating local time preserves daily wall-clock recurrences through DST
        // without relying on nonportable TZID/VTIMEZONE definitions.
        add_line(output, &format!("DTSTART:{}", local_stamp(event.start)));
        add_line(output, &format!("DTEND:{}", local_stamp(event.end)));
    } else {
        // Nonrepeating events use absolute UTC timestamps for portability.
        add_line(output, &format!("DTSTART:{}", stamp(event.start)));
        add_line(output, &format!("DTEND:{}", stamp(event.end)));
    }
    if let Some(rule) = rule {
        let mut rrule = format!("RRULE:FREQ=DAILY;INTERVAL={}", rule.every_days);
        if let Some(count) = rule.count { rrule.push_str(&format!(";COUNT={count}")); }
        add_line(output, &rrule);
        let origin = event.start.with_timezone(&Local);
        for index in &rule.excluded {
            let Some(offset) = (i64::from(*index)).checked_mul(i64::from(rule.every_days)) else {
                continue;
            };
            let Some(day) = origin.date_naive().checked_add_signed(Duration::days(offset)) else {
                continue;
            };
            if event.all_day {
                add_line(output, &format!("EXDATE;VALUE=DATE:{}", date_stamp(day)));
            } else {
                let local = day.and_time(origin.time());
                add_line(output, &format!("EXDATE:{}", local.format("%Y%m%dT%H%M%S")));
            }
        }
    }
    add_line(output, "END:VEVENT");
}

/// Export *all persisted events*, including hidden calendars. Native recurring
/// series remain RRULEs (forever series are not materialized).
/// Imported ICS occurrences have already been expanded and export individually.
pub fn serialize(store: &CalendarStore, calendars: &[&Calendar]) -> String {
    let mut output = String::new();
    let generated_at = stamp(Utc::now());
    for cal in calendars {
        add_line(&mut output, "BEGIN:VCALENDAR");
        add_line(&mut output, "VERSION:2.0");
        add_line(&mut output, "PRODID:-//Vimcal//Vimcal Rust TUI//EN");
        add_line(&mut output, "CALSCALE:GREGORIAN");
        add_line(&mut output, &format!("X-WR-CALNAME:{}", escape_text(&cal.name)));
        add_line(&mut output, &format!("X-APPLE-CALENDAR-COLOR:{}", cal.color));
        for event in store.events.iter().filter(|event| event.calendar_id == cal.id) {
            add_event(&mut output, cal, event, &generated_at);
        }
        add_line(&mut output, "END:VCALENDAR");
    }
    output
}

/// Keep the generated filename in the working directory, even when the
/// calendar's display name contains slashes, spaces, or shell metacharacters.
pub fn default_filename(calendar_name: Option<&str>) -> String {
    match calendar_name {
        None => "vimcal-all.ics".into(),
        Some(name) => {
            let safe: String = name.chars()
                .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
                .collect();
            format!("vimcal-{}.ics", if safe.is_empty() { "calendar" } else { &safe })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{all_day_bounds, Recurrence};
    use chrono::TimeZone;

    fn event(id: &str, calendar_id: &str, date: NaiveDate) -> CalEvent {
        let (start, end) = all_day_bounds(date, 1);
        CalEvent {
            id: id.into(), calendar_id: calendar_id.into(), title: "Busy; meeting, café\\night\nline".into(),
            start, end, all_day: true, location: "Somewhere, Europe".into(), imported: false,
            recurrence: None, series_id: None, occurrence_index: None,
        }
    }

    #[test]
    fn exports_separate_calendars_and_preserves_unicode_escapes() {
        let mut store = CalendarStore::default();
        let second = Calendar { id: "work".into(), name: "Munkák/Work".into(),
            color: "#FF00EE".into(), visible: false };
        store.calendars.push(second);
        let day = Local::now().date_naive();
        store.events.push(event("one", "default", day));
        store.events.push(event("two", "work", day));
        let calendars: Vec<&Calendar> = store.calendars.iter().collect();
        let exported = serialize(&store, &calendars);
        assert_eq!(exported.matches("BEGIN:VCALENDAR").count(), 2);
        assert_eq!(exported.matches("BEGIN:VEVENT").count(), 2);
        assert!(exported.contains("X-APPLE-CALENDAR-COLOR:#FF00EE"));
        assert!(exported.contains("SUMMARY:Busy\\; meeting\\, café\\\\night\\nline"));
        assert!(exported.contains("DTSTART;VALUE=DATE:"));
        assert!(exported.lines().all(|line| line.trim_end_matches('\r').len() <= 75));
        assert_eq!(default_filename(Some("Munkák/Work")), "vimcal-Munkák_Work.ics");
    }

    #[test]
    fn native_recurrence_uses_rule_and_exdate_instead_of_expansion() {
        let mut store = CalendarStore::default();
        let day = Local::now().date_naive();
        let mut repeating = event("daily", "default", day);
        repeating.recurrence = Some(Recurrence {
            every_days: 3, count: Some(10), excluded: vec![1, 4],
        });
        store.events.push(repeating);
        let exported = serialize(&store, &[&store.calendars[0]]);
        assert!(exported.contains("RRULE:FREQ=DAILY;INTERVAL=3;COUNT=10"));
        assert_eq!(exported.matches("EXDATE;VALUE=DATE:").count(), 2);
        assert_eq!(exported.matches("BEGIN:VEVENT").count(), 1);
        store.events[0].recurrence.as_mut().unwrap().count = None;
        let infinite = serialize(&store, &[&store.calendars[0]]);
        assert!(infinite.contains("RRULE:FREQ=DAILY;INTERVAL=3\r\n"));
        assert!(!infinite.contains("COUNT="));
    }

    #[test]
    fn timed_single_events_use_utc_and_round_trip_import() {
        let mut store = CalendarStore::default();
        let date = Local::now().date_naive();
        let local = date.and_hms_opt(9, 30, 0).unwrap();
        let start = Local.from_local_datetime(&local).earliest().unwrap().with_timezone(&Utc);
        let event = CalEvent { start, end: start + Duration::minutes(90), all_day: false,
            ..event("utc", "default", date) };
        store.events.push(event);
        let content = serialize(&store, &[&store.calendars[0]]);
        assert!(content.contains(&format!("DTSTART:{}", stamp(start))));
        let path = std::env::temp_dir().join(format!("vimcal-export-test-{}.ics", uuid::Uuid::new_v4()));
        std::fs::write(&path, &content).unwrap();
        let parsed = crate::ics::import_file(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        assert_eq!(parsed.events.len(), 1);
        assert_eq!(parsed.events[0].start, start);
        assert_eq!(parsed.events[0].title, store.events[0].title.replace('\n', " "));
        assert_eq!(parsed.events[0].location, store.events[0].location);
    }
}
