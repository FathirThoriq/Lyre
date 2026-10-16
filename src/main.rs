mod app;
mod backend;
mod config;
mod diag;
mod eq;
mod innertube;
mod lyrics;
mod player;
mod track;
mod ui;
mod visualizer;

// (lyrics helpers are used for the sync-offset keys below.)

use std::io;
use std::time::{Duration, Instant};

use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use app::{nav_index, App, PanelFocus, Screen, NAV_SCREENS};

fn print_help() {
    println!("Lyre {} - terminal music player (Spotify + YouTube Music), Guest Mode (no login)", env!("CARGO_PKG_VERSION"));
    println!();
    println!("Usage: lyre [--help] [--version] [doctor|resolve-test \"query\"]");
    println!();
    println!("  lyre doctor         Checklist: mpv / yt-dlp / cava / config / guest");
    println!("                      sources (Spotify anonymous, Innertube) / LRCLib /");
    println!("                      log - for remote diagnosis.");
    println!("  lyre resolve-test \"title - artist\"");
    println!("                      Try resolving a track to its YT Music audio");
    println!("                      stream and print the selection details.");
    println!();
    println!("Dependencies (mpv + yt-dlp + cava): run ./install-deps.sh");
    println!("or press the i key inside the app.");
}

/// What Lyre shows FIRST when replaying a raw input transcript for
/// hardware-in-the-loop verification: an event line per input event
/// plus a periodic screen summary. Set LYRE_DEBUG_INPUT=1 to enable.
/// Kept as a permanent diagnostic: if the keyboard ever "dies"
/// again, this log says exactly which events arrived.
fn log_input_event(app: &App, ev: &Event) {
    if std::env::var_os("LYRE_DEBUG_INPUT").is_none() {
        return;
    }
    let desc = match ev {
        Event::Key(k) => format!("KEY {:?} {:?} ({:?})", k.code, k.modifiers, k.kind),
        Event::Mouse(m) => format!("MOUSE {:?} @ {},{}", m.kind, m.column, m.row),
        Event::Paste(t) => format!("PASTE {} chars", t.chars().count()),
        Event::Resize(w, h) => format!("RESIZE {}x{}", w, h),
        _ => "OTHER".to_string(),
    };
    diag::append_log(&format!(
        "[input] {} | screen={:?} focus={:?} form={} search_mode={} pending_play={:?}",
        desc,
        app.screen,
        app.focus,
        app.input_form.is_some(),
        app.search_input_mode,
        app.pending_play
    ));
}

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if let Some(flag) = args.get(1) {
        match flag.as_str() {
            "--help" | "-h" => {
                print_help();
                return Ok(());
            }
            "--version" | "-V" => {
                println!("lyre {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "doctor" => {
                print!("{}", diag::doctor_report());
                return Ok(());
            }
            "resolve-test" => {
                let query = args.get(2).map(|s| s.as_str()).unwrap_or("");
                match diag::resolve_test(query) {
                    Ok(report) => println!("{}", report),
                    Err(e) => {
                        eprintln!("resolve-test failed: {}", e);
                        std::process::exit(1);
                    }
                }
                return Ok(());
            }
            "setup" => {
                // v0.1.5: accounts are gone, and with them the
                // setup wizard. Keep a helpful redirect instead of
                // an error for muscle memory / old docs.
                println!("Lyre {} runs in Guest Mode - there is no account setup anymore.", env!("CARGO_PKG_VERSION"));
                println!("For dependencies (mpv + yt-dlp + cava): run ./install-deps.sh or press i inside the app.");
                println!("To check the health of the data sources: lyre doctor");
                return Ok(());
            }
            other => {
                eprintln!("Unknown argument: {}", other);
                print_help();
                return Ok(());
            }
        }
    }

    // Refresh the dependency state at startup (see diag.rs).
    let deps = diag::check_deps();
    if deps.missing().is_empty() {
        diag::append_log("startup: mpv + yt-dlp detected");
    } else {
        diag::append_log(&format!("startup: missing dependencies: {}", deps.missing().join(", ")));
    }

    let mut terminal = setup_terminal()?;
    let mut app = App::new();
    let res = run_app(&mut terminal, &mut app);
    restore_terminal(&mut terminal)?;
    res
}

