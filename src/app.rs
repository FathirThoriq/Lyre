use std::time::Instant;

use ratatui::layout::Rect;

use crate::backend::{PlayerBackend, SpotifyBackend, YtMusicBackend};
use crate::config::{self, SavedKind, SavedPlaylist, Settings};
use crate::diag;
use crate::innertube;
use crate::lyrics::{fetch_lyrics, LyricsResult};
use crate::player::{PlayOutcome, PlayerEngine};
use crate::track::{demo_tracks, AlbumRef, PlaylistRef, Source, Track, TrackKey};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Screen {
    Splash,
    NowPlaying,
    Search,
    Queue,
    Equalizer,
    Settings,
}

impl Screen {
    pub fn from_number(n: u8) -> Screen {
        match n {
            1 => Screen::NowPlaying,
            2 => Screen::Search,
            3 => Screen::Queue,
            4 => Screen::Equalizer,
            5 => Screen::Settings,
            _ => Screen::NowPlaying,
        }
    }
}

/// THE navigation system: one list shared by the sidebar and every
/// other reference - same order, same numbers, same names. v0.1.5:
/// five screens (the Accounts screen is gone with the logins).
pub const NAV_SCREENS: [Screen; 5] = [
    Screen::NowPlaying,
    Screen::Search,
    Screen::Queue,
    Screen::Equalizer,
    Screen::Settings,
];

/// Index of a screen inside NAV_SCREENS (Splash highlights Home).
pub fn nav_index(s: Screen) -> usize {
    NAV_SCREENS.iter().position(|x| *x == s).unwrap_or(0)
}

/// Which panel currently owns the arrow keys. Cycled with Tab:
/// Navigasi -> Daftar (main box) -> EQ Sidebar -> Now Playing bar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PanelFocus {
    Nav,
    Main,
    EqSidebar,
    Player,
}

impl PanelFocus {
    pub fn next(&self) -> PanelFocus {
        match self {
            PanelFocus::Nav => PanelFocus::Main,
            PanelFocus::Main => PanelFocus::EqSidebar,
            PanelFocus::EqSidebar => PanelFocus::Player,
            PanelFocus::Player => PanelFocus::Nav,
        }
    }
    pub fn prev(&self) -> PanelFocus {
        match self {
            PanelFocus::Nav => PanelFocus::Player,
            PanelFocus::Main => PanelFocus::Nav,
            PanelFocus::EqSidebar => PanelFocus::Main,
            PanelFocus::Player => PanelFocus::EqSidebar,
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            PanelFocus::Nav => "Navigation",
            PanelFocus::Main => "List",
            PanelFocus::EqSidebar => "EQ Sidebar",
            PanelFocus::Player => "Now Playing",
        }
    }
    pub fn keys_hint(&self) -> &'static str {
        match self {
            PanelFocus::Nav => "↑↓ select (incl. playlists), Enter open, x delete saved playlist",
            PanelFocus::Main => "↑↓ select, Enter play/open, ←→ seek ±5s / adjust",
            PanelFocus::EqSidebar => "↑↓ select row (preset/band/PRE/VOL), ←→ adjust",
            PanelFocus::Player => "←→ seek ±5s, ↑↓ volume, Space play/pause",
        }
    }
}

/// The one remaining input form (v0.1.5): paste a playlist share
/// link (Feature A). Rendered as a centered MODAL by ui.rs - it is
/// visible from every screen, mouse clicks are suspended while it
/// is open, and Esc always closes it (the v0.1.4 lesson: a form that
/// captures keys must never be invisible).
#[derive(Debug, Clone)]
pub struct InputForm {
    pub title: String,
    pub label: String,
    pub value: String,
}

/// What the Home middle box currently shows (Feature B).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HomeKind {
    /// Trending fetch in flight, nothing cached to show yet.
    Loading,
    /// Real trending tracks (fresh, or the disk cache while a
    /// refresh runs / after a failed refresh).
    Trending,
    /// Both sources failed and no cache exists: the seeded
    /// examples, titled honestly as CONTOH LAGU - never "tren".
    Examples,
}

/// Result of the background trending fetch: per-source outcomes so
/// the merge can note a partial failure honestly.
pub struct TrendingOutcome {
    pub spotify: Result<Vec<Track>, String>,
    pub ytm: Result<Vec<Track>, String>,
    /// Set when the Spotify half was served from its disk cache
    /// because a refresh was skipped or failed (v0.1.7: the 6-hour
    /// TTL / rate-limit cooldown) - shown under the Home box.
    pub spotify_note: Option<String>,
}

/// Merge the two trending lists into one ranked mixed list:
/// interleave by rank (Spotify #1, YTM #1, Spotify #2, ...),
/// dedupe the same song across sources (normalized artist+title;
/// the earlier - Spotify - copy wins; a dropped duplicate
/// backfills from the same source so the alternation survives),
/// cap at 30.
pub fn merge_trending(spotify: Vec<Track>, ytm: Vec<Track>) -> Vec<Track> {
    fn norm_key(t: &Track) -> String {
        format!("{}|{}", t.artist, t.title)
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '|')
            .flat_map(|c| c.to_lowercase())
            .collect::<String>()
    }
    fn take_unique(it: &mut std::vec::IntoIter<Track>, seen: &[String]) -> Option<Track> {
        while let Some(t) = it.next() {
            if !seen.contains(&norm_key(&t)) {
                return Some(t);
            }
        }
        None
    }
    let mut out: Vec<Track> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let mut si = spotify.into_iter();
    let mut yi = ytm.into_iter();
    loop {
        let mut progressed = false;
        if let Some(t) = take_unique(&mut si, &seen) {
            seen.push(norm_key(&t));
            out.push(t);
            progressed = true;
        }
        if let Some(t) = take_unique(&mut yi, &seen) {
            seen.push(norm_key(&t));
            out.push(t);
            progressed = true;
        }
        if !progressed || out.len() >= 30 {
            break;
        }
    }
    out.truncate(30);
    out
}

/// One row of the unified Search display: section headers group the
/// LAGU / ALBUM / PLAYLIST results; only item rows are selectable.
#[derive(Debug, Clone, PartialEq)]
pub enum SearchRow {
    Section(&'static str),
    Track(usize),
    Album(usize),
    Playlist(usize),
}

impl SearchRow {
    pub fn is_selectable(&self) -> bool {
        !matches!(self, SearchRow::Section(_))
    }
}

/// One entry of the sidebar's playlist list. Since v0.1.6 these
/// are ONLY playlists/albums saved from pasted share links
/// (Feature A) - the seeded example playlists are gone. Every entry
/// has a real id, opens into its real tracks, and is deletable
/// with `x`.
#[derive(Debug, Clone, PartialEq)]
pub struct SidebarPlaylist {
    pub source: Source,
    pub title: String,
    pub id: Option<String>,
    /// Spotify albums open through the album endpoint; everything
    /// else here is a playlist.
    pub album: bool,
    /// Index into settings.saved_playlists.
    pub saved_index: Option<usize>,
}

/// A deferred request to open a browse view (album / playlist track
/// list). Processed after a draw, like pending_play, so the
/// "Loading..." frame is visible first.
#[derive(Debug, Clone)]
pub enum BrowseRequest {
    Playlist { source: Source, id: String, title: String },
    Album { source: Source, id: String, title: String },
}

/// Which sources the Search screen queries (v0.1.6 platform
/// filter, key `f` / clickable chips). The filter ROUTES fetching:
/// Spotify never triggers Innertube/yt-dlp calls, YT Music never
/// triggers the Spotify anonymous-token API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SearchPlatform {
    All,
    Spotify,
    YtMusic,
}

impl SearchPlatform {
    pub fn next(&self) -> SearchPlatform {
        match self {
            SearchPlatform::All => SearchPlatform::Spotify,
            SearchPlatform::Spotify => SearchPlatform::YtMusic,
            SearchPlatform::YtMusic => SearchPlatform::All,
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            SearchPlatform::All => "All",
            SearchPlatform::Spotify => "Spotify",
            SearchPlatform::YtMusic => "YT Music",
        }
    }
    pub fn spotify_active(&self) -> bool {
        matches!(self, SearchPlatform::All | SearchPlatform::Spotify)
    }
    pub fn ytmusic_active(&self) -> bool {
        matches!(self, SearchPlatform::All | SearchPlatform::YtMusic)
    }
}

/// The open browse view: a titled track list (an opened playlist or
/// album) rendered over the main area. Tracks play with Enter and
/// queue with `a`, exactly like Search results.
#[derive(Debug, Clone)]
pub struct BrowseView {
    pub title: String,
    /// Honest provenance note (e.g. contents came from search
    /// matching rather than a real playlist). None for direct
    /// catalog content.
    pub note: Option<String>,
    pub tracks: Vec<Track>,
    pub selected: usize,
}

/// A parsed playlist/album share link (Feature A).
#[derive(Debug, Clone, PartialEq)]
pub struct LinkFetch {
    pub source: Source,
    pub kind: SavedKind,
    /// Spotify id, or the YT Music VL... browseId.
    pub id: String,
    /// The link as pasted (kept for the saved entry).
    pub url: String,
}

/// Parse + validate a pasted share link. Accepts:
/// - open.spotify.com/playlist/<id> and /album/<id> (intl- prefixed
///   paths and ?si= tracking params included)
/// - music.youtube.com/playlist?list=<id> and youtube.com/playlist?list=<id>
/// Everything else gets a specific, plain English refusal.
pub fn parse_playlist_link(input: &str) -> Result<LinkFetch, String> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err("Empty link - paste a Spotify / YT Music playlist link first.".to_string());
    }
    let url = if raw.contains("://") {
        raw.to_string()
    } else {
        format!("https://{}", raw)
    };
    let after_scheme = url.split("://").nth(1).unwrap_or("");
    let host = after_scheme.split(['/', '?', '#']).next().unwrap_or("").to_lowercase();
    let path_and_query = &after_scheme[host.len()..];

    if host == "spotify.link" {
        return Err(
            "That is a short spotify.link link - open it in a browser first, then paste the full open.spotify.com link."
                .to_string(),
        );
    }
    if host == "youtu.be" {
        return Err(
            "A youtu.be link points to a video, not a playlist - paste the full playlist link (music.youtube.com/playlist?list=...)."
                .to_string(),
        );
    }
    if host == "open.spotify.com" {
        let segments: Vec<&str> = path_and_query
            .split(['/', '?', '#'])
            .filter(|s| !s.is_empty())
            .collect();
        let mut idx = 0;
        // Localized share links carry an intl-xx segment first.
        if segments.first().map(|s| s.starts_with("intl-")).unwrap_or(false) {
            idx = 1;
        }
        let kind_seg = segments.get(idx).copied().unwrap_or("");
        let id_seg = segments.get(idx + 1).copied().unwrap_or("");
        let kind = match kind_seg {
            "playlist" => SavedKind::Playlist,
            "album" => SavedKind::Album,
            _ => {
                return Err(
                    "That Spotify link is not a playlist/album - paste a link containing /playlist/ or /album/."
                        .to_string(),
                )
            }
        };
        let id: String = id_seg.chars().take_while(|c| c.is_alphanumeric()).collect();
        if id.len() < 10 {
            return Err("The ID in that Spotify link is unreadable - make sure the link is complete.".to_string());
        }
        return Ok(LinkFetch {
            source: Source::Spotify,
            kind,
            id,
            url: raw.to_string(),
        });
    }
    if host == "music.youtube.com" || host == "youtube.com" || host == "www.youtube.com" || host == "m.youtube.com" {
        if !path_and_query.starts_with("/playlist") {
            return Err(
                "That YouTube link is not a playlist page - paste a music.youtube.com/playlist?list=... link".to_string(),
            );
        }
        let list = path_and_query
            .split("list=")
            .nth(1)
            .map(|rest| rest.chars().take_while(|c| c.is_alphanumeric() || *c == '-' || *c == '_').collect::<String>())
            .unwrap_or_default();
        if list.is_empty() {
            return Err("That link has no list= parameter - copy the playlist link from the Share button.".to_string());
        }
        // ytmusicapi get_playlist pattern: browse with "VL" + listId.
        let browse_id = if list.starts_with("VL") {
            list
        } else {
            format!("VL{}", list)
        };
        return Ok(LinkFetch {
            source: Source::YtMusic,
            kind: SavedKind::Playlist,
            id: browse_id,
            url: raw.to_string(),
        });
    }
    Err(format!(
        "Unknown host '{}' - paste an open.spotify.com or music.youtube.com link.",
        host
    ))
}

/// Pointer x as a 0.0..=1.0 ratio inside a rect (mouse seek / EQ).
fn ratio_in(rect: Rect, col: u16) -> f64 {
    if rect.width <= 1 {
        return 0.0;
    }
    ((col.saturating_sub(rect.x)) as f64 / (rect.width - 1) as f64).clamp(0.0, 1.0)
}

/// A clickable region registered during rendering (mouse support).
/// ui.rs rebuilds the map every frame; main.rs routes clicks
/// through App::mouse_click / mouse_drag.
#[derive(Debug, Clone, PartialEq)]
pub enum ClickTarget {
    NavItem(usize),
    PlaylistItem(usize),
    /// The "+ Tempel Link Playlist" sidebar entry (Feature A).
    PasteLink,
    HomeTrack(usize),
    QueueTrack(usize),
    /// Selectable-row ordinal within search_rows().
    SearchRow(usize),
    BrowseTrack(usize),
    ProgressBar,
    EqPresetPrev,
    EqPresetNext,
    /// Sidebar EQ band rows: 1..=10 bands, 11 = preamp (0 is the
    /// preset row, served by EqPresetPrev/Next).
    EqBandRow(usize),
    /// Sidebar EQ VOL row (Defect 3): click/drag sets the volume.
    EqVolumeRow,
    SearchChip(u8),
    /// The platform selector chips on the Search screen (v0.1.6).
    SearchPlatformChip(SearchPlatform),
    /// The sidebar visualizer box: a click cycles the template
    /// (v0.1.7, same as key `V`).
    VizBox,
    LyricsToggle,
    SplashEnter,
}

