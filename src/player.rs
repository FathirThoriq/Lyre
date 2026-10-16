use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::backend::missing_binary_hint;
use crate::config::Settings;
use crate::track::{Source, Track};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RepeatMode {
    Off,
    All,
    One,
}

impl RepeatMode {
    pub fn label(&self) -> &'static str {
        match self {
            RepeatMode::Off => "Off",
            RepeatMode::All => "All",
            RepeatMode::One => "One",
        }
    }
    pub fn next(&self) -> RepeatMode {
        match self {
            RepeatMode::Off => RepeatMode::All,
            RepeatMode::All => RepeatMode::One,
            RepeatMode::One => RepeatMode::Off,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlayOutcome {
    /// A real stream was handed to mpv.
    Playing(Track),
    /// Track was an ad and Clean Playback skipped it.
    SkippedAd(Track),
    /// Backend could not resolve a stream (message explains why).
    Unresolvable(Track, String),
    EndOfQueue,
}

/// Clean Playback hook: decide whether a track must be skipped as an ad.
pub fn should_skip_as_ad(track: &Track, settings: &Settings) -> bool {
    track.is_ad() && track.source == Source::Spotify && settings.spotify_clean_active()
}

/// Snapshot of what mpv itself reports over its IPC socket. The
/// reader thread owns the socket and keeps this current; the player
/// engine reads it on every UI tick. Position/duration/paused ALL come
/// from mpv - there is no local free-running timer anywhere (that
/// timer was the v0.1.3 bug that made lyrics run ahead of the audio:
/// it started counting at resolve time, ignoring yt-dlp resolution +
/// mpv startup/buffering).
#[derive(Debug, Clone, Default)]
pub struct IpcSnapshot {
    pub connected: bool,
    pub time_pos: Option<f64>,
    pub duration: Option<f64>,
    pub pause: bool,
    pub eof: bool,
}

// Observe-property ids used on the IPC channel.
const OBS_TIME_POS: u64 = 1;
const OBS_DURATION: u64 = 2;
const OBS_PAUSE: u64 = 3;
const OBS_EOF: u64 = 4;

/// mpv IPC client (JSON lines over mpv's unix socket). Commands are
/// fire-and-forget writes; a reader thread folds every
/// `property-change` event into the shared snapshot. Unix only - on
/// other platforms the player runs without IPC (position display
/// falls back to the track metadata; seek reports unavailable),
/// matching this project's honest Windows status.
pub struct MpvIpc {
    state: Arc<Mutex<IpcSnapshot>>,
    #[cfg(unix)]
    writer: Arc<Mutex<Option<std::os::unix::net::UnixStream>>>,
    alive: Arc<AtomicBool>,
    next_request: Arc<AtomicU64>,
}

impl MpvIpc {
    /// Connect to the mpv IPC socket at `path`, retrying briefly
    /// while mpv creates it, then observe the properties the player
    /// needs. Spawns the reader thread; the returned client is cheap
    /// to query from the UI loop.
    #[cfg(unix)]
    pub fn connect(path: &std::path::Path) -> MpvIpc {
        use std::os::unix::net::UnixStream;
        let state = Arc::new(Mutex::new(IpcSnapshot::default()));
        let writer: Arc<Mutex<Option<UnixStream>>> = Arc::new(Mutex::new(None));
        let alive = Arc::new(AtomicBool::new(true));
        let next_request = Arc::new(AtomicU64::new(100));
        let ipc = MpvIpc {
            state: state.clone(),
            writer: writer.clone(),
            alive: alive.clone(),
            next_request: next_request.clone(),
        };
        let path = path.to_path_buf();
        std::thread::spawn(move || {
            // mpv creates the socket during startup; retry for ~5s.
            let mut stream: Option<UnixStream> = None;
            for _ in 0..50 {
                if !alive.load(Ordering::Relaxed) {
                    return;
                }
                match UnixStream::connect(&path) {
                    Ok(s) => {
                        stream = Some(s);
                        break;
                    }
                    Err(_) => std::thread::sleep(std::time::Duration::from_millis(100)),
                }
            }
            let stream = match stream {
                Some(s) => s,
                None => return,
            };
            let read_half = match stream.try_clone() {
                Ok(s) => s,
                Err(_) => return,
            };
            {
                let mut w = writer.lock().unwrap();
                *w = Some(stream);
            }
            {
                let mut st = state.lock().unwrap();
                st.connected = true;
            }
            // Observe the properties the engine mirrors.
            let send = |cmd: Value| {
                if let Ok(mut w) = writer.lock() {
                    if let Some(s) = w.as_mut() {
                        let _ = writeln!(s, "{}", cmd.to_string());
                    }
                }
            };
            send(json!({"command": ["observe_property", OBS_TIME_POS, "time-pos"]}));
            send(json!({"command": ["observe_property", OBS_DURATION, "duration"]}));
            send(json!({"command": ["observe_property", OBS_PAUSE, "pause"]}));
            send(json!({"command": ["observe_property", OBS_EOF, "eof-reached"]}));
            let mut reader = BufReader::new(read_half);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if let Ok(v) = serde_json::from_str::<Value>(&line) {
                            apply_ipc_message(&state, &v);
                        }
                    }
                }
            }
            let mut st = state.lock().unwrap();
            st.connected = false;
        });
        ipc
    }

    #[cfg(not(unix))]
    pub fn connect(_path: &std::path::Path) -> MpvIpc {
        MpvIpc {
            state: Arc::new(Mutex::new(IpcSnapshot::default())),
            alive: Arc::new(AtomicBool::new(true)),
            next_request: Arc::new(AtomicU64::new(100)),
        }
    }

    pub fn snapshot(&self) -> IpcSnapshot {
        self.state.lock().unwrap().clone()
    }

    pub fn is_connected(&self) -> bool {
        self.state.lock().unwrap().connected
    }

    /// Build the exact JSON line for one IPC command. Pure - the
    /// fake-mpv tests assert on this byte shape.
    pub fn command_json(&self, args: &[Value]) -> String {
        let id = self.next_request.fetch_add(1, Ordering::Relaxed);
        json!({"command": args, "request_id": id}).to_string()
    }

    /// Send one command. Returns false when no IPC connection exists
    /// (caller decides the fallback); never panics.
    pub fn send_command(&self, args: &[Value]) -> bool {
        #[cfg(unix)]
        {
            let line = self.command_json(args);
            if let Ok(mut w) = self.writer.lock() {
                if let Some(s) = w.as_mut() {
                    return writeln!(s, "{}", line).is_ok();
                }
            }
            false
        }
        #[cfg(not(unix))]
        {
            let _ = args;
            false
        }
    }

    /// ["seek", secs, "relative"] - the Bug-4 fix: the seek goes to
    /// mpv itself, not into a local display variable.
    pub fn seek_relative(&self, secs: f64) -> bool {
        self.send_command(&[json!("seek"), json!(secs), json!("relative")])
    }

    pub fn seek_absolute(&self, secs: f64) -> bool {
        self.send_command(&[json!("seek"), json!(secs), json!("absolute")])
    }

    pub fn set_paused(&self, paused: bool) -> bool {
        self.send_command(&[json!("set_property"), json!("pause"), json!(paused)])
    }

    pub fn set_af_filter(&self, filter: &str) -> bool {
        self.send_command(&[json!("set_property"), json!("af"), json!(filter)])
    }

    pub fn set_volume(&self, volume: u8) -> bool {
        self.send_command(&[json!("set_property"), json!("volume"), json!(volume)])
    }

    /// Disconnect: closing the writer ends the reader thread.
    pub fn close(&self) {
        self.alive.store(false, Ordering::Relaxed);
        #[cfg(unix)]
        {
            if let Ok(mut w) = self.writer.lock() {
                if let Some(s) = w.take() {
                    let _ = s.shutdown(std::net::Shutdown::Both);
                }
            }
        }
        let mut st = self.state.lock().unwrap();
        st.connected = false;
    }
}