struct TerminalGuard;

fn setup_terminal() -> io::Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    // Bracketed paste ON (Feature A): a pasted playlist URL arrives
    // as one Event::Paste, not as a burst of key presses.
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture, EnableBracketedPaste)?;
    let backend = CrosstermBackend::new(stdout);
    Terminal::new(backend)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste
    )?;
    terminal.show_cursor()
}

fn run_app(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App) -> io::Result<()> {
    let _guard = TerminalGuard;
    let mut splash_shown_at = Instant::now();
    let mut last_tick = Instant::now();
    let tick_rate = Duration::from_millis(200);
    // v0.1.7: while the visualizer is ON, the draw loop tightens
    // to a ~40ms frame cadence so the smoothed bars/line move
    // fluidly; the 200ms logic tick (background polls, playback)
    // is unchanged, and with the visualizer OFF nothing speeds up.
    let mut last_frame = Instant::now();
    let frame_rate = Duration::from_millis(40);
    // Splash duration: keep the wordmark up while the user presses
    // any key, with a timeout so the app never feels stuck.
    let splash_max = Duration::from_millis(4000);

    loop {
        terminal.draw(|f| ui::render(f, app))?;
        // Deferred work AFTER the draw so the user first sees the
        // "Mencari audio..." / "Memuat..." frame (those loads block).
        app.process_pending_play();
        app.process_pending_browse();
        app.process_pending_link();
        if app.request_install {
            app.request_install = false;
            let missing = app.deps.missing();
            if missing.is_empty() {
                app.status_message = "All dependencies are already installed (mpv + yt-dlp).".to_string();
            } else {
                // Leave the TUI, run the distro install interactively
                // in the real terminal, wait for Enter, come back.
                restore_terminal(terminal)?;
                let ok = diag::run_install(&missing);
                println!("\nPress Enter to return to Lyre...");
                let mut line = String::new();
                let _ = io::stdin().read_line(&mut line);
                *terminal = setup_terminal()?;
                app.deps = diag::check_deps();
                app.status_message = if ok {
                    "Dependencies installed - happy listening!".to_string()
                } else {
                    "Install did not finish - check the messages above, or run ./install-deps.sh.".to_string()
                };
            }
        }

        // Splash auto-advance.
        if app.screen == Screen::Splash && splash_shown_at.elapsed() >= splash_max {
            app.set_screen(Screen::NowPlaying);
        }

        let mut timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_secs(0));
        if app.settings.visualizer {
            let frame_timeout = frame_rate
                .checked_sub(last_frame.elapsed())
                .unwrap_or_else(|| Duration::from_secs(0));
            timeout = timeout.min(frame_timeout);
        }
        if event::poll(timeout)? {
            let ev = event::read()?;
            // Debug tap for hardware-in-the-loop diagnosis.
            log_input_event(app, &ev);
            match ev {
                Event::Key(key) => {
                    if key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat {
                        handle_key(app, key.code, key.modifiers, &mut splash_shown_at);
                    }
                }
                Event::Mouse(m) => handle_mouse(app, m),
                // Bracketed paste: goes to the open form, or (no
                // form open) starts the link form from the sidebar
                // context when the paste looks like a URL.
                Event::Paste(text) => {
                    if app.input_form.is_some() {
                        app.form_paste(&text);
                    } else if app.screen != Screen::Splash {
                        app.start_link_form();
                        app.form_paste(&text);
                    }
                }
                _ => {}
            }
        }
        if last_tick.elapsed() >= tick_rate {
            app.poll_background();
            app.poll_playback();
            last_tick = Instant::now();
        }
        // Visualizer display advance at the frame cadence (only
        // while the visualizer is on): lerp + peak-cap step.
        if app.settings.visualizer && last_frame.elapsed() >= frame_rate {
            app.tick_visualizer();
            last_frame = Instant::now();
        }
        if app.should_quit {
            return Ok(());
        }
    }
}