pub struct App {
    pub screen: Screen,
    pub should_quit: bool,
    pub engine: PlayerEngine,
    pub settings: Settings,
    /// The shared Spotify guest (anonymous-token) catalog client.
    pub spotify: SpotifyBackend,
    // Home (Feature B trending)
    pub home_tracks: Vec<Track>,
    pub home_kind: HomeKind,
    /// Footnote under the Home box: cache timestamp / partial
    /// failure note. None when fresh and complete.
    pub home_note: Option<String>,
    pub trending_rx: Option<std::sync::mpsc::Receiver<TrendingOutcome>>,
    // Selection state
    pub home_selected: usize,
    pub search_selected: usize,
    pub queue_selected: usize,
    // Search (unified: songs + albums + playlists, grouped)
    pub search_input_mode: bool,
    pub search_query: String,
    pub search_tracks: Vec<Track>,
    pub search_albums: Vec<AlbumRef>,
    pub search_playlists: Vec<PlaylistRef>,
    /// Kind filter chip: 0 = All, 1 = Songs, 2 = Albums, 3 = Playlists.
    pub search_kind: u8,
    /// Platform filter (v0.1.6): which sources run_search queries.
    pub search_platform: SearchPlatform,
    /// Inline per-source failure notes from the last search,
    /// deduplicated (the v0.1.4 duplicate-note bug).
    pub search_notes: Vec<String>,
    /// Session cache of completed searches (v0.1.7): key =
    /// (query, kind, platform filter). Re-running the same search
    /// (toggling the filters back and forth) restores the stored
    /// result instead of calling any API again. Only searches
    /// where every active source succeeded are stored.
    pub search_cache: std::collections::HashMap<(String, u8, SearchPlatform), (Vec<Track>, Vec<AlbumRef>, Vec<PlaylistRef>)>,
    // Browse view (opened playlist / album)
    pub browse: Option<BrowseView>,
    pub pending_browse: Option<BrowseRequest>,
    /// A pasted share link waiting to be fetched (Feature A).
    pub pending_link: Option<LinkFetch>,
    // Mouse (regions are rebuilt by ui::render every frame)
    pub click_map: Vec<(Rect, ClickTarget)>,
    pub last_click: Option<(Instant, ClickTarget)>,
    // Lyrics (in the Home middle box, replacing it in place).
    // v0.1.6: lyrics state is BOUND to the current track's identity
    // (TrackKey). Any track change invalidates the old lyrics
    // immediately and starts a background fetch for the new track;
    // a fetch result is applied only when its key still matches
    // the current track (stale results are dropped).
    pub show_lyrics: bool,
    pub lyrics: Option<LyricsResult>,
    /// The track the current lyrics (or in-flight fetch) belong to.
    pub lyrics_key: Option<TrackKey>,
    /// A lyrics fetch for `lyrics_key` is running in the background.
    pub lyrics_loading: bool,
    pub lyrics_rx: Option<std::sync::mpsc::Receiver<(TrackKey, Result<LyricsResult, String>)>>,
    pub lyrics_offset: f64,
    pub lyrics_status: String,
    // Autoplay (v0.1.6): YT Music radio picks appended to the queue.
    /// Queue index where the autoplay-generated block starts;
    /// tracks at/after it are machine picks. None = no picks.
    pub autoplay_from: Option<usize>,
    /// Identity of every track played this session (dedupe source
    /// for radio picks, together with the queue itself).
    pub played_history: Vec<TrackKey>,
    /// The track the in-flight / last radio fetch was keyed to.
    pub radio_for: Option<TrackKey>,
    pub radio_rx: Option<std::sync::mpsc::Receiver<(TrackKey, Result<Vec<Track>, String>)>>,
    /// Track for which the "autoplay unavailable" note was already
    /// shown (the note appears once per track, never in a loop).
    pub autoplay_note_for: Option<TrackKey>,
    // Sidebar visualizer (v0.1.6, external `cava`).
    pub viz: crate::visualizer::Viz,
    /// Sidebar inner width from the last render (the visualizer
    /// adapts its bar count to it; 0 until the first frame).
    pub last_sidebar_width: u16,
    // Equalizer
    pub eq_selected: usize, // 0..=9 bands, 10 = preamp
    pub eq_preset_selected: usize,
    /// Sidebar EQ section selection: 0 = preset row, 1..=10 = bands,
    /// 11 = preamp, 12 = VOL. Edits the SAME state as the EQ screen.
    pub eq_sidebar_selected: usize,
    // The paste-link form (modal)
    pub input_form: Option<InputForm>,
    // Navigation focus (one system: sidebar marker == active screen)
    pub focus: PanelFocus,
    pub nav_selected: usize,
    // Playback UX: deferred play (so "Mencari audio..." renders
    // first), last playback error (persistent until the next
    // successful play), runtime dependency state, install flag.
    pub pending_play: Option<usize>,
    pub play_error: Option<String>,
    pub deps: diag::DepStatus,
    pub request_install: bool,
    // Settings
    pub settings_selected: usize,
    pub status_message: String,
}

impl App {
    pub fn new() -> App {
        let settings = Settings::load();
        let demo = demo_tracks();
        let mut engine = PlayerEngine::new();
        for t in demo.iter() {
            engine.enqueue(t.clone());
        }
        engine.current_index = Some(0);
        if engine.queue.is_empty() {
            engine.current_index = None;
        }
        let deps = diag::check_deps();
        let initial_status = match deps.warning() {
            Some(w) => format!("{}.", w),
            None => "Guest Mode: search & play freely, no login. Example tracks play right away (Enter) - audio is matched via YT Music.".to_string(),
        };
        // Home (Feature B): show the cached trending list instantly
        // when one exists, then refresh it in the background.
        let (home_tracks, home_kind, home_note) = match config::load_trending_cache() {
            Some(cache) => (
                cache.tracks,
                HomeKind::Trending,
                Some(format!("updated {} (cache)", config::hhmm_of(cache.fetched_at))),
            ),
            None => (Vec::new(), HomeKind::Loading, None),
        };
        let mut app = App {
            screen: Screen::Splash,
            should_quit: false,
            engine,
            settings,
            spotify: SpotifyBackend::new(),
            home_tracks,
            home_kind,
            home_note,
            trending_rx: None,
            home_selected: 0,
            search_selected: 0,
            queue_selected: 0,
            search_input_mode: false,
            search_query: String::new(),
            // Seeded with the design dataset so the Search screen is
            // navigable before the first real query; a real search
            // replaces them.
            search_tracks: demo_tracks(),
            search_albums: Vec::new(),
            search_playlists: Vec::new(),
            search_kind: 0,
            search_platform: SearchPlatform::All,
            search_notes: Vec::new(),
            search_cache: std::collections::HashMap::new(),
            browse: None,
            pending_browse: None,
            pending_link: None,
            click_map: Vec::new(),
            last_click: None,
            show_lyrics: false,
            lyrics: None,
            lyrics_key: None,
            lyrics_loading: false,
            lyrics_rx: None,
            lyrics_offset: 0.0,
            lyrics_status: String::new(),
            autoplay_from: None,
            played_history: Vec::new(),
            radio_for: None,
            radio_rx: None,
            autoplay_note_for: None,
            viz: crate::visualizer::Viz::new(),
            last_sidebar_width: 0,
            eq_selected: 0,
            eq_preset_selected: 0,
            eq_sidebar_selected: 0,
            input_form: None,
            focus: PanelFocus::Main,
            nav_selected: 0,
            pending_play: None,
            play_error: None,
            deps,
            request_install: false,
            settings_selected: 0,
            status_message: initial_status,
        };
        app.start_trending_fetch();
        app
    }

    /// Switch screens AND keep the one navigation system in sync:
    /// sidebar marker and screen always agree. Switching screens
    /// also closes any open browse view and (v0.1.5, Defect 2) ends
    /// search typing mode - a text-capture state must never follow
    /// the user invisibly onto another screen.
    pub fn set_screen(&mut self, s: Screen) {
        self.screen = s;
        self.nav_selected = nav_index(s);
        self.browse = None;
        if s != Screen::Search {
            self.search_input_mode = false;
        }
    }

    pub fn current_track(&self) -> Option<Track> {
        self.engine.current_track().cloned()
    }

    pub fn outcome_message(&mut self, outcome: PlayOutcome) {
        match outcome {
            PlayOutcome::Playing(t) => {
                self.play_error = None;
                if t.source == Source::Spotify {
                    self.status_message = format!("Playing: {} - {} (Spotify - audio via YT Music)", t.title, t.artist);
                } else {
                    self.status_message = format!("Playing: {} - {}", t.title, t.artist);
                }
                diag::append_log(&format!("playing {} - {} [{}]", t.title, t.artist, t.source.name()));
                // State bound to the track's identity follows it:
                // lyrics re-bind (old lyrics invalidated at once)
                // and autoplay records the play + seeds radio picks.
                self.sync_lyrics_track();
                self.note_played(&t);
                self.maybe_seed_autoplay();
                // A new track start grants the visualizer one
                // respawn if its cava died earlier.
                self.viz.allow_one_respawn();
            }
            PlayOutcome::SkippedAd(t) => {
                self.status_message = format!("Clean Playback: skipped an ad ({})", t.title);
            }
            PlayOutcome::Unresolvable(t, e) => {
                // Persistent, clearly visible error + log file entry.
                self.play_error = Some(format!("Could not play '{}': {}", t.title, e));
                self.status_message = self.play_error.clone().unwrap_or_default();
                diag::append_log(&format!("failed to play {} - {}: {}", t.title, t.artist, e));
            }
            PlayOutcome::EndOfQueue => {
                self.status_message = "End of queue.".to_string();
                // Release the (now idle) mpv so it does not hold the
                // audio device after the queue ran out.
                self.engine.mpv.stop();
            }
        }
    }

    /// Ask for playback of a queue index. Deferred by one frame so
    /// the UI first renders "Searching for audio via yt-dlp..."
    /// (resolution is blocking) and only then the result - see
    /// process_pending_play, called by the main loop after each draw.
    pub fn request_play(&mut self, idx: usize) {
        if let Some(t) = self.engine.queue.get(idx) {
            self.status_message = format!("Searching for audio via yt-dlp... ({} - {})", t.title, t.artist);
        } else {
            self.status_message = "Searching for audio via yt-dlp...".to_string();
        }
        self.pending_play = Some(idx);
    }

    pub fn process_pending_play(&mut self) {
        if let Some(idx) = self.pending_play.take() {
            self.play_now(idx);
        }
    }

    /// Play a queue index NOW (a manual start: Home/Search/browse/
    /// Queue Enter, prev). Autoplay rule: a manual start drops the
    /// pending autoplay picks queued after the new current track -
    /// the radio re-seeds from the new song instead (see
    /// maybe_seed_autoplay, fired by the Playing outcome).
    pub fn play_now(&mut self, idx: usize) {
        let idx = self.drop_autoplay_picks_for_manual_play(idx);
        let yt = YtMusicBackend;
        let spot = self.spotify.clone();
        let outcome = self.engine.play_index(idx, &self.settings, &yt, &spot);
        self.outcome_message(outcome);
    }

    /// Remove pending (not yet played) autoplay picks around a
    /// manual jump to queue index `idx`; returns the adjusted index
    /// of the target track after the removals. The target itself is
    /// kept even when it sits inside the autoplay block (the user
    /// chose it - it stops being a machine pick).
    fn drop_autoplay_picks_for_manual_play(&mut self, idx: usize) -> usize {
        let boundary = match self.autoplay_from {
            Some(b) => b,
            None => return idx,
        };
        if idx >= self.engine.queue.len() {
            return idx;
        }
        let mut removed_before = 0usize;
        let mut j = self.engine.queue.len();
        while j > boundary {
            j -= 1;
            if j == idx {
                continue;
            }
            self.engine.queue.remove(j);
            if j < idx {
                removed_before += 1;
            }
        }
        self.autoplay_from = None;
        self.radio_for = None;
        idx - removed_before
    }

    /// Kept name for existing call sites: plays immediately.
    pub fn play_queue_index(&mut self, idx: usize) {
        self.play_now(idx);
    }

    pub fn next_track(&mut self) {
        let yt = YtMusicBackend;
        let spot = self.spotify.clone();
        let outcome = self.engine.next(&self.settings, &yt, &spot);
        self.outcome_message(outcome);
    }

    pub fn prev_track(&mut self) {
        if let Some(idx) = self.engine.prev_index() {
            self.engine.position_secs = 0.0;
            self.play_queue_index(idx);
        }
    }

