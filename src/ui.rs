use crate::{
    app::{App, CalendarEditor, Editor, ImportEditor, Mode, SearchEditor, View},
    vim_input::{VimInput, InputMode},
    model::{ALL_DAY_ROWS, CalEvent, SLOT_MINUTES, SLOTS_PER_DAY, week_start},
};
use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, Utc};
use ratatui::{
    Frame,
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

const BACKGROUND: Color = Color::Rgb(13, 19, 31);
const PANEL: Color = Color::Rgb(20, 29, 45);
const SUBTLE: Color = Color::Rgb(105, 124, 149);
const TEXT: Color = Color::Rgb(222, 232, 244);
const CYAN: Color = Color::Rgb(103, 223, 217);
const YELLOW: Color = Color::Rgb(247, 200, 116);
const SELECTED: Color = Color::Rgb(58, 73, 100);
const BORDER: Color = Color::Rgb(62, 80, 107);

fn panel(title: &'static str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(format!(" {title} "), Style::default().fg(CYAN).add_modifier(Modifier::BOLD)))
        .border_style(Style::default().fg(BORDER))
        .style(Style::default().bg(PANEL).fg(TEXT))
}

fn rgb(hex: &str) -> Color {
    if hex.len() == 7 && hex.starts_with('#') {
        if let (Ok(r), Ok(g), Ok(b)) = (
            u8::from_str_radix(&hex[1..3], 16),
            u8::from_str_radix(&hex[3..5], 16),
            u8::from_str_radix(&hex[5..7], 16),
        ) { return Color::Rgb(r, g, b); }
    }
    CYAN
}

fn event_color(app: &App, event: &CalEvent) -> Color {
    app.data.calendar(&event.calendar_id)
        .map(|cal| rgb(&cal.color)).unwrap_or(CYAN)
}

fn event_background(color: Color) -> Color {
    match color {
        Color::Rgb(r, g, b) => Color::Rgb(
            ((r as u16 * 40 + 13 * 60) / 100) as u8,
            ((g as u16 * 40 + 19 * 60) / 100) as u8,
            ((b as u16 * 40 + 31 * 60) / 100) as u8,
        ),
        _ => PANEL,
    }
}

fn fit(text: &str, max: usize) -> String {
    if max == 0 { return String::new(); }
    let count = text.chars().count();
    if count <= max { return text.into(); }
    if max == 1 { return "…".into(); }
    let mut value: String = text.chars().take(max - 1).collect();
    value.push('…');
    value
}

/// Show the actual insertion point, including inside long UTF-8 strings.
/// Horizontal scrolling keeps the caret in view on narrow terminals.
fn input_display(value: &str, input: &VimInput, max_width: usize) -> String {
    if max_width < 3 { return String::new(); }
    let chars: Vec<char> = value.chars().collect();
    let cursor = input.cursor_for(value);
    let available = max_width.saturating_sub(2);
    let first = cursor.saturating_sub(available.saturating_sub(2));
    let last = chars.len().min(first + available);
    let mut result = String::new();
    if first > 0 { result.push('‹'); }
    for i in first..last {
        if i == cursor { result.push(if input.mode == InputMode::Insert { '▏' } else { '▮' }); }
        result.push(chars[i]);
    }
    if cursor >= last && cursor == chars.len() {
        result.push(if input.mode == InputMode::Insert { '▏' } else { '▮' });
    }
    if last < chars.len() { result.push('›'); }
    result
}

fn input_mode(input: &VimInput) -> &'static str {
    if input.mode == InputMode::Insert { "INSERT" } else { "NORMAL" }
}

fn calendar_name(app: &App, id: &str) -> String {
    app.data.calendar(id).map(|c| c.name.clone()).unwrap_or_else(|| "Unknown".into())
}

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    frame.render_widget(Block::default().style(Style::default().bg(BACKGROUND)), area);
    if area.width < 77 || area.height < 17 {
        frame.render_widget(
            Paragraph::new("vimcal needs a terminal at least 77 columns wide and 17 rows high.")
                .style(Style::default().fg(YELLOW)).wrap(Wrap { trim: true }), area,
        );
        return;
    }
    let outer = Layout::vertical([
        Constraint::Length(3), Constraint::Min(8), Constraint::Length(3),
    ]).split(area);
    render_header(frame, app, outer[0]);
    if app.view == View::Month {
        // A month deserves the same large central canvas as the week grid;
        // do not reduce the day cells to a tiny sidebar mini-calendar.
        render_month(frame, app, outer[1]);
    } else if outer[1].width >= 111 {
        let columns = Layout::horizontal([
            Constraint::Min(72), Constraint::Length(32),
        ]).split(outer[1]);
        render_week(frame, app, columns[0]);
        render_sidebar(frame, app, columns[1]);
    } else { render_week(frame, app, outer[1]); }
    render_footer(frame, app, outer[2]);
    match &app.mode {
        Mode::Edit(editor) => render_editor(frame, area, app, editor),
        Mode::Import(dialog) => render_import(frame, area, app, dialog),
        Mode::Search(dialog) => render_search(frame, area, app, dialog),
        Mode::Delete(id) => render_delete(frame, area, app, id),
        Mode::CalendarList { selected } => render_calendars(frame, area, app, *selected),
        Mode::CalendarEdit(editor) => render_calendar_editor(frame, area, editor),
        Mode::CalendarDelete { id, .. } => render_calendar_delete(frame, area, app, id),
        Mode::Help => render_help(frame, area),
        _ => {},
    }
}