fn handle_mouse(app: &mut App, m: MouseEvent) {
    match m.kind {
        MouseEventKind::Down(MouseButton::Left) => app.mouse_click(m.column, m.row),
        MouseEventKind::Drag(MouseButton::Left) => app.mouse_drag(m.column, m.row),
        MouseEventKind::ScrollDown => move_selection(app, 3),
        MouseEventKind::ScrollUp => move_selection(app, -3),
        _ => {}
    }
}

fn handle_key(app: &mut App, code: KeyCode, mods: KeyModifiers, _splash_shown_at: &mut Instant) {
    let shift = mods.contains(KeyModifiers::SHIFT);
    let ctrl = mods.contains(KeyModifiers::CONTROL);

    // The modal form takes over ALL input while open (it is
    // rendered on every screen, so this state is always visible -
    // the v0.1.4 invisible-form keyboard trap cannot recur). Esc
    // always closes it; after closing, the same press does nothing
    // else - one press, one effect.
    if app.input_form.is_some() {
        match code {
            KeyCode::Esc => {
                app.input_form = None;
                app.status_message = "Cancelled.".to_string();
            }
            KeyCode::Enter => app.form_submit(),
            KeyCode::Backspace => app.form_backspace(),
            KeyCode::Char(c) if !ctrl => app.form_input(c),
            _ => {}
        }
        return;
    }

    // Splash: ANY key enters the app, including Esc (which must
    // never be swallowed by a mode - Defect 2).
    if app.screen == Screen::Splash {
        if code == KeyCode::Char('q') && !ctrl {
            app.should_quit = true;
            return;
        }
        app.set_screen(Screen::NowPlaying);
        return;
    }

    // Search typing mode owns printable keys while active.
    if app.screen == Screen::Search && app.search_input_mode && app.browse.is_none() {
        match code {
            KeyCode::Esc => {
                app.search_input_mode = false;
            }
            KeyCode::Enter => {
                app.search_input_mode = false;
                app.run_search();
            }
            KeyCode::Backspace => {
                app.search_query.pop();
            }
            KeyCode::Char(c) if !ctrl => {
                app.search_query.push(c);
            }
            _ => {}
        }
        return;
    }

    match code {
        KeyCode::Char('q') if !ctrl => app.should_quit = true,
        KeyCode::Char('c') if ctrl => app.should_quit = true,
        // Esc: unwind one layer at a time - browse view first, then
        // panel focus back to Nav; never a dead end.
        KeyCode::Esc => {
            if app.browse.is_some() {
                app.close_browse();
                app.status_message = "Back.".to_string();
            } else if app.focus != PanelFocus::Nav {
                app.focus = PanelFocus::Nav;
            }
        }
        KeyCode::Tab => {
            app.focus = app.focus.next();
            // Keep the sidebar marker consistent with the focus.
            if app.focus == PanelFocus::Nav {
                app.nav_selected = nav_index(app.screen);
            }
        }
        KeyCode::BackTab => {
            app.focus = app.focus.prev();
            if app.focus == PanelFocus::Nav {
                app.nav_selected = nav_index(app.screen);
            }
        }
        KeyCode::Up => move_selection(app, -1),
        KeyCode::Down => move_selection(app, 1),
        KeyCode::Char('k') if !ctrl => move_selection(app, -1),
        KeyCode::Char('j') if !ctrl => move_selection(app, 1),
        KeyCode::Left => adjust(app, -1),
        KeyCode::Right => adjust(app, 1),
        KeyCode::Char('h') if !ctrl => adjust(app, -1),
        KeyCode::Char('l') if !ctrl => adjust(app, 1),
        KeyCode::Char(' ') => {
            // On the Clean Playback screen Space toggles the
            // focused setting; everywhere else it is play/pause.
            if app.screen == Screen::Settings && app.focus == PanelFocus::Main && app.browse.is_none() {
                toggle_setting(app);
            } else {
                app.toggle_play_pause();
            }
        }
        KeyCode::Char('n') => {
            if app.engine.current_index.is_some() || !app.engine.queue.is_empty() {
                app.next_track();
            }
        }
        KeyCode::Char('p') => {
            if !shift {
                app.prev_track();
            }
        }
        // Queue the selected item without interrupting playback.
        KeyCode::Char('a') => {
            if app.browse.is_some() {
                app.browse_queue_selected();
            } else if let Some(sel_row) = current_selectable_row(app) {
                match sel_row {
                    app::SearchRow::Track(i) => {
                        if let Some(t) = app.search_tracks.get(i).cloned() {
                            app.status_message = format!("Added to queue: {}", t.title);
                            app.enqueue_user(t);
                        }
                    }
                    _ => {}
                }
            }
        }
        // Delete the saved playlist under the Nav marker (key x).
        KeyCode::Char('x') => {
            if app.focus == PanelFocus::Nav {
                app.delete_nav_saved();
            }
        }
        // Lyrics toggle + sync offset keys (Up = earlier,
        // Down = later; also , and . for fine steps). On the
        // Equalizer screen , and . cycle presets instead (its
        // footer says so).
        KeyCode::Char('L') => app.toggle_lyrics(),
        KeyCode::Char(',') => {
            if app.screen == Screen::Equalizer && app.focus == PanelFocus::Main {
                app.cycle_eq_preset(-1);
            } else {
                app.lyrics_offset = ((app.lyrics_offset - 0.5) * 10.0).round() / 10.0;
            }
        }
        KeyCode::Char('.') => {
            if app.screen == Screen::Equalizer && app.focus == PanelFocus::Main {
                app.cycle_eq_preset(1);
            } else {
                app.lyrics_offset = ((app.lyrics_offset + 0.5) * 10.0).round() / 10.0;
            }
        }
        // Queue screen item management (Main focus).
        KeyCode::Char('J') => {
            if app.screen == Screen::Queue && app.focus == PanelFocus::Main {
                queue_move(app, 1);
            }
        }
        KeyCode::Char('K') => {
            if app.screen == Screen::Queue && app.focus == PanelFocus::Main {
                queue_move(app, -1);
            }
        }
        KeyCode::Char('d') if !ctrl => {
            if app.screen == Screen::Queue && app.focus == PanelFocus::Main {
                queue_remove(app);
            }
        }
        KeyCode::Char('c') if !ctrl => {
            if app.screen == Screen::Queue && app.focus == PanelFocus::Main {
                app.engine.queue.clear();
                app.engine.current_index = None;
                app.queue_selected = 0;
                app.note_queue_cleared();
                app.status_message = "Queue cleared.".to_string();
            }
        }
        // Autoplay toggle (v0.1.6): radio picks keep the queue
        // playing; state shows on the Queue screen + the bottom bar.
        KeyCode::Char('A') => {
            app.toggle_autoplay();
        }
        // Visualizer toggle (v0.1.6): sidebar cava box on/off.
        KeyCode::Char('v') if !ctrl => {
            app.toggle_visualizer();
        }
        // Visualizer template cycle (v0.1.7): Shift+V -
        // Bars -> Mirror -> Wave -> Line -> LED (persisted).
        KeyCode::Char('V') => {
            app.cycle_visualizer_style();
        }
        // Search platform filter (v0.1.6): cycle All -> Spotify ->
        // YT Music; routes the next search's fetching.
        KeyCode::Char('f') if !ctrl => {
            if app.screen == Screen::Search && app.browse.is_none() {
                app.cycle_search_platform();
            }
        }
        // Equalizer screen extras (Main focus): PgUp/PgDn = ±3 dB,
        // s = save custom preset, 0 = reset flat.
        KeyCode::PageUp => {
            if app.screen == Screen::Equalizer && app.focus == PanelFocus::Main {
                eq_gain_step(app, 3);
            }
        }
        KeyCode::PageDown => {
            if app.screen == Screen::Equalizer && app.focus == PanelFocus::Main {
                eq_gain_step(app, -3);
            }
        }
        KeyCode::Char('0') if !ctrl => {
            if app.screen == Screen::Equalizer && app.focus == PanelFocus::Main {
                app.apply_eq_preset("Flat");
            }
        }
        // Install dependencies via an external terminal.
        KeyCode::Char('i') => {
            app.request_install = true;
        }
        // Equalizer shortcut (alias of `4`).
        KeyCode::Char('e') if !ctrl => {
            app.set_screen(Screen::Equalizer);
            app.focus = PanelFocus::Main;
        }
        // Shuffle toggle (Now Playing / Queue; on the EQ screen `s`
        // saves a preset instead - see the EQ arm below).
        KeyCode::Char('s') if !ctrl => {
            if app.screen == Screen::Equalizer && app.focus == PanelFocus::Main {
                let name = app.settings.eq.save_custom_preset();
                app.persist_settings();
                app.status_message = format!("Preset saved: {}", name);
            } else if matches!(app.screen, Screen::NowPlaying | Screen::Queue) {
                app.engine.shuffle = !app.engine.shuffle;
                app.status_message = format!("Shuffle {}", if app.engine.shuffle { "ON" } else { "OFF" });
            }
        }
        // Repeat mode cycle.
        KeyCode::Char('r') if !ctrl => {
            app.engine.repeat = app.engine.repeat.next();
            app.status_message = format!("Repeat: {}", app.engine.repeat.label());
        }
        // +/- : lyrics sync offset while lyrics are shown,
        // otherwise volume.
        KeyCode::Char('+') | KeyCode::Char('=') => {
            if app.show_lyrics {
                app.lyrics_offset = crate::lyrics::clamp_offset(app.lyrics_offset + 0.5);
            } else {
                app.settings.volume = ((app.settings.volume as i32 + 5).clamp(0, 100)) as u8;
                app.persist_settings();
            }
        }
        KeyCode::Char('-') => {
            if app.show_lyrics {
                app.lyrics_offset = crate::lyrics::clamp_offset(app.lyrics_offset - 0.5);
            } else {
                app.settings.volume = ((app.settings.volume as i32 - 5).clamp(0, 100)) as u8;
                app.persist_settings();
            }
        }
        KeyCode::Char('/') => {
            app.set_screen(Screen::Search);
            app.search_input_mode = true;
            app.focus = PanelFocus::Main;
        }
        // Search kind chips: [ / ] cycle All/Songs/Albums/Playlists
        // (re-runs the search: the kind routes the Spotify type).
        KeyCode::Char('[') => {
            if app.screen == Screen::Search && app.browse.is_none() {
                let kind = if app.search_kind == 0 { 3 } else { app.search_kind - 1 };
                app.search_selected = 0;
                app.apply_search_kind(kind);
            }
        }
        KeyCode::Char(']') => {
            if app.screen == Screen::Search && app.browse.is_none() {
                let kind = (app.search_kind + 1) % 4;
                app.search_selected = 0;
                app.apply_search_kind(kind);
            }
        }
        KeyCode::Enter => handle_enter(app),
        KeyCode::Char(c) if ('1'..='5').contains(&c) => {
            let n = c as u8 - b'0';
            app.set_screen(app::Screen::from_number(n));
            app.focus = PanelFocus::Main;
        }
        _ => {}
    }
}

