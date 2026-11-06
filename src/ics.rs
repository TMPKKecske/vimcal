use crate::model::{self, CalEvent};
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Duration, Local, LocalResult, NaiveDate, NaiveDateTime, TimeZone, Utc};
use ical::{property::Property, IcalParser};
use rrule::{RRule, RRuleSet, Tz as RuleTz, Unvalidated};
use std::{fs::File, io::BufReader, path::Path};

const MAX_OCCURRENCES: u16 = 1000;

#[derive(Default, Debug)]
pub struct ImportResult {
    pub events: Vec<CalEvent>,
    pub skipped: usize,
    pub limited: bool,
}

fn prop<'a>(props: &'a [Property], key: &str) -> Option<&'a Property> {
    props.iter().find(|p| p.name == key)
}

fn value<'a>(props: &'a [Property], key: &str) -> Option<&'a str> {
    prop(props, key)?.value.as_deref()
}

fn param<'a>(property: &'a Property, key: &str) -> Option<&'a str> {
    property.params.as_ref()?.iter()
        .find(|(name, _)| name == key)
        .and_then(|(_, values)| values.first().map(String::as_str))
}

fn unescape(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' { out.push(c); continue; }
        match chars.next() {
            Some('n' | 'N') => out.push(' '),
            Some('\\') => out.push('\\'),
            Some(',') => out.push(','),
            Some(';') => out.push(';'),
            Some(other) => { out.push('\\'); out.push(other); }
            None => out.push('\\'),
        }
    }
    out
}

fn parse_date(property: &Property) -> Result<(DateTime<Utc>, bool, RuleTz)> {
    let raw = property.value.as_deref().ok_or_else(|| anyhow!("missing date value"))?;
    let tz_name = param(property, "TZID").map(|s| s.trim_matches('"'));
    let tz: RuleTz = match tz_name {
        Some(name) => name.parse::<chrono_tz::Tz>()
            .with_context(|| format!("unknown timezone {name}"))?.into(),
        None if raw.ends_with('Z') => RuleTz::UTC,
        None => RuleTz::LOCAL,
    };
    let all_day = raw.len() == 8 || param(property, "VALUE") == Some("DATE");
    if all_day {
        let date = NaiveDate::parse_from_str(raw, "%Y%m%d")?;
        let naive = date.and_hms_opt(0, 0, 0).unwrap();
        let parsed = tz.from_local_datetime(&naive).earliest()
            .ok_or_else(|| anyhow!("invalid all-day date"))?;
        return Ok((parsed.with_timezone(&Utc), true, tz));
    }
    let naive = if raw.ends_with('Z') {
        NaiveDateTime::parse_from_str(raw, "%Y%m%dT%H%M%SZ")?
    } else if raw.len() == 13 {
        NaiveDateTime::parse_from_str(raw, "%Y%m%dT%H%M")?
    } else {
        NaiveDateTime::parse_from_str(raw, "%Y%m%dT%H%M%S")?
    };
    let parsed = match tz.from_local_datetime(&naive) {
        LocalResult::Single(dt) | LocalResult::Ambiguous(dt, _) => dt,
        LocalResult::None => return Err(anyhow!("nonexistent local time: {raw}")),
    };
    Ok((parsed.with_timezone(&Utc), false, tz))
}

// Supports the common RFC 5545 duration forms PnW and PnDTnHnMnS.
fn parse_duration(raw: &str) -> Result<Duration> {
    let raw = raw.strip_prefix('P').ok_or_else(|| anyhow!("invalid ICS duration"))?;
    let mut amount = String::new();
    let mut seconds = 0_i64;
    let mut time_part = false;
    for ch in raw.chars() {
        if ch == 'T' { time_part = true; continue; }
        if ch.is_ascii_digit() { amount.push(ch); continue; }
        let n: i64 = amount.parse().context("invalid ICS duration amount")?;
        amount.clear();
        let mult = match (ch, time_part) {
            ('W', false) => 604800,
            ('D', false) => 86400,
            ('H', true) => 3600,
            ('M', true) => 60,
            ('S', true) => 1,
            _ => return Err(anyhow!("unsupported ICS duration component")),
        };
        seconds = seconds.checked_add(n.checked_mul(mult).ok_or_else(|| anyhow!("duration overflow"))?)
            .ok_or_else(|| anyhow!("duration overflow"))?;
    }
    if !amount.is_empty() || seconds == 0 { return Err(anyhow!("invalid ICS duration")); }
    Ok(Duration::seconds(seconds))
}