    pub fn toggle_play_pause(&mut self) {
        if self.engine.is_playing {
            // Pause THROUGH mpv (IPC): the audio position lives in
            // mpv and survives the pause - nothing restarts, nothing
            // re-resolves. Fallback for a player without IPC: stop
            // the child, as before.
            if self.engine.set_paused(true) {
                self.status_message = "Paused.".to_string();
                return;
            }
            self.engine.mpv.stop();
            self.engine.mpv_active = false;
            self.engine.is_playing = false;
            self.status_message = "Paused.".to_string();
            return;
        }
        // Resume through mpv when a session is alive (position kept).
        if self.engine.mpv_active && self.engine.set_paused(false) {
            self.status_message = "Resumed.".to_string();
            return;
        }
        // Start: re-run the real play path for the current track.
        if let Some(idx) = self.engine.current_index {
            if self.engine.queue.get(idx).is_some() {
                self.request_play(idx);
            }
        }
    }

    /// Per-tick playback housekeeping (main loop, ~200ms): push live
    /// EQ/volume changes to mpv, mirror mpv's real
    /// position/duration/pause state into the engine, and advance
    /// the queue when the track ended. There is NO local position
    /// timer - position comes from mpv or stays put.
    ///
    /// v0.1.6: the end-of-track decision lives in the engine
    /// (check_track_ended - IPC EOF report first, dead-child
    /// fallback second); before the fix this poll discarded the EOF
    /// flag and the queue only moved on a manual `n`.
    pub fn poll_playback(&mut self) {
        self.engine.sync_live_settings(&self.settings);
        if self.engine.check_track_ended() {
            diag::append_log("playback: track ended - auto-advancing");
            self.next_track();
        }
    }

    /// Toggle the Lyrics view inside the Home middle box (the box
    /// is replaced in place, per the approved design). Fetching is
    /// asynchronous since v0.1.6 (it used to block the UI thread);
    /// the fetch itself is (re)started by sync_lyrics_track.
    pub fn toggle_lyrics(&mut self) {
        self.show_lyrics = !self.show_lyrics;
        if self.show_lyrics {
            self.set_screen(Screen::NowPlaying);
            if self.current_track().is_none() {
                self.lyrics_status = "No track selected.".to_string();
            }
            self.sync_lyrics_track();
        }
    }

    /// The identity of the track lyrics should describe right now.
    pub fn current_lyrics_key(&self) -> Option<TrackKey> {
        self.engine.current_track().map(TrackKey::of)
    }

    /// Bind the lyrics state to the current track. Called on every
    /// background tick AND on every play outcome, so EVERY track
    /// change path (auto-advance, n/p, Home/Search/browse/Queue
    /// plays) invalidates the previous track's lyrics immediately:
    /// the old lines are dropped, the offset resets to 0, and - if
    /// the lyrics box is visible or lyrics were fetched before - a
    /// background fetch for the NEW track starts.
    pub fn sync_lyrics_track(&mut self) {
        let cur = self.current_lyrics_key();
        if cur == self.lyrics_key {
            return;
        }
        let had_fetch = self.lyrics_key.is_some();
        self.lyrics_key = cur.clone();
        self.lyrics = None;
        self.lyrics_offset = 0.0;
        self.lyrics_loading = false;
        self.lyrics_rx = None;
        match cur {
            Some(key) if self.show_lyrics || had_fetch => {
                self.start_lyrics_fetch(key);
            }
            Some(_) => {}
            None => {
                self.lyrics_status = "No track selected.".to_string();
            }
        }
    }

    fn start_lyrics_fetch(&mut self, key: TrackKey) {
        self.lyrics_loading = true;
        self.lyrics_status = format!("Loading lyrics for {} - {}...", key.title, key.artist);
        let (tx, rx) = std::sync::mpsc::channel::<(TrackKey, Result<LyricsResult, String>)>();
        self.lyrics_rx = Some(rx);
        let (artist, title) = (key.artist.clone(), key.title.clone());
        std::thread::spawn(move || {
            let result = fetch_lyrics(&artist, &title);
            let _ = tx.send((key, result));
        });
    }