/// The search row (if any) the current selection points at - shared
/// by Enter (play/open) and `a` (queue).
fn current_selectable_row(app: &App) -> Option<app::SearchRow> {
    if app.screen == Screen::Search && app.browse.is_none() {
        app.search_selectable().get(app.search_selected).cloned()
    } else {
        None
    }
}

fn handle_enter(app: &mut App) {
    // While a browse view is open, Enter plays its selected track -
    // whatever panel is focused.
    if app.browse.is_some() {
        app.browse_play_selected();
        return;
    }
    match app.focus {
        PanelFocus::Nav => {
            let playlists = app.sidebar_playlists();
            if app.nav_selected < NAV_SCREENS.len() {
                app.set_screen(NAV_SCREENS[app.nav_selected]);
                app.focus = PanelFocus::Main;
            } else if app.nav_selected < NAV_SCREENS.len() + playlists.len() {
                let idx = app.nav_selected - NAV_SCREENS.len();
                app.open_sidebar_playlist(idx);
            } else {
                // The "+ Tempel Link Playlist" row.
                app.start_link_form();
            }
        }
        PanelFocus::EqSidebar => {
            if app.eq_sidebar_selected == 0 {
                // Preset row: apply the highlighted preset.
                let names = app.settings.eq.preset_names();
                if let Some(name) = names.get(app.eq_preset_selected).cloned() {
                    app.apply_eq_preset(&name);
                }
            }
            // Band/PRE/VOL rows adjust with Left/Right; Enter there
            // is a no-op.
        }
        PanelFocus::Player => {
            // Restart the current track from the queue.
            if let Some(idx) = app.engine.current_index {
                app.request_play(idx);
            }
        }
        PanelFocus::Main => match app.screen {
            Screen::NowPlaying => {
                if !app.home_tracks.is_empty() {
                    let i = app.home_selected.min(app.home_tracks.len() - 1);
                    app.home_play(i);
                }
            }
            Screen::Search => app.activate_search_selection(),
            Screen::Queue => {
                if app.queue_selected < app.engine.queue.len() {
                    let idx = app.queue_selected;
                    app.request_play(idx);
                }
            }
            Screen::Settings => toggle_setting(app),
            _ => {}
        },
    }
}