/// Fold one IPC message into the snapshot. Property-change events
/// carry the values; an `end-file` event also marks EOF (mpv exits
/// right after, but the flag makes the transition explicit).
fn apply_ipc_message(state: &Arc<Mutex<IpcSnapshot>>, v: &Value) {
    let mut st = state.lock().unwrap();
    if v.get("event").and_then(|e| e.as_str()) == Some("end-file") {
        st.eof = true;
        return;
    }
    if v.get("event").and_then(|e| e.as_str()) != Some("property-change") {
        return;
    }
    let data = v.get("data");
    match v.get("id").and_then(|i| i.as_u64()) {
        Some(OBS_TIME_POS) => {
            if let Some(p) = data.and_then(|d| d.as_f64()) {
                st.time_pos = Some(p);
                st.eof = false;
            }
        }
        Some(OBS_DURATION) => {
            if let Some(d) = data.and_then(|d| d.as_f64()) {
                st.duration = Some(d);
            }
        }
        Some(OBS_PAUSE) => {
            if let Some(p) = data.and_then(|d| d.as_bool()) {
                st.pause = p;
            }
        }
        Some(OBS_EOF) => {
            if let Some(e) = data.and_then(|d| d.as_bool()) {
                st.eof = e;
            }
        }
        _ => {}
    }
}

static SOCKET_COUNTER: AtomicU64 = AtomicU64::new(0);

fn ipc_socket_path() -> std::path::PathBuf {
    let n = SOCKET_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("lyre-mpv-{}-{}.sock", std::process::id(), n))
}

/// mpv subprocess wrapper. All arguments are passed as an arg array,
/// never through a shell, so it works on Linux and Windows alike.
/// mpv is always launched with an IPC socket (Unix) so position,
/// pause, seek and EQ changes are real mpv state.
pub struct MpvPlayer {
    child: Option<Child>,
    pub ipc: Option<MpvIpc>,
    socket_path: Option<std::path::PathBuf>,
}

impl MpvPlayer {
    pub fn new() -> Self {
        MpvPlayer { child: None, ipc: None, socket_path: None }
    }

    pub fn play(&mut self, stream_url: &str, eq_filter: &str, volume: u8, start_secs: f64) -> Result<(), String> {
        #[cfg(test)]
        {
            // Test builds may fake mpv so playback logic (including the
            // Spotify -> YT Music match path) is exercised without the
            // real binary. Production builds never see this branch.
            if std::env::var("LYRE_TEST_FAKE_MPV").is_ok() {
                let _ = (stream_url, eq_filter, volume, start_secs);
                self.child = None;
                return Ok(());
            }
        }
        self.stop();
        if !crate::backend::binary_available("mpv") {
            return Err(missing_binary_hint("mpv"));
        }
        let mut cmd = Command::new("mpv");
        cmd.arg("--no-video")
            .arg(format!("--volume={}", volume))
            // Keep mpv alive after the file ends so the end-of-track
            // EOF state stays observable over IPC (v0.1.6 auto-advance
            // fix: without --idle, mpv exits in the same millisecond
            // as end-file and the EOF flag raced with the socket
            // teardown, so the UI poll never saw it).
            .arg("--idle=yes")
            .arg("--really-quiet");
        #[cfg(unix)]
        {
            let sock = ipc_socket_path();
            let _ = std::fs::remove_file(&sock);
            cmd.arg(format!("--input-ipc-server={}", sock.display()));
            self.socket_path = Some(sock);
        }
        if start_secs > 1.0 {
            cmd.arg(format!("--start={:.0}", start_secs));
        }
        if !eq_filter.is_empty() {
            cmd.arg(format!("--af={}", eq_filter));
        }
        cmd.arg(stream_url);
        cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        match cmd.spawn() {
            Ok(child) => {
                self.child = Some(child);
                #[cfg(unix)]
                {
                    if let Some(sock) = &self.socket_path {
                        self.ipc = Some(MpvIpc::connect(sock));
                    }
                }
                Ok(())
            }
            Err(e) => Err(format!("Failed to start mpv: {}", e)),
        }
    }

    pub fn stop(&mut self) {
        if let Some(ipc) = self.ipc.take() {
            ipc.close();
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(sock) = self.socket_path.take() {
            let _ = std::fs::remove_file(sock);
        }
    }

    /// True while an mpv child is running; reaps it when it ended.
    pub fn is_running(&mut self) -> bool {
        if let Some(child) = &mut self.child {
            match child.try_wait() {
                Ok(Some(_)) => {
                    self.child = None;
                    false
                }
                Ok(None) => true,
                Err(_) => {
                    self.child = None;
                    false
                }
            }
        } else {
            false
        }
    }
}

impl Drop for MpvPlayer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// What one IPC poll found (see PlayerEngine::poll_ipc).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum IpcPoll {
    /// Nothing newsworthy.
    None,
    /// mpv reports the track ended (eof-reached / end-file).
    Ended,
}

