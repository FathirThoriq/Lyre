//! Rendering that mirrors the approved design artifact
//! (desain-tui-spotify-yt-music): same palette, panel composition,
//! English labels (v0.1.6 full-English pass), demo dataset and
//! footer keybinding lines.
//! Icons are plain terminal glyphs (the artifact's Lucide SVGs have no
//! terminal equivalent) - no emoji anywhere.

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Gauge, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{nav_index, App, ClickTarget, HomeKind, PanelFocus, Screen, SearchRow, NAV_SCREENS};
use crate::eq::{freq_label, BAND_FREQS};
use crate::lyrics::{active_line_index, effective_position, LyricsResult};
use crate::track::{Source, Track};

// Palette extracted from the design artifact (dark theme variables):
// --term-bg #0e1013, --term-panel #15181d, --term-line #2b3038,
// --term-ink #dfe3e8, --term-dim #878e99, --spot #1ed760,
// --ytm #ff5147, --amber #e8b93e, --sel-bg #232b38.
const GOLD: Color = Color::Rgb(202, 160, 92); // wordmark gold (design brief)
const AMBER: Color = Color::Rgb(232, 185, 62);
const SPOT: Color = Color::Rgb(30, 215, 96);
const YTM: Color = Color::Rgb(255, 81, 71);
const INK: Color = Color::Rgb(223, 227, 232);
const DIM: Color = Color::Rgb(135, 142, 153);
const FAINT: Color = Color::Rgb(91, 98, 109);
const SEL_BG: Color = Color::Rgb(35, 43, 56);
const LINE_C: Color = Color::Rgb(43, 48, 56);

/// The one navigation list: sidebar items and header tabs are built
/// from these labels + NAV_SCREENS, so the two can never drift apart.
pub const NAV_LABELS: [&str; 5] = ["Now Playing", "Search", "Queue", "Equalizer", "Clean Playback"];

fn source_color(s: Source) -> Color {
    match s {
        Source::Spotify => SPOT,
        Source::YtMusic => YTM,
    }
}

fn sel_style() -> Style {
    Style::default().fg(INK).bg(SEL_BG).add_modifier(Modifier::BOLD)
}

/// THE wordmark (v0.1.7, Fathir's pick from rendered samples):
/// plain text in a classical serif-italic form - the Mathematical
/// Italic Unicode letters (U+1D43F, U+1D466, U+1D45F, U+1D452),
/// styled gold with NO bold and no block/ASCII art around it. It
/// replaces every block-letter wordmark (splash v0.1.3, sidebar
/// v0.1.6) and matches the design artifact's Instrument Serif
/// Italic treatment. One shared constant, used everywhere.
pub const WORDMARK: &str = "𝐿𝑦𝑟𝑒";

fn wordmark() -> Span<'static> {
    Span::styled(WORDMARK, Style::default().fg(GOLD))
}

/// Border style for a panel: amber when it owns the panel focus.
fn focus_border(app: &App, panel: PanelFocus) -> Style {
    if app.focus == panel {
        Style::default().fg(AMBER)
    } else {
        Style::default().fg(LINE_C)
    }
}

fn badge(t: &Track) -> Span<'static> {
    Span::styled(t.source.label().to_string(), Style::default().fg(source_color(t.source)).add_modifier(Modifier::BOLD))
}

fn track_line(t: &Track, selected: bool, playing: bool, numbered: Option<usize>) -> Line<'static> {
    let marker = if playing { "> " } else if selected { "* " } else { "  " };
    let num = numbered.map(|n| format!("{} ", n + 1)).unwrap_or_default();
    let style = if selected { sel_style() } else { Style::default().fg(INK) };
    Line::from(vec![
        Span::styled(format!("{}{}", marker, num), style),
        badge(t),
        Span::styled(format!(" {} - {}  ({})", t.title, t.artist, t.duration_label()), style),
    ])
}


/// The one status line kept under the sidebar wordmark (v0.1.6):
/// Clean Playback state + the active EQ preset.
fn sidebar_clean_line(app: &App) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("Clean Playback: {}", if app.settings.any_clean_active() { "ON" } else { "OFF" }),
            Style::default().fg(if app.settings.any_clean_active() { SPOT } else { FAINT }),
        ),
        Span::styled(format!("  ·  EQ: {}", app.settings.eq.preset_name), Style::default().fg(GOLD)),
    ])
}

fn footer(app: &App) -> Paragraph<'static> {
    let keys = if app.browse.is_some() {
        "Enter play · a +queue · Esc back to results · double-click a track = play"
    } else {
        match app.screen {
            Screen::NowPlaying => "Space play/pause · ←→ seek ±5s · n/p next/prev · / search · L lyrics · A autoplay · v visualizer · V viz style · q quit",
            Screen::Search => "Enter play/open · a +queue · [ ] kind chips · f platform (All/Spotify/YT Music) · ←→ seek ±5s · Esc back · q quit",
            Screen::Queue => "J/K reorder · d remove · c clear · A autoplay on/off · ←→ seek ±5s · double-click = play · q quit",
            Screen::Equalizer => "←→ select band · ↑↓ gain ±1 dB · PgUp/PgDn ±3 dB · s save preset · 0 reset flat · ,/. preset · q quit",
            Screen::Settings => "Space/Enter toggle · ←→ seek ±5s (volume row: change volume) · q quit",
            Screen::Splash => "any key enters Now Playing · q quit",
        }
    };
    // A playback error stays visible (red, bold) until the next
    // successful play; otherwise the rolling status line shows.
    let line1 = match &app.play_error {
        Some(e) => Line::from(Span::styled(e.clone(), Style::default().fg(YTM).add_modifier(Modifier::BOLD))),
        None => Line::from(Span::styled(app.status_message.clone(), Style::default().fg(DIM))),
    };
    let focus_line = format!(
        "Focus: {} — {} · Tab switches focus · {}",
        app.focus.label(),
        app.focus.keys_hint(),
        keys
    );
    let text = vec![
        line1,
        Line::from(Span::styled(focus_line, Style::default().fg(FAINT))),
    ];
    Paragraph::new(text).wrap(Wrap { trim: true })
}

fn format_time(secs: f64) -> String {
    let s = secs as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

fn now_playing_bar(app: &App) -> Paragraph<'static> {
    match app.engine.current_track() {
        Some(t) => {
            let state = if app.engine.is_playing { "❚❚" } else { "▶" };
            let prov = app
                .engine
                .now_playing_provenance
                .clone()
                .unwrap_or_else(|| t.audio_provenance());
            let line = format!(
                "{} {} {} - {}   {} / {}   Vol {}   Shuffle {}   Repeat {}   Autoplay {}   Audio: {}",
                state,
                t.source.label(),
                t.title,
                t.artist,
                format_time(app.engine.position_secs),
                format_time(app.engine.display_duration()),
                app.settings.volume,
                if app.engine.shuffle { "ON" } else { "OFF" },
                app.engine.repeat.label(),
                if app.settings.autoplay { "ON" } else { "OFF" },
                prov
            );
            Paragraph::new(line).block(Block::default().borders(Borders::TOP).border_style(focus_border(app, PanelFocus::Player)).title(" NOW PLAYING "))
        }
        None => Paragraph::new("No track. Find one via Search (2).")
            .block(Block::default().borders(Borders::TOP).border_style(focus_border(app, PanelFocus::Player)).title(" NOW PLAYING ")),
    }
}

fn progress_gauge(app: &mut App, area: Rect, f: &mut Frame) {
    // The progress bar is a seek control now (Bug 3/4): clicking it
    // seeks mpv to the proportional position via IPC.
    app.click_map.push((area, ClickTarget::ProgressBar));
    if app.engine.current_track().is_some() {
        let dur = app.engine.display_duration().max(0.1);
        let ratio = ((app.engine.position_secs / dur).clamp(0.0, 1.0) * 100.0) as u16;
        let g = Gauge::default().percent(ratio).gauge_style(Style::default().fg(AMBER));
        f.render_widget(g, area);
    }
}