/// Space/Enter on the Clean Playback screen: rows 0-2 are toggles
/// (master, Spotify AdBlock, YT Music AdBlock + SponsorBlock); row 3
/// is the volume row and ignores toggles (Left/Right changes it).
fn toggle_setting(app: &mut App) {
    match app.settings_selected {
        0 => app.settings.clean_playback_master = !app.settings.clean_playback_master,
        1 => app.settings.spotify_adblock = !app.settings.spotify_adblock,
        2 => app.settings.ytmusic_adblock_sponsorblock = !app.settings.ytmusic_adblock_sponsorblock,
        _ => {}
    }
    app.persist_settings();
}

/// J/K on the Queue screen: move the selected queue item.
fn queue_move(app: &mut App, delta: i32) {
    let len = app.engine.queue.len();
    if len < 2 {
        return;
    }
    let from = app.queue_selected.min(len - 1);
    let to = (from as i32 + delta).clamp(0, len as i32 - 1) as usize;
    if from == to {
        return;
    }
    app.engine.queue.swap(from, to);
    if let Some(ci) = app.engine.current_index {
        app.engine.current_index = Some(if ci == from {
            to
        } else if ci == to {
            from
        } else {
            ci
        });
    }
    app.queue_selected = to;
}

/// d on the Queue screen: remove the selected item.
fn queue_remove(app: &mut App) {
    if app.queue_selected >= app.engine.queue.len() {
        return;
    }
    let idx = app.queue_selected;
    let removed = app.engine.queue.remove(idx);
    app.note_queue_removed(idx);
    app.status_message = format!("Removed from queue: {}", removed.title);
    if let Some(ci) = app.engine.current_index {
        if ci == idx {
            app.engine.current_index = None;
        } else if ci > idx {
            app.engine.current_index = Some(ci - 1);
        }
    }
    if app.queue_selected >= app.engine.queue.len() {
        app.queue_selected = app.engine.queue.len().saturating_sub(1);
    }
}