fn render_header(frame: &mut Frame, app: &App, area: Rect) {
    let first = week_start(app.cursor_date);
    let last = first + Duration::days(6);
    let period = if app.view == View::Week {
        format!("{} — {}", first.format("%b %d"), last.format("%b %d, %Y"))
    } else {
        app.cursor_date.format("%B %Y").to_string()
    };
    let mode = match &app.mode {
        Mode::Visual { .. } => "-- VISUAL --",
        Mode::Edit(editor) => if editor.vim.mode == InputMode::Insert { "-- INSERT --" } else { "-- INPUT NORMAL --" },
        Mode::CalendarEdit(editor) => if editor.vim.mode == InputMode::Insert { "-- INSERT --" } else { "-- INPUT NORMAL --" },
        Mode::Import(dialog) => if dialog.vim.mode == InputMode::Insert { "-- IMPORT INSERT --" } else { "-- IMPORT NORMAL --" },
        Mode::Search(dialog) => if dialog.vim.mode == InputMode::Insert { "-- SEARCH INSERT --" } else { "-- SEARCH NORMAL --" },
        Mode::CalendarList { .. } => "-- CALENDARS --",
        _ => "-- NORMAL --",
    };
    let cal = app.data.calendar(&app.data.active_calendar_id);
    let name = cal.map(|c| c.name.as_str()).unwrap_or("?");
    let color = cal.map(|c| rgb(&c.color)).unwrap_or(CYAN);
    frame.render_widget(Paragraph::new(vec![
        Line::from(vec![
            Span::styled("  ◈ V I M C A L   ", Style::default().fg(CYAN).add_modifier(Modifier::BOLD)),
            // Contrasting padded highlight: the period stands out without taking
            // any extra vertical space or changing the text size.
            Span::styled(format!("  {period}  "), Style::default().bg(SELECTED).fg(YELLOW)
                .add_modifier(Modifier::BOLD)),
            Span::styled(format!("   {mode}"), Style::default().fg(YELLOW)),
        ]),
        Line::from(vec![
            Span::styled(format!("  {}  ·  {} events  ·  {} calendars  ·  active: ",
                if app.view == View::Week { format!("Week {}", first.iso_week().week()) }
                else { "Month view".into() },
                app.data.events.len(), app.data.calendars.len()),
                Style::default().fg(SUBTLE)),
            Span::styled(format!("● {name}"), Style::default().fg(color).add_modifier(Modifier::BOLD)),
        ]),
        Line::from(Span::styled("  ──────────────────────────────────────────────────────────────────",
            Style::default().fg(BORDER))),
    ]), area);
}

fn cell_x_and_width(inner: Rect, column: usize) -> (u16, u16) {
    let available = inner.width.saturating_sub(7);
    let basic = available / 7;
    let extra = available % 7;
    let mut x = inner.x + 7;
    for col in 0..column { x += basic + if (col as u16) < extra { 1 } else { 0 }; }
    (x, basic + if (column as u16) < extra { 1 } else { 0 })
}

fn selection_contains(app: &App, date: NaiveDate, slot: usize) -> bool {
    let Mode::Visual { anchor_date, anchor_slot } = &app.mode else { return false; };
    let a = (*anchor_date, *anchor_slot);
    let b = (app.cursor_date, app.cursor_slot);
    let point = (date, slot);
    point >= a.min(b) && point <= a.max(b)
}

fn paint(buf: &mut Buffer, x: u16, y: u16, width: u16, style: Style, label: &str) {
    if width == 0 { return; }
    buf.set_style(Rect::new(x, y, width, 1), style);
    buf.set_stringn(x, y, fit(label, width as usize), width as usize, style);
}

/// Stable left-to-right lanes for overlapping timed events on one local day.
/// Half-open intervals mean an event ending at 10:00 does not overlap one
/// starting at 10:00. Overlapping chains share a common lane count so event
/// blocks never jump between columns halfway through their duration.
#[derive(Debug)]
struct TimedPlacement<'a> {
    event: &'a CalEvent,
    lane: usize,
    lanes: usize,
}

fn timed_placements<'a>(events: &[&'a CalEvent], date: NaiveDate) -> Vec<TimedPlacement<'a>> {
    let day_start = crate::model::local_time(date, 0).with_timezone(&Utc);
    let day_end = crate::model::local_time(date + Duration::days(1), 0).with_timezone(&Utc);
    let mut segments: Vec<(&CalEvent, DateTime<Utc>, DateTime<Utc>)> = events.iter()
        .copied().filter(|event| !event.all_day && event.start < day_end && event.end > day_start)
        .map(|event| (event, event.start.max(day_start), event.end.min(day_end)))
        .collect();
    segments.sort_by(|a, b| a.1.cmp(&b.1).then(a.2.cmp(&b.2))
        .then(a.0.id.cmp(&b.0.id)));
    let mut placements: Vec<TimedPlacement<'a>> = segments.iter().map(|(event, _, _)| {
        TimedPlacement { event: *event, lane: 0, lanes: 1 }
    }).collect();
    let mut group_start = 0;
    while group_start < segments.len() {
        let mut group_end = group_start + 1;
        let mut latest_end = segments[group_start].2;
        while group_end < segments.len() && segments[group_end].1 < latest_end {
            latest_end = latest_end.max(segments[group_end].2);
            group_end += 1;
        }
        let mut occupied_until: Vec<DateTime<Utc>> = Vec::new();
        for i in group_start..group_end {
            let lane = if let Some(lane) = occupied_until.iter()
                .position(|end| *end <= segments[i].1) { lane }
            else {
                occupied_until.push(segments[i].2);
                occupied_until.len() - 1
            };
            occupied_until[lane] = segments[i].2;
            placements[i].lane = lane;
        }
        let total = occupied_until.len();
        for placement in &mut placements[group_start..group_end] {
            placement.lanes = total;
        }
        group_start = group_end;
    }
    placements
}

