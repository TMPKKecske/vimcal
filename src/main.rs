mod app;
mod ics;
mod ics_export;
mod model;
mod vim_input;
mod ui;

use anyhow::{Context, Result};
use app::{App, Mode};
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyboardEnhancementFlags,
        PushKeyboardEnhancementFlags, PopKeyboardEnhancementFlags},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::{io, path::PathBuf, time::{Duration, Instant}};

fn usage() {
    println!("vimcal — Vim-style terminal calendar\n\n\
        Usage: vimcal [--import FILE.ics] [--calendar NAME] [--data events.json]\n\
               vimcal export all [--output FILE.ics] [--data events.json]\n\
               vimcal export <calendar-name> [-o FILE.ics] [--data events.json]\n\n\
        Exports: default filename is vimcal-all.ics or vimcal-<name>.ics in the current directory.\n\
        All includes hidden calendars; existing export files are overwritten.\n\n\
        Navigation: h/j/k/l = day/time; gg = 08:00; t = today; m = week/month; / = search;\n\
        a = timed; d = all-day; c = calendars; +/- = duration; i = import .ics;\n\
        v = visual range; ? = help; q = quit.\n\n\
        Event editor: Esc INSERT → NORMAL → close editor; NORMAL j/k switch fields,\n\
        h/l adjust options or numbers; i/a insert text; Enter saves.\n\n\
        --calendar NAME selects or creates a destination calendar for --import.\n\
        Environment: VIMCAL_DATA overrides the default event storage path.");
}

fn is_navigation_key(app: &App, key: &KeyEvent) -> bool {
    match &app.mode {
        Mode::Normal | Mode::Visual { .. } => matches!(key.code,
            KeyCode::Char('h' | 'j' | 'k' | 'l') |
            KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down),
        Mode::CalendarList { .. } => matches!(key.code,
            KeyCode::Char('j' | 'k') | KeyCode::Up | KeyCode::Down),
        _ => false,
    }
}

fn run_app(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App) -> Result<()> {
    loop {
        let screen = terminal.size()?;
        app.ensure_visible(screen.height);
        terminal.draw(|frame| ui::draw(frame, app))?;

        if !event::poll(Duration::from_millis(35))? { continue; }

        // Drain pending terminal input *before* drawing again. On terminals without
        // key-release reporting, auto-repeat emits ordinary KeyPress events; if we
        // render after every one, the input queue can stay seconds behind a release.
        // Limit movement to three steps per redraw and discard excess repeat bursts.
        // This preserves smooth hold-to-move while preventing "ghost movement".
        let burst_start = Instant::now();
        let mut moves = 0;
        loop {
            if let Event::Key(key) = event::read()? {
                let movement = is_navigation_key(app, &key);
                if !movement || moves < 3 {
                    if movement { moves += 1; }
                    if app.on_key(key) { return Ok(()); }
                }
            }
            if burst_start.elapsed() >= Duration::from_millis(12) || !event::poll(Duration::ZERO)? {
                break;
            }
        }
    }
}

fn main() -> Result<()> {
    let mut import = None;
    let mut calendar = None;
    let mut export_target = None;
    let mut output = None;
    let mut data = model::default_data_path();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => { usage(); return Ok(()); }
            "export" if export_target.is_none() => {
                export_target = Some(args.next().context("export needs 'all' or a calendar name")?);
            }
            "--import" => import = Some(args.next().context("--import needs a filename")?),
            "--calendar" => calendar = Some(args.next().context("--calendar needs a name")?),
            "--output" | "-o" => output = Some(PathBuf::from(args.next()
                .context("--output needs a filename")?)),
            "--data" => data = PathBuf::from(args.next().context("--data needs a filename")?),
            other => anyhow::bail!("unknown argument: {other}. Try --help"),
        }
    }

    if let Some(target) = export_target {
        anyhow::ensure!(import.is_none() && calendar.is_none(),
            "export cannot be combined with --import or --calendar");
        let store = model::load_data(&data)?;
        let all = target.eq_ignore_ascii_case("all");
        let selected: Vec<&model::Calendar> = if all {
            store.calendars.iter().collect()
        } else {
            vec![store.calendars.iter().find(|c| c.name.eq_ignore_ascii_case(&target))
                .with_context(|| format!("calendar '{target}' not found; available: {}",
                    store.calendars.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(", ")))?]
        };
        let path = output.unwrap_or_else(|| PathBuf::from(ics_export::default_filename(
            if all { None } else { Some(selected[0].name.as_str()) })));
        let contents = ics_export::serialize(&store, &selected);
        std::fs::write(&path, contents.as_bytes())
            .with_context(|| format!("writing export to {}", path.display()))?;
        let event_count = store.events.iter().filter(|event|
            selected.iter().any(|cal| cal.id == event.calendar_id)).count();
        println!("Exported {} calendar(s), {event_count} stored event(s) to {}",
            selected.len(), path.display());
        return Ok(());
    }
    anyhow::ensure!(output.is_none(), "--output is only valid with export");
    let mut app = App::new(data)?;
    if let Some(name) = calendar { app.select_or_create_calendar(&name)?; }
    if let Some(path) = import { app.import_path(&path); }

    enable_raw_mode().context("enabling terminal raw mode")?;
    let mut stdout = io::stdout();
    if let Err(err) = execute!(stdout, EnterAlternateScreen) {
        disable_raw_mode()?;
        return Err(err.into());
    }
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = match Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(err) => {
            disable_raw_mode()?;
            execute!(io::stdout(), LeaveAlternateScreen)?;
            return Err(err.into());
        }
    };

    // In Kitty-keyboard compatible terminals, request explicit Repeat/Release
    // events (including unmodified letter keys). The buffered input loop above
    // remains the fallback for terminals with legacy press-only input.
    let enhanced = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
    if enhanced {
        execute!(terminal.backend_mut(), PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES,
        ))?;
    }
    let result = run_app(&mut terminal, &mut app);
    if enhanced { execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags)?; }
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}