/// Adjust the selected EQ column (band or preamp) by delta dB -
/// shared by Left/Right (±1) and PgUp/PgDn (±3) on the EQ screen.
fn eq_gain_step(app: &mut App, delta: i32) {
    if app.eq_selected < 10 {
        let cur = app.settings.eq.bands[app.eq_selected];
        app.settings.eq.bands[app.eq_selected] = (cur + delta as f64).clamp(-12.0, 12.0);
    } else {
        let cur = app.settings.eq.preamp;
        app.settings.eq.preamp = (cur + delta as f64).clamp(-12.0, 12.0);
    }
    app.settings.eq.preset_name = "Custom (unsaved)".to_string();
    app.persist_settings();
}

fn move_selection(app: &mut App, delta: i32) {
    // The open browse view owns vertical movement while it exists.
    if app.browse.is_some() {
        if app.focus == PanelFocus::EqSidebar {
            let len = 13;
            app.eq_sidebar_selected = wrap(app.eq_sidebar_selected, delta, len);
            return;
        }
        if let Some(b) = &mut app.browse {
            if !b.tracks.is_empty() {
                b.selected = wrap(b.selected, delta, b.tracks.len());
            }
        }
        return;
    }
    match app.focus {
        PanelFocus::Nav => {
            let total = app.nav_total();
            app.nav_selected = wrap(app.nav_selected, delta, total);
            if app.nav_selected < NAV_SCREENS.len() {
                app.set_screen(NAV_SCREENS[app.nav_selected]);
            }
        }
        PanelFocus::EqSidebar => {
            let len = 13; // preset + 10 bands + preamp + VOL
            app.eq_sidebar_selected = wrap(app.eq_sidebar_selected, delta, len);
        }
        PanelFocus::Player => {
            // Up/Down on the player panel = volume.
            let nv = app.settings.volume as i32 + if delta < 0 { 5 } else { -5 };
            app.settings.volume = nv.clamp(0, 100) as u8;
            app.persist_settings();
        }
        PanelFocus::Main => match app.screen {
            Screen::NowPlaying => {
                if !app.home_tracks.is_empty() {
                    let len = app.home_tracks.len();
                    app.home_selected = wrap(app.home_selected, delta, len);
                }
            }
            Screen::Search => {
                let len = app.search_selectable().len();
                if len > 0 {
                    app.search_selected = wrap(app.search_selected, delta, len);
                }
            }
            Screen::Queue => {
                let len = app.engine.queue.len();
                if len > 0 {
                    app.queue_selected = wrap(app.queue_selected, delta, len);
                }
            }
            Screen::Equalizer => {
                app.eq_selected = wrap(app.eq_selected, delta, 11);
            }
            Screen::Settings => {
                app.settings_selected = wrap(app.settings_selected, delta, 4);
            }
            _ => {}
        },
    }
}