fn render_week(frame: &mut Frame, app: &App, area: Rect) {
    let block = panel("WEEK GRID · 15 MIN / ROW · ALL-DAY ABOVE");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let header_rows = 1 + ALL_DAY_ROWS;
    if inner.width < 14 || inner.height as usize <= header_rows { return; }
    let monday = week_start(app.cursor_date);
    let days = ["MON", "TUE", "WED", "THU", "FRI", "SAT", "SUN"];
    let visible_rows = inner.height as usize - header_rows;
    // Index visible events once per day, rather than repeatedly scanning every
    // imported occurrence for each 15-minute cell on each screen refresh.
    let day_events: [Vec<&CalEvent>; 7] = std::array::from_fn(|col| {
        let date = monday + Duration::days(col as i64);
        app.visible_events().filter(|e| e.intersects_day(date)).collect()
    });
    let timed: [Vec<TimedPlacement<'_>>; 7] = std::array::from_fn(|col| {
        timed_placements(&day_events[col], monday + Duration::days(col as i64))
    });
    let focused_id = app.selected_event().map(|e| e.id.as_str());
    let buf = frame.buffer_mut();

    paint(buf, inner.x, inner.y, 7, Style::default().bg(PANEL).fg(SUBTLE), " TIME");
    for row in 0..ALL_DAY_ROWS {
        let text = if row == 0 { " ALL-D" } else { "      " };
        paint(buf, inner.x, inner.y + 1 + row as u16, 7,
            Style::default().bg(PANEL).fg(YELLOW), text);
    }
    for col in 0..7 {
        let date = monday + Duration::days(col as i64);
        let (x, w) = cell_x_and_width(inner, col);
        let is_today = date == Local::now().date_naive();
        let is_current_day = date == app.cursor_date;
        let head_style = Style::default()
            .bg(if is_current_day { SELECTED } else { PANEL })
            .fg(if is_today { YELLOW } else { TEXT })
            .add_modifier(if is_current_day { Modifier::BOLD } else { Modifier::empty() });
        paint(buf, x, inner.y, w, head_style, &format!(" {:<3} {:02}", days[col], date.day()));
        let all_day: Vec<&CalEvent> = day_events[col].iter().copied()
            .filter(|e| e.all_day).collect();
        for row in 0..ALL_DAY_ROWS {
            let event = all_day.get(row);
            let overflow = if row == ALL_DAY_ROWS - 1 && all_day.len() > ALL_DAY_ROWS {
                Some(all_day.len() - ALL_DAY_ROWS)
            } else { None };
            let bg = Color::Rgb(35, 40, 56);
            let mut style = Style::default().fg(SUBTLE).bg(bg);
            let mut label = String::new();
            if let Some(e) = event {
                let color = event_color(app, e);
                style = Style::default().fg(color).bg(event_background(color));
                label = format!("▰ {}", e.title);
                if let Some(extra) = overflow { label.push_str(&format!(" +{extra}")); }
                if app.focus_id.as_deref() == Some(e.id.as_str()) && is_current_day {
                    style = style.add_modifier(Modifier::BOLD | Modifier::REVERSED);
                }
            }
            paint(buf, x, inner.y + 1 + row as u16, w, style, &label);
        }
    }

    for row in 0..visible_rows {
        let slot = app.scroll_slot + row;
        if slot >= SLOTS_PER_DAY { break; }
        let y = inner.y + header_rows as u16 + row as u16;
        let minute = slot as i64 * SLOT_MINUTES;
        let time = if minute % 60 == 0 { format!(" {:02}:00", minute / 60) }
            else if minute % 60 == 30 { format!(" {:02}:30", minute / 60) }
            else { "   ·".into() };
        paint(buf, inner.x, y, 7, Style::default().fg(SUBTLE).bg(PANEL), &time);
        for col in 0..7 {
            let date = monday + Duration::days(col as i64);
            let (x, w) = cell_x_and_width(inner, col);
            let highlighted = date == app.cursor_date && slot == app.cursor_slot;
            let in_visual = selection_contains(app, date, slot);
            let default_bg = if slot % 4 == 0 { Color::Rgb(29, 40, 58) }
                             else { Color::Rgb(24, 34, 51) };
            let base_style = Style::default().fg(SUBTLE).bg(default_bg);
            // Paint the day cell once, then paint *each* concurrent event
            // within its own horizontal lane rather than drawing only the first.
            paint(buf, x, y, w, base_style, "");
            let mut occupied = false;
            for placed in &timed[col] {
                let event = placed.event;
                if !event.intersects_slot(date, slot) { continue; }
                occupied = true;
                let count = placed.lanes as u32;
                let lane_x = (placed.lane as u32 * w as u32 / count) as u16;
                let right = ((placed.lane as u32 + 1) * w as u32 / count) as u16;
                let lane_w = right.saturating_sub(lane_x);
                if lane_w == 0 { continue; }
                let color = event_color(app, event);
                let mut style = Style::default().fg(color).bg(event_background(color));
                let is_focused = focused_id == Some(event.id.as_str());
                if highlighted && is_focused {
                    style = Style::default().fg(BACKGROUND).bg(color)
                        .add_modifier(Modifier::BOLD);
                } else if in_visual { style = style.add_modifier(Modifier::REVERSED); }
                let title_start = slot == app.scroll_slot || slot == 0 ||
                    !event.intersects_slot(date, slot - 1);
                let label = if title_start { format!("▌{}", event.title) }
                            else { "▌".into() };
                paint(buf, x + lane_x, y, lane_w, style, &label);
            }
            if !occupied && in_visual {
                paint(buf, x, y, w, Style::default().fg(TEXT)
                    .bg(Color::Rgb(86, 74, 105)), "▒▒");
            }
            if !occupied && highlighted {
                paint(buf, x, y, w, Style::default().fg(Color::Black)
                    .bg(Color::Rgb(112, 136, 166)).add_modifier(Modifier::BOLD), "▸ cursor");
            }
        }
    }
}