pub fn render(f: &mut Frame, app: &mut App) {
    // Click regions are rebuilt from scratch every frame.
    app.click_map.clear();
    if app.screen == Screen::Splash {
        render_splash(f, app);
        return;
    }
    // v0.1.4 layout: NO top brand/status strip, NO tab row. The
    // sidebar (wordmark + statuses + nav + playlists + EQ) is the
    // whole navigation system, driven by Tab focus, numbers, arrows
    // and the mouse.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(5),
            Constraint::Length(1),
            // The now-playing bar is a top border + exactly one
            // text row; v0.1.5 reserved a third, always blank, row.
            Constraint::Length(2),
            Constraint::Length(2),
        ])
        .split(f.area());
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(24), Constraint::Percentage(76)])
        .split(chunks[0]);
    render_sidebar(f, app, body[0]);
    if app.browse.is_some() {
        render_browse(f, app, body[1]);
    } else {
        match app.screen {
            Screen::NowPlaying => render_home(f, app, body[1]),
            Screen::Search => render_search(f, app, body[1]),
            Screen::Queue => render_queue(f, app, body[1]),
            Screen::Equalizer => render_eq(f, app, body[1]),
            Screen::Settings => render_settings(f, app, body[1]),
            Screen::Splash => {}
        }
    }
    progress_gauge(app, chunks[1], f);
    f.render_widget(now_playing_bar(app), chunks[2]);
    f.render_widget(footer(app), chunks[3]);
    // The paste-link form is a MODAL: rendered last, on top of
    // whatever screen is active, because it captures the keyboard
    // (v0.1.5 Defect-2 fix: a key-capturing state is always
    // visible; mouse input is suspended while it is open).
    if app.input_form.is_some() {
        render_form_modal(f, app);
    }
}

/// The centered modal for the one remaining input form (paste a
/// playlist link). Clears its background so the form unmistakably
/// owns the screen while it is open.
fn render_form_modal(f: &mut Frame, app: &mut App) {
    let form = match &app.input_form {
        Some(fm) => fm.clone(),
        None => return,
    };
    let area = centered(f.area(), 64, 26);
    f.render_widget(Clear, area);
    let lines = vec![
        Line::from(Span::styled(form.label.clone(), Style::default().fg(DIM))),
        Line::from(""),
        Line::from(vec![
            Span::styled("> ", Style::default().fg(AMBER).add_modifier(Modifier::BOLD)),
            Span::raw(form.value.clone()),
            Span::styled("_", Style::default().fg(AMBER)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "Paste directly with Ctrl+V (bracketed paste is on).",
            Style::default().fg(DIM),
        )),
        Line::from(Span::styled(
            "Enter = open playlist · Esc = cancel",
            Style::default().fg(FAINT),
        )),
        Line::from(Span::styled(
            "Accepted: open.spotify.com/playlist|album · music.youtube.com/playlist?list=...",
            Style::default().fg(FAINT),
        )),
    ];
    let p = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(AMBER))
                .title(format!(" {} ", form.title)),
        )
        .wrap(Wrap { trim: true });
    f.render_widget(p, area);
}

fn centered(area: Rect, width_pct: u16, height_pct: u16) -> Rect {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - height_pct) / 2),
            Constraint::Percentage(height_pct),
            Constraint::Percentage((100 - height_pct) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - width_pct) / 2),
            Constraint::Percentage(width_pct),
            Constraint::Percentage((100 - width_pct) / 2),
        ])
        .split(v[1])[1]
}

fn render_splash(f: &mut Frame, app: &mut App) {
    let area = centered(f.area(), 70, 72);
    // v0.1.7: a centered title card - the serif-italic text
    // wordmark on its own line with breathing room around it (the
    // 7-row block-letter wordmark is gone, per Fathir's pick).
    let mut lines: Vec<Line> = vec![Line::from(""), Line::from(""), Line::from("")];
    lines.push(Line::from(wordmark()).alignment(Alignment::Center));
    lines.push(Line::from(""));
    lines.push(
        Line::from(Span::styled(
            "The lyre of Apollo — one terminal player for Spotify + YT Music",
            Style::default().fg(DIM).add_modifier(Modifier::ITALIC),
        ))
        .alignment(Alignment::Center),
    );
    lines.push(Line::from(""));
    // Guest mode (v0.1.6): no accounts, no login - a single line
    // (the two old per-platform status lines are gone).
    lines.push(
        Line::from(Span::styled(
            "Guest Mode — no login needed",
            Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
        ))
        .alignment(Alignment::Center),
    );
    lines.push(
        Line::from(Span::styled(
            format!("Clean Playback: {}", if app.settings.any_clean_active() { "ON" } else { "OFF" }),
            Style::default().fg(if app.settings.any_clean_active() { SPOT } else { FAINT }),
        ))
        .alignment(Alignment::Center),
    );
    if !app.deps.all_present() {
        lines.push(
            Line::from(Span::styled(
                format!("Missing dependencies: {} — press i to install automatically", app.deps.missing().join(", ")),
                Style::default().fg(YTM).add_modifier(Modifier::BOLD),
            ))
            .alignment(Alignment::Center),
        );
    }
    lines.push(Line::from(""));
    let p = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(LINE_C)).title(format!(" {} ", WORDMARK)));
    f.render_widget(p, area);
    // Bordered action box inside the splash, like the artifact.
    let action_area = Rect {
        x: area.x + area.width / 4,
        y: area.y + area.height.saturating_sub(6),
        width: area.width / 2,
        height: 3,
    };
    let action = Paragraph::new(Line::from(Span::styled(
        "Enter — Now Playing [1]",
        Style::default().fg(INK).add_modifier(Modifier::BOLD),
    )))
    .alignment(Alignment::Center)
    .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(AMBER)));
    app.click_map.push((action_area, ClickTarget::SplashEnter));
    f.render_widget(action, action_area);
    let hint = Paragraph::new("Press any key to enter · q quit")
        .style(Style::default().fg(FAINT))
        .alignment(Alignment::Center);
    let hint_area = Rect { x: area.x, y: action_area.y + 4, width: area.width, height: 1 };
    f.render_widget(hint, hint_area);
    let foot = Paragraph::new(app.status_message.clone())
        .style(Style::default().fg(DIM))
        .alignment(Alignment::Center);
    let fa = Rect { x: 0, y: f.area().height.saturating_sub(1), width: f.area().width, height: 1 };
    f.render_widget(foot, fa);
}

// ---------------------------------------------------------------------------
// Home / Now Playing: sidebar | Terakhir Diputar/Lirik | Now Playing card
// ---------------------------------------------------------------------------

fn render_home(f: &mut Frame, app: &mut App, area: Rect) {
    // Sidebar lives in the shared layout now; Home's own area is the
    // Terakhir Diputar/Lirik box + the Now Playing card.
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(59), Constraint::Percentage(41)])
        .split(area);
    if app.show_lyrics {
        render_lyrics_box(f, app, cols[0]);
    } else {
        render_recent_box(f, app, cols[0]);
    }
    render_np_card(f, app, cols[1]);
}

/// How many lines the sidebar playlist block wants: the PLAYLISTS
/// header + the paste entry always, one section header per source
/// present, one line per saved entry, and a hint line when empty.
/// Capped so a long saved list can never starve the visualizer/EQ.
fn playlist_lines_wanted(app: &App) -> u16 {
    let entries = app.sidebar_playlists();
    if entries.is_empty() {
        return 3; // header + hint + paste entry
    }
    let mut sections = 0usize;
    let mut cur: Option<Source> = None;
    for e in &entries {
        if cur != Some(e.source) {
            sections += 1;
            cur = Some(e.source);
        }
    }
    (2 + sections + entries.len()).min(10) as u16
}