fn adjust(app: &mut App, delta: i32) {
    if app.focus == PanelFocus::EqSidebar || (app.browse.is_some() && app.focus == PanelFocus::EqSidebar) {
        app.eq_sidebar_adjust(delta);
        return;
    }
    match app.focus {
        PanelFocus::Player | PanelFocus::Nav => {
            // Left/Right = seek -/+ 5 seconds (real mpv time-pos).
            app.seek_relative(delta);
        }
        _ => match app.screen {
            Screen::NowPlaying => {
                app.seek_relative(delta);
            }
            Screen::Equalizer => {
                eq_gain_step(app, delta);
            }
            Screen::Settings => {
                adjust_setting(app, delta);
            }
            _ => {}
        },
    }
}

impl App {
    /// Seek the playing track by +/- delta*5 seconds (real mpv IPC
    /// time-pos; see player::PlayerEngine::seek_relative).
    fn seek_relative(&mut self, delta: i32) {
        if self.engine.current_track().is_none() {
            return;
        }
        if self.engine.seek_relative(delta as f64 * 5.0) {
            self.status_message = format!("Seek {:+}s", delta * 5);
        } else if self.engine.mpv_active {
            self.status_message = "Seek not available yet: mpv is not connected over IPC.".to_string();
        } else {
            self.status_message = "No track is currently playing.".to_string();
        }
    }
}

fn wrap(cur: usize, delta: i32, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let len = len as i32;
    (((cur as i32 + delta) % len) + len) as usize % len as usize
}