fn month_cell(inner: Rect, column: usize) -> (u16, u16) {
    let base = inner.width / 7;
    let extra = inner.width % 7;
    let col = column as u16;
    (inner.x + base * col + col.min(extra), base + (if col < extra { 1 } else { 0 }))
}

fn render_month(frame: &mut Frame, app: &App, area: Rect) {
    let block = panel("MONTH GRID · m WEEK · h/l DAY · j/k WEEK");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height < 2 { return; }
    let month_first = NaiveDate::from_ymd_opt(app.cursor_date.year(), app.cursor_date.month(), 1)
        .expect("valid selected month");
    let next_month = month_first.checked_add_months(chrono::Months::new(1))
        .expect("a supported calendar month");
    let month_last = next_month - Duration::days(1);
    let grid_start = week_start(month_first);
    let week_count = ((month_last - grid_start).num_days() / 7 + 1) as usize;
    let grid_height = inner.height - 1;
    let row_height = grid_height / week_count as u16;
    let extra_rows = grid_height % week_count as u16;
    let today = Local::now().date_naive();
    let buf = frame.buffer_mut();
    for (col, day) in ["MON", "TUE", "WED", "THU", "FRI", "SAT", "SUN"]
        .iter().enumerate() {
        let (x, w) = month_cell(inner, col);
        paint(buf, x, inner.y, w, Style::default().fg(CYAN).bg(PANEL)
            .add_modifier(Modifier::BOLD), &format!("  {day}"));
    }
    let mut y = inner.y + 1;
    for week in 0..week_count {
        let height = row_height + (if (week as u16) < extra_rows { 1 } else { 0 });
        for col in 0..7 {
            let date = grid_start + Duration::days((week * 7 + col) as i64);
            let (x, w) = month_cell(inner, col);
            if height == 0 || w == 0 { continue; }
            let current = date == app.cursor_date;
            let in_month = date.month() == app.cursor_date.month();
            let bg = if current { SELECTED }
                else if !in_month { Color::Rgb(16, 23, 35) }
                else if week % 2 == 0 { Color::Rgb(24, 34, 51) }
                else { Color::Rgb(28, 39, 58) };
            buf.set_style(Rect::new(x, y, w, height), Style::default().bg(bg));
            let day_style = Style::default().fg(if date == today { YELLOW }
                else if in_month { TEXT } else { SUBTLE }).bg(bg)
                .add_modifier(if current || date == today { Modifier::BOLD }
                    else { Modifier::empty() });
            let marker = if date == today { " •" } else { "" };
            paint(buf, x, y, w, day_style, &format!(" {:02}{marker}", date.day()));
            let available = height.saturating_sub(1) as usize;
            if available == 0 { continue; }
            let mut events: Vec<&CalEvent> = app.visible_events()
                .filter(|event| event.intersects_day(date)).collect();
            events.sort_by(|a, b| b.all_day.cmp(&a.all_day)
                .then(a.start.cmp(&b.start)).then(a.id.cmp(&b.id)));
            let truncated = events.len() > available;
            let shown = if truncated { available.saturating_sub(1) }
                        else { events.len() };
            for (idx, event) in events.iter().take(shown).enumerate() {
                let color = event_color(app, event);
                let label = if event.all_day {
                    format!("▰ {}", event.title)
                } else if w >= 19 {
                    format!("{} {}", event.start.with_timezone(&Local).format("%H:%M"), event.title)
                } else { format!("▌{}", event.title) };
                let mut style = Style::default().fg(color).bg(event_background(color));
                if current && app.focus_id.as_deref() == Some(event.id.as_str()) {
                    style = Style::default().fg(BACKGROUND).bg(color)
                        .add_modifier(Modifier::BOLD);
                }
                paint(buf, x, y + 1 + idx as u16, w, style, &label);
            }
            if truncated {
                paint(buf, x, y + height - 1, w,
                    Style::default().fg(YELLOW).bg(bg),
                    &format!(" +{} more", events.len() - shown));
            }
        }
        y += height;
    }
}