fn expand_starts(props: &[Property], initial: DateTime<Utc>, timezone: RuleTz)
    -> Result<(Vec<DateTime<Utc>>, bool)>
{
    let Some(rule_str) = value(props, "RRULE") else {
        return Ok((vec![initial], false));
    };
    let start = initial.with_timezone(&timezone);
    let unvalidated: RRule<Unvalidated> = rule_str.parse().context("parsing RRULE")?;
    let mut rules = RRuleSet::new(start).rrule(unvalidated.validate(start)?);
    for exception in props.iter().filter(|p| p.name == "EXDATE") {
        if let Some(items) = &exception.value {
            for item in items.split(',') {
                let mut single = exception.clone();
                single.value = Some(item.to_string());
                let (date, _, _) = parse_date(&single)?;
                rules = rules.exdate(date.with_timezone(&timezone));
            }
        }
    }
    // Bound infinite and massive recurrences: 1 year back, 3 years forward.
    // Single events (without RRULE) are always imported, regardless of age.
    let lower = (Utc::now() - Duration::days(366)).with_timezone(&timezone);
    let upper = (Utc::now() + Duration::days(3 * 366)).with_timezone(&timezone);
    let result = rules.after(lower).before(upper).all(MAX_OCCURRENCES);
    Ok((result.dates.into_iter().map(|d| d.with_timezone(&Utc)).collect(), result.limited))
}

pub fn import_file(path: &Path) -> Result<ImportResult> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut result = ImportResult::default();
    let mut calendars = 0;
    for parsed in IcalParser::new(BufReader::new(file)) {
        let calendar = parsed.context("invalid iCalendar file")?;
        calendars += 1;
        for event in calendar.events {
            let p = &event.properties;
            if value(p, "STATUS") == Some("CANCELLED") { continue; }
            let item = (|| -> Result<(Vec<CalEvent>, bool)> {
                let start_prop = prop(p, "DTSTART").ok_or_else(|| anyhow!("missing DTSTART"))?;
                let (start, all_day, tz) = parse_date(start_prop)?;
                let end = if let Some(end_prop) = prop(p, "DTEND") {
                    parse_date(end_prop)?.0
                } else if let Some(duration) = value(p, "DURATION") {
                    let length = parse_duration(duration)?;
                    if all_day {
                        anyhow::ensure!(length.num_seconds() % 86400 == 0,
                            "all-day DURATION must be whole days");
                        model::all_day_bounds(start.with_timezone(&Local).date_naive(),
                            length.num_days()).1
                    } else { start + length }
                } else if all_day {
                    // All-day DTSTART without DTEND means one local calendar day.
                    model::all_day_bounds(start.with_timezone(&Local).date_naive(), 1).1
                } else {
                    start + Duration::hours(1)
                };
                if end <= start { return Err(anyhow!("event ends before it starts")); }
                let duration = end - start;
                // Calendar dates, not 24-hour blocks: DST days can be 23 or 25 hours.
                let day_count = (end.with_timezone(&Local).date_naive()
                    - start.with_timezone(&Local).date_naive()).num_days();
                let uid = value(p, "UID").unwrap_or("anonymous");
                let title = unescape(value(p, "SUMMARY").unwrap_or("Untitled event"));
                let location = unescape(value(p, "LOCATION").unwrap_or(""));
                let (starts, limited) = expand_starts(p, start, tz)?;
                let recurring = value(p, "RRULE").is_some();
                let events = starts.into_iter().map(|time| {
                    let end_time = if all_day && day_count > 0 {
                        let date = time.with_timezone(&Local).date_naive();
                        model::local_time(date + Duration::days(day_count), 0).with_timezone(&Utc)
                    } else { time + duration };
                    CalEvent {
                        id: if recurring { format!("ics:{uid}:{}", time.timestamp()) }
                            else { format!("ics:{uid}") },
                        calendar_id: model::DEFAULT_CALENDAR_ID.into(), // assigned by importer
                        title: title.clone(), start: time, end: end_time,
                        all_day, location: location.clone(), imported: true,
                        recurrence: None, series_id: if recurring { Some(format!("ics:{uid}")) }
                            else { None }, occurrence_index: None,
                    }
                }).collect();
                Ok((events, limited))
            })();
            match item {
                Ok((events, limited)) => {
                    result.limited |= limited;
                    result.events.extend(events);
                }
                Err(_) => result.skipped += 1,
            }
        }
    }
    if calendars == 0 { return Err(anyhow!("no VCALENDAR found")); }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_duration() {
        assert_eq!(parse_duration("PT1H30M").unwrap(), Duration::minutes(90));
        assert_eq!(parse_duration("P2D").unwrap(), Duration::days(2));
    }

    #[test]
    fn imports_sample_calendar() {
        let file = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/demo.ics"));
        let imported = import_file(file).unwrap();
        assert_eq!(imported.skipped, 0);
        assert!(imported.events.iter().any(|e| e.title == "Deep work / build in Rust"));
        assert!(imported.events.iter().any(|e| e.all_day));
    }

    #[test]
    fn recognizes_timezone() {
        let p = Property {
            name: "DTSTART".into(),
            params: Some(vec![("TZID".into(), vec!["Europe/Budapest".into()])]),
            value: Some("20261104T090000".into()),
        };
        let (time, all_day, _) = parse_date(&p).unwrap();
        assert!(!all_day);
        assert_eq!(time.format("%Y%m%dT%H%MZ").to_string(), "20261104T0800Z");
    }
}