/// The sidebar (v0.1.6): the italic Lyre logo on top (the old
/// guest-status lines are gone), the 5 nav screens, the playlists
/// (saved link entries only + the paste-link entry - clickable,
/// they OPEN), the cava VISUALIZER section, and the Equalizer
/// section in the space below. Layout discipline: header 2
/// (v0.1.7: the text wordmark needs one line), nav 5,
/// EQ full 13 (compact 3 on short terminals); playlists take what
/// they need (capped); the VISUALIZER gets what remains (max 8)
/// and is the FIRST element to shrink - below a 35-row terminal
/// it collapses to a single line, and it vanishes before
/// playlists/EQ lose a single row.
fn render_sidebar(f: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(focus_border(app, PanelFocus::Nav))
        .title(" NAVIGATION ");
    let inner = block.inner(area);
    f.render_widget(block, area);
    app.last_sidebar_width = inner.width;
    // Compact EQ (preset row + hint carrying the volume) only when
    // vertical space is really tight (e.g. 100x30); at 140x40 the
    // full section - preset row + 10 bands + preamp + VOL - fits
    // and is always shown.
    let compact_eq = inner.height < 30;
    let eq_h: u16 = if compact_eq { 3 } else { 13 };
    let playlists_need = playlist_lines_wanted(app);
    // v0.1.7: the header is 2 rows now (the plain-text serif-
    // italic wordmark + the Clean Playback line) - the freed rows
    // flow to playlists first, then the visualizer, as before.
    let avail_flex = inner.height.saturating_sub(2 + 5 + eq_h);
    let viz_h: u16 = if !app.settings.visualizer {
        0
    } else if f.area().height < 35 {
        // Short terminal: a single collapsed line, and only when
        // playlists keep everything they need.
        if avail_flex > playlists_need {
            1
        } else {
            0
        }
    } else {
        avail_flex.saturating_sub(playlists_need).min(8)
    };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),      // wordmark + Clean Playback line
            Constraint::Length(5),      // nav items
            Constraint::Min(0),         // playlists
            Constraint::Length(viz_h),  // visualizer
            Constraint::Length(eq_h),   // equalizer section
        ])
        .split(inner);

    // -- Top: the serif-italic text wordmark + the Clean line -------
    // v0.1.7: one line of styled text (WORDMARK) - it IS text, so
    // narrow terminals need no fallback path at all.
    let top_lines: Vec<Line> = vec![Line::from(wordmark()), sidebar_clean_line(app)];
    f.render_widget(Paragraph::new(top_lines), rows[0]);

    // -- Nav: the same 5 screens, marker follows focus/screen ---------
    // The focus marker can also rest on a playlist row (nav_selected
    // continues past the 5 screens into the playlist entries) and on
    // the paste-link entry after them.
    let marker = if app.focus == PanelFocus::Nav {
        app.nav_selected
    } else {
        nav_index(app.screen)
    };
    let mut nav_lines: Vec<Line> = Vec::new();
    for (i, label) in NAV_LABELS.iter().enumerate() {
        let is_marker = i == marker;
        let is_active = NAV_SCREENS[i] == app.screen;
        let style = if is_marker {
            sel_style()
        } else if is_active {
            Style::default().fg(INK).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(INK)
        };
        nav_lines.push(Line::from(Span::styled(
            format!(" {} {:<15} {}", if is_marker { ">" } else { " " }, label, i + 1),
            style,
        )));
        app.click_map.push((
            Rect { x: rows[1].x, y: rows[1].y + i as u16, width: rows[1].width, height: 1 },
            ClickTarget::NavItem(i),
        ));
    }
    f.render_widget(Paragraph::new(nav_lines), rows[1]);

    render_sidebar_playlists(f, app, rows[2], marker);
    render_sidebar_viz(f, app, rows[3]);
    render_sidebar_eq(f, app, rows[4], compact_eq);
}

/// The sidebar VISUALIZER section (v0.1.6, templates v0.1.7):
/// live spectrum data from the external `cava` process (see
/// visualizer.rs), drawn by the active template from the smoothed
/// display buffer. The header names the template; clicking the
/// box cycles it (key `V` does the same). Height 1 is the
/// collapsed short-terminal form. The display renders cava's
/// actual output ONLY - a missing binary shows an install hint
/// and an exited cava shows "visualizer unavailable"; nothing is
/// ever animated artificially.
fn render_sidebar_viz(f: &mut Frame, app: &mut App, area: Rect) {
    use crate::visualizer::VizState;
    if area.height == 0 || !app.settings.visualizer {
        return;
    }
    if area.height == 1 {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "VISUALIZER — press v / open a taller terminal",
                Style::default().fg(FAINT),
            ))),
            area,
        );
        return;
    }
    // Clicking the box cycles the template (v0.1.7).
    app.click_map.push((area, ClickTarget::VizBox));
    let style = crate::visualizer::VizStyle::from_name(&app.settings.visualizer_style);
    let mut lines: Vec<Line> = vec![Line::from(Span::styled(
        format!("VISUALIZER — {}", style.name()),
        Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
    ))];
    match app.viz.state {
        VizState::MissingBinary => {
            lines.push(Line::from(Span::styled(
                "Visualizer needs cava — run install-deps.sh or press i",
                Style::default().fg(DIM),
            )));
        }
        VizState::Unavailable => {
            lines.push(Line::from(Span::styled(
                "visualizer unavailable",
                Style::default().fg(DIM),
            )));
        }
        VizState::Off => {}
        VizState::Idle | VizState::Running => {
            let bar_rows = (area.height - 1) as usize;
            let width = area.width as usize;
            // LED's top segment, when lit, takes the alert color -
            // the one red row in the box (real LED meters do this);
            // everything else stays gold/amber.
            let alert_row = if style == crate::visualizer::VizStyle::Led {
                crate::visualizer::led_alert_row(&app.viz.display.values, bar_rows, width)
            } else {
                None
            };
            let rendered = crate::visualizer::render_style(style, &app.viz.display, bar_rows, width);
            for (i, row) in rendered.into_iter().enumerate() {
                let color = if alert_row == Some(i) { YTM } else { AMBER };
                lines.push(Line::from(Span::styled(row, Style::default().fg(color))));
            }
        }
    }
    f.render_widget(Paragraph::new(lines), area);
}

/// Sidebar playlist sections (v0.1.6): saved link entries ONLY
/// (the seeded examples are deleted), grouped under per-source
/// section headers that appear only when that source has at least
/// one saved entry. Below the sections sits the
/// "[ + Paste Playlist Link ]" entry (Feature A). With nothing
/// saved, one dim hint line stands under the PLAYLISTS header.
fn render_sidebar_playlists(f: &mut Frame, app: &mut App, area: Rect, marker: usize) {
    if area.height == 0 {
        return;
    }
    let entries = app.sidebar_playlists();
    let height = area.height as usize;
    let mut lines: Vec<Line> = vec![Line::from(Span::styled(
        "PLAYLISTS",
        Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
    ))];
    let mut entry_rects: Vec<(u16, usize)> = Vec::new(); // (line idx, combined index)
    if entries.is_empty() {
        lines.push(Line::from(Span::styled(
            "No saved playlists yet.",
            Style::default().fg(FAINT),
        )));
    } else {
        let mut cur_source: Option<Source> = None;
        for (ci, e) in entries.iter().enumerate() {
            if cur_source != Some(e.source) {
                // A section header only when its first entry also
                // fits (header + entry + paste row must all fit).
                if lines.len() + 3 > height {
                    break;
                }
                let (label, color) = match e.source {
                    Source::Spotify => ("PLAYLIST — SPOTIFY", SPOT),
                    Source::YtMusic => ("PLAYLIST — YT MUSIC", YTM),
                };
                lines.push(Line::from(Span::styled(
                    label,
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                )));
                cur_source = Some(e.source);
            }
            if lines.len() + 2 > height {
                break; // keep room for the paste entry
            }
            let combined_marker = NAV_SCREENS.len() + ci;
            let is_marker = combined_marker == marker && app.focus == PanelFocus::Nav;
            let style = if is_marker {
                sel_style()
            } else {
                Style::default().fg(INK)
            };
            entry_rects.push((lines.len() as u16, ci));
            lines.push(Line::from(Span::styled(
                format!(" {} {}", if is_marker { ">" } else { " " }, e.title),
                style,
            )));
        }
    }
    // The paste-link entry (Feature A): the last row of the
    // playlist area; the Nav marker can rest on it too.
    if lines.len() < height {
        let paste_marker = NAV_SCREENS.len() + entries.len();
        let paste_is_marker = paste_marker == marker && app.focus == PanelFocus::Nav;
        let paste_style = if paste_is_marker {
            sel_style()
        } else {
            Style::default().fg(AMBER).add_modifier(Modifier::BOLD)
        };
        let paste_line = lines.len() as u16;
        lines.push(Line::from(Span::styled(
            format!(" {}[ + Paste Playlist Link ]", if paste_is_marker { ">" } else { " " }),
            paste_style,
        )));
        app.click_map.push((
            Rect { x: area.x, y: area.y + paste_line, width: area.width, height: 1 },
            ClickTarget::PasteLink,
        ));
    }
    for (line_idx, ci) in entry_rects {
        app.click_map.push((
            Rect { x: area.x, y: area.y + line_idx, width: area.width, height: 1 },
            ClickTarget::PlaylistItem(ci),
        ));
    }
    f.render_widget(Paragraph::new(lines), area);
}