fn event_time(event: &CalEvent) -> String {
    if event.all_day {
        let days = event.all_day_days();
        return if days == 1 { "All day".into() } else { format!("All day · {days} days") };
    }
    let start = event.start.with_timezone(&Local);
    let end = event.end.with_timezone(&Local);
    if start.date_naive() == end.date_naive() {
        format!("{}–{}", start.format("%H:%M"), end.format("%H:%M"))
    } else { format!("{} → {} {}", start.format("%H:%M"), end.format("%a"), end.format("%H:%M")) }
}

fn render_sidebar(frame: &mut Frame, app: &App, area: Rect) {
    let show_legend = area.height >= 23;
    let pieces = if show_legend {
        Layout::vertical([Constraint::Length(9), Constraint::Min(5), Constraint::Length(8)]).split(area)
    } else {
        Layout::vertical([Constraint::Length(9), Constraint::Min(5), Constraint::Length(0)]).split(area)
    };
    let date_label = app.cursor_date.format("%A, %b %d").to_string();
    let slot_time = format!("{:02}:{:02}", app.cursor_slot / 4, app.cursor_slot % 4 * 15);
    let focus = match app.selected_event() {
        Some(e) => {
            let fg = event_color(app, e);
            vec![
                Line::from(Span::styled(date_label, Style::default().fg(SUBTLE))),
                Line::from(Span::styled(format!(" {} ", fit(&e.title, 26)),
                    Style::default().fg(fg).add_modifier(Modifier::BOLD))),
                Line::from(format!(" {}", event_time(e))),
                Line::from(format!(" {} · {}", calendar_name(app, &e.calendar_id),
                    if e.imported { "ICS" } else { "local" })),
                Line::from(format!(" {}", fit(&e.location, 27))),
                Line::from(""),
                Line::from(Span::styled(" +/- resize · </> shift", Style::default().fg(CYAN))),
            ]
        }
        None => vec![
            Line::from(Span::styled(date_label, Style::default().fg(SUBTLE))),
            Line::from(Span::styled(format!(" {slot_time}  FREE SLOT"), Style::default().fg(CYAN)
                .add_modifier(Modifier::BOLD))),
            Line::from(""),
            Line::from(" a      add timed event"),
            Line::from(" d      add all-day event"),
            Line::from(" c      manage calendars"),
        ],
    };
    frame.render_widget(Paragraph::new(focus).block(panel("FOCUS")), pieces[0]);

    let mut events: Vec<&CalEvent> = app.visible_events()
        .filter(|e| e.intersects_day(app.cursor_date)).collect();
    events.sort_by_key(|e| (e.start, e.id.clone()));
    let mut agenda = vec![
        Line::from(Span::styled(format!(" {} visible event(s) today", events.len()),
            Style::default().fg(SUBTLE))),
        Line::from(""),
    ];
    for event in events.iter().take((pieces[1].height as usize).saturating_sub(5) / 2) {
        let fg = event_color(app, event);
        agenda.push(Line::from(vec![
            Span::styled(" ▌", Style::default().fg(fg)),
            Span::styled(fit(&event.title, 25), Style::default().fg(TEXT)),
        ]));
        agenda.push(Line::from(Span::styled(format!("   {}", event_time(event)), Style::default().fg(SUBTLE))));
    }
    frame.render_widget(Paragraph::new(agenda).block(panel("AGENDA")), pieces[1]);

    if show_legend {
        let mut lines = Vec::new();
        for cal in app.data.calendars.iter().take(6) {
            let active = if cal.id == app.data.active_calendar_id { "▸" } else { " " };
            let visibility = if cal.visible { "" } else { " [hidden]" };
            lines.push(Line::from(vec![
                Span::styled(format!(" {active}● "), Style::default().fg(rgb(&cal.color))),
                Span::styled(fit(&format!("{}{}", cal.name, visibility), 25),
                    Style::default().fg(if cal.visible { TEXT } else { SUBTLE })),
            ]));
        }
        frame.render_widget(Paragraph::new(lines).block(panel("CALENDARS · c")), pieces[2]);
    }
}

fn render_footer(frame: &mut Frame, app: &App, area: Rect) {
    let status = fit(&app.status, area.width.saturating_sub(5) as usize);
    frame.render_widget(Paragraph::new(vec![
        Line::from(vec![
            Span::styled("  ▍ ", Style::default().fg(CYAN)),
            Span::styled(status, Style::default().fg(TEXT)),
        ]),
        Line::from(vec![
            Span::styled("  h/j/k/l", Style::default().fg(CYAN)), Span::styled(" move  ", Style::default().fg(SUBTLE)),
            Span::styled("a", Style::default().fg(CYAN)), Span::styled(" add  ", Style::default().fg(SUBTLE)),
            Span::styled("d", Style::default().fg(CYAN)), Span::styled(" all-day  ", Style::default().fg(SUBTLE)),
            Span::styled("c", Style::default().fg(CYAN)), Span::styled(" calendars  ", Style::default().fg(SUBTLE)),
            Span::styled("m", Style::default().fg(CYAN)), Span::styled(" week/month  ", Style::default().fg(SUBTLE)),
            Span::styled("/", Style::default().fg(CYAN)), Span::styled(" search  ", Style::default().fg(SUBTLE)),
            Span::styled("i", Style::default().fg(CYAN)), Span::styled(" import", Style::default().fg(SUBTLE)),
        ]),
        Line::from(Span::styled(if app.view == View::Week {
            "  j/k 15 min · Tab event · v range · +/- duration · g today · x delete · ? help · q quit"
        } else {
            "  h/l day · j/k week · Tab event · a/d add · g today · x delete · ? help · q quit"
        }, Style::default().fg(SUBTLE))),
    ]), area);
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = area.width.min(width);
    let h = area.height.min(height);
    Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h)
}