    /// Apply a finished lyrics fetch - ONLY when its track key is
    /// still the current track's key. A result that arrives after
    /// the user moved on is stale and is dropped (logged), never
    /// rendered under the new track.
    pub fn poll_lyrics(&mut self) {
        let (key, result) = match &self.lyrics_rx {
            Some(rx) => match rx.try_recv() {
                Ok(v) => v,
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.lyrics_rx = None;
                    if self.lyrics_loading {
                        self.lyrics_loading = false;
                        self.lyrics = Some(LyricsResult::NotFound);
                        self.lyrics_status = "Lyrics fetch failed.".to_string();
                    }
                    return;
                }
            },
            None => return,
        };
        self.lyrics_rx = None;
        if self.current_lyrics_key().as_ref() != Some(&key) || self.lyrics_key.as_ref() != Some(&key) {
            diag::append_log(&format!(
                "lyrics: dropped a stale result for {} - {} (track changed)",
                key.title, key.artist
            ));
            return;
        }
        self.lyrics_loading = false;
        match result {
            Ok(result) => {
                self.lyrics_status = match &result {
                    LyricsResult::Synced(_) => "Synced lyrics - LRCLib".to_string(),
                    LyricsResult::Plain(_) => "Plain lyrics (no timing) - LRCLib".to_string(),
                    LyricsResult::NotFound => {
                        format!("No lyrics found on LRCLib for {} - {}.", key.title, key.artist)
                    }
                };
                self.lyrics = Some(result);
            }
            Err(e) => {
                self.lyrics = Some(LyricsResult::NotFound);
                self.lyrics_status = e;
            }
        }
    }

    // -------------------------------------------------------------------
    // Autoplay (v0.1.6): YT Music radio picks for the playing song
    // -------------------------------------------------------------------

    /// Record a started track in the session's played history
    /// (autoplay dedupe source).
    fn note_played(&mut self, t: &Track) {
        let key = TrackKey::of(t);
        if !self.played_history.iter().any(|k| *k == key) {
            self.played_history.push(key);
        }
    }

    /// Whether queue index `idx` is an autoplay-generated pick.
    pub fn is_autoplay_pick(&self, idx: usize) -> bool {
        self.autoplay_from.map(|b| idx >= b).unwrap_or(false)
    }

    /// Enqueue a track the USER picked (key `a`, browse queue):
    /// inserted BEFORE the autoplay block so user picks always play
    /// before machine picks.
    pub fn enqueue_user(&mut self, track: Track) {
        match self.autoplay_from {
            Some(b) if b <= self.engine.queue.len() => {
                self.engine.queue.insert(b, track);
                self.autoplay_from = Some(b + 1);
            }
            _ => self.engine.enqueue(track),
        }
    }

    /// Keep the autoplay boundary consistent after the user
    /// removes a queue item (key `d`): items before the boundary
    /// shift it down; losing the last pick clears it.
    pub fn note_queue_removed(&mut self, idx: usize) {
        if let Some(b) = self.autoplay_from {
            let nb = if idx < b { b.saturating_sub(1) } else { b };
            self.autoplay_from = if nb >= self.engine.queue.len() { None } else { Some(nb) };
        }
    }

    /// Queue cleared (key `c`): no picks, no pending seed key.
    pub fn note_queue_cleared(&mut self) {
        self.autoplay_from = None;
        self.radio_for = None;
    }

    /// Toggle autoplay (key `A`). OFF drops the pending machine
    /// picks (the queue then behaves exactly as it did before the
    /// feature) and stops future fetches; ON re-checks seeding for
    /// the playing track.
    pub fn toggle_autoplay(&mut self) {
        self.settings.autoplay = !self.settings.autoplay;
        self.persist_settings();
        if self.settings.autoplay {
            self.status_message = "Autoplay: ON".to_string();
            self.maybe_seed_autoplay();
        } else {
            self.status_message = "Autoplay: OFF".to_string();
            self.radio_rx = None;
            self.radio_for = None;
            if let Some(b) = self.autoplay_from.take() {
                let cur = self.engine.current_index.unwrap_or(usize::MAX);
                let mut j = self.engine.queue.len();
                while j > b {
                    j -= 1;
                    if j > cur {
                        self.engine.queue.remove(j);
                    }
                }
            }
        }
    }

    /// Start a radio fetch for the CURRENT track when fewer than 3
    /// tracks remain after it. Fetches are keyed by track identity:
    /// at most one fetch per track, and results for a track that is
    /// no longer current are dropped by poll_radio.
    pub fn maybe_seed_autoplay(&mut self) {
        if !self.settings.autoplay {
            return;
        }
        let (idx, track) = match (self.engine.current_index, self.engine.current_track()) {
            (Some(i), Some(t)) => (i, t.clone()),
            _ => return,
        };
        let remaining = self.engine.queue.len().saturating_sub(idx + 1);
        if remaining >= 3 {
            return;
        }
        let key = TrackKey::of(&track);
        if self.radio_for.as_ref() == Some(&key) {
            return; // already fetched / fetching for this track
        }
        self.radio_for = Some(key.clone());
        let (tx, rx) = std::sync::mpsc::channel::<(TrackKey, Result<Vec<Track>, String>)>();
        self.radio_rx = Some(rx);
        std::thread::spawn(move || {
            let result = innertube::fetch_radio_for_track(&track);
            let _ = tx.send((key, result));
        });
    }

    /// Poll the radio fetch; apply picks only for the still-current
    /// track (rapid skipping must not pile up stale appends).
    pub fn poll_radio(&mut self) {
        let (key, result) = match &self.radio_rx {
            Some(rx) => match rx.try_recv() {
                Ok(v) => v,
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.radio_rx = None;
                    return;
                }
            },
            None => return,
        };
        self.radio_rx = None;
        self.apply_radio_result(key, result);
    }

    /// Append radio picks for `key` to the queue: dedupe against
    /// the whole queue AND the session history AND the current
    /// track itself, cap at 10. The autoplay boundary is set when
    /// the first block is appended.
    pub fn apply_radio_result(&mut self, key: TrackKey, result: Result<Vec<Track>, String>) {
        if self.current_lyrics_key().as_ref() != Some(&key) {
            diag::append_log(&format!(
                "autoplay: dropped stale radio picks for {} - {} (track changed)",
                key.title, key.artist
            ));
            return;
        }
        if !self.settings.autoplay {
            return;
        }
        let cur_idx = match self.engine.current_index {
            Some(i) => i,
            None => return,
        };
        let remaining = self.engine.queue.len().saturating_sub(cur_idx + 1);
        match result {
            Ok(picks) => {
                let mut seen: Vec<TrackKey> =
                    self.engine.queue.iter().map(TrackKey::of).collect();
                seen.extend(self.played_history.iter().cloned());
                let mut fresh: Vec<Track> = Vec::new();
                for p in picks {
                    let pk = TrackKey::of(&p);
                    if seen.iter().any(|s| s.same_song(&pk)) {
                        continue;
                    }
                    seen.push(pk);
                    fresh.push(p);
                    if fresh.len() >= 10 {
                        break;
                    }
                }
                if fresh.is_empty() {
                    if remaining == 0 {
                        self.note_autoplay_unavailable(
                            &key,
                            "Autoplay: no radio picks found for this song.".to_string(),
                        );
                    }
                    return;
                }
                if self.autoplay_from.is_none() {
                    self.autoplay_from = Some(self.engine.queue.len());
                }
                let n = fresh.len();
                self.engine.queue.extend(fresh);
                diag::append_log(&format!(
                    "autoplay: appended {} radio picks for {} - {}",
                    n, key.title, key.artist
                ));
            }
            Err(e) => {
                diag::append_log(&format!(
                    "autoplay: radio fetch failed for {} - {}: {}",
                    key.title, key.artist, e
                ));
                if remaining == 0 {
                    self.note_autoplay_unavailable(
                        &key,
                        format!("Autoplay unavailable ({}). Queue ends here.", e),
                    );
                }
            }
        }
    }

    /// The honest "autoplay could not extend the queue" status note:
    /// once per track, never in a retry loop.
    fn note_autoplay_unavailable(&mut self, key: &TrackKey, msg: String) {
        if self.autoplay_note_for.as_ref() != Some(key) {
            self.autoplay_note_for = Some(key.clone());
            self.status_message = msg;
        }
    }

    // -------------------------------------------------------------------
    // Trending (Feature B)
    // -------------------------------------------------------------------

    /// Fetch trending from both guest sources in the background;
    /// the UI never blocks on it. Called once at startup.
    pub fn start_trending_fetch(&mut self) {
        if self.trending_rx.is_some() {
            return;
        }
        let spotify = self.spotify.clone();
        let (tx, rx) = std::sync::mpsc::channel::<TrendingOutcome>();
        std::thread::spawn(move || {
            let ytm = innertube::fetch_trending_tracks();
            // v0.1.7: the Spotify half goes through its 6-hour disk
            // cache + cooldown gate - within the TTL this makes no
            // API call at all.
            let (spot, spot_note) = match spotify.trending_tracks_cached() {
                Ok((tracks, note)) => (Ok(tracks), note),
                Err(e) => (Err(e), None),
            };
            let _ = tx.send(TrendingOutcome {
                spotify: spot,
                ytm,
                spotify_note: spot_note,
            });
        });
        self.trending_rx = Some(rx);
    }

    fn poll_trending(&mut self) {
        let outcome = match &self.trending_rx {
            Some(rx) => match rx.try_recv() {
                Ok(o) => o,
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.trending_rx = None;
                    return;
                }
            },
            None => return,
        };
        self.trending_rx = None;
        let (spot_tracks, spot_err) = match outcome.spotify {
            Ok(t) => (t, None),
            Err(e) => (Vec::new(), Some(e)),
        };
        let (ytm_tracks, ytm_err) = match outcome.ytm {
            Ok(t) => (t, None),
            Err(e) => (Vec::new(), Some(e)),
        };
        let merged = merge_trending(spot_tracks, ytm_tracks);
        if !merged.is_empty() {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            config::save_trending_cache(&merged, now);
            self.home_tracks = merged;
            self.home_kind = HomeKind::Trending;
            if self.home_selected >= self.home_tracks.len() {
                self.home_selected = 0;
            }
            // One source failing while the other delivers: say so,
            // briefly, under the box. A Spotify cache/cooldown note
            // (v0.1.7) is more specific than "unreachable" and wins.
            self.home_note = match (outcome.spotify_note, spot_err, ytm_err) {
                (Some(note), _, _) => Some(note),
                (None, Some(_), None) => Some("Spotify unreachable - trending from YT Music only".to_string()),
                (None, None, Some(_)) => Some("YT Music unreachable - trending from Spotify only".to_string()),
                _ => None,
            };
        } else {
            // Both sources failed: never relabel anything as trending.
            diag::append_log(&format!(
                "trending failed to load: spotify={:?} ytm={:?}",
                spot_err, ytm_err
            ));
            if self.home_kind == HomeKind::Trending {
                // A cache is on screen; keep it, note the failure.
                self.home_note = Some("Could not refresh trending - showing the last data".to_string());
            } else {
                self.home_tracks = demo_tracks();
                self.home_kind = HomeKind::Examples;
                self.home_selected = 0;
                self.home_note = Some("Trending unreachable - showing example tracks".to_string());
            }
        }
    }

    /// Background results the main loop polls every tick.
    pub fn poll_background(&mut self) {
        self.poll_trending();
        self.sync_lyrics_track();
        self.poll_lyrics();
        self.poll_radio();
        self.poll_viz();
    }

    /// Visualizer housekeeping: start/stop the cava child per the
    /// persisted setting (spawn only once a real sidebar width is
    /// known, i.e. after the first frame rendered).
    pub fn poll_viz(&mut self) {
        if self.last_sidebar_width >= 8 {
            let w = self.last_sidebar_width;
            self.viz.maintain(self.settings.visualizer, w);
        }
    }

    /// Cycle the visualizer template (key `V` or a click on the
    /// visualizer box, v0.1.7): Bars -> Mirror -> Wave -> Line ->
    /// LED. The choice is persisted as `visualizer_style`.
    pub fn cycle_visualizer_style(&mut self) {
        let next = crate::visualizer::VizStyle::from_name(&self.settings.visualizer_style).next();
        self.settings.visualizer_style = next.name().to_string();
        self.persist_settings();
        self.status_message = format!("Visualizer style: {}", next.name());
    }

    /// Advance the visualizer's smoothed display buffer by one
    /// step (called at the tightened frame cadence while the
    /// visualizer is on - see the main loop). No-op otherwise.
    pub fn tick_visualizer(&mut self) {
        if self.settings.visualizer {
            self.viz.tick_display();
        }
    }

    /// Toggle the sidebar visualizer (key `v`, persisted). ON
    /// grants a spawn; OFF kills the cava process and the box
    /// disappears (handled by the sidebar layout).
    pub fn toggle_visualizer(&mut self) {
        self.settings.visualizer = !self.settings.visualizer;
        self.persist_settings();
        if self.settings.visualizer {
            self.viz.allow_one_respawn();
            self.status_message = "Visualizer: ON".to_string();
        } else {
            self.viz.stop();
            self.status_message = "Visualizer: OFF".to_string();
        }
        self.poll_viz();
    }

    /// Play a Home list entry (trending / cache / examples alike):
    /// enqueue + play through the normal paths (Spotify = YT match,
    /// YTM = direct).
    pub fn home_play(&mut self, index: usize) {
        if let Some(t) = self.home_tracks.get(index).cloned() {
            self.home_selected = index;
            self.engine.enqueue(t);
            let idx = self.engine.queue.len() - 1;
            self.request_play(idx);
        }
    }

    // -------------------------------------------------------------------
    // Search (unified: songs + albums + playlists, guest both sides)
    // -------------------------------------------------------------------

    /// The flattened, grouped display rows for the current search
    /// state. In "Semua" the sections appear in LAGU / ALBUM /
    /// PLAYLIST order (empty sections omitted); a kind filter shows
    /// only its own section. Rendering AND keyboard/mouse selection
    /// both derive from this one function, so they can never drift.
    pub fn search_rows(&self) -> Vec<SearchRow> {
        let mut rows: Vec<SearchRow> = Vec::new();
        let show_tracks = self.search_kind == 0 || self.search_kind == 1;
        let show_albums = self.search_kind == 0 || self.search_kind == 2;
        let show_playlists = self.search_kind == 0 || self.search_kind == 3;
        if show_tracks && !self.search_tracks.is_empty() {
            rows.push(SearchRow::Section("SONGS"));
            for i in 0..self.search_tracks.len() {
                rows.push(SearchRow::Track(i));
            }
        }
        if show_albums && !self.search_albums.is_empty() {
            rows.push(SearchRow::Section("ALBUMS"));
            for i in 0..self.search_albums.len() {
                rows.push(SearchRow::Album(i));
            }
        }
        if show_playlists && !self.search_playlists.is_empty() {
            rows.push(SearchRow::Section("PLAYLISTS"));
            for i in 0..self.search_playlists.len() {
                rows.push(SearchRow::Playlist(i));
            }
        }
        rows
    }

    /// The selectable rows only (selection indices count these).
    pub fn search_selectable(&self) -> Vec<SearchRow> {
        self.search_rows().into_iter().filter(|r| r.is_selectable()).collect()
    }

    /// Cycle the Search platform filter (key `f` / chips) and say
    /// so. The filter takes effect on the next search run.
    pub fn cycle_search_platform(&mut self) {
        self.search_platform = self.search_platform.next();
        self.apply_search_platform();
    }

    /// After the platform filter changes (key `f` or a chip
    /// click): re-run the current query so the results, counts and
    /// notes actually reflect the newly active sources.
    fn apply_search_platform(&mut self) {
        if self.search_query.trim().is_empty() {
            self.status_message = format!("Search platform: {}", self.search_platform.label());
        } else {
            self.run_search();
        }
    }

    /// Set the Search kind filter (`[`, `]` keys or a chip click)
    /// and re-run the current query: since v0.1.7 the kind routes
    /// the Spotify request's type parameter, so results must be
    /// re-fetched (the session cache makes toggling back free).
    pub fn apply_search_kind(&mut self, kind: u8) {
        self.search_kind = kind.min(3);
        if self.search_query.trim().is_empty() {
            let names = ["All", "Songs", "Albums", "Playlists"];
            self.status_message = format!("Search kind: {}", names[self.search_kind as usize]);
        } else {
            self.run_search();
        }
    }

    /// Unified guest search: songs from yt-dlp + Innertube + Spotify
    /// guest, albums + playlists from Innertube (YTM) and the Spotify
    /// Web API (anonymous token). Any single source failing becomes
    /// an inline note - the rest still shows. Notes are deduplicated:
    /// the same source reporting the same failure twice (the
    /// v0.1.4 double "Spotify login needed" lines) shows once.
    ///
    /// The platform filter ROUTES the fetching: with "Spotify" the
    /// Innertube/yt-dlp side is never called, with "YT Music" the
    /// Spotify anonymous-token API is never called, and the result
    /// counts/notes only ever mention the active sources.
    pub fn run_search(&mut self) {
        let q = self.search_query.trim().to_string();
        if q.is_empty() {
            self.status_message = "Type a search term first (press / then Enter).".to_string();
            return;
        }
        let platform = self.search_platform;
        let scope = match platform {
            SearchPlatform::All => "all sources".to_string(),
            p => format!("{} only", p.label()),
        };
        // v0.1.7 session result cache: the same (query, kind,
        // platform) search runs at most once per session - toggling
        // the filters back and forth restores the stored result
        // with NO new API call on either source.
        let cache_key = (q.clone(), self.search_kind, platform);
        if let Some((tracks, albums, playlists)) = self.search_cache.get(&cache_key).cloned() {
            self.search_selected = 0;
            let total = tracks.len() + albums.len() + playlists.len();
            self.status_message = format!(
                "{} results for '{}' [{}] ({} songs, {} albums, {} playlists) (cached)",
                total,
                q,
                scope,
                tracks.len(),
                albums.len(),
                playlists.len()
            );
            // A cooldown that started after this result was cached
            // is still reported, with its CURRENT retry time.
            let mut notes: Vec<String> = Vec::new();
            if platform.spotify_active() {
                if let Some(until) = self.spotify.guest.cooldown_until() {
                    notes.push(format!("Spotify: {}", crate::backend::rate_limit_note(until)));
                }
            }
            self.search_tracks = tracks;
            self.search_albums = albums;
            self.search_playlists = playlists;
            self.search_notes = notes;
            return;
        }
        let mut tracks: Vec<Track> = Vec::new();
        let mut albums: Vec<AlbumRef> = Vec::new();
        let mut playlists: Vec<PlaylistRef> = Vec::new();
        let mut notes: Vec<String> = Vec::new();
        let mut any_failure = false;
        let push_note = |notes: &mut Vec<String>, n: String| {
            if !notes.contains(&n) {
                notes.push(n);
            }
        };

        if platform.ytmusic_active() {
            // Songs: yt-dlp search (guest, always available).
            let yt = YtMusicBackend;
            match yt.search(&q) {
                Ok(v) => tracks.extend(v),
                Err(e) => {
                    any_failure = true;
                    push_note(&mut notes, format!("YT Music (yt-dlp): {}", e))
                }
            }
            // YTM unified: guest Innertube songs/albums/playlists.
            match innertube::search_all(&q) {
                Ok(res) => {
                    for t in res.tracks {
                        if !tracks.iter().any(|x| x.id == t.id) {
                            tracks.push(t);
                        }
                    }
                    albums.extend(res.albums);
                    playlists.extend(res.playlists);
                    if !res.notes.is_empty() {
                        any_failure = true;
                    }
                    for n in res.notes {
                        push_note(&mut notes, n);
                    }
                }
                Err(e) => {
                    any_failure = true;
                    push_note(&mut notes, format!("YT Music: {}", e))
                }
            }
        }
        if platform.spotify_active() {
            // Spotify guest catalog: ONE combined search request
            // (v0.1.7 - v0.1.6 made two: tracks, then
            // albums+playlists). The kind filter picks the type
            // parameter; the single response fills all sections.
            // During a rate-limit cooldown this makes no call at
            // all and the note names the retry time.
            let spot = self.spotify.clone();
            match spot.search_catalog(&q, self.search_kind) {
                Ok((st, sa, sp)) => {
                    for t in st {
                        if !tracks.iter().any(|x| x.id == t.id && x.source == t.source) {
                            tracks.push(t);
                        }
                    }
                    albums.extend(sa);
                    playlists.extend(sp);
                }
                Err(e) => {
                    any_failure = true;
                    push_note(&mut notes, format!("Spotify: {}", e))
                }
            }
        }
        self.search_selected = 0;
        let total = tracks.len() + albums.len() + playlists.len();
        if notes.is_empty() {
            self.status_message = format!("{} results for '{}' [{}] ({} songs, {} albums, {} playlists)", total, q, scope, tracks.len(), albums.len(), playlists.len());
        } else {
            self.status_message = format!("{} results for '{}' [{}]. Some sources failed: {}", total, q, scope, notes.join(" | "));
        }
        // Only fully successful searches are cached: a partial
        // failure re-runs next time (the Spotify cooldown still
        // gates any API call in that case).
        if !any_failure {
            self.search_cache.insert(
                cache_key,
                (tracks.clone(), albums.clone(), playlists.clone()),
            );
        }
        self.search_tracks = tracks;
        self.search_albums = albums;
        self.search_playlists = playlists;
        self.search_notes = notes;
    }

    pub fn persist_settings(&mut self) {
        let _ = self.settings.save();
    }

    // -------------------------------------------------------------------
    // Equalizer (sidebar section + full screen share this state)
    // -------------------------------------------------------------------

    /// Cycle the EQ preset (12 builtins + Custom slots) and apply it
    /// IMMEDIATELY - the same state the Equalizer screen and the
    /// sidebar EQ section edit; the running mpv gets the new filter
    /// on the next tick via sync_live_settings.
    pub fn cycle_eq_preset(&mut self, delta: i32) {
        let names = self.settings.eq.preset_names();
        if names.is_empty() {
            return;
        }
        let cur = names.iter().position(|n| n == &self.settings.eq.preset_name).unwrap_or(0);
        let len = names.len() as i32;
        let next = ((cur as i32 + delta) % len + len) as usize % names.len();
        self.eq_preset_selected = next;
        let name = names[next].clone();
        self.apply_eq_preset(&name);
    }

    /// Apply a named EQ preset immediately (sidebar preset-row Enter
    /// / mouse click on the preset name).
    pub fn apply_eq_preset(&mut self, name: &str) {
        if self.settings.eq.apply_preset_by_name(name) {
            self.persist_settings();
            self.status_message = format!("Preset EQ: {}", name);
        }
    }

    /// Left/Right while the SIDEBAR EQ panel is focused: the preset
    /// row (0) cycles presets (applied immediately), band rows
    /// (1..=10) and the preamp row (11) adjust gain by 1 dB, and the
    /// VOL row (12) adjusts the shared volume by 5 (Defect 3) - the
    /// same settings.volume the bottom bar shows and mpv receives.
    pub fn eq_sidebar_adjust(&mut self, delta: i32) {
        match self.eq_sidebar_selected {
            0 => self.cycle_eq_preset(delta),
            12 => {
                let nv = self.settings.volume as i32 + delta * 5;
                self.settings.volume = nv.clamp(0, 100) as u8;
                self.persist_settings();
            }
            row => {
                // clamp_band: 0..=9 bands, 10 = preamp.
                self.settings.eq.clamp_band(row - 1, delta as f64);
                self.persist_settings();
            }
        }
    }

    /// Set one sidebar-EQ row's value from a mouse x-ratio (0.0..=1.0
    /// across the row's bar area): band/preamp rows map to
    /// -12..=+12 dB; the VOL row (12) maps to 0..=100. Row 0 is the
    /// preset row and ignores drags.
    pub fn eq_sidebar_set_from_ratio(&mut self, row: usize, ratio: f64) {
        let ratio = ratio.clamp(0.0, 1.0);
        match row {
            0 => {}
            12 => {
                self.settings.volume = (ratio * 100.0).round().clamp(0.0, 100.0) as u8;
                self.persist_settings();
            }
            1..=11 => {
                let target = -12.0 + ratio * 24.0;
                let idx = row - 1;
                if idx < 10 {
                    self.settings.eq.bands[idx] = target;
                } else {
                    self.settings.eq.preamp = target;
                }
                self.settings.eq.preset_name = "Custom (unsaved)".to_string();
                self.persist_settings();
            }
            _ => {}
        }
    }

    // -------------------------------------------------------------------
    // Sidebar playlists + browse view + pasted links
    // -------------------------------------------------------------------

    /// The sidebar's playlist entries: saved link playlists only
    /// (v0.1.6 removed the seeded examples), grouped per source -
    /// Spotify entries first, then YT Music. Discovery of new
    /// playlists happens in Search; this list is for reopening.
    pub fn sidebar_playlists(&self) -> Vec<SidebarPlaylist> {
        let mut out: Vec<SidebarPlaylist> = Vec::new();
        for source in [Source::Spotify, Source::YtMusic] {
            for (saved_index, p) in self.settings.saved_playlists.iter().enumerate() {
                if p.source == source {
                    out.push(SidebarPlaylist {
                        source,
                        title: p.name.clone(),
                        id: Some(p.id.clone()),
                        album: p.kind == SavedKind::Album,
                        saved_index: Some(saved_index),
                    });
                }
            }
        }
        out
    }

    /// Total rows the Nav marker can rest on: the screens, the
    /// playlist entries, and the "+ Paste Playlist Link" entry.
    pub fn nav_total(&self) -> usize {
        NAV_SCREENS.len() + self.sidebar_playlists().len() + 1
    }

    /// Whether the Nav marker currently rests on the paste entry.
    pub fn nav_on_paste(&self) -> bool {
        self.nav_selected == NAV_SCREENS.len() + self.sidebar_playlists().len()
    }

    /// Ask to open a sidebar playlist (deferred one frame, like
    /// playback, so the "Memuat..." status renders first).
    pub fn open_sidebar_playlist(&mut self, index: usize) {
        let entry = match self.sidebar_playlists().get(index) {
            Some(e) => e.clone(),
            None => return,
        };
        self.status_message = format!("Loading playlist '{}'...", entry.title);
        self.pending_browse = Some(match &entry.id {
            Some(id) if entry.album => BrowseRequest::Album {
                source: entry.source,
                id: id.clone(),
                title: entry.title.clone(),
            },
            Some(id) => BrowseRequest::Playlist {
                source: entry.source,
                id: id.clone(),
                title: entry.title.clone(),
            },
            None => return,
        });
    }

    /// Delete the saved playlist under the Nav marker (key `x`).
    /// Screens are not deletable.
    pub fn delete_nav_saved(&mut self) {
        if self.nav_selected < NAV_SCREENS.len() {
            return;
        }
        let idx = self.nav_selected - NAV_SCREENS.len();
        let entry = self.sidebar_playlists().get(idx).cloned();
        if let Some(entry) = entry {
            if let Some(saved_index) = entry.saved_index {
                if let Some(removed) = self.settings.remove_saved_playlist(saved_index) {
                    self.persist_settings();
                    self.status_message = format!("Saved playlist '{}' deleted.", removed.name);
                    // Keep the marker on a valid row.
                    let total = self.nav_total();
                    if self.nav_selected >= total {
                        self.nav_selected = total.saturating_sub(1);
                    }
                }
            }
        }
    }

    /// Ask to open a search-result album / playlist.
    pub fn open_search_item(&mut self, row: &SearchRow) {
        match row {
            SearchRow::Album(i) => {
                if let Some(a) = self.search_albums.get(*i) {
                    self.status_message = format!("Loading album '{}'...", a.title);
                    self.pending_browse = Some(BrowseRequest::Album {
                        source: a.source,
                        id: a.id.clone(),
                        title: a.title.clone(),
                    });
                }
            }
            SearchRow::Playlist(i) => {
                if let Some(p) = self.search_playlists.get(*i) {
                    self.status_message = format!("Loading playlist '{}'...", p.title);
                    self.pending_browse = Some(BrowseRequest::Playlist {
                        source: p.source,
                        id: p.id.clone(),
                        title: p.title.clone(),
                    });
                }
            }
            _ => {}
        }
    }

    /// Run the deferred browse load (blocking network; called by the
    /// main loop right after a draw, exactly like
    /// process_pending_play).
    pub fn process_pending_browse(&mut self) {
        let req = match self.pending_browse.take() {
            Some(r) => r,
            None => return,
        };
        match req {
            BrowseRequest::Playlist { source, id, title } => {
                let result = match source {
                    Source::Spotify => self.spotify.clone().playlist_tracks(&id),
                    Source::YtMusic => innertube::browse_tracks(&id),
                };
                self.finish_browse(title, None, result);
            }
            BrowseRequest::Album { source, id, title } => {
                let result = match source {
                    Source::Spotify => self.spotify.clone().album_tracks(&id),
                    Source::YtMusic => innertube::browse_tracks(&id),
                };
                self.finish_browse(title, None, result);
            }
        }
    }

    fn finish_browse(&mut self, title: String, note: Option<String>, result: Result<Vec<Track>, String>) {
        match result {
            Ok(tracks) => {
                let n = tracks.len();
                self.status_message = format!("Playlist/album '{}' open: {} tracks. Enter = play, a = queue, Esc = back.", title, n);
                self.browse = Some(BrowseView {
                    title,
                    note,
                    tracks,
                    selected: 0,
                });
                self.focus = PanelFocus::Main;
            }
            Err(e) => {
                // Honest failure surface: an empty view with the
                // reason in the note, never fabricated tracks.
                self.status_message = format!("Could not load '{}': {}", title, e);
                diag::append_log(&format!("failed to load browse '{}': {}", title, e));
                self.browse = Some(BrowseView {
                    title,
                    note: Some(format!("Could not be loaded: {}", e)),
                    tracks: Vec::new(),
                    selected: 0,
                });
                self.focus = PanelFocus::Main;
            }
        }
    }

    pub fn close_browse(&mut self) {
        self.browse = None;
    }

    /// Play the browse view's selected track (enqueue + play, like
    /// Search's Enter).
    pub fn browse_play_selected(&mut self) {
        let track = match &self.browse {
            Some(b) => b.tracks.get(b.selected).cloned(),
            None => None,
        };
        if let Some(t) = track {
            self.engine.enqueue(t);
            let idx = self.engine.queue.len() - 1;
            self.request_play(idx);
        }
    }

    /// Queue the browse view's selected track without playing it.
    pub fn browse_queue_selected(&mut self) {
        let track = match &self.browse {
            Some(b) => b.tracks.get(b.selected).cloned(),
            None => None,
        };
        if let Some(t) = track {
            self.status_message = format!("Added to queue: {}", t.title);
            self.enqueue_user(t);
        }
    }

    // -------------------------------------------------------------------
    // Paste-link form (Feature A)
    // -------------------------------------------------------------------

    /// Open the paste-link modal form.
    pub fn start_link_form(&mut self) {
        self.input_form = Some(InputForm {
            title: "Paste Playlist Link".to_string(),
            label: "Paste a Spotify / YT Music playlist link:".to_string(),
            value: String::new(),
        });
        self.status_message = "Paste the link (Ctrl+V), Enter = open, Esc = cancel.".to_string();
    }

    pub fn form_input(&mut self, c: char) {
        if let Some(f) = &mut self.input_form {
            f.value.push(c);
        }
    }

    pub fn form_backspace(&mut self) {
        if let Some(f) = &mut self.input_form {
            f.value.pop();
        }
    }

    /// Bracketed paste (terminal paste event): the whole pasted text
    /// lands in the form value, whitespace-trimmed - a pasted URL
    /// arrives cleanly, never as a burst of key events.
    pub fn form_paste(&mut self, text: &str) {
        if let Some(f) = &mut self.input_form {
            let cleaned: String = text.chars().filter(|c| !c.is_control()).collect();
            f.value.push_str(cleaned.trim());
        }
    }

    /// Enter in the modal: validate the link now (instant, no
    /// network); a valid link defers its fetch to after the draw.
    pub fn form_submit(&mut self) {
        let form = match self.input_form.take() {
            Some(f) => f,
            None => return,
        };
        match parse_playlist_link(&form.value) {
            Ok(link) => {
                self.status_message = "Loading playlist from link...".to_string();
                self.pending_link = Some(link);
            }
            Err(msg) => {
                self.status_message = msg;
            }
        }
    }

    /// Run the deferred pasted-link fetch (blocking network, after
    /// a draw like the other deferred loads). On success the entry
    /// is PINNED into settings.saved_playlists and the browse view
    /// opens under the playlist's real name.
    pub fn process_pending_link(&mut self) {
        let link = match self.pending_link.take() {
            Some(l) => l,
            None => return,
        };
        let fetched: Result<(String, Vec<Track>), String> = match link.source {
            Source::Spotify => {
                let spot = self.spotify.clone();
                let tracks = match link.kind {
                    SavedKind::Playlist => spot.playlist_tracks(&link.id),
                    SavedKind::Album => spot.album_tracks(&link.id),
                };
                tracks.map(|tracks| {
                    let kind_word = match link.kind {
                        SavedKind::Playlist => "playlist",
                        SavedKind::Album => "album",
                    };
                    // The real name comes from the catalog itself;
                    // a name lookup failing never blocks the tracks.
                    let name = spot
                        .catalog_name(kind_word, &link.id)
                        .unwrap_or_else(|_| format!("{} Spotify", kind_word));
                    (name, tracks)
                })
            }
            Source::YtMusic => innertube::browse_playlist(&link.id)
                .map(|(title, tracks)| (title.unwrap_or_else(|| "YouTube Music Playlist".to_string()), tracks)),
        };
        match fetched {
            Ok((name, tracks)) => {
                self.settings.upsert_saved_playlist(SavedPlaylist {
                    source: link.source,
                    kind: link.kind,
                    id: link.id.clone(),
                    name: name.clone(),
                    url: link.url.clone(),
                });
                self.persist_settings();
                self.finish_browse(
                    name,
                    Some("Opened from a pasted link - saved in the sidebar, reopenable any time (x deletes it).".to_string()),
                    Ok(tracks),
                );
            }
            Err(e) => {
                self.status_message = format!("Could not load the link: {}", e);
                diag::append_log(&format!("failed to load link '{}': {}", link.url, e));
            }
        }
    }

    // -------------------------------------------------------------------
    // Mouse dispatch
    // -------------------------------------------------------------------

    fn click_target_at(&self, col: u16, row: u16) -> Option<(Rect, ClickTarget)> {
        // Later registrations render on top; search in reverse.
        self.click_map
            .iter()
            .rev()
            .find(|(r, _)| col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height)
            .cloned()
    }

    /// One left-button press at (col, row). Track rows select on a
    /// single click and play/open on a double click; everything
    /// else acts immediately.
    ///
    /// Defect 2 (v0.1.5): while the modal form is open, clicks are
    /// IGNORED - the form is modal, so a click can never strand the
    /// user on another screen with the keyboard silently captured
    /// (the v0.1.4 bug: clicking Spotify [Login]/[Setup] opened a
    /// key-capturing form rendered only inside the Accounts bottom
    /// panel; clicking away with the mouse then left every key
    /// "dead"). Esc closes the form from anywhere, always.
    pub fn mouse_click(&mut self, col: u16, row: u16) {
        if self.input_form.is_some() {
            return;
        }
        let (rect, target) = match self.click_target_at(col, row) {
            Some(x) => x,
            None => return,
        };
        let now = Instant::now();
        let is_double = match &self.last_click {
            Some((t, prev)) => *prev == target && now.duration_since(*t).as_millis() <= 450,
            None => false,
        };
        self.last_click = Some((now, target.clone()));
        match target {
            ClickTarget::NavItem(i) => {
                if i < NAV_SCREENS.len() {
                    self.browse = None;
                    self.set_screen(NAV_SCREENS[i]);
                    self.focus = PanelFocus::Main;
                }
            }
            ClickTarget::PlaylistItem(i) => {
                self.focus = PanelFocus::Nav;
                self.open_sidebar_playlist(i);
            }
            ClickTarget::PasteLink => {
                self.focus = PanelFocus::Nav;
                self.start_link_form();
            }
            ClickTarget::HomeTrack(i) => {
                self.focus = PanelFocus::Main;
                if is_double {
                    self.home_play(i);
                } else {
                    self.home_selected = i;
                }
            }
            ClickTarget::QueueTrack(i) => {
                self.focus = PanelFocus::Main;
                if is_double {
                    if i < self.engine.queue.len() {
                        self.request_play(i);
                    }
                } else {
                    self.queue_selected = i;
                }
            }
            ClickTarget::SearchRow(ord) => {
                self.focus = PanelFocus::Main;
                self.search_selected = ord;
                if is_double {
                    self.activate_search_selection();
                }
            }
            ClickTarget::BrowseTrack(i) => {
                self.focus = PanelFocus::Main;
                if let Some(b) = &mut self.browse {
                    b.selected = i;
                }
                if is_double {
                    self.browse_play_selected();
                }
            }
            ClickTarget::ProgressBar => {
                self.seek_to_ratio(rect, col);
            }
            ClickTarget::EqPresetPrev => {
                self.focus = PanelFocus::EqSidebar;
                self.eq_sidebar_selected = 0;
                self.cycle_eq_preset(-1);
            }
            ClickTarget::EqPresetNext => {
                self.focus = PanelFocus::EqSidebar;
                self.eq_sidebar_selected = 0;
                self.cycle_eq_preset(1);
            }
            ClickTarget::EqBandRow(r) => {
                self.focus = PanelFocus::EqSidebar;
                self.eq_sidebar_selected = r;
                let ratio = ratio_in(rect, col);
                self.eq_sidebar_set_from_ratio(r, ratio);
            }
            ClickTarget::EqVolumeRow => {
                self.focus = PanelFocus::EqSidebar;
                self.eq_sidebar_selected = 12;
                let ratio = ratio_in(rect, col);
                self.eq_sidebar_set_from_ratio(12, ratio);
            }
            ClickTarget::SearchChip(kind) => {
                self.search_selected = 0;
                self.apply_search_kind(kind);
            }
            ClickTarget::SearchPlatformChip(p) => {
                self.search_platform = p;
                self.apply_search_platform();
            }
            ClickTarget::VizBox => {
                self.cycle_visualizer_style();
            }
            ClickTarget::LyricsToggle => {
                self.toggle_lyrics();
            }
            ClickTarget::SplashEnter => {
                self.set_screen(Screen::NowPlaying);
            }
        }
    }

    /// Left-button drag: EQ rows take the value from the pointer x;
    /// the progress bar scrubs. Also suspended while the modal form
    /// is open (same Defect-2 reasoning as clicks).
    pub fn mouse_drag(&mut self, col: u16, row: u16) {
        if self.input_form.is_some() {
            return;
        }
        let (rect, target) = match self.click_target_at(col, row) {
            Some(x) => x,
            None => return,
        };
        match target {
            ClickTarget::EqBandRow(r) => {
                self.eq_sidebar_selected = r;
                let ratio = ratio_in(rect, col);
                self.eq_sidebar_set_from_ratio(r, ratio);
            }
            ClickTarget::EqVolumeRow => {
                self.eq_sidebar_selected = 12;
                let ratio = ratio_in(rect, col);
                self.eq_sidebar_set_from_ratio(12, ratio);
            }
            ClickTarget::ProgressBar => {
                self.seek_to_ratio(rect, col);
            }
            _ => {}
        }
    }

    /// Click/drag on the now-playing progress bar: seek mpv to the
    /// proportional position (real IPC seek, like the arrow keys).
    fn seek_to_ratio(&mut self, rect: Rect, col: u16) {
        let ratio = ratio_in(rect, col);
        let dur = self.engine.display_duration();
        if dur <= 0.0 || self.engine.current_track().is_none() {
            return;
        }
        if !self.engine.seek_absolute(ratio * dur) {
            self.status_message = "Seek not available yet: mpv is not connected over IPC.".to_string();
        }
    }

    /// Activate the currently selected search row (Enter key /
    /// double click share this): tracks play, albums & playlists
    /// open.
    pub fn activate_search_selection(&mut self) {
        let row = self.search_selectable().get(self.search_selected).cloned();
        match row {
            Some(SearchRow::Track(i)) => {
                if let Some(t) = self.search_tracks.get(i).cloned() {
                    self.engine.enqueue(t);
                    let idx = self.engine.queue.len() - 1;
                    self.request_play(idx);
                    self.set_screen(Screen::NowPlaying);
                }
            }
            Some(SearchRow::Album(_)) | Some(SearchRow::Playlist(_)) => {
                if let Some(r) = row {
                    self.open_search_item(&r);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nav_has_five_screens_and_no_accounts() {
        // v0.1.5 scope change: the Accounts screen is GONE.
        assert_eq!(NAV_SCREENS.len(), 5);
        assert!(!NAV_SCREENS.contains(&Screen::Settings) == false);
        assert_eq!(NAV_SCREENS[4], Screen::Settings);
        for n in 1..=5u8 {
            let s = Screen::from_number(n);
            assert_eq!(s, NAV_SCREENS[(n - 1) as usize]);
            assert_eq!(nav_index(s), (n - 1) as usize);
        }
    }

    #[test]
    fn set_screen_keeps_sidebar_marker_in_sync() {
        let mut app = App::new();
        for (i, s) in NAV_SCREENS.iter().enumerate() {
            app.set_screen(*s);
            assert_eq!(app.screen, *s);
            assert_eq!(app.nav_selected, i, "marker must follow screen {:?}", s);
        }
        app.set_screen(Screen::Splash);
        assert_eq!(app.nav_selected, 0);
    }

    #[test]
    fn set_screen_ends_search_typing_mode() {
        // Defect 2 belt-and-braces: a text-capture state never
        // follows the user onto another screen.
        let mut app = App::new();
        app.set_screen(Screen::Search);
        app.search_input_mode = true;
        app.set_screen(Screen::NowPlaying);
        assert!(!app.search_input_mode);
        // ...and staying on Search keeps it.
        app.set_screen(Screen::Search);
        app.search_input_mode = true;
        app.set_screen(Screen::Search);
        assert!(app.search_input_mode);
    }

    #[test]
    fn panel_focus_cycles_nav_main_player() {
        assert_eq!(PanelFocus::Nav.next(), PanelFocus::Main);
        assert_eq!(PanelFocus::Main.next(), PanelFocus::EqSidebar);
        assert_eq!(PanelFocus::EqSidebar.next(), PanelFocus::Player);
        assert_eq!(PanelFocus::Player.next(), PanelFocus::Nav);
    }

    #[test]
    fn search_rows_group_sections_and_filters() {
        let mut app = App::new();
        app.search_tracks = vec![
            Track::new("t1", "Lagu Satu", "Artis", 100, Source::Spotify),
            Track::new("t2", "Lagu Dua", "Artis", 100, Source::YtMusic),
        ];
        app.search_albums = vec![AlbumRef {
            id: "a1".into(),
            title: "Album Satu".into(),
            artist: "Artis".into(),
            year: "2020".into(),
            source: Source::YtMusic,
        }];
        app.search_playlists = vec![PlaylistRef {
            id: "p1".into(),
            title: "Playlist Satu".into(),
            info: "oleh x".into(),
            source: Source::Spotify,
        }];
        app.search_kind = 0;
        let rows = app.search_rows();
        assert_eq!(rows[0], SearchRow::Section("SONGS"));
        assert_eq!(rows[1], SearchRow::Track(0));
        assert_eq!(rows[3], SearchRow::Section("ALBUMS"));
        assert_eq!(rows[4], SearchRow::Album(0));
        assert_eq!(rows[5], SearchRow::Section("PLAYLISTS"));
        assert_eq!(rows[6], SearchRow::Playlist(0));
        assert_eq!(app.search_selectable().len(), 4);
        app.search_kind = 2;
        assert_eq!(
            app.search_rows(),
            vec![SearchRow::Section("ALBUMS"), SearchRow::Album(0)]
        );
        app.search_kind = 3;
        app.search_playlists.clear();
        assert!(app.search_rows().is_empty());
    }

    #[test]
    fn sidebar_playlists_are_saved_links_only() {
        // v0.1.6: the seeded example playlists are GONE - with
        // nothing saved, the sidebar playlist list is empty.
        let mut app = App::new();
        // Pin: the dev machine's persisted settings may already
        // hold saved links from earlier runs.
        app.settings.saved_playlists.clear();
        assert!(app.sidebar_playlists().is_empty());
        // A saved link entry appears under its own source, carrying
        // its real id, and opens by that id.
        app.settings.saved_playlists.push(SavedPlaylist {
            source: Source::YtMusic,
            kind: SavedKind::Playlist,
            id: "VLPLsaved".to_string(),
            name: "My Pasted Playlist".to_string(),
            url: "https://music.youtube.com/playlist?list=PLsaved".to_string(),
        });
        app.settings.saved_playlists.push(SavedPlaylist {
            source: Source::Spotify,
            kind: SavedKind::Playlist,
            id: "37i9dQZEVXbMDoHDwVN2tF".to_string(),
            name: "Top 50".to_string(),
            url: "https://open.spotify.com/playlist/37i9dQZEVXbMDoHDwVN2tF".to_string(),
        });
        app.persist_settings();
        let list = app.sidebar_playlists();
        assert_eq!(list.len(), 2);
        // Spotify block first, then YT Music (sidebar grouping).
        assert_eq!(list[0].source, Source::Spotify);
        assert_eq!(list[1].source, Source::YtMusic);
        assert_eq!(list[1].title, "My Pasted Playlist");
        assert_eq!(list[1].id.as_deref(), Some("VLPLsaved"));
        app.open_sidebar_playlist(1);
        match app.pending_browse {
            Some(BrowseRequest::Playlist { source, ref id, .. }) => {
                assert_eq!(source, Source::YtMusic);
                assert_eq!(id, "VLPLsaved");
            }
            other => panic!("unexpected pending browse: {:?}", other),
        }
        // The paste row sits right after the last playlist entry.
        assert!(app.nav_total() == NAV_SCREENS.len() + list.len() + 1);
        app.nav_selected = NAV_SCREENS.len() + 1;
        app.delete_nav_saved();
        assert_eq!(app.settings.saved_playlists.len(), 1, "x deletes the saved entry");
        assert_eq!(app.settings.saved_playlists[0].source, Source::Spotify);
    }

    #[test]
    fn playlist_link_parsing_fixtures() {
        // Valid Spotify playlist + album (tracking params, intl).
        let p = parse_playlist_link("https://open.spotify.com/playlist/37i9dQZEVXbMDoHDwVN2tF?si=abc123").unwrap();
        assert_eq!(p.source, Source::Spotify);
        assert_eq!(p.kind, SavedKind::Playlist);
        assert_eq!(p.id, "37i9dQZEVXbMDoHDwVN2tF");
        let a = parse_playlist_link("https://open.spotify.com/intl-id/album/4aawyAB9vmqN3uQ7FjRGTy?si=x").unwrap();
        assert_eq!(a.kind, SavedKind::Album);
        assert_eq!(a.id, "4aawyAB9vmqN3uQ7FjRGTy");
        // Valid YT Music + plain YouTube hosts; VL prefix added once.
        let y = parse_playlist_link("https://music.youtube.com/playlist?list=PLmKg-TtEEqaIS2Cv3XzdJojJWsHtT3gz_").unwrap();
        assert_eq!(y.source, Source::YtMusic);
        assert_eq!(y.id, "VLPLmKg-TtEEqaIS2Cv3XzdJojJWsHtT3gz_");
        let y2 = parse_playlist_link("https://www.youtube.com/playlist?list=VLPLalready").unwrap();
        assert_eq!(y2.id, "VLPLalready");
        // No scheme still parses.
        assert!(parse_playlist_link("open.spotify.com/playlist/37i9dQZEVXbMDoHDwVN2tF").is_ok());
        // Short link: a specific, actionable refusal.
        let e = parse_playlist_link("https://spotify.link/AbCdEf").unwrap_err();
        assert!(e.contains("spotify.link") && e.contains("open.spotify.com"), "err: {}", e);
        // Wrong host.
        let e = parse_playlist_link("https://example.com/playlist/37i9dQZEVXbMDoHDwVN2tF").unwrap_err();
        assert!(e.contains("Unknown host"), "err: {}", e);
        // Spotify link that is not a playlist/album.
        let e = parse_playlist_link("https://open.spotify.com/track/4uLU6hMCjMI75M1A2tKUQC").unwrap_err();
        assert!(e.contains("not a playlist/album"), "err: {}", e);
        // YTM link without list=.
        let e = parse_playlist_link("https://music.youtube.com/watch?v=abc").unwrap_err();
        assert!(e.contains("not a playlist page"), "err: {}", e);
        let e = parse_playlist_link("https://music.youtube.com/playlist").unwrap_err();
        assert!(e.contains("list="), "err: {}", e);
        // Empty.
        assert!(parse_playlist_link("   ").is_err());
    }

    #[test]
    fn modal_form_suspends_mouse_and_paste_fills_it() {
        // Defect 2 regression (state-machine level): with the form
        // open, a click that WOULD navigate does nothing at all;
        // after Esc-equivalent (input_form = None), clicks work
        // again.
        let mut app = App::new();
        app.set_screen(Screen::NowPlaying);
        // Register a fake nav click region, as ui::render would.
        app.click_map.push((
            Rect { x: 0, y: 0, width: 10, height: 1 },
            ClickTarget::NavItem(1),
        ));
        app.start_link_form();
        assert!(app.input_form.is_some());
        app.mouse_click(2, 0);
        assert_eq!(app.screen, Screen::NowPlaying, "click during modal must not navigate");
        assert!(app.input_form.is_some(), "click during modal must not dismiss the form");
        // Bracketed paste lands in the form as one clean string.
        app.form_paste("  https://open.spotify.com/playlist/37i9dQZEVXbMDoHDwVN2tF?si=x\n");
        assert_eq!(
            app.input_form.as_ref().unwrap().value,
            "https://open.spotify.com/playlist/37i9dQZEVXbMDoHDwVN2tF?si=x"
        );
        app.form_submit();
        assert!(app.input_form.is_none());
        assert!(app.pending_link.is_some(), "valid pasted link defers its fetch");
        // Clicks work again once the modal is gone.
        app.mouse_click(2, 0);
        assert_eq!(app.screen, Screen::Search);
    }

    #[test]
    fn browse_open_lists_tracks_and_plays() {
        let mut app = App::new();
        let queue_before = app.engine.queue.len();
        let tracks = vec![
            Track::new("b1", "Browse Satu", "Artis", 120, Source::YtMusic),
            Track::new("b2", "Browse Dua", "Artis", 130, Source::YtMusic),
        ];
        app.finish_browse("Album Uji".to_string(), None, Ok(tracks));
        let b = app.browse.as_ref().expect("browse view open");
        assert_eq!(b.title, "Album Uji");
        assert_eq!(b.tracks.len(), 2);
        assert_eq!(b.selected, 0);
        app.browse_play_selected();
        assert_eq!(app.engine.queue.len(), queue_before + 1);
        assert_eq!(app.pending_play, Some(queue_before));
        app.browse_queue_selected();
        assert_eq!(app.engine.queue.len(), queue_before + 2);
        // A failed load opens an honest empty view with the reason.
        app.finish_browse("Gagal".to_string(), None, Err("jaringan mati".to_string()));
        let b = app.browse.as_ref().expect("error browse view");
        assert!(b.tracks.is_empty());
        assert!(b.note.as_deref().unwrap_or("").contains("jaringan mati"));
        app.close_browse();
        assert!(app.browse.is_none());
    }

    #[test]
    fn trending_merge_interleaves_dedupes_and_caps() {
        let spot = vec![
            Track::new("s1", "Lagu Sama", "Artis", 100, Source::Spotify),
            Track::new("s2", "Spotify Dua", "B", 100, Source::Spotify),
            Track::new("s3", "Spotify Tiga", "C", 100, Source::Spotify),
        ];
        let ytm = vec![
            Track::new("y1", "Lagu Sama", "Artis", 100, Source::YtMusic),
            Track::new("y2", "YTM Dua", "D", 100, Source::YtMusic),
        ];
        let merged = merge_trending(spot, ytm);
        // Interleaved by rank; the cross-source duplicate appears
        // once (the Spotify copy, ranked first).
        assert_eq!(merged.len(), 4);
        assert_eq!(merged[0].id, "s1");
        assert_eq!(merged[1].id, "y2");
        assert_eq!(merged[2].id, "s2");
        assert_eq!(merged[3].id, "s3");
        // Cap at 30.
        let many: Vec<Track> = (0..40)
            .map(|i| Track::new(format!("x{}", i), format!("T{}", i), "A", 60, Source::Spotify))
            .collect();
        assert_eq!(merge_trending(many, Vec::new()).len(), 30);
        assert!(merge_trending(Vec::new(), Vec::new()).is_empty());
    }

    #[test]
    fn sidebar_eq_edits_share_state_with_eq_screen() {
        let mut app = App::new();
        app.eq_sidebar_selected = 0;
        let before = app.settings.eq.preset_name.clone();
        app.eq_sidebar_adjust(1);
        assert_ne!(app.settings.eq.preset_name, before);
        app.settings.eq.apply_builtin_preset("Flat");
        app.eq_sidebar_selected = 1;
        app.eq_sidebar_adjust(1);
        assert_eq!(app.settings.eq.bands[0], 1.0);
        assert_eq!(app.settings.eq.preset_name, "Custom (unsaved)");
        app.eq_sidebar_selected = 11;
        for _ in 0..30 {
            app.eq_sidebar_adjust(1);
        }
        assert_eq!(app.settings.eq.preamp, 12.0);
        app.eq_sidebar_set_from_ratio(2, 0.5);
        assert_eq!(app.settings.eq.bands[1], 0.0);
        app.eq_sidebar_set_from_ratio(2, 1.0);
        assert_eq!(app.settings.eq.bands[1], 12.0);
    }

    #[test]
    fn sidebar_eq_vol_row_edits_shared_volume() {
        // Defect 3: the VOL row (12) drives settings.volume - the
        // same value the bottom bar shows and sync_live_settings
        // pushes to mpv.
        let mut app = App::new();
        app.settings.volume = 70;
        app.eq_sidebar_selected = 12;
        app.eq_sidebar_adjust(1);
        assert_eq!(app.settings.volume, 75);
        app.eq_sidebar_adjust(-1);
        app.eq_sidebar_adjust(-1);
        assert_eq!(app.settings.volume, 65);
        for _ in 0..30 {
            app.eq_sidebar_adjust(1);
        }
        assert_eq!(app.settings.volume, 100, "clamped at 100");
        for _ in 0..40 {
            app.eq_sidebar_adjust(-1);
        }
        assert_eq!(app.settings.volume, 0, "clamped at 0");
        // Mouse ratio: left edge = 0, right edge = 100, mid = 50.
        app.eq_sidebar_set_from_ratio(12, 0.0);
        assert_eq!(app.settings.volume, 0);
        app.eq_sidebar_set_from_ratio(12, 1.0);
        assert_eq!(app.settings.volume, 100);
        app.eq_sidebar_set_from_ratio(12, 0.5);
        assert_eq!(app.settings.volume, 50);
    }

    // -----------------------------------------------------------------
    // v0.1.6: lyrics bound to track identity
    // -----------------------------------------------------------------

    fn synced(text: &str) -> LyricsResult {
        LyricsResult::Synced(vec![crate::lyrics::LyricLine {
            time_secs: 0.0,
            text: text.to_string(),
        }])
    }

    fn lyrics_lines(app: &App) -> Vec<String> {
        match &app.lyrics {
            Some(LyricsResult::Synced(lines)) => lines.iter().map(|l| l.text.clone()).collect(),
            _ => Vec::new(),
        }
    }

    #[test]
    fn lyrics_invalidate_immediately_on_track_change() {
        // Point LRCLib at a dead port so the background fetch fails
        // fast and deterministically; the STATE assertions below do
        // not depend on the fetch outcome.
        std::env::set_var("LYRE_LRCLIB_BASE", "http://127.0.0.1:9");
        let mut app = App::new();
        // The demo queue: track 0 = "Monokrom" (Spotify), 1 = "As It Was".
        let key_a = TrackKey::of(&app.engine.queue[0].clone());
        app.show_lyrics = true;
        app.lyrics = Some(synced("A's lyric line"));
        app.lyrics_key = Some(key_a);
        app.lyrics_offset = 1.5;
        // The track changes (any path - here the engine index moves,
        // then the per-tick sync runs, as poll_background does).
        app.engine.current_index = Some(1);
        app.sync_lyrics_track();
        // The rendered model must NOT still hold A's lines.
        assert!(app.lyrics.is_none(), "old lyrics must be dropped at once");
        assert!(lyrics_lines(&app).is_empty());
        assert_eq!(app.lyrics_offset, 0.0, "offset resets on track change");
        assert!(app.lyrics_loading, "a fetch for the NEW track is in flight");
        assert!(
            app.lyrics_status.contains("Loading lyrics") && app.lyrics_status.contains("As It Was"),
            "status: {}",
            app.lyrics_status
        );
        assert_eq!(app.lyrics_key, app.current_lyrics_key());
    }

    #[test]
    fn stale_lyrics_result_is_dropped_race() {
        std::env::set_var("LYRE_LRCLIB_BASE", "http://127.0.0.1:9");
        let mut app = App::new();
        let track_a = app.engine.queue[0].clone();
        let track_b = app.engine.queue[1].clone();
        let key_a = TrackKey::of(&track_a);
        let key_b = TrackKey::of(&track_b);
        // Current track is B, lyrics bound to B, fetch in flight.
        app.engine.current_index = Some(1);
        app.show_lyrics = true;
        app.lyrics_key = Some(key_b.clone());
        app.lyrics_loading = true;
        // A's fetch completes LATE (after the switch): its result
        // must be discarded, B's state untouched.
        let (tx, rx) = std::sync::mpsc::channel();
        app.lyrics_rx = Some(rx);
        tx.send((key_a, Ok(synced("A's late line")))).unwrap();
        app.poll_lyrics();
        assert!(app.lyrics.is_none(), "stale lines must never land");
        assert!(app.lyrics_loading, "B's fetch is still the live one");
        // B's own result then applies normally.
        let (tx2, rx2) = std::sync::mpsc::channel();
        app.lyrics_rx = Some(rx2);
        tx2.send((key_b, Ok(synced("B's line")))).unwrap();
        app.poll_lyrics();
        assert_eq!(lyrics_lines(&app), vec!["B's line".to_string()]);
        assert!(!app.lyrics_loading);
    }

    #[test]
    fn lyrics_key_includes_full_track_identity() {
        // Same title + artist, different id/source: different keys,
        // so B can never be served A's entry.
        let a = Track::new("id-1", "Same Title", "Same Artist", 100, Source::Spotify);
        let b = Track::new("id-2", "Same Title", "Same Artist", 100, Source::YtMusic);
        assert_ne!(TrackKey::of(&a), TrackKey::of(&b));
        // And the state machine honors it: bound to A's key, moving
        // the current track to the same-titled B invalidates.
        std::env::set_var("LYRE_LRCLIB_BASE", "http://127.0.0.1:9");
        let mut app = App::new();
        app.engine.queue = vec![a.clone(), b.clone()];
        app.engine.current_index = Some(0);
        app.show_lyrics = true;
        app.lyrics = Some(synced("A only"));
        app.lyrics_key = Some(TrackKey::of(&a));
        app.engine.current_index = Some(1);
        app.sync_lyrics_track();
        assert!(app.lyrics.is_none());
        assert_eq!(app.lyrics_key, Some(TrackKey::of(&b)));
    }

    // -----------------------------------------------------------------
    // v0.1.6: search platform filter
    // -----------------------------------------------------------------

    #[test]
    fn search_platform_cycles_all_spotify_ytmusic() {
        let mut app = App::new();
        assert_eq!(app.search_platform, SearchPlatform::All);
        app.cycle_search_platform();
        assert_eq!(app.search_platform, SearchPlatform::Spotify);
        assert!(app.search_platform.spotify_active());
        assert!(!app.search_platform.ytmusic_active());
        app.cycle_search_platform();
        assert_eq!(app.search_platform, SearchPlatform::YtMusic);
        assert!(!app.search_platform.spotify_active());
        assert!(app.search_platform.ytmusic_active());
        app.cycle_search_platform();
        assert_eq!(app.search_platform, SearchPlatform::All);
        assert!(app.search_platform.spotify_active() && app.search_platform.ytmusic_active());
    }

    // -----------------------------------------------------------------
    // v0.1.6: autoplay
    // -----------------------------------------------------------------

    fn radio_fixture() -> Vec<Track> {
        vec![
            Track::new("r1", "Radio One", "Artist R", 180, Source::YtMusic),
            Track::new("r2", "Radio Two", "Artist R", 181, Source::YtMusic),
            Track::new("r3", "Radio Three", "Artist R", 182, Source::YtMusic),
        ]
    }

    fn autoplay_app() -> App {
        let mut app = App::new();
        // Pin the toggle: the dev machine's persisted settings may
        // carry a different value (other tests save real settings).
        app.settings.autoplay = true;
        app.engine.queue = vec![
            Track::new("u1", "User One", "Artist U", 100, Source::YtMusic),
            Track::new("u2", "User Two", "Artist U", 100, Source::YtMusic),
        ];
        app.engine.current_index = Some(0);
        app.played_history = vec![TrackKey::of(&app.engine.queue[0].clone())];
        app
    }

    #[test]
    fn autoplay_appends_deduped_capped_picks_and_sets_boundary() {
        let mut app = autoplay_app();
        let key = TrackKey::of(&app.engine.queue[0].clone());
        let mut picks = radio_fixture();
        // The radio list contains the playing song itself, a queued
        // song, and a duplicate pair - all must be deduped away.
        picks.push(Track::new("u1-dup", "User One", "Artist U", 100, Source::YtMusic));
        picks.push(Track::new("u2", "User Two", "Artist U", 100, Source::YtMusic));
        picks.push(Track::new("r1", "Radio One", "Artist R", 180, Source::YtMusic));
        app.apply_radio_result(key, Ok(picks));
        assert_eq!(app.autoplay_from, Some(2), "boundary at the old queue end");
        let ids: Vec<&str> = app.engine.queue.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["u1", "u2", "r1", "r2", "r3"]);
        assert!(app.is_autoplay_pick(2) && app.is_autoplay_pick(4));
        assert!(!app.is_autoplay_pick(1));
        // Cap: a 15-pick radio adds at most 10.
        let mut app2 = autoplay_app();
        let key2 = TrackKey::of(&app2.engine.queue[0].clone());
        let many: Vec<Track> = (0..15)
            .map(|i| Track::new(format!("m{}", i), format!("Pick {}", i), "Artist M", 100, Source::YtMusic))
            .collect();
        app2.apply_radio_result(key2, Ok(many));
        assert_eq!(app2.engine.queue.len(), 2 + 10);
        // Stale results (keyed to a track no longer current) drop.
        let mut app3 = autoplay_app();
        let other = TrackKey::of(&app3.engine.queue[1].clone());
        app3.apply_radio_result(other, Ok(radio_fixture()));
        assert_eq!(app3.engine.queue.len(), 2, "stale picks must not append");
        assert!(app3.autoplay_from.is_none());
    }

    #[test]
    fn autoplay_user_enqueue_inserts_before_the_block() {
        let mut app = autoplay_app();
        let key = TrackKey::of(&app.engine.queue[0].clone());
        app.apply_radio_result(key, Ok(radio_fixture()));
        app.enqueue_user(Track::new("u3", "User Three", "Artist U", 100, Source::YtMusic));
        let ids: Vec<&str> = app.engine.queue.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["u1", "u2", "u3", "r1", "r2", "r3"]);
        assert_eq!(app.autoplay_from, Some(3), "boundary shifts with the insert");
    }

    #[test]
    fn autoplay_manual_play_clears_pending_picks_and_reseeds() {
        let mut app = autoplay_app();
        let key = TrackKey::of(&app.engine.queue[0].clone());
        app.apply_radio_result(key, Ok(radio_fixture()));
        assert_eq!(app.engine.queue.len(), 5);
        // The user manually starts u2 (index 1): picks after it go.
        let new_idx = app.drop_autoplay_picks_for_manual_play(1);
        assert_eq!(new_idx, 1);
        let ids: Vec<&str> = app.engine.queue.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["u1", "u2"]);
        assert!(app.autoplay_from.is_none());
        assert!(app.radio_for.is_none(), "the old seed key is released");
        // Re-seeding is keyed to the NEW current track.
        app.engine.current_index = Some(1);
        app.maybe_seed_autoplay();
        assert_eq!(app.radio_for, Some(TrackKey::of(&app.engine.queue[1].clone())));
        // Playing an autoplay pick directly keeps THAT pick, drops
        // the rest of the block, and adjusts the index.
        let mut app2 = autoplay_app();
        let key2 = TrackKey::of(&app2.engine.queue[0].clone());
        app2.apply_radio_result(key2, Ok(radio_fixture()));
        let idx = app2.drop_autoplay_picks_for_manual_play(3); // r2
        assert_eq!(idx, 2);
        let ids2: Vec<&str> = app2.engine.queue.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids2, vec!["u1", "u2", "r2"]);
    }

    // -----------------------------------------------------------------
    // v0.1.6: platform filter ROUTING (stub servers + request counts)
    // -----------------------------------------------------------------

    /// Minimal HTTP stub: counts every request, answers by path.
    /// `search_hits` counts only requests carrying THIS test's
    /// query ("stub") - other tests run App::new() in parallel and
    /// their startup trending fetches legitimately reach these
    /// process-wide stub endpoints too; only the tagged counts may
    /// be asserted on.
    struct StubHttpServer {
        hits: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        search_hits: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        base: String,
    }

    fn start_stub_server(kind: &'static str) -> StubHttpServer {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let search_hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hits2 = hits.clone();
        let search2 = search_hits.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = match stream {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                hits2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                // Read headers + the full body (Content-Length):
                // a single read() can return headers alone, which
                // would hide the JSON body from the tagging below.
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(2)));
                let mut raw: Vec<u8> = Vec::new();
                let mut chunk = [0u8; 16384];
                loop {
                    match stream.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) => {
                            raw.extend_from_slice(&chunk[..n]);
                            let text = String::from_utf8_lossy(&raw).to_string();
                            if let Some(hend) = text.find("\r\n\r\n") {
                                let clen = text[..hend]
                                    .lines()
                                    .find_map(|l| {
                                        l.to_lowercase()
                                            .strip_prefix("content-length:")
                                            .and_then(|v| v.trim().parse::<usize>().ok())
                                    })
                                    .unwrap_or(0);
                                if raw.len() >= hend + 4 + clen {
                                    break;
                                }
                            }
                        }
                        Err(_) => break,
                    }
                }
                let req = String::from_utf8_lossy(&raw).to_string();
                // Tag this test's own search calls: Spotify carries
                // q=stub in the URL, Innertube carries the query in
                // the JSON body.
                let tagged = if kind == "spotify" {
                    req.contains("/v1/search") && req.contains("q=stub")
                } else {
                    req.contains("\"query\"") && req.contains("stub")
                };
                if tagged {
                    search2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
                let body = if kind == "spotify" {
                    if req.contains("get_access_token") {
                        let exp = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_millis() as u64)
                            .unwrap_or(0)
                            + 3_600_000;
                        format!(
                            "{{\"accessToken\":\"STUBTOKEN\",\"accessTokenExpirationTimestampMs\":{},\"isAnonymous\":true}}",
                            exp
                        )
                    } else if req.contains("/v1/playlists/") {
                        "{\"items\":[]}".to_string()
                    } else if req.contains("/v1/search") {
                        "{\"tracks\":{\"items\":[{\"id\":\"spT1\",\"name\":\"Stub Song\",\"artists\":[{\"name\":\"Stub Artist\"}],\"duration_ms\":123000}]},\"albums\":{\"items\":[{\"id\":\"spA1\",\"name\":\"Stub Album\",\"artists\":[{\"name\":\"Stub Artist\"}],\"release_date\":\"2024-01-01\"}]},\"playlists\":{\"items\":[{\"id\":\"spP1\",\"name\":\"Stub Playlist\",\"owner\":{\"display_name\":\"stub\"},\"tracks\":{\"total\":7}}]}}".to_string()
                    } else {
                        "{}".to_string()
                    }
                } else {
                    // Innertube stub: valid-but-empty JSON for any call.
                    "{}".to_string()
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        StubHttpServer {
            hits,
            search_hits,
            base: format!("http://127.0.0.1:{}", port),
        }
    }

    #[test]
    fn search_platform_filter_routes_fetching() {
        let _env_guard = crate::diag::env_lock();
        let spot = start_stub_server("spotify");
        let inner = start_stub_server("innertube");
        // A fake yt-dlp that only records its invocation.
        let fakebin = std::env::temp_dir().join(format!("lyre-fakebin-{}", std::process::id()));
        std::fs::create_dir_all(&fakebin).unwrap();
        let counter = fakebin.join("ytdlp-calls.txt");
        let script = fakebin.join("yt-dlp");
        std::fs::write(&script, format!("#!/bin/sh\necho call >> {}\nexit 1\n", counter.display())).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let saved: Vec<(&str, Option<String>)> = [
            "LYRE_SPOTIFY_TOKEN_URL",
            "LYRE_SPOTIFY_API_BASE",
            "LYRE_INNERTUBE_BASE",
            "PATH",
        ]
        .iter()
        .map(|k| (*k, std::env::var(k).ok()))
        .collect();
        std::env::set_var("LYRE_SPOTIFY_TOKEN_URL", format!("{}/get_access_token?reason=transport", spot.base));
        std::env::set_var("LYRE_SPOTIFY_API_BASE", spot.base.clone());
        std::env::set_var("LYRE_INNERTUBE_BASE", format!("{}/youtubei/v1", inner.base));
        let old_path = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{}", fakebin.display(), old_path));

        let mut app = App::new();
        // Drain the startup trending fetch (it legitimately hits
        // both sources), then zero the counters.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while app.trending_rx.is_some() && std::time::Instant::now() < deadline {
            app.poll_background();
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        let reset = |app: &App| {
            let _ = app;
            spot.hits.store(0, std::sync::atomic::Ordering::SeqCst);
            inner.hits.store(0, std::sync::atomic::Ordering::SeqCst);
            spot.search_hits.store(0, std::sync::atomic::Ordering::SeqCst);
            inner.search_hits.store(0, std::sync::atomic::Ordering::SeqCst);
            let _ = std::fs::remove_file(&counter);
        };
        app.search_query = "stub".to_string();
        let tagged = |s: &StubHttpServer| s.search_hits.load(std::sync::atomic::Ordering::SeqCst);

        // Spotify only: Innertube and yt-dlp must NEVER be invoked.
        reset(&app);
        app.search_platform = SearchPlatform::Spotify;
        app.run_search();
        assert!(tagged(&spot) > 0, "spotify search must be hit");
        assert_eq!(tagged(&inner), 0, "Innertube search must not be called");
        assert!(!counter.exists(), "yt-dlp must not be invoked");
        assert!(!app.search_tracks.is_empty());
        assert!(app.search_tracks.iter().all(|t| t.source == Source::Spotify));
        assert!(app.status_message.contains("[Spotify only]"), "status: {}", app.status_message);
        assert!(app.search_notes.iter().all(|n| !n.contains("YT Music")), "notes: {:?}", app.search_notes);

        // YT Music only: the Spotify API must NEVER be invoked.
        reset(&app);
        app.search_platform = SearchPlatform::YtMusic;
        app.run_search();
        assert_eq!(tagged(&spot), 0, "Spotify search must not be called");
        assert!(tagged(&inner) > 0, "Innertube search must be hit");
        assert!(app.status_message.contains("[YT Music only]"), "status: {}", app.status_message);
        assert!(app.search_notes.iter().all(|n| !n.starts_with("Spotify")), "notes: {:?}", app.search_notes);

        // All: both sides are queried again.
        reset(&app);
        app.search_platform = SearchPlatform::All;
        app.run_search();
        assert!(tagged(&spot) > 0);
        assert!(tagged(&inner) > 0);

        for (k, v) in saved {
            match v {
                Some(val) => std::env::set_var(k, val),
                None => std::env::remove_var(k),
            }
        }
        let _ = std::fs::remove_dir_all(&fakebin);
    }

    #[test]
    fn same_search_twice_hits_the_session_cache() {
        // v0.1.7 test (e): the same (query, kind, platform) search
        // runs ONCE per session; re-running it - e.g. toggling the
        // platform filter away and back - restores the cached
        // result with no new API request on either source.
        let _env_guard = crate::diag::env_lock();
        let spot = start_stub_server("spotify");
        let inner = start_stub_server("innertube");
        // A fake yt-dlp that SUCCEEDS with no output (a failing
        // one would make the search partial, hence uncacheable).
        let fakebin = std::env::temp_dir().join(format!("lyre-fakebin-cache-{}", std::process::id()));
        std::fs::create_dir_all(&fakebin).unwrap();
        let script = fakebin.join("yt-dlp");
        std::fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let saved: Vec<(&str, Option<String>)> = [
            "LYRE_SPOTIFY_TOKEN_URL",
            "LYRE_SPOTIFY_API_BASE",
            "LYRE_INNERTUBE_BASE",
            "PATH",
        ]
        .iter()
        .map(|k| (*k, std::env::var(k).ok()))
        .collect();
        std::env::set_var("LYRE_SPOTIFY_TOKEN_URL", format!("{}/get_access_token?reason=transport", spot.base));
        std::env::set_var("LYRE_SPOTIFY_API_BASE", spot.base.clone());
        std::env::set_var("LYRE_INNERTUBE_BASE", format!("{}/youtubei/v1", inner.base));
        let old_path = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{}", fakebin.display(), old_path));

        let mut app = App::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while app.trending_rx.is_some() && std::time::Instant::now() < deadline {
            app.poll_background();
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        spot.search_hits.store(0, std::sync::atomic::Ordering::SeqCst);
        inner.search_hits.store(0, std::sync::atomic::Ordering::SeqCst);
        let tagged = |s: &StubHttpServer| s.search_hits.load(std::sync::atomic::Ordering::SeqCst);

        app.search_query = "stub".to_string();
        app.search_platform = SearchPlatform::All;
        app.run_search();
        assert_eq!(tagged(&spot), 1, "All search = ONE combined Spotify request, got {}", tagged(&spot));
        let inner_first = tagged(&inner);
        assert!(inner_first > 0, "Innertube searched too");
        // Identical re-run: fully cached, zero new requests.
        app.run_search();
        assert_eq!(tagged(&spot), 1, "second run must not re-call Spotify");
        assert_eq!(tagged(&inner), inner_first, "second run must not re-call Innertube");
        assert!(app.status_message.contains("(cached)"), "status: {}", app.status_message);
        // Toggle away (a different cache key -> one new fetch) and
        // back (the original key -> cache hit, no new fetch).
        app.search_platform = SearchPlatform::Spotify;
        app.run_search();
        assert_eq!(tagged(&spot), 2, "the new platform key fetches once");
        app.search_platform = SearchPlatform::All;
        app.run_search();
        assert_eq!(tagged(&spot), 2, "toggling back is a cache hit");
        assert_eq!(tagged(&inner), inner_first, "toggling back does not re-call Innertube");

        for (k, v) in saved {
            match v {
                Some(val) => std::env::set_var(k, val),
                None => std::env::remove_var(k),
            }
        }
        let _ = std::fs::remove_dir_all(&fakebin);
    }

    #[test]
    fn autoplay_off_means_no_fetches_and_queue_untouched() {
        let mut app = autoplay_app();
        app.settings.autoplay = false;
        app.maybe_seed_autoplay();
        assert!(app.radio_for.is_none(), "OFF must never start a fetch");
        assert!(app.radio_rx.is_none());
        // Toggling OFF with picks pending drops the pending picks
        // after the current track.
        let mut app2 = autoplay_app();
        let key = TrackKey::of(&app2.engine.queue[0].clone());
        app2.apply_radio_result(key, Ok(radio_fixture()));
        assert_eq!(app2.engine.queue.len(), 5);
        app2.toggle_autoplay();
        assert!(!app2.settings.autoplay);
        assert_eq!(app2.engine.queue.len(), 2);
        assert!(app2.autoplay_from.is_none());
        // A radio result arriving while OFF appends nothing.
        let key3 = TrackKey::of(&app2.engine.queue[0].clone());
        app2.apply_radio_result(key3, Ok(radio_fixture()));
        assert_eq!(app2.engine.queue.len(), 2);
    }
}