/// One compact EQ band row: " 31  ██████░░░░░░  +6" style. The fill
/// maps -12..=+12 dB onto the bar width.
fn eq_band_line(label: &str, value: f64, selected: bool, width: usize) -> Line<'static> {
    let cells = width.max(4);
    let filled = (((value + 12.0) / 24.0) * cells as f64).round() as usize;
    let filled = filled.min(cells);
    let bar: String = "█".repeat(filled) + &"░".repeat(cells - filled);
    let style = if selected { sel_style() } else { Style::default().fg(INK) };
    let bar_style = if selected { sel_style() } else { Style::default().fg(AMBER) };
    Line::from(vec![
        Span::styled(format!("{}{:>4} ", if selected { ">" } else { " " }, label), style),
        Span::styled(bar, bar_style),
        Span::styled(format!(" {:+3.0}", value), style),
    ])
}

/// The VOL row (Defect 3): same bar shape as the band rows, but the
/// fill maps 0..=100 and the value is the shared settings.volume.
fn vol_line(volume: u8, selected: bool, width: usize) -> Line<'static> {
    let cells = width.max(4);
    let filled = ((volume as f64 / 100.0) * cells as f64).round() as usize;
    let filled = filled.min(cells);
    let bar: String = "█".repeat(filled) + &"░".repeat(cells - filled);
    let style = if selected { sel_style() } else { Style::default().fg(INK) };
    let bar_style = if selected { sel_style() } else { Style::default().fg(SPOT) };
    Line::from(vec![
        Span::styled(format!("{}{:>4} ", if selected { ">" } else { " " }, "VOL"), style),
        Span::styled(bar, bar_style),
        Span::styled(format!(" {:>3}", volume), style),
    ])
}

/// The sidebar EQUALIZER section (Bug 5 + Defect 3). Top: preset
/// selector with clickable ◀ ▶ (applies immediately on cycle).
/// Below: the 10 bands + preamp + the VOL row as compact rows -
/// the SAME state the full Equalizer screen edits (VOL edits
/// settings.volume itself), so both views are always in sync.
/// Compact mode (short terminals) keeps the preset row and folds
/// VOL into the hint row.
fn render_sidebar_eq(f: &mut Frame, app: &mut App, area: Rect, compact: bool) {
    if area.height == 0 {
        return;
    }
    let focused = app.focus == PanelFocus::EqSidebar;
    let sel = app.eq_sidebar_selected;
    let mut lines: Vec<Line> = Vec::new();
    // Header + preset share one row (v0.1.6: the row the layout
    // budget cannot afford twice - the visualizer lives above).
    let preset_style = if focused && sel == 0 {
        sel_style()
    } else {
        Style::default().fg(INK)
    };
    lines.push(Line::from(vec![
        Span::styled(if focused && sel == 0 { "> " } else { "  " }, preset_style),
        Span::styled("EQUALIZER  ", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
        Span::styled("◀ ", Style::default().fg(AMBER).add_modifier(Modifier::BOLD)),
        Span::styled(app.settings.eq.preset_name.clone(), preset_style),
        Span::styled(" ▶", Style::default().fg(AMBER).add_modifier(Modifier::BOLD)),
    ]));
    // Click targets on the preset row: left part = prev, right = next.
    let prow = Rect { x: area.x, y: area.y, width: area.width, height: 1 };
    let third = (prow.width / 3).max(1);
    app.click_map.push((Rect { width: third, ..prow }, ClickTarget::EqPresetPrev));
    app.click_map.push((
        Rect { x: prow.x + prow.width.saturating_sub(third), width: third, ..prow },
        ClickTarget::EqPresetNext,
    ));
    if compact {
        // VOL folds into the hint row at small heights (Defect 3's
        // documented compact behavior); the full row returns at
        // taller sizes, and key 4 opens the full EQ screen anyway.
        lines.push(Line::from(vec![
            Span::styled(format!("VOL {} ", app.settings.volume), Style::default().fg(INK).add_modifier(Modifier::BOLD)),
            Span::styled("· press 4 for the full EQ", Style::default().fg(FAINT)),
        ]));
        f.render_widget(Paragraph::new(lines), area);
        return;
    }
    // Manual band rows: label width adapts to the sidebar width.
    let bar_w = (area.width as usize).saturating_sub(11).clamp(4, 18);
    for (i, freq) in BAND_FREQS.iter().enumerate() {
        let row_idx = i + 1;
        lines.push(eq_band_line(&freq_label(*freq), app.settings.eq.bands[i], focused && sel == row_idx, bar_w));
        app.click_map.push((
            Rect { x: area.x, y: area.y + 1 + i as u16, width: area.width, height: 1 },
            ClickTarget::EqBandRow(row_idx),
        ));
    }
    lines.push(eq_band_line("PRE", app.settings.eq.preamp, focused && sel == 11, bar_w));
    app.click_map.push((
        Rect { x: area.x, y: area.y + 11, width: area.width, height: 1 },
        ClickTarget::EqBandRow(11),
    ));
    // VOL row below PRE (Defect 3): click/drag sets the volume.
    lines.push(vol_line(app.settings.volume, focused && sel == 12, bar_w));
    app.click_map.push((
        Rect { x: area.x, y: area.y + 12, width: area.width, height: 1 },
        ClickTarget::EqVolumeRow,
    ));
    let border = if focused { Style::default().fg(AMBER) } else { Style::default().fg(LINE_C) };
    let _ = border;
    f.render_widget(Paragraph::new(lines), area);
}

/// The Home middle box (Feature B, v0.1.5): trending music from
/// BOTH guest sources, merged. The title is honest about what is
/// on screen: TRENDING NOW only for real trending data (fresh or
/// cached, the cache carrying its "updated HH:MM" footnote);
/// EXAMPLE TRACKS when both sources failed and only the seeded
/// examples remain - seed data is never labeled trending.
fn render_recent_box(f: &mut Frame, app: &mut App, area: Rect) {
    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled("                                              [ Lyrics (L) ]", Style::default().fg(AMBER).add_modifier(Modifier::BOLD))),
        Line::from(""),
    ];
    // Mouse: the Lyrics header line toggles the lyrics view; track
    // rows select (click) / play (double click).
    app.click_map.push((
        Rect { x: area.x + 1, y: area.y + 1, width: area.width.saturating_sub(2), height: 1 },
        ClickTarget::LyricsToggle,
    ));
    let title = match app.home_kind {
        HomeKind::Trending => " TRENDING NOW — FROM BOTH SOURCES ",
        HomeKind::Examples => " EXAMPLE TRACKS ",
        HomeKind::Loading => " TRENDING NOW — FROM BOTH SOURCES ",
    };
    if app.home_kind == HomeKind::Loading {
        lines.push(Line::from(Span::styled("Loading trending...", Style::default().fg(DIM))));
    }
    let current_idx = app.engine.current_index;
    let has_note = app.home_note.is_some();
    let visible = (area.height as usize).saturating_sub(4 + if has_note { 1 } else { 0 }).max(1);
    for (i, t) in app.home_tracks.iter().enumerate().take(visible) {
        let playing = current_idx.map(|ci| app.engine.queue.get(ci).map(|q| q.id == t.id).unwrap_or(false)).unwrap_or(false);
        lines.push(track_line(t, i == app.home_selected.min(app.home_tracks.len().saturating_sub(1)), playing, Some(i)));
        app.click_map.push((
            Rect { x: area.x + 1, y: area.y + 3 + i as u16, width: area.width.saturating_sub(2), height: 1 },
            ClickTarget::HomeTrack(i),
        ));
    }
    if let Some(note) = &app.home_note {
        lines.push(Line::from(Span::styled(note.clone(), Style::default().fg(FAINT))));
    }
    let p = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).border_style(focus_border(app, PanelFocus::Main)).title(title));
    f.render_widget(p, area);
}