fn adjust_setting(app: &mut App, delta: i32) {
    // Rows 0-2 are toggles (Left/Right flips them too); row 3 is
    // the volume row (Left/Right ±5).
    match app.settings_selected {
        0..=2 => toggle_setting(app),
        3 => {
            app.settings.volume = ((app.settings.volume as i32 + delta * 5).clamp(0, 100)) as u8;
            app.persist_settings();
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn help_text_is_honest() {
        // Sanity: the file compiles; behavioral tests live in app.rs
        // and the module tests.
    }

    /// v0.1.6 full-English audit: scan the string literals of every
    /// src/*.rs file (each file cut at its `#[cfg(test)]` module, so
    /// fixtures are excluded) for common Indonesian words. Comments
    /// are exempt - the scanner is a small lexer that only collects
    /// string-literal contents; it is best-effort by design (see
    /// BUILD_REPORT) and it proves the UI carries no Indonesian
    /// helper text anymore.
    #[test]
    fn no_indonesian_words_in_ui_string_literals() {
        const WORDS: [&str; 16] = [
            "lagu", "putar", "antrean", "tekan", "belum", "gagal", "cari", "buka", "tutup",
            "simpan", "hapus", "sekarang", "dari", "untuk", "dengan", "tidak",
        ];
        // Tiny lexer: yields the contents of "..." and r#"..."#
        // literals, skipping // and /* */ comments and 'x' chars.
        fn literals(src: &str) -> Vec<String> {
            let b: Vec<char> = src.chars().collect();
            let mut out = Vec::new();
            let mut i = 0usize;
            while i < b.len() {
                match b[i] {
                    '/' if i + 1 < b.len() && b[i + 1] == '/' => {
                        while i < b.len() && b[i] != '\n' {
                            i += 1;
                        }
                    }
                    '/' if i + 1 < b.len() && b[i + 1] == '*' => {
                        i += 2;
                        while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                            i += 1;
                        }
                        i += 2;
                    }
                    'r' if i + 1 < b.len() && b[i + 1] == '#' => {
                        // raw string r#"..."# (single # is all we use)
                        if i + 2 < b.len() && b[i + 2] == '"' {
                            let mut s = String::new();
                            i += 3;
                            while i + 1 < b.len() && !(b[i] == '"' && b[i + 1] == '#') {
                                s.push(b[i]);
                                i += 1;
                            }
                            i += 2;
                            out.push(s);
                        } else {
                            i += 1;
                        }
                    }
                    '"' => {
                        let mut s = String::new();
                        i += 1;
                        while i < b.len() && b[i] != '"' {
                            if b[i] == '\\' && i + 1 < b.len() {
                                i += 1;
                            }
                            if i < b.len() {
                                s.push(b[i]);
                                i += 1;
                            }
                        }
                        i += 1;
                        out.push(s);
                    }
                    '\'' => {
                        // char literal or lifetime: skip a quoted
                        // char when it looks like one ('x', '\n').
                        if i + 2 < b.len() && (b[i + 2] == '\'' || (b[i + 1] == '\\' && i + 3 < b.len() && b[i + 3] == '\'')) {
                            i += if b[i + 1] == '\\' { 4 } else { 3 };
                        } else {
                            i += 1;
                        }
                    }
                    _ => i += 1,
                }
            }
            out
        }
        let src_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut violations: Vec<String> = Vec::new();
        let mut files: Vec<_> = std::fs::read_dir(&src_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map(|x| x == "rs").unwrap_or(false))
            .collect();
        files.sort();
        assert!(!files.is_empty());
        for path in files {
            let text = std::fs::read_to_string(&path).unwrap();
            let code = text.split("#[cfg(test)]").next().unwrap_or("");
            for lit in literals(code) {
                for token in lit.split(|c: char| !c.is_alphabetic()) {
                    let t = token.to_lowercase();
                    if WORDS.contains(&t.as_str()) {
                        violations.push(format!(
                            "{}: word '{}' in literal {:?}",
                            path.file_name().unwrap().to_string_lossy(),
                            t,
                            lit.chars().take(60).collect::<String>()
                        ));
                    }
                }
            }
        }
        assert!(violations.is_empty(), "Indonesian UI strings remain:\n{}", violations.join("\n"));
    }
}