pub struct PlayerEngine {
    pub queue: Vec<Track>,
    pub current_index: Option<usize>,
    pub is_playing: bool,
    /// DISPLAY position in seconds. Since v0.1.4 this is fed by mpv
    /// itself through the IPC snapshot (poll_ipc); nothing advances
    /// it locally. It resets to 0 when a track starts and stays 0
    /// until mpv reports its real position - resolve and startup
    /// time can no longer make lyrics/progress run ahead of the
    /// audio (Bug 2).
    pub position_secs: f64,
    /// Duration as reported by mpv (preferred over track metadata,
    /// which can be 0 or approximate for search results).
    pub mpv_duration: Option<f64>,
    /// True between a successful mpv start and end-of-queue/stop:
    /// lets the tick distinguish "mpv child died mid-track" from
    /// "nothing was ever started".
    pub mpv_active: bool,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    pub mpv: MpvPlayer,
    pub last_backend_source: Option<Source>,
    pub backend_switches: u32,
    /// Honest audio provenance of the currently playing track, e.g.
    /// "Spotify - audio via YT Music". Set on every successful play.
    pub now_playing_provenance: Option<String>,
    /// Last EQ filter / volume actually pushed over IPC, so the UI
    /// tick only re-sends them when they change (live EQ/volume).
    ipc_filter_sent: Option<String>,
    ipc_volume_sent: Option<u8>,
    /// Latch: EOF has already been reported for the current track,
    /// so one track end produces exactly one auto-advance. Reset on
    /// every successful play_index.
    eof_reported: bool,
}

impl PlayerEngine {
    pub fn new() -> Self {
        PlayerEngine {
            queue: Vec::new(),
            current_index: None,
            is_playing: false,
            position_secs: 0.0,
            mpv_duration: None,
            mpv_active: false,
            shuffle: false,
            repeat: RepeatMode::Off,
            mpv: MpvPlayer::new(),
            last_backend_source: None,
            backend_switches: 0,
            now_playing_provenance: None,
            ipc_filter_sent: None,
            ipc_volume_sent: None,
            eof_reported: false,
        }
    }

    /// Duration to display: mpv's own report wins, track metadata is
    /// the fallback.
    pub fn display_duration(&self) -> f64 {
        match self.mpv_duration {
            Some(d) if d > 0.0 => d,
            _ => self.current_track().map(|t| t.duration_secs as f64).unwrap_or(0.0),
        }
    }

    /// Fold the latest mpv IPC snapshot into the engine. Called on
    /// every UI tick. Returns Ended when mpv says the track is over.
    ///
    /// v0.1.6 root-cause fix (queue did not auto-advance): the EOF
    /// flag is checked BEFORE the connection gate. With a real mpv,
    /// `eof-reached` never surfaces as boolean true (its final
    /// change is to null as the property goes away) and the
    /// `end-file` event that sets our EOF flag lands in the same
    /// millisecond as the socket teardown when mpv exits without
    /// --idle; the old order (`if !connected { return None }` first)
    /// threw the recorded EOF away before the advance logic ever
    /// saw it. The flag lives in the snapshot and survives the
    /// disconnect, so read it first. The eof_reported latch makes
    /// each track end report Ended exactly once.
    pub fn poll_ipc(&mut self) -> IpcPoll {
        let ipc = match &self.mpv.ipc {
            Some(i) => i,
            None => return IpcPoll::None,
        };
        let snap = ipc.snapshot();
        if snap.eof && !self.eof_reported {
            self.eof_reported = true;
            return IpcPoll::Ended;
        }
        if !snap.connected {
            return IpcPoll::None;
        }
        if let Some(tp) = snap.time_pos {
            self.position_secs = tp;
        }
        if let Some(d) = snap.duration {
            if d > 0.0 {
                self.mpv_duration = Some(d);
            }
        }
        self.is_playing = !snap.pause;
        IpcPoll::None
    }

    /// The single end-of-track decision used by the UI tick: true
    /// when the current track genuinely ended and playback should
    /// advance. Covers both the IPC EOF report and the fallback
    /// "mpv child died under an active track" (e.g. a stream error
    /// that killed mpv before any EOF could be observed).
    pub fn check_track_ended(&mut self) -> bool {
        if self.poll_ipc() == IpcPoll::Ended {
            return true;
        }
        if !self.mpv_active {
            return false;
        }
        let ipc_up = self
            .mpv
            .ipc
            .as_ref()
            .map(|i| i.is_connected())
            .unwrap_or(false);
        if !ipc_up && !self.mpv.is_running() {
            self.mpv_active = false;
            return true;
        }
        false
    }

    /// Push live setting changes (EQ filter, volume) to the running
    /// mpv over IPC - called on every UI tick; only sends on change.
    pub fn sync_live_settings(&mut self, settings: &Settings) {
        let ipc = match &self.mpv.ipc {
            Some(i) if i.is_connected() => i,
            _ => return,
        };
        let filter = settings.eq.to_mpv_filter();
        if self.ipc_filter_sent.as_deref() != Some(filter.as_str()) {
            if ipc.set_af_filter(&filter) {
                self.ipc_filter_sent = Some(filter);
            }
        }
        if self.ipc_volume_sent != Some(settings.volume) {
            if ipc.set_volume(settings.volume) {
                self.ipc_volume_sent = Some(settings.volume);
            }
        }
    }

    /// Real seek in mpv (Bug 4). False when no IPC connection exists.
    pub fn seek_relative(&mut self, secs: f64) -> bool {
        match &self.mpv.ipc {
            Some(ipc) => ipc.seek_relative(secs),
            None => false,
        }
    }

    pub fn seek_absolute(&mut self, secs: f64) -> bool {
        match &self.mpv.ipc {
            Some(ipc) => ipc.seek_absolute(secs),
            None => false,
        }
    }