fn popup(frame: &mut Frame, area: Rect, title: &'static str) -> Rect {
    frame.render_widget(Clear, area);
    let block = panel(title).border_style(Style::default().fg(CYAN));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    inner
}

fn render_editor(frame: &mut Frame, screen: Rect, app: &App, editor: &Editor) {
    let inner = popup(frame, centered(screen, 77, 21), "EVENT EDITOR · VIM INPUT");
    let cal_name = calendar_name(app, &editor.calendar_id);
    let allday = if editor.all_day { "Yes" } else { "No" };
    let duration_label = if editor.all_day { "Duration (days)" } else { "Duration (min)" };
    let time = if editor.all_day { "(disabled)" } else { &editor.time };
    let recurring = if editor.recurring { "Yes" } else { "No" };
    let ends = if editor.forever { "Forever" } else { "After N occurrences" };
    let every = if editor.recurring { editor.every_days.as_str() } else { "(off)" };
    let count = if !editor.recurring { "(off)" } else if editor.forever {
        "∞" } else { editor.repeat_count.as_str() };
    let fields = [
        ("Title", editor.title.as_str(), true),
        ("Date", editor.date.as_str(), true),
        ("All day", allday, false),
        ("Time", time, !editor.all_day),
        (duration_label, editor.duration.as_str(), true),
        ("Calendar", cal_name.as_str(), false),
        ("Repeat", recurring, false),
        ("Every N days", every, editor.recurring),
        ("Repeat ends", ends, false),
        ("Occurrences", count, editor.recurring && !editor.forever),
    ];
    let mut lines = vec![
        Line::from(Span::styled(format!(" {} MODE · j/k fields (NORMAL) · Enter save · Esc cancel (NORMAL)",
            input_mode(&editor.vim)), Style::default().fg(YELLOW))),
        if editor.error.is_empty() { Line::from("") } else {
            Line::from(Span::styled(fit(&editor.error, inner.width as usize),
                Style::default().fg(YELLOW)))
        },
    ];
    for (index, (name, value, editable)) in fields.iter().enumerate() {
        let active = editor.field == index;
        let style = if active { Style::default().bg(SELECTED).fg(CYAN).add_modifier(Modifier::BOLD) }
                    else { Style::default().fg(TEXT) };
        let visible = if active && *editable {
            input_display(value, &editor.vim, inner.width.saturating_sub(26) as usize)
        } else { fit(value, inner.width.saturating_sub(27) as usize) };
        lines.push(Line::from(Span::styled(format!(" {} {:<19} {}",
            if active { "▸" } else { " " }, name, visible), style)));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(" Esc: INSERT → NORMAL → close · j/k fields · i/a insert · w/b, x, dw, u",
        Style::default().fg(CYAN))));
    lines.push(Line::from(Span::styled(" h/l adjusts toggles/numbers; text h/l moves caret · Tab switches fields",
        Style::default().fg(SUBTLE))));
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_import(frame: &mut Frame, screen: Rect, app: &App, dialog: &ImportEditor) {
    let inner = popup(frame, centered(screen, 70, 10), "IMPORT ICALENDAR · CHOOSE DESTINATION");
    let name = calendar_name(app, &dialog.calendar_id);
    let color = app.data.calendar(&dialog.calendar_id).map(|c| rgb(&c.color)).unwrap_or(CYAN);
    frame.render_widget(Paragraph::new(vec![
        Line::from(""),
        Line::from(Span::styled(" Path to .ics file:", Style::default().fg(SUBTLE))),
        Line::from(Span::styled(format!(" {} > {}{}", if dialog.field == 0 { "▸" } else { " " },
            if dialog.field == 0 { input_display(&dialog.path, &dialog.vim,
                inner.width.saturating_sub(8) as usize) } else {
                fit(&dialog.path, inner.width.saturating_sub(8) as usize) }, ""),
            Style::default().fg(if dialog.field == 0 { CYAN } else { TEXT }))),
        Line::from(""),
        Line::from(vec![
            Span::styled(if dialog.field == 1 { " ▸ Calendar: " } else { "   Calendar: " },
                Style::default().fg(SUBTLE)),
            Span::styled(format!("● {name}  ◀ ▶"), Style::default().fg(color)),
        ]),
        Line::from(""),
        Line::from(Span::styled(" Tab field · ←/→ calendar · Esc input normal · Ctrl-C cancel",
            Style::default().fg(SUBTLE))),
    ]), inner);
}