fn render_lyrics_box(f: &mut Frame, app: &mut App, area: Rect) {
    let track = app.current_track();
    let (title, artist) = match &track {
        Some(t) => (t.title.clone(), t.artist.clone()),
        None => ("-".to_string(), "-".to_string()),
    };
    // v0.1.6: while a fetch for the CURRENT track is in flight the
    // box says so - the previous track's lines are already gone
    // (the state was invalidated at the track change, not here).
    let mode = if app.lyrics_loading {
        "loading"
    } else {
        match &app.lyrics {
            Some(LyricsResult::Synced(_)) => "synced (LRC)",
            Some(LyricsResult::Plain(_)) => "plain (no timing)",
            _ => "not available",
        }
    };
    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(title.clone(), Style::default().fg(INK).add_modifier(Modifier::BOLD))),
        Line::from(Span::styled(artist.clone(), Style::default().fg(DIM))),
        Line::from(Span::styled(format!("Source: LRCLib · {}", mode), Style::default().fg(GOLD))),
        Line::from(format!("Lyrics offset: {:+.1}s  ( +/- shift · L back to Trending Now )", app.lyrics_offset)),
        Line::from(""),
    ];
    if app.lyrics_loading {
        lines.push(Line::from(Span::styled(
            format!("Loading lyrics for {} - {}...", title, artist),
            Style::default().fg(DIM),
        )));
    }
    match &app.lyrics {
        Some(LyricsResult::Synced(lrc)) => {
            // Position comes from mpv's IPC report (Bug 2), shifted
            // by the user's sync offset.
            let pos = effective_position(app.engine.position_secs, app.lyrics_offset);
            let active = active_line_index(lrc, pos);
            let height = (area.height as usize).saturating_sub(8).max(3);
            let center = active.unwrap_or(0);
            let start = center.saturating_sub(height / 2);
            for (i, l) in lrc.iter().enumerate().skip(start).take(height) {
                if Some(i) == active {
                    lines.push(Line::from(Span::styled(
                        format!("> {}", l.text),
                        Style::default().fg(Color::Black).bg(AMBER).add_modifier(Modifier::BOLD),
                    )));
                } else {
                    lines.push(Line::from(Span::styled(l.text.clone(), Style::default().fg(DIM))));
                }
            }
        }
        Some(LyricsResult::Plain(text)) => {
            for l in text.lines().take(28) {
                lines.push(Line::from(Span::styled(l.to_string(), Style::default().fg(INK))));
            }
        }
        _ => {
            if !app.lyrics_loading {
                // The honest not-available message names the NEW
                // (current) track - never the previous track's text.
                lines.push(Line::from("Lyrics not available"));
                lines.push(Line::from(format!(
                    "LRCLib has no synced or plain lyrics for {} - {}.",
                    title, artist
                )));
                lines.push(Line::from("Playback continues normally — this box shows an honest status, not fabricated lyrics."));
            }
        }
    }
    let p = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).border_style(focus_border(app, PanelFocus::Main)).title(format!(" LYRICS — {} — SYNCED (LRCLIB) ", title)))
        .wrap(Wrap { trim: false });
    f.render_widget(p, area);
}

fn render_np_card(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(52), Constraint::Percentage(48)])
        .split(area);
    let mut lines: Vec<Line> = Vec::new();
    match app.engine.current_track() {
        Some(t) => {
            let prov = app.engine.now_playing_provenance.clone().unwrap_or_else(|| t.audio_provenance());
            lines.push(Line::from(vec![
                badge(t),
                Span::styled(format!(" {}", t.title), Style::default().fg(INK).add_modifier(Modifier::BOLD)),
            ]));
            lines.push(Line::from(Span::styled(t.artist.clone(), Style::default().fg(DIM))));
            lines.push(Line::from(""));
            lines.push(Line::from(format!("{} / {}", format_time(app.engine.position_secs), format_time(app.engine.display_duration()))));
            let play_glyph = if app.engine.is_playing { "❚❚" } else { "▶" };
            lines.push(Line::from(Span::styled(
                format!("[|<] [{}] [>|]   Vol {}", play_glyph, app.settings.volume),
                Style::default().fg(INK),
            )));
            lines.push(Line::from(format!(
                "Shuffle {} · Repeat {}",
                if app.engine.shuffle { "ON" } else { "OFF" },
                app.engine.repeat.label()
            )));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(format!("Audio: {}", prov), Style::default().fg(AMBER))));
            lines.push(Line::from(Span::styled(format!("EQ: {}", app.settings.eq.preset_name), Style::default().fg(GOLD))));
            lines.push(Line::from(Span::styled(
                format!("Clean Playback: {}", if app.settings.any_clean_active() { "ON" } else { "OFF" }),
                Style::default().fg(if app.settings.any_clean_active() { SPOT } else { FAINT }),
            )));
        }
        None => lines.push(Line::from("No track selected.")),
    }
    let p = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(LINE_C)).title(" NOW PLAYING "))
        .wrap(Wrap { trim: true });
    f.render_widget(p, rows[0]);

    // Up Next mini list under the Now Playing card.
    let mut lines: Vec<Line> = Vec::new();
    let cur = app.engine.current_index.unwrap_or(0);
    for (off, t) in app.engine.queue.iter().skip(cur + 1).take(6).enumerate() {
        lines.push(track_line(t, false, false, Some(off)));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled("Up next is empty.", Style::default().fg(DIM))));
    }
    let p2 = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(LINE_C)).title(" UP NEXT "));
    f.render_widget(p2, rows[1]);
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

fn render_search(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(3)])
        .split(area);
    let mode = if app.search_input_mode { "typing — Enter to search" } else { "press / to type" };
    let input = Paragraph::new(Line::from(vec![
        Span::styled("> ", Style::default().fg(AMBER).add_modifier(Modifier::BOLD)),
        Span::raw(app.search_query.clone()),
        Span::styled(format!("   ({})", mode), Style::default().fg(FAINT)),
    ]))
    .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(LINE_C)).title(" SEARCH — SPOTIFY + YT MUSIC "));
    f.render_widget(input, rows[0]);

    // Kind filter chips (Bug 6): All / Songs / Albums / Playlists -
    // [ ] keys or a click. Next to them, the PLATFORM selector
    // (v0.1.6): All / Spotify / YT Music - key f or a click; it
    // routes which sources the search actually fetches from.
    let chips = ["All", "Songs", "Albums", "Playlists"];
    let mut chip_spans: Vec<Span> = Vec::new();
    let mut chip_x = rows[0].x + 1;
    for (i, c) in chips.iter().enumerate() {
        let st = if app.search_kind as usize == i { sel_style() } else { Style::default().fg(DIM) };
        let label = format!(" {} ", c);
        let w = label.chars().count() as u16;
        app.click_map.push((
            Rect { x: chip_x, y: rows[1].y + 1, width: w, height: 1 },
            ClickTarget::SearchChip(i as u8),
        ));
        chip_x += w + 1;
        chip_spans.push(Span::styled(label, st));
        chip_spans.push(Span::raw(" "));
    }
    chip_spans.push(Span::styled(" │ Platform: ", Style::default().fg(FAINT)));
    chip_x += 13;
    let platforms = [
        crate::app::SearchPlatform::All,
        crate::app::SearchPlatform::Spotify,
        crate::app::SearchPlatform::YtMusic,
    ];
    for p in platforms {
        let st = if app.search_platform == p { sel_style() } else { Style::default().fg(DIM) };
        let label = format!(" {} ", p.label());
        let w = label.chars().count() as u16;
        app.click_map.push((
            Rect { x: chip_x, y: rows[1].y + 1, width: w, height: 1 },
            ClickTarget::SearchPlatformChip(p),
        ));
        chip_x += w + 1;
        chip_spans.push(Span::styled(label, st));
        chip_spans.push(Span::raw(" "));
    }
    chip_spans.push(Span::styled("  ([ ] kind · f platform · Enter on a track = play · Enter on album/playlist = open)", Style::default().fg(FAINT)));

    let all_rows = app.search_rows();
    // Scroll window: keep the selected selectable row visible.
    let height = (rows[1].height as usize).saturating_sub(7).max(1);
    let mut sel_abs = 0usize;
    {
        let mut ord = 0usize;
        for (ai, r) in all_rows.iter().enumerate() {
            if r.is_selectable() {
                if ord == app.search_selected {
                    sel_abs = ai;
                    break;
                }
                ord += 1;
            }
        }
    }
    let start = if sel_abs >= height { sel_abs + 1 - height } else { 0 };
    let mut lines: Vec<Line> = vec![Line::from(chip_spans), Line::from("")];
    let mut ord = 0usize;
    for (ai, r) in all_rows.iter().enumerate() {
        if r.is_selectable() {
            if ai >= start && ai < start + height {
                let selected = ord == app.search_selected;
                let row_y = rows[1].y + 1 + (lines.len() as u16);
                match r {
                    SearchRow::Track(i) => {
                        if let Some(t) = app.search_tracks.get(*i) {
                            lines.push(track_line(t, selected, false, None));
                        }
                    }
                    SearchRow::Album(i) => {
                        if let Some(a) = app.search_albums.get(*i) {
                            lines.push(catalog_line(
                                "ALBUM",
                                &a.title,
                                &a.artist,
                                &a.year,
                                a.source,
                                selected,
                            ));
                        }
                    }
                    SearchRow::Playlist(i) => {
                        if let Some(p) = app.search_playlists.get(*i) {
                            lines.push(catalog_line(
                                "PLAYLIST",
                                &p.title,
                                &p.info,
                                "",
                                p.source,
                                selected,
                            ));
                        }
                    }
                    SearchRow::Section(_) => {}
                }
                app.click_map.push((
                    Rect { x: rows[1].x + 1, y: row_y, width: rows[1].width.saturating_sub(2), height: 1 },
                    ClickTarget::SearchRow(ord),
                ));
            }
            ord += 1;
        } else if let SearchRow::Section(name) = r {
            if ai >= start && ai < start + height {
                lines.push(Line::from(Span::styled(
                    format!("── {} ──", name),
                    Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
                )));
            }
        }
    }
    if all_rows.is_empty() {
        lines.push(Line::from(Span::styled(
            "No results yet. Press / and type a search term — both sources run in Guest Mode, no login.",
            Style::default().fg(DIM),
        )));
    }
    // Inline per-source failure notes (honest, never blocking).
    for n in &app.search_notes {
        lines.push(Line::from(Span::styled(format!("Note: {}", n), Style::default().fg(YTM))));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Albums & playlists from YT Music (guest Innertube) + Spotify (Guest Mode). Enter on an album/playlist opens its track list.",
        Style::default().fg(FAINT),
    )));
    let scope = match app.search_platform {
        crate::app::SearchPlatform::All => "",
        crate::app::SearchPlatform::Spotify => " [SPOTIFY ONLY]",
        crate::app::SearchPlatform::YtMusic => " [YT MUSIC ONLY]",
    };
    let title = format!(
        " RESULTS{} — {} SONGS · {} ALBUMS · {} PLAYLISTS ",
        scope,
        app.search_tracks.len(),
        app.search_albums.len(),
        app.search_playlists.len()
    );
    let p = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).border_style(focus_border(app, PanelFocus::Main)).title(title))
        .wrap(Wrap { trim: true });
    f.render_widget(p, rows[1]);
}