    /// Pause/resume THROUGH mpv (the audio and the position stay in
    /// mpv; nothing restarts). False when no IPC connection exists.
    pub fn set_paused(&mut self, paused: bool) -> bool {
        match &self.mpv.ipc {
            Some(ipc) => {
                if ipc.set_paused(paused) {
                    self.is_playing = !paused;
                    return true;
                }
                false
            }
            None => false,
        }
    }

    pub fn current_track(&self) -> Option<&Track> {
        self.current_index.and_then(|i| self.queue.get(i))
    }

    /// The backend source that the engine is currently using. When the
    /// current track's source differs from the previous track's source the
    /// player auto-switches backends (counted in `backend_switches`).
    pub fn active_backend_source(&self) -> Option<Source> {
        self.current_track().map(|t| t.source)
    }

    pub fn enqueue(&mut self, track: Track) {
        self.queue.push(track);
        if self.current_index.is_none() {
            self.current_index = Some(0);
        }
    }

    /// Play the track at queue index `idx`, applying the ad-skip hook.
    /// YouTube Music tracks with a real video id resolve their own
    /// stream; Spotify tracks (any account type - Free and Premium
    /// behave identically, the account product is never consulted) and
    /// seeded starter tracks (placeholder ids) resolve through the
    /// Spotube model: a "Artist - Title" match on the YouTube Music
    /// backend. Returns what actually happened.
    pub fn play_index(
        &mut self,
        idx: usize,
        settings: &Settings,
        yt_backend: &dyn crate::backend::PlayerBackend,
        _spotify_backend: &dyn crate::backend::PlayerBackend,
    ) -> PlayOutcome {
        let track = match self.queue.get(idx) {
            Some(t) => t.clone(),
            None => return PlayOutcome::EndOfQueue,
        };
        if should_skip_as_ad(&track, settings) {
            self.current_index = Some(idx);
            return PlayOutcome::SkippedAd(track);
        }
        let resolved = match track.source {
            // Spotify metadata always plays via the YT Music match;
            // seeded tracks (either source) have placeholder ids, so
            // they take the same match path.
            Source::Spotify => yt_backend.resolve_match(&track.match_query(), settings),
            Source::YtMusic if track.demo => yt_backend.resolve_match(&track.match_query(), settings),
            Source::YtMusic => yt_backend.resolve_stream(&track, settings),
        };
        match resolved {
            Ok(url) => {
                let filter = settings.eq.to_mpv_filter();
                match self.mpv.play(&url, &filter, settings.volume, 0.0) {
                    Ok(()) => {
                        if self.last_backend_source != Some(track.source) {
                            if self.last_backend_source.is_some() {
                                self.backend_switches += 1;
                            }
                            self.last_backend_source = Some(track.source);
                        }
                        self.current_index = Some(idx);
                        self.position_secs = 0.0;
                        self.mpv_duration = None;
                        self.mpv_active = true;
                        self.eof_reported = false;
                        // Spawn args already carried these; the live
                        // sync only needs to send later CHANGES.
                        self.ipc_filter_sent = Some(filter);
                        self.ipc_volume_sent = Some(settings.volume);
                        self.is_playing = true;
                        self.now_playing_provenance = Some(track.audio_provenance());
                        PlayOutcome::Playing(track)
                    }
                    Err(e) => PlayOutcome::Unresolvable(track, e),
                }
            }
            Err(e) => {
                self.current_index = Some(idx);
                self.is_playing = false;
                PlayOutcome::Unresolvable(track, e)
            }
        }
    }

    /// Advance to the next playable track, skipping ads when Clean Playback
    /// asks for it. Returns the outcome of the track it landed on.
    pub fn next(
        &mut self,
        settings: &Settings,
        yt_backend: &dyn crate::backend::PlayerBackend,
        spotify_backend: &dyn crate::backend::PlayerBackend,
    ) -> PlayOutcome {
        if self.queue.is_empty() {
            return PlayOutcome::EndOfQueue;
        }
        if self.repeat == RepeatMode::One {
            if let Some(idx) = self.current_index {
                return self.play_index(idx, settings, yt_backend, spotify_backend);
            }
        }
        let mut idx = match self.current_index {
            Some(i) => i + 1,
            None => 0,
        };
        loop {
            if idx >= self.queue.len() {
                if self.repeat == RepeatMode::All {
                    idx = 0;
                } else {
                    self.is_playing = false;
                    self.mpv_active = false;
                    return PlayOutcome::EndOfQueue;
                }
            }
            let outcome = self.play_index(idx, settings, yt_backend, spotify_backend);
            match outcome {
                PlayOutcome::SkippedAd(_) => {
                    idx += 1;
                    // Guard against an all-ad queue looping forever.
                    if idx > self.queue.len() + 1 {
                        self.is_playing = false;
                        self.mpv_active = false;
                        return PlayOutcome::EndOfQueue;
                    }
                    continue;
                }
                other => return other,
            }
        }
    }

    pub fn prev_index(&self) -> Option<usize> {
        match self.current_index {
            Some(0) => None,
            Some(i) => Some(i - 1),
            None => None,
        }
    }