fn render_search(frame: &mut Frame, screen: Rect, app: &App, search: &SearchEditor) {
    let height = screen.height.saturating_sub(4).min(24);
    let inner = popup(frame, centered(screen, 90, height), "SEARCH EVENTS · / ");
    let results = app.search_results(&search.query);
    let mut lines = vec![
        Line::from(Span::styled(" Type to filter by event title, location, or calendar",
            Style::default().fg(SUBTLE))),
        Line::from(Span::styled(format!(" / {}", input_display(&search.query, &search.vim,
            inner.width.saturating_sub(7) as usize)), Style::default().fg(YELLOW)
            .add_modifier(Modifier::BOLD))),
        Line::from(Span::styled(format!(" {} match(es) · visible calendars only", results.len()),
            Style::default().fg(SUBTLE))),
        Line::from(""),
    ];
    let room = inner.height.saturating_sub(6) as usize;
    let start = search.selected.saturating_sub(room.saturating_sub(1));
    if search.query.trim().is_empty() {
        lines.push(Line::from(Span::styled(" Start typing to find an event…",
            Style::default().fg(SUBTLE))));
    } else if results.is_empty() {
        lines.push(Line::from(Span::styled(" No matching events",
            Style::default().fg(YELLOW))));
    } else {
        for (index, event) in results.iter().enumerate().skip(start).take(room) {
            let color = event_color(app, event);
            let stamp = event.start.with_timezone(&Local).format("%Y-%m-%d");
            let prefix = if event.all_day { "all-day".into() }
                else { event.start.with_timezone(&Local).format("%H:%M").to_string() };
            let title = format!(" {} {} {:<7} {} · {}",
                if index == search.selected { "▸" } else { " " },
                stamp, prefix, event.title, calendar_name(app, &event.calendar_id));
            let style = Style::default().fg(color)
                .bg(if index == search.selected { SELECTED } else { PANEL })
                .add_modifier(if index == search.selected { Modifier::BOLD }
                    else { Modifier::empty() });
            lines.push(Line::from(Span::styled(fit(&title, inner.width as usize), style)));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(" ↑/↓ matches · Enter jump · Esc input normal · Ctrl-C close",
        Style::default().fg(CYAN))));
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_delete(frame: &mut Frame, screen: Rect, app: &App, id: &str) {
    let inner = popup(frame, centered(screen, 64, 9), "DELETE EVENT?");
    let event = app.shown_events.iter().find(|e| e.id == id);
    let title = event.map(|e| e.title.as_str()).unwrap_or("selected event");
    let recurring = event.is_some_and(|e| e.series_id.is_some());
    let mut lines = vec![
        Line::from(""), Line::from(format!(" Delete: {}", fit(title, 48))),
        Line::from(""),
    ];
    if recurring {
        lines.push(Line::from(Span::styled(" o  Only this occurrence", Style::default().fg(CYAN))));
        lines.push(Line::from(Span::styled(" a  Entire recurring series", Style::default().fg(YELLOW))));
        lines.push(Line::from(" Any other key cancels."));
    } else {
        lines.push(Line::from(Span::styled(" y / o  Delete event · any other key cancel",
            Style::default().fg(YELLOW))));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_calendars(frame: &mut Frame, screen: Rect, app: &App, selected: usize) {
    let height = (app.data.calendars.len() + 8).min(22) as u16;
    let inner = popup(frame, centered(screen, 70, height), "CALENDARS · MANAGE");
    let mut lines = vec![
        Line::from(Span::styled(" j/k select · Enter activate · Space show/hide", Style::default().fg(CYAN))),
        Line::from(Span::styled(" a add · e rename/recolor · x delete · Esc close", Style::default().fg(SUBTLE))),
        Line::from(""),
    ];
    let room = (inner.height as usize).saturating_sub(5).max(1);
    let start = selected.saturating_sub(room - 1);
    for (i, cal) in app.data.calendars.iter().enumerate().skip(start).take(room) {
        let style = if i == selected { Style::default().bg(SELECTED).fg(TEXT) }
                    else { Style::default().fg(TEXT) };
        let active = if cal.id == app.data.active_calendar_id { " active" } else { "" };
        let visible = if cal.visible { "shown" } else { "hidden" };
        lines.push(Line::from(vec![
            Span::styled(if i == selected { " ▸ " } else { "   " }, style),
            Span::styled("● ", Style::default().fg(rgb(&cal.color)).bg(if i == selected { SELECTED } else { PANEL })),
            Span::styled(format!("{:<22} {}  {}{active}", fit(&cal.name, 22),
                cal.color, visible), style),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(" Deleting a calendar moves its events to another calendar.",
        Style::default().fg(SUBTLE))));
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_calendar_editor(frame: &mut Frame, screen: Rect, editor: &CalendarEditor) {
    let inner = popup(frame, centered(screen, 72, 12), "CALENDAR · NAME & COLOR");
    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(format!(" {} MODE · Color: #RRGGBB (e.g. #FF8C42)",
            input_mode(&editor.vim)), Style::default().fg(SUBTLE))),
        Line::from(""),
    ];
    for (i, (name, value)) in [("Name", &editor.name), ("Color", &editor.color)].iter().enumerate() {
        let style = if i == editor.field { Style::default().bg(SELECTED).fg(CYAN) }
                    else { Style::default().fg(TEXT) };
        let visible = if i == editor.field {
            input_display(value, &editor.vim, inner.width.saturating_sub(16) as usize)
        } else { fit(value, inner.width.saturating_sub(16) as usize) };
        lines.push(Line::from(Span::styled(format!(" {} {:<8} {}",
            if i == editor.field { "▸" } else { " " }, name, visible), style)));
        lines.push(Line::from(""));
    }
    lines.push(Line::from(Span::styled(" Tab field · Enter save · Esc input NORMAL · Ctrl-C cancel",
        Style::default().fg(CYAN))));
    if !editor.error.is_empty() {
        lines.push(Line::from(Span::styled(fit(&editor.error, inner.width as usize), Style::default().fg(YELLOW))));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_calendar_delete(frame: &mut Frame, screen: Rect, app: &App, id: &str) {
    let inner = popup(frame, centered(screen, 66, 9), "DELETE CALENDAR?");
    let name = calendar_name(app, id);
    let count = app.data.events.iter().filter(|e| e.calendar_id == id).count();
    frame.render_widget(Paragraph::new(vec![
        Line::from(""), Line::from(format!(" Delete calendar: {}", fit(&name, 40))),
        Line::from(""), Line::from(format!(" {count} event(s) will move to another calendar.")),
        Line::from(""),
        Line::from(Span::styled(" y confirm · any other key cancel", Style::default().fg(YELLOW))),
    ]), inner);
}

fn render_help(frame: &mut Frame, screen: Rect) {
    let inner = popup(frame, centered(screen, 72, 31), "VIMCAL · KEYBOARD MAP");
    let help = [
        " NORMAL · WEEK VIEW",
        "   h/l and Left/Right       previous / next day",
        "   j/k and Down/Up         down / up 15 minutes",
        "   m                       toggle WEEK / MONTH view",
        "   /                       SEARCH event title/location/calendar",
        "   g / 0 / G               today / start / end of day",
        "   a                       add timed event at cursor",
        "   d                       add ALL-DAY event (1+ days)",
        "   e / Enter               edit selected event",
        "   + / -                   grow / shrink event",
        "   < / >                   move event earlier/later",
        "   H / L                   move event previous/next day",
        "   Tab / Shift-Tab         next/previous event on day",
        "   v                       VISUAL time range (week view)",
        "   i                       import .ics into chosen calendar",
        "   c                       manage calendars",
        "   x                       delete event (confirm)",
        "",
        " MONTH VIEW · fills calendar area",
        "   h/l move one DAY · j/k move one WEEK",
        "   a/d add event on selected day · Tab cycle events",
        "   m return to week · / search · g today",
        "",
        " SEARCH · type words, ↑/↓ select, Enter jump",
        " CALENDARS · c then a add / e edit / Space show-hide",
        " EDITOR · NORMAL: j/k field · h/l options/numbers · Enter save",
        "   Repeat every N days · N times or forever",
        "   Esc: INSERT -> NORMAL -> close editor (never quits app)",
        "   i/a/I/A insert · h/l/w/b/0/$ move · x/dw/cw/u edit",
        "   Ctrl-C cancels input dialog",
        " DELETE · o one occurrence / a entire series",
        "",
        "   q quit from main calendar ONLY · autosaved",
    ];
    let lines: Vec<Line> = help.iter().map(|line| Line::from(*line)).collect();
    frame.render_widget(Paragraph::new(lines).style(Style::default().fg(TEXT)), inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{self, DEFAULT_CALENDAR_ID};

    fn timed(id: &str, date: NaiveDate, start: i64, duration: i64) -> CalEvent {
        let begins = model::local_time(date, start).with_timezone(&Utc);
        CalEvent {
            id: id.into(), calendar_id: DEFAULT_CALENDAR_ID.into(),
            title: id.into(), start: begins, end: begins + Duration::minutes(duration),
            all_day: false, location: String::new(), imported: false,
            recurrence: None, series_id: None, occurrence_index: None,
        }
    }

    #[test]
    fn overlapping_events_use_distinct_stable_lanes() {
        let date = NaiveDate::from_ymd_opt(2026, 11, 6).unwrap();
        let a = timed("A", date, 9 * 60, 90);
        let b = timed("B", date, 9 * 60 + 15, 60);
        let items = [&a, &b];
        let lanes = timed_placements(&items, date);
        assert_eq!(lanes.len(), 2);
        assert_eq!((lanes[0].lane, lanes[0].lanes), (0, 2));
        assert_eq!((lanes[1].lane, lanes[1].lanes), (1, 2));
    }

    #[test]
    fn touching_events_share_full_width_without_overlapping() {
        let date = NaiveDate::from_ymd_opt(2026, 11, 6).unwrap();
        let a = timed("A", date, 9 * 60, 60);
        let b = timed("B", date, 10 * 60, 60);
        let items = [&a, &b];
        let lanes = timed_placements(&items, date);
        assert!(lanes.iter().all(|lane| lane.lane == 0 && lane.lanes == 1));
    }

    #[test]
    fn chained_overlaps_keep_shared_lane_width() {
        let date = NaiveDate::from_ymd_opt(2026, 11, 6).unwrap();
        let a = timed("A", date, 9 * 60, 90);
        let b = timed("B", date, 10 * 60, 90);
        let c = timed("C", date, 11 * 60, 90);
        let items = [&a, &b, &c];
        let lanes = timed_placements(&items, date);
        assert_eq!(lanes.len(), 3);
        assert!(lanes.iter().all(|lane| lane.lanes == 2));
        assert_eq!(lanes.iter().map(|lane| lane.lane).collect::<Vec<_>>(), vec![0, 1, 0]);
    }
}