/// One album/playlist result row: kind tag + source badge + title.
fn catalog_line(kind: &str, title: &str, sub: &str, year: &str, source: Source, selected: bool) -> Line<'static> {
    let style = if selected { sel_style() } else { Style::default().fg(INK) };
    let mut spans = vec![
        Span::styled(if selected { "* " } else { "  " }, style),
        Span::styled(format!("[{}] ", kind), Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
        Span::styled(source.label().to_string(), Style::default().fg(source_color(source)).add_modifier(Modifier::BOLD)),
        Span::styled(format!(" {}", title), style),
    ];
    if !sub.is_empty() {
        spans.push(Span::styled(format!(" — {}", sub), if selected { sel_style() } else { Style::default().fg(DIM) }));
    }
    if !year.is_empty() {
        spans.push(Span::styled(format!(" ({})", year), Style::default().fg(FAINT)));
    }
    Line::from(spans)
}

/// The opened playlist/album track list (Bug 3/6 browse view),
/// rendered over the main area with the sidebar still visible.
fn render_browse(f: &mut Frame, app: &mut App, area: Rect) {
    let b = match &app.browse {
        Some(b) => b.clone(),
        None => return,
    };
    let mut lines: Vec<Line> = Vec::new();
    if let Some(note) = &b.note {
        lines.push(Line::from(Span::styled(note.clone(), Style::default().fg(AMBER))));
        lines.push(Line::from(""));
    }
    let height = (area.height as usize).saturating_sub(4 + if b.note.is_some() { 2 } else { 0 }).max(1);
    let start = if b.selected >= height { b.selected + 1 - height } else { 0 };
    for (i, t) in b.tracks.iter().enumerate().skip(start).take(height) {
        lines.push(track_line(t, i == b.selected, false, Some(i)));
        app.click_map.push((
            Rect { x: area.x + 1, y: area.y + 1 + (lines.len() as u16) - 1, width: area.width.saturating_sub(2), height: 1 },
            ClickTarget::BrowseTrack(i),
        ));
    }
    if b.tracks.is_empty() && b.note.is_none() {
        lines.push(Line::from(Span::styled("This list is empty.", Style::default().fg(DIM))));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Enter / double-click = play · a = add to queue · Esc = back",
        Style::default().fg(FAINT),
    )));
    let p = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).border_style(focus_border(app, PanelFocus::Main)).title(format!(" {} — {} TRACKS ", b.title, b.tracks.len())))
        .wrap(Wrap { trim: true });
    f.render_widget(p, area);
}

// ---------------------------------------------------------------------------
// Queue
// ---------------------------------------------------------------------------

fn render_queue(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(5), Constraint::Min(4)])
        .split(area);
    let mut now_lines: Vec<Line> = Vec::new();
    match app.engine.current_track() {
        Some(t) => {
            let prov = app.engine.now_playing_provenance.clone().unwrap_or_else(|| t.audio_provenance());
            now_lines.push(Line::from(vec![
                Span::styled(if app.engine.is_playing { "❚❚ " } else { "▶ " }, Style::default().fg(AMBER)),
                badge(t),
                Span::styled(format!(" {} — {}   {} / {}", t.title, t.artist, format_time(app.engine.position_secs), format_time(app.engine.display_duration())), Style::default().fg(INK)),
            ]));
            now_lines.push(Line::from(Span::styled(format!("Audio: {}", prov), Style::default().fg(DIM))));
        }
        None => now_lines.push(Line::from("Nothing playing yet.")),
    }
    // Autoplay state lives in the Queue header area (key A toggles).
    now_lines.push(Line::from(vec![
        Span::styled(
            format!("Autoplay: {}", if app.settings.autoplay { "ON" } else { "OFF" }),
            Style::default().fg(if app.settings.autoplay { SPOT } else { FAINT }).add_modifier(Modifier::BOLD),
        ),
        Span::styled("  (A toggles · radio picks from YT Music)", Style::default().fg(FAINT)),
        Span::styled(
            match app.autoplay_from {
                Some(b) => format!("  ·  autoplay picks start at #{}", b + 1),
                None => String::new(),
            },
            Style::default().fg(FAINT),
        ),
    ]));
    let now_p = Paragraph::new(now_lines)
        .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(LINE_C)).title(" NOW PLAYING "));
    f.render_widget(now_p, rows[0]);

    let mut lines: Vec<Line> = Vec::new();
    let height = (rows[1].height as usize).saturating_sub(5).max(1);
    for (i, t) in app.engine.queue.iter().enumerate().take(height) {
        let mut line = track_line(t, i == app.queue_selected, Some(i) == app.engine.current_index, Some(i));
        if app.is_autoplay_pick(i) {
            // Machine picks are labeled, never disguised as the
            // user's own choices.
            line.spans.push(Span::styled(" · autoplay", Style::default().fg(FAINT)));
        }
        lines.push(line);
        app.click_map.push((
            Rect { x: rows[1].x + 1, y: rows[1].y + 1 + i as u16, width: rows[1].width.saturating_sub(2), height: 1 },
            ClickTarget::QueueTrack(i),
        ));
    }
    if lines.is_empty() {
        lines.push(Line::from("Queue is empty. Add tracks from Search (key a)."));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Every track carries a SPOT/YTM label — when the turn changes source, the player switches backends automatically (Spotify: audio matched via yt-dlp + mpv) with no manual pause.",
        Style::default().fg(DIM),
    )));
    lines.push(Line::from(Span::styled(
        format!(
            "Active backend: {} · Automatic backend switches: {}",
            app.engine.active_backend_source().map(|s| s.name()).unwrap_or("-"),
            app.engine.backend_switches
        ),
        Style::default().fg(FAINT),
    )));
    let title = format!(" UP NEXT — {} TRACKS · MIXED 2 SOURCES ", app.engine.queue.len());
    let p = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).border_style(focus_border(app, PanelFocus::Main)).title(title))
        .wrap(Wrap { trim: true });
    f.render_widget(p, rows[1]);
}

// ---------------------------------------------------------------------------
// Equalizer
// ---------------------------------------------------------------------------