    #[allow(dead_code)]
    pub fn toggle_play_pause(&mut self, _settings: &Settings) {
        // Pause/resume goes through mpv IPC (the audio position lives
        // in mpv); App::toggle_play_pause is the live call site.
        if self.is_playing {
            let _ = self.set_paused(true);
        } else {
            let _ = self.set_paused(false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::SpotifyBackend;
    use crate::track::demo_tracks;

    fn ad_track() -> Track {
        let mut t = Track::new("ad-1", "Advertisement", "Spotify Ads", 15, Source::Spotify);
        t.track_type = "ad".to_string();
        t
    }

    #[test]
    fn ad_skip_hook_respects_clean_playback() {
        let settings = Settings::default();
        assert!(should_skip_as_ad(&ad_track(), &settings));
        let mut off = Settings::default();
        off.spotify_adblock = false;
        assert!(!should_skip_as_ad(&ad_track(), &off));
        let mut off2 = Settings::default();
        off2.clean_playback_master = false;
        assert!(!should_skip_as_ad(&ad_track(), &off2));
        // A normal track is never skipped.
        assert!(!should_skip_as_ad(&Track::new("x", "Song", "A", 1, Source::Spotify), &settings));
    }

    #[test]
    fn queue_auto_switch_source_logic() {
        let settings = Settings::default();
        let mock = MockMatchBackend::new();
        let spot = SpotifyBackend::new();
        let mut engine = PlayerEngine::new();
        // Seeded tracks play through the match backend; their source
        // still drives which backend the engine reports as active.
        for t in demo_tracks() {
            engine.enqueue(t);
        }
        let o1 = engine.play_index(0, &settings, &mock, &spot);
        assert!(matches!(o1, PlayOutcome::Playing(_)), "outcome: {:?}", o1);
        assert_eq!(engine.active_backend_source(), Some(Source::Spotify));
        let o2 = engine.play_index(1, &settings, &mock, &spot);
        assert!(matches!(o2, PlayOutcome::Playing(_)), "outcome: {:?}", o2);
        assert_eq!(engine.active_backend_source(), Some(Source::YtMusic));
        assert_eq!(engine.backend_switches, 1);
    }

    #[test]
    fn next_skips_ads_and_lands_on_real_track() {
        let settings = Settings::default();
        let mock = MockMatchBackend::new();
        let spot = SpotifyBackend::new();
        let mut engine = PlayerEngine::new();
        let real = Track::new("r1", "Real Song", "Artist", 100, Source::YtMusic);
        engine.enqueue(ad_track());
        engine.enqueue(real);
        engine.current_index = Some(0);
        // Starting next() from the ad: the ad itself is skipped by play, and
        // next() must land on the following track and actually play it.
        let outcome = engine.next(&settings, &mock, &spot);
        assert!(matches!(outcome, PlayOutcome::Playing(_)), "outcome: {:?}", outcome);
        assert_eq!(engine.current_index, Some(1));
    }

    #[test]
    fn seeded_tracks_play_via_match_query() {
        // v0.1.3: the seeded starter list is playable. Both the Spotify
        // seed and the YT Music seed resolve through the match query
        // built from their real artist/title metadata.
        let settings = Settings::default();
        let mock = MockMatchBackend::new();
        let spot = SpotifyBackend::new();
        let mut engine = PlayerEngine::new();
        for t in demo_tracks() {
            engine.enqueue(t);
        }
        let o = engine.play_index(0, &settings, &mock, &spot);
        assert!(matches!(o, PlayOutcome::Playing(_)), "outcome: {:?}", o);
        let o = engine.play_index(1, &settings, &mock, &spot);
        assert!(matches!(o, PlayOutcome::Playing(_)), "outcome: {:?}", o);
        assert_eq!(
            mock.queries.borrow().clone(),
            vec!["Tulus - Monokrom".to_string(), "Harry Styles - As It Was".to_string()]
        );
    }

    /// Mock YouTube Music backend: records match queries and returns a
    /// fake stream URL, so the Spotify -> YT Music match path can be
    /// tested without yt-dlp/network. mpv is faked via LYRE_TEST_FAKE_MPV.
    struct MockMatchBackend {
        queries: std::cell::RefCell<Vec<String>>,
    }

    impl MockMatchBackend {
        fn new() -> Self {
            std::env::set_var("LYRE_TEST_FAKE_MPV", "1");
            MockMatchBackend { queries: std::cell::RefCell::new(Vec::new()) }
        }
    }

    impl crate::backend::PlayerBackend for MockMatchBackend {
        fn source(&self) -> Source {
            Source::YtMusic
        }
        fn search(&self, _query: &str) -> Result<Vec<Track>, String> {
            Ok(vec![])
        }
        fn resolve_stream(&self, track: &Track, _settings: &Settings) -> Result<String, String> {
            Ok(format!("https://stream.example/ytm/{}", track.id))
        }
        fn resolve_match(&self, query: &str, _settings: &Settings) -> Result<String, String> {
            self.queries.borrow_mut().push(query.to_string());
            Ok("https://stream.example/match".to_string())
        }
    }

    #[test]
    fn match_query_built_from_artist_and_title() {
        let t = Track::new("s1", "Monokrom", "Tulus", 234, Source::Spotify);
        assert_eq!(t.match_query(), "Tulus - Monokrom");
        assert_eq!(t.audio_provenance(), "Spotify - audio via YT Music");
    }

    #[test]
    fn free_spotify_track_plays_via_yt_match() {
        // A Spotify track from a Free account: Lyre stores no product
        // type and never consults one - the track simply plays through
        // the YouTube Music match path.
        let settings = Settings::default();
        let mock = MockMatchBackend::new();
        let spot = SpotifyBackend::new();
        let mut engine = PlayerEngine::new();
        engine.enqueue(Track::new("s1", "Monokrom", "Tulus", 234, Source::Spotify));
        let outcome = engine.play_index(0, &settings, &mock, &spot);
        assert!(matches!(outcome, PlayOutcome::Playing(_)), "outcome: {:?}", outcome);
        assert!(engine.is_playing);
        assert_eq!(mock.queries.borrow().clone(), vec!["Tulus - Monokrom".to_string()]);
        assert_eq!(engine.now_playing_provenance.as_deref(), Some("Spotify - audio via YT Music"));
    }

    #[test]
    fn spotify_playback_never_gated_by_account() {
        // v0.1.5: there is no account/token concept left at all -
        // the plain guest backend plays Spotify tracks through the
        // YT match path, identically, every time.
        let settings = Settings::default();
        for _ in 0..2 {
            let mock = MockMatchBackend::new();
            let spot = SpotifyBackend::new();
            let mut engine = PlayerEngine::new();
            engine.enqueue(Track::new("s1", "Song", "Artist", 100, Source::Spotify));
            let outcome = engine.play_index(0, &settings, &mock, &spot);
            assert!(matches!(outcome, PlayOutcome::Playing(_)), "outcome: {:?}", outcome);
            assert!(engine.is_playing);
        }
    }

    #[test]
    fn guest_mode_ytm_track_plays_without_any_account() {
        // Mode Tamu: a YouTube Music track plays with zero accounts
        // connected anywhere - no Google login gates search/playback.
        let settings = Settings::default();
        let mock = MockMatchBackend::new();
        let spot = SpotifyBackend::new();
        let mut engine = PlayerEngine::new();
        engine.enqueue(Track::new("ytm1", "As It Was", "Harry Styles", 167, Source::YtMusic));
        let outcome = engine.play_index(0, &settings, &mock, &spot);
        assert!(matches!(outcome, PlayOutcome::Playing(_)), "outcome: {:?}", outcome);
        assert!(engine.is_playing);
        assert_eq!(engine.now_playing_provenance.as_deref(), Some("YouTube Music"));
    }
}

// ---------------------------------------------------------------------------
// mpv IPC tests (Bugs 2 & 4): a fake mpv serving the real IPC
// protocol over a real unix socket. The client under test cannot
// tell it apart from mpv - commands are parsed as JSON lines and
// property-change events flow back, exactly like the real thing.
// ---------------------------------------------------------------------------

#[cfg(all(test, unix))]
mod ipc_tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    struct FakeMpv {
        path: PathBuf,
        commands: Arc<Mutex<Vec<Value>>>,
        pos: Arc<Mutex<f64>>,
    }

    fn start_fake_mpv() -> FakeMpv {
        let path = std::env::temp_dir().join(format!(
            "lyre-fake-mpv-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let listener = UnixListener::bind(&path).unwrap();
        let commands = Arc::new(Mutex::new(Vec::new()));
        let pos = Arc::new(Mutex::new(0.0f64));
        let (cmds2, pos2) = (commands.clone(), pos.clone());
        std::thread::spawn(move || {
            let (stream, _) = match listener.accept() {
                Ok(x) => x,
                Err(_) => return,
            };
            let read_half = stream.try_clone().unwrap();
            let write = Arc::new(Mutex::new(stream));
            // Playback ticker: advance the virtual position and push
            // time-pos property changes, like a playing mpv.
            {
                let (w2, p2) = (write.clone(), pos2.clone());
                std::thread::spawn(move || loop {
                    std::thread::sleep(Duration::from_millis(50));
                    let v = {
                        let mut p = p2.lock().unwrap();
                        *p += 0.05;
                        *p
                    };
                    let msg = json!({"event": "property-change", "id": 1, "name": "time-pos", "data": v});
                    let mut w = w2.lock().unwrap();
                    if writeln!(w, "{}", msg).is_err() {
                        break;
                    }
                });
            }
            let mut reader = BufReader::new(read_half);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                let v: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                cmds2.lock().unwrap().push(v.clone());
                let cmd = v.get("command").and_then(|c| c.as_array()).cloned().unwrap_or_default();
                match cmd.first().and_then(|c| c.as_str()) {
                    Some("observe_property") => {
                        let id = cmd.get(1).and_then(|i| i.as_u64()).unwrap_or(0);
                        let (name, data) = match id {
                            2 => ("duration", json!(300.0)),
                            3 => ("pause", json!(false)),
                            4 => ("eof-reached", json!(false)),
                            _ => ("time-pos", json!(*pos2.lock().unwrap())),
                        };
                        let msg = json!({"event": "property-change", "id": id, "name": name, "data": data});
                        let mut w = write.lock().unwrap();
                        let _ = writeln!(w, "{}", msg);
                    }
                    Some("seek") => {
                        let delta = cmd.get(1).and_then(|x| x.as_f64()).unwrap_or(0.0);
                        let mode = cmd.get(2).and_then(|x| x.as_str()).unwrap_or("");
                        let nv = {
                            let mut p = pos2.lock().unwrap();
                            if mode == "relative" {
                                *p += delta;
                            } else {
                                *p = delta;
                            }
                            *p
                        };
                        let msg = json!({"event": "property-change", "id": 1, "name": "time-pos", "data": nv});
                        let mut w = write.lock().unwrap();
                        let _ = writeln!(w, "{}", msg);
                    }
                    Some("set_property") => {
                        if cmd.get(1).and_then(|x| x.as_str()) == Some("pause") {
                            let paused = cmd.get(2).and_then(|x| x.as_bool()).unwrap_or(false);
                            let msg = json!({"event": "property-change", "id": 3, "name": "pause", "data": paused});
                            let mut w = write.lock().unwrap();
                            let _ = writeln!(w, "{}", msg);
                        }
                    }
                    _ => {}
                }
            }
        });
        FakeMpv { path, commands, pos }
    }

    fn wait_until<F: FnMut() -> bool>(what: &str, timeout: Duration, mut f: F) {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if f() {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("timed out waiting for {}", what);
    }

    #[test]
    fn seek_command_json_shape_is_exact() {
        // Pure builder check: the seek that leaves for mpv is exactly
        // ["seek", <secs>, "relative"] on the command channel.
        let ipc = MpvIpc::connect(std::path::Path::new("/nonexistent/lyre-test.sock"));
        let line = ipc.command_json(&[json!("seek"), json!(5.0), json!("relative")]);
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["command"], json!(["seek", 5.0, "relative"]));
        assert!(v["request_id"].as_u64().is_some());
        let line2 = ipc.command_json(&[json!("set_property"), json!("pause"), json!(true)]);
        let v2: Value = serde_json::from_str(&line2).unwrap();
        assert_eq!(v2["command"], json!(["set_property", "pause", true]));
        ipc.close();
    }

    #[test]
    fn ipc_seek_reaches_mpv_and_position_follows() {
        let fake = start_fake_mpv();
        let ipc = MpvIpc::connect(&fake.path);
        wait_until("ipc connect", Duration::from_secs(3), || ipc.is_connected());
        wait_until("duration observed", Duration::from_secs(3), || {
            ipc.snapshot().duration == Some(300.0)
        });
        // The Bug-4 assertion: the exact seek JSON arrives at mpv...
        assert!(ipc.seek_relative(5.0));
        wait_until("seek command recorded", Duration::from_secs(3), || {
            fake.commands
                .lock()
                .unwrap()
                .iter()
                .any(|c| c["command"] == json!(["seek", 5.0, "relative"]))
        });
        // ...and the displayed position follows mpv's own report
        // afterwards (the seek jumped the fake's playhead by +5s).
        let before = *fake.pos.lock().unwrap();
        assert!(before >= 5.0, "fake playhead after seek: {}", before);
        wait_until("client position mirrors mpv", Duration::from_secs(3), || {
            ipc.snapshot().time_pos.map(|p| p >= 5.0).unwrap_or(false)
        });
        ipc.close();
        let _ = std::fs::remove_file(&fake.path);
    }

    #[test]
    fn engine_position_and_duration_come_from_ipc() {
        let fake = start_fake_mpv();
        let ipc = MpvIpc::connect(&fake.path);
        let mut engine = PlayerEngine::new();
        engine.mpv.ipc = Some(ipc);
        engine.mpv_active = true;
        // Poll like the UI tick does until the snapshot lands.
        wait_until("engine mirrors ipc", Duration::from_secs(3), || {
            engine.poll_ipc();
            engine.mpv_duration == Some(300.0) && engine.position_secs > 0.0
        });
        assert_eq!(engine.display_duration(), 300.0);
        assert!(engine.is_playing, "pause=false must read as playing");
        // Pause via IPC: the engine's state follows mpv's report.
        assert!(engine.set_paused(true));
        wait_until("engine sees pause", Duration::from_secs(3), || {
            engine.poll_ipc();
            !engine.is_playing
        });
        let _ = std::fs::remove_file(&fake.path);
    }

    #[test]
    fn ipc_message_folding_handles_all_properties() {
        let state = Arc::new(Mutex::new(IpcSnapshot::default()));
        apply_ipc_message(&state, &json!({"event": "property-change", "id": 1, "name": "time-pos", "data": 12.5}));
        apply_ipc_message(&state, &json!({"event": "property-change", "id": 2, "name": "duration", "data": 234.0}));
        apply_ipc_message(&state, &json!({"event": "property-change", "id": 3, "name": "pause", "data": true}));
        {
            let s = state.lock().unwrap();
            assert_eq!(s.time_pos, Some(12.5));
            assert_eq!(s.duration, Some(234.0));
            assert!(s.pause);
            assert!(!s.eof);
        }
        // Null data (property unavailable) must not clear the value.
        apply_ipc_message(&state, &json!({"event": "property-change", "id": 1, "name": "time-pos", "data": null}));
        assert_eq!(state.lock().unwrap().time_pos, Some(12.5));
        // end-file marks EOF; unknown messages are ignored.
        apply_ipc_message(&state, &json!({"event": "end-file", "reason": "eof"}));
        assert!(state.lock().unwrap().eof);
        apply_ipc_message(&state, &json!({"event": "property-change", "id": 99, "data": 1}));
        apply_ipc_message(&state, &json!({"garbage": true}));
    }

    /// A fake mpv that plays briefly, then reports the end of the
    /// track exactly the way real mpv does: an `eof-reached=true`
    /// property change and an `end-file` event, immediately followed
    /// by the socket closing (mpv exits - the v0.1.5 spawn had no
    /// --idle, so EOF and the disconnect were the same millisecond).
    fn start_fake_mpv_eof() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "lyre-fake-mpv-eof-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let listener = UnixListener::bind(&path).unwrap();
        let ret = path.clone();
        std::thread::spawn(move || {
            let (stream, _) = match listener.accept() {
                Ok(x) => x,
                Err(_) => return,
            };
            let read_half = stream.try_clone().unwrap();
            let write = Arc::new(Mutex::new(stream));
            // After 400ms: report EOF and hang up, like real mpv.
            {
                let w2 = write.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(400));
                    {
                        let mut w = w2.lock().unwrap();
                        let _ = writeln!(w, "{}", json!({"event": "property-change", "id": 4, "name": "eof-reached", "data": true}));
                        let _ = writeln!(w, "{}", json!({"event": "end-file", "reason": "eof"}));
                    }
                    std::thread::sleep(Duration::from_millis(50));
                    // Shut the socket down: from the client's side
                    // the connection is now dead, which is what hid
                    // the EOF flag before v0.1.6.
                    let _ = w2.lock().unwrap().shutdown(std::net::Shutdown::Both);
                });
            }
            let mut reader = BufReader::new(read_half);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                let v: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let cmd = v.get("command").and_then(|c| c.as_array()).cloned().unwrap_or_default();
                if cmd.first().and_then(|c| c.as_str()) == Some("observe_property") {
                    let id = cmd.get(1).and_then(|i| i.as_u64()).unwrap_or(0);
                    let (name, data) = match id {
                        2 => ("duration", json!(2.0)),
                        3 => ("pause", json!(false)),
                        4 => ("eof-reached", json!(false)),
                        _ => ("time-pos", json!(0.5)),
                    };
                    let msg = json!({"event": "property-change", "id": id, "name": name, "data": data});
                    let mut w = write.lock().unwrap();
                    if writeln!(w, "{}", msg).is_err() {
                        break;
                    }
                }
            }
        });
        ret
    }

    /// Backend stub that "resolves" every track to a dummy URL.
    struct StubBackend;
    impl crate::backend::PlayerBackend for StubBackend {
        fn source(&self) -> Source {
            Source::YtMusic
        }
        fn search(&self, _query: &str) -> Result<Vec<Track>, String> {
            Ok(vec![])
        }
        fn resolve_stream(&self, track: &Track, _settings: &Settings) -> Result<String, String> {
            Ok(format!("file:///tmp/lyre-stub-{}.wav", track.id))
        }
        fn resolve_match(&self, _query: &str, _settings: &Settings) -> Result<String, String> {
            Ok("file:///tmp/lyre-stub-match.wav".to_string())
        }
    }

    #[test]
    fn eof_event_advances_engine_without_any_keypress() {
        // Regression test for the v0.1.6 auto-advance defect: the EOF
        // report must be seen by the tick-side decision even though
        // the IPC connection dies in the same instant (pre-fix,
        // poll_ipc discarded the flag behind the connection gate and
        // the queue only moved when the user pressed `n`).
        std::env::set_var("LYRE_TEST_FAKE_MPV", "1");
        let settings = Settings::default();
        let backend = StubBackend;
        let mut engine = PlayerEngine::new();
        engine.enqueue(Track::new("eof-a", "First", "Artist", 2, Source::YtMusic));
        engine.enqueue(Track::new("eof-b", "Second", "Artist", 2, Source::YtMusic));
        let outcome = engine.play_index(0, &settings, &backend, &backend);
        assert!(matches!(outcome, PlayOutcome::Playing(_)), "outcome: {:?}", outcome);
        // Attach the EOF fake as the live IPC (play_index under the
        // fake-mpv env var installs none).
        let path = start_fake_mpv_eof();
        engine.mpv.ipc = Some(MpvIpc::connect(&path));
        // NO key calls from here on: the tick decision alone must
        // notice the end of the track.
        let mut ended = false;
        wait_until("track end detected via tick decision", Duration::from_secs(5), || {
            ended = engine.check_track_ended();
            ended
        });
        assert!(ended);
        // The App-side reaction to `ended` is engine.next():
        let outcome = engine.next(&settings, &backend, &backend);
        assert!(matches!(outcome, PlayOutcome::Playing(_)), "outcome: {:?}", outcome);
        assert_eq!(engine.current_index, Some(1));
        assert_eq!(engine.current_track().map(|t| t.id.as_str()), Some("eof-b"));
        // A fresh mpv/IPC replaces the dead one on a real play;
        // attach a live (still playing) fake like the respawn would
        // provide, and assert no phantom second advance fires.
        if let Some(ipc) = engine.mpv.ipc.take() {
            ipc.close();
        }
        let live = start_fake_mpv();
        engine.mpv.ipc = Some(MpvIpc::connect(&live.path));
        wait_until("fresh ipc connected", Duration::from_secs(3), || {
            engine.mpv.ipc.as_ref().map(|i| i.is_connected()).unwrap_or(false)
        });
        assert!(!engine.check_track_ended());
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&live.path);
    }

    /// Minimal 16-bit PCM WAV writer for the real-mpv test.
    fn write_wav(path: &std::path::Path, freq: f64, seconds: f64) {
        let rate: u32 = 8000;
        let n = (rate as f64 * seconds) as usize;
        let mut data: Vec<u8> = Vec::new();
        data.extend_from_slice(b"RIFF");
        data.extend_from_slice(&(36u32 + (n * 2) as u32).to_le_bytes());
        data.extend_from_slice(b"WAVEfmt ");
        data.extend_from_slice(&16u32.to_le_bytes());
        data.extend_from_slice(&1u16.to_le_bytes()); // PCM
        data.extend_from_slice(&1u16.to_le_bytes()); // mono
        data.extend_from_slice(&rate.to_le_bytes());
        data.extend_from_slice(&(rate * 2).to_le_bytes());
        data.extend_from_slice(&2u16.to_le_bytes());
        data.extend_from_slice(&16u16.to_le_bytes());
        data.extend_from_slice(b"data");
        data.extend_from_slice(&((n * 2) as u32).to_le_bytes());
        for i in 0..n {
            let t = i as f64 / rate as f64;
            let s = (2.0 * std::f64::consts::PI * freq * t).sin();
            data.extend_from_slice(&((s * 12000.0) as i16).to_le_bytes());
        }
        std::fs::write(path, data).unwrap();
    }

    #[test]
    fn real_mpv_auto_advances_through_queue() {
        // End-to-end proof with the REAL mpv binary: three 2-second
        // WAV files must play one after another with NO keypresses,
        // driven only by the engine's tick-side end detection, and
        // the queue must stop cleanly at its end (Repeat Off).
        let _env_guard = crate::diag::env_lock();
        if !crate::backend::binary_available("mpv") {
            eprintln!("skipping: mpv not installed");
            return;
        }
        // The VM has no audio device; give the test's mpv a null AO
        // through a sandboxed HOME's mpv.conf (mpv reads user config
        // because Lyre does not pass --no-config).
        let home = std::env::temp_dir().join(format!("lyre-mpv-home-{}", std::process::id()));
        let conf_dir = home.join(".config").join("mpv");
        std::fs::create_dir_all(&conf_dir).unwrap();
        std::fs::write(conf_dir.join("mpv.conf"), "ao=null\n").unwrap();
        let prev_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", &home);
        let prev_fake = std::env::var("LYRE_TEST_FAKE_MPV").ok();
        std::env::remove_var("LYRE_TEST_FAKE_MPV");

        let dir = std::env::temp_dir().join(format!("lyre-mpv-wavs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut paths = std::collections::HashMap::new();
        for (id, freq) in [("ra", 440.0), ("rb", 550.0), ("rc", 660.0)] {
            let p = dir.join(format!("{}.wav", id));
            write_wav(&p, freq, 2.0);
            paths.insert(id.to_string(), p.to_string_lossy().to_string());
        }
        struct FileBackend {
            paths: std::collections::HashMap<String, String>,
        }
        impl crate::backend::PlayerBackend for FileBackend {
            fn source(&self) -> Source {
                Source::YtMusic
            }
            fn search(&self, _q: &str) -> Result<Vec<Track>, String> {
                Ok(vec![])
            }
            fn resolve_stream(&self, track: &Track, _s: &Settings) -> Result<String, String> {
                self.paths
                    .get(&track.id)
                    .cloned()
                    .ok_or_else(|| "no file".to_string())
            }
            fn resolve_match(&self, _q: &str, _s: &Settings) -> Result<String, String> {
                Err("unused".to_string())
            }
        }
        let backend = FileBackend { paths };
        let settings = Settings::default();
        let mut engine = PlayerEngine::new();
        for id in ["ra", "rb", "rc"] {
            engine.enqueue(Track::new(id, "Wav", "Test", 2, Source::YtMusic));
        }
        let outcome = engine.play_index(0, &settings, &backend, &backend);
        assert!(matches!(outcome, PlayOutcome::Playing(_)), "outcome: {:?}", outcome);

        // Tick exactly like the app does; no keys anywhere.
        let mut seen = vec![0usize];
        let mut end_of_queue = false;
        let deadline = Instant::now() + Duration::from_secs(40);
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
            if engine.check_track_ended() {
                match engine.next(&settings, &backend, &backend) {
                    PlayOutcome::Playing(_) => {
                        seen.push(engine.current_index.unwrap());
                    }
                    PlayOutcome::EndOfQueue => {
                        end_of_queue = true;
                        break;
                    }
                    other => panic!("unexpected outcome while advancing: {:?}", other),
                }
            }
        }
        engine.mpv.stop();
        match prev_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        if let Some(f) = prev_fake {
            std::env::set_var("LYRE_TEST_FAKE_MPV", f);
        }
        assert_eq!(seen, vec![0, 1, 2], "queue must advance by itself");
        assert!(end_of_queue, "queue must stop cleanly at its end");
        assert!(!engine.is_playing);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&home);
    }
}