fn render_eq(f: &mut Frame, app: &mut App, area: Rect) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(68), Constraint::Percentage(32)])
        .split(area);
    let mut gains: Vec<f64> = app.settings.eq.bands.to_vec();
    gains.push(app.settings.eq.preamp);
    let mut lines: Vec<Line> = vec![
        Line::from(vec![
            Span::styled("Active preset: ", Style::default().fg(DIM)),
            Span::styled(app.settings.eq.preset_name.clone(), Style::default().fg(AMBER).add_modifier(Modifier::BOLD)),
            Span::styled("   DSP · applies to Spotify + YT Music", Style::default().fg(FAINT)),
        ]),
        Line::from(""),
    ];
    // dB values ABOVE the columns, like the artifact's sliders.
    let mut vals = String::new();
    for (i, g) in gains.iter().enumerate() {
        let v = format!("{:+.0}", g);
        if i == app.eq_selected {
            vals.push_str(&format!("[{:^4}]", v));
        } else {
            vals.push_str(&format!("{:^6}", v));
        }
    }
    lines.push(Line::from(Span::styled(vals, Style::default().fg(INK).add_modifier(Modifier::BOLD))));
    for level in (1..=6).rev() {
        lines.push(bar_row(&gains, (level * 2) as f64, true));
    }
    lines.push(bar_row_zero());
    for level in 1..=6 {
        lines.push(bar_row(&gains, -(level * 2) as f64, false));
    }
    let mut labels = String::new();
    for (i, fr) in BAND_FREQS.iter().enumerate() {
        let lab = freq_label(*fr);
        if i == app.eq_selected {
            labels.push_str(&format!("[{:^4}]", lab));
        } else {
            labels.push_str(&format!("{:^6}", lab));
        }
    }
    labels.push_str(&format!("{:^6}", "PRE"));
    lines.push(Line::from(Span::styled(labels, Style::default().fg(AMBER))));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("Gain range -12 to +12 dB per band", Style::default().fg(DIM))));
    let p = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).border_style(focus_border(app, PanelFocus::Main)).title(" EQUALIZER — 10 BAND + PREAMP "));
    f.render_widget(p, cols[0]);

    let names = app.settings.eq.preset_names();
    let mut plines: Vec<Line> = Vec::new();
    for (i, n) in names.iter().enumerate() {
        let active = *n == app.settings.eq.preset_name;
        let sel = i == app.eq_preset_selected;
        let style = if sel {
            sel_style()
        } else if active {
            Style::default().fg(AMBER).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(INK)
        };
        plines.push(Line::from(Span::styled(format!("{}{}", if active { "* " } else { "  " }, n), style)));
    }
    // Always show the three Custom slots, even before any is saved.
    for slot in 1..=3 {
        let nm = format!("Custom {}", slot);
        if !names.contains(&nm) {
            plines.push(Line::from(Span::styled(format!("  {} (empty)", nm), Style::default().fg(FAINT))));
        }
    }
    plines.push(Line::from(""));
    plines.push(Line::from(Span::styled("[ Save Preset (s) ]  [ Reset Flat (0) ]", Style::default().fg(INK).add_modifier(Modifier::BOLD))));
    plines.push(Line::from(""));
    // Volume mirror (Defect 3): the same settings.volume the
    // sidebar VOL row and the bottom bar edit - one state, three
    // views, always in sync.
    plines.push(Line::from(vec![
        Span::styled("Volume: ", Style::default().fg(DIM)),
        Span::styled(format!("{}", app.settings.volume), Style::default().fg(INK).add_modifier(Modifier::BOLD)),
        Span::styled("  (VOL in the sidebar EQ / bottom bar)", Style::default().fg(FAINT)),
    ]));
    plines.push(Line::from(Span::styled(
        format!("Preset {} applied to both backends.", app.settings.eq.preset_name),
        Style::default().fg(DIM),
    )));
    plines.push(Line::from(""));
    plines.push(Line::from("mpv filter:"));
    let filter = app.settings.eq.to_mpv_filter();
    plines.push(Line::from(Span::styled(
        if filter.is_empty() { "(Flat — no filter)".to_string() } else { filter },
        Style::default().fg(FAINT),
    )));
    let pp = Paragraph::new(plines)
        .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(LINE_C)).title(" Template / Preset "))
        .wrap(Wrap { trim: false });
    f.render_widget(pp, cols[1]);
}

fn bar_row(gains: &[f64], threshold: f64, positive: bool) -> Line<'static> {
    let mut s = String::new();
    for g in gains {
        let filled = if positive { *g >= threshold } else { *g <= threshold };
        s.push_str(if filled { "  ██  " } else { "  ░░  " });
    }
    Line::from(Span::styled(s, Style::default().fg(AMBER)))
}

fn bar_row_zero() -> Line<'static> {
    let mut s = String::new();
    for _ in 0..11 {
        s.push_str("  ==  ");
    }
    Line::from(Span::styled(s, Style::default().fg(FAINT)))
}

// Settings / Clean Playback
// ---------------------------------------------------------------------------

fn render_settings(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(5), Constraint::Min(6), Constraint::Length(9)])
        .split(area);
    // Master toggle box
    let master_sel = app.settings_selected == 0;
    let master_style = if master_sel { sel_style() } else { Style::default().fg(INK) };
    let master = Paragraph::new(vec![
        Line::from(Span::styled(
            format!("Clean Playback — master switch    [ {} ]", if app.settings.clean_playback_master { "ON" } else { "OFF" }),
            master_style.add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "Controls both AdBlocks at once. Turning this switch off disables the sub-toggles below it.",
            Style::default().fg(DIM),
        )),
        Line::from(Span::styled(
            "The Clean Playback status always shows in the sidebar on every screen and in the Now Playing panel.",
            Style::default().fg(FAINT),
        )),
    ])
    .block(Block::default().borders(Borders::ALL).border_style(focus_border(app, PanelFocus::Main)).title(" SETTINGS — CLEAN PLAYBACK (ADBLOCK) "))
    .wrap(Wrap { trim: true });
    f.render_widget(master, rows[0]);

    // Two sub-panels side by side, like the artifact.
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows[1]);
    render_clean_panel(
        f,
        cols[0],
        " Spotify AdBlock ",
        SPOT,
        app.settings.spotify_adblock && app.settings.clean_playback_master,
        app.settings_selected == 1,
        "Ad-typed items detected in the queue → mute + auto-skip automatically. Banners & upsells are never rendered because the TUI does not draw Spotify's UI.",
    );
    render_clean_panel(
        f,
        cols[1],
        " YT Music AdBlock + SponsorBlock ",
        YTM,
        app.settings.ytmusic_adblock_sponsorblock && app.settings.clean_playback_master,
        app.settings_selected == 2,
        "The audio stream is taken directly via yt-dlp, so the player's pre-roll/mid-roll ads are never fetched. SponsorBlock cuts sponsor / non-music segments inside the video itself.",
    );

    // Other settings
    let vol_sel = app.settings_selected == 3;
    let vstyle = if vol_sel { sel_style() } else { Style::default().fg(INK) };
    let lines = vec![
        Line::from(format!("Active equalizer preset: {} (key 4 = Equalizer)", app.settings.eq.preset_name)),
        Line::from("Lyrics source: LRCLib · synced LRC — shown replacing the Trending Now box on Home (key L)"),
        Line::from("Spotify audio: matched & played via YT Music (yt-dlp + mpv) — Guest Mode, no account at all"),
        Line::from(format!("Autoplay (YT Music radio picks): {} (key A toggles)", if app.settings.autoplay { "ON" } else { "OFF" })),
        Line::from(format!("Visualizer (cava): {} (key v toggles, V cycles the {} template)", if app.settings.visualizer { "ON" } else { "OFF" }, app.settings.visualizer_style)),
        Line::from(Span::styled(format!("Volume: {} (select this row, Left/Right to change)", app.settings.volume), vstyle)),
        Line::from(Span::styled(
            format!("Settings saved in: {}", crate::config::Settings::file_path().display()),
            Style::default().fg(FAINT),
        )),
    ];
    let p = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(LINE_C)).title(" SETTINGS — OTHER "))
        .wrap(Wrap { trim: true });
    f.render_widget(p, rows[2]);
}

fn render_clean_panel(f: &mut Frame, area: Rect, title: &str, color: Color, on: bool, selected: bool, desc: &str) {
    let switch_style = if selected { sel_style() } else { Style::default().fg(INK).add_modifier(Modifier::BOLD) };
    let lines = vec![
        Line::from(Span::styled(format!("[ {} ]", if on { "ON" } else { "OFF" }), switch_style)),
        Line::from(""),
        Line::from(Span::styled(desc.to_string(), Style::default().fg(DIM))),
    ];
    let p = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(title.to_string()).border_style(Style::default().fg(color)))
        .wrap(Wrap { trim: true });
    f.render_widget(p, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wordmark_is_the_exact_mathematical_italic_string() {
        // v0.1.7 (Fathir's pick): the wordmark is plain text -
        // Mathematical Italic L, y, r, e and nothing else.
        assert_eq!(WORDMARK, "𝐿𝑦𝑟𝑒");
        let cps: Vec<u32> = WORDMARK.chars().map(|c| c as u32).collect();
        assert_eq!(cps, vec![0x1D43F, 0x1D466, 0x1D45F, 0x1D452]);
        // No block characters, no ASCII art around it.
        assert!(!WORDMARK.contains('█'));
        assert_eq!(WORDMARK.chars().count(), 4);
    }

    #[test]
    fn nav_labels_align_with_nav_screens() {
        assert_eq!(NAV_LABELS.len(), NAV_SCREENS.len());
        assert_eq!(NAV_LABELS.len(), 5, "v0.1.5: five screens, Accounts is gone");
        assert_eq!(NAV_LABELS[0], "Now Playing");
        assert_eq!(NAV_LABELS[4], "Clean Playback");
        // No nav item may be a non-screen (the old "Library" entry),
        // and there is no Accounts entry any more.
        assert!(!NAV_LABELS.contains(&"Library"));
        assert!(!NAV_LABELS.contains(&"Settings"));
        assert!(!NAV_LABELS.contains(&"Accounts"));
    }

    fn render_text(app: &mut App, w: u16, h: u16) -> String {
        let backend = ratatui::backend::TestBackend::new(w, h);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| render(f, app)).unwrap();
        let buf = term.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    fn clean_app() -> App {
        let mut app = App::new();
        // Deterministic state regardless of the dev machine's
        // saved settings file.
        app.settings.clean_playback_master = true;
        app.settings.spotify_adblock = true;
        app.settings.autoplay = true;
        app.settings.visualizer = true;
        app.settings.visualizer_style = "Bars".to_string();
        // Deterministic sidebar: exactly one saved Spotify link.
        app.settings.saved_playlists = vec![crate::config::SavedPlaylist {
            source: Source::Spotify,
            kind: crate::config::SavedKind::Playlist,
            id: "37i9dQZEVXbMDoHDwVN2tF".to_string(),
            name: "Top 50 - Global".to_string(),
            url: "https://open.spotify.com/playlist/37i9dQZEVXbMDoHDwVN2tF".to_string(),
        }];
        app
    }

    #[test]
    fn sidebar_shows_logo_and_no_guest_status_or_seeded_playlists() {
        let mut app = clean_app();
        app.set_screen(Screen::NowPlaying);
        let text = render_text(&mut app, 140, 40);
        // The v0.1.7 wordmark: plain serif-italic text in the
        // sidebar header - and NO block-character logo rows there.
        assert!(text.contains(WORDMARK), "sidebar wordmark missing");
        let header_slice: String = text
            .lines()
            .take(3)
            .map(|l| l.chars().take(32).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(header_slice.contains(WORDMARK), "wordmark in the header rows: {:?}", header_slice);
        assert!(!header_slice.contains('█'), "no block logo rows in the header: {:?}", header_slice);
        assert!(text.contains("Clean Playback: ON"), "clean line kept");
        // ...and the deleted lines are GONE.
        for gone in [
            "Mode Tamu",
            "tanpa login",
            "Liked Songs",
            "Senja & Kopi",
            "Fokus Coding",
            "Liked Music",
            "Lo-fi Indonesia",
            "Live Sessions",
            "Search Terpadu",
            "Queue Campuran",
        ] {
            assert!(!text.contains(gone), "removed string still rendered: {}", gone);
        }
        // Saved-only playlists: section + entry + paste entry + VOL.
        assert!(text.contains("PLAYLISTS"), "playlist header");
        assert!(text.contains("PLAYLIST — SPOTIFY"), "spotify section");
        assert!(text.contains("Top 50 - Global"), "saved entry");
        assert!(!text.contains("PLAYLIST — YT MUSIC"), "empty source gets no section");
        assert!(text.contains("[ + Paste Playlist Link ]"), "paste entry");
        assert!(text.contains("VOL"), "sidebar VOL row");
        assert!(text.contains("TRENDING NOW — FROM BOTH SOURCES"), "home box title");
    }

    #[test]
    fn splash_shows_single_guest_line() {
        let mut app = clean_app();
        assert_eq!(app.screen, Screen::Splash);
        let text = render_text(&mut app, 140, 40);
        assert!(text.contains("Guest Mode — no login needed"), "guest line");
        assert!(text.contains("Enter — Now Playing [1]"), "enter box");
        assert!(!text.contains("Mode Tamu"), "old status gone");
        assert!(!text.contains("Tamu"), "no per-source guest lines");
        // The v0.1.7 title card: the text wordmark, and NO block
        // characters anywhere on the splash (the block-letter
        // wordmark is gone; nothing else on this screen uses them).
        assert!(text.contains(WORDMARK), "splash wordmark");
        assert!(!text.contains('█'), "no block glyphs on the splash");
        // The splash layout also fits the smaller terminal.
        let small = render_text(&mut app, 100, 30);
        assert!(small.contains(WORDMARK), "wordmark at 100x30");
        assert!(small.contains("Guest Mode — no login needed"), "guest line at 100x30");
        assert!(!small.contains('█'), "no block glyphs at 100x30");
    }

    #[test]
    fn visualizer_bars_follow_the_injected_frame() {
        let mut app = clean_app();
        app.set_screen(Screen::NowPlaying);
        app.viz.state = crate::visualizer::VizState::Running;
        app.viz.set_frame(vec![255, 0, 128, 255]);
        app.tick_visualizer(); // prime the display buffer
        let text = render_text(&mut app, 140, 40);
        let lines: Vec<&str> = text.lines().collect();
        let at = lines
            .iter()
            .position(|l| l.contains("VISUALIZER"))
            .expect("visualizer header at 140x40");
        // The header names the active template (v0.1.7).
        assert!(lines[at].contains("VISUALIZER — Bars"), "header: {:?}", lines[at]);
        // The first bar (3 wide) sits at the sidebar's left edge:
        // frame[0]=255 tops it out; the bar after the gap (fed by
        // frame[1]=0 through the resample) stays low.
        let top_bar_row = lines[at + 1];
        let cells: Vec<char> = top_bar_row.chars().collect();
        assert_eq!(cells[1], '█', "255 bar must top out: {:?}", top_bar_row);
        assert_eq!(cells[5], ' ', "next bar stays low at the top: {:?}", top_bar_row);
        // Silence in -> silence out (no fake motion): the smoothed
        // display needs its capped-release ticks to fall, then the
        // box is empty.
        app.viz.set_frame(vec![0, 0, 0, 0]);
        for _ in 0..40 {
            app.tick_visualizer();
        }
        let text2 = render_text(&mut app, 140, 40);
        let lines2: Vec<&str> = text2.lines().collect();
        let at2 = lines2.iter().position(|l| l.contains("VISUALIZER")).unwrap();
        let row2: String = lines2[at2 + 1].chars().skip(1).take(8).collect();
        assert!(!row2.contains('█'), "silent frame must render empty bars: {:?}", row2);
    }

    #[test]
    fn visualizer_header_names_the_chosen_template() {
        let mut app = clean_app();
        app.set_screen(Screen::NowPlaying);
        app.viz.state = crate::visualizer::VizState::Running;
        app.viz.set_frame(vec![200; 16]);
        app.tick_visualizer();
        for style in ["Bars", "Mirror", "Wave", "Line", "LED"] {
            app.settings.visualizer_style = style.to_string();
            let text = render_text(&mut app, 140, 40);
            assert!(
                text.contains(&format!("VISUALIZER — {}", style)),
                "header names {}: missing",
                style
            );
        }
        // Cycling (key V / click) advances and persists the choice.
        app.settings.visualizer_style = "Bars".to_string();
        app.cycle_visualizer_style();
        assert_eq!(app.settings.visualizer_style, "Mirror");
        app.cycle_visualizer_style();
        assert_eq!(app.settings.visualizer_style, "Wave");
        assert!(app.status_message.contains("Wave"), "status: {}", app.status_message);
    }

    #[test]
    fn visualizer_collapses_to_one_line_on_short_terminals() {
        let mut app = clean_app();
        app.set_screen(Screen::NowPlaying);
        let text = render_text(&mut app, 100, 30);
        assert!(text.contains("VISUALIZER — press v"), "collapsed line: {}", text);
        // EQ + playlists keep priority in the small layout.
        assert!(text.contains("PLAYLISTS"), "playlists survive");
        assert!(text.contains("EQUALIZER"), "EQ survives");
    }
}
