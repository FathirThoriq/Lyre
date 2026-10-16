use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::eq::EqState;
use crate::track::{Source, Track};

/// Lyre's per-user config directory: Linux `~/.config/lyre`,
/// Windows `%APPDATA%\Lyre` (via the `dirs` crate). Accounts,
/// settings, the log and caches all live here.
pub fn lyre_config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("lyre")
}

/// What a saved playlist link points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SavedKind {
    Playlist,
    Album,
}

/// A playlist/album the user pasted as a share link (Feature A,
/// v0.1.5). Persisted in settings.json so it can be reopened from
/// the sidebar at any time; `id` is the source's own identifier
/// (Spotify id, or a YT Music VL... browseId).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedPlaylist {
    pub source: Source,
    pub kind: SavedKind,
    pub id: String,
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub clean_playback_master: bool,
    pub spotify_adblock: bool,
    pub ytmusic_adblock_sponsorblock: bool,
    pub eq: EqState,
    pub volume: u8,
    /// Built-in autoplay: keep playing with YT Music radio picks
    /// based on the currently playing song (v0.1.6). Default ON.
    #[serde(default = "default_true")]
    pub autoplay: bool,
    /// Sidebar audio visualizer (external `cava`) on/off (v0.1.6).
    /// Default ON.
    #[serde(default = "default_true")]
    pub visualizer: bool,
    /// Visualizer template (v0.1.7): "Bars", "Mirror", "Wave",
    /// "Line" or "LED" - cycled with key `V` or by clicking the
    /// visualizer box. Stored as the template's display name.
    #[serde(default = "default_visualizer_style")]
    pub visualizer_style: String,
    /// Playlists/albums saved from pasted share links (Feature A).
    #[serde(default)]
    pub saved_playlists: Vec<SavedPlaylist>,
}

fn default_true() -> bool {
    true
}

fn default_visualizer_style() -> String {
    "Bars".to_string()
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            clean_playback_master: true,
            spotify_adblock: true,
            ytmusic_adblock_sponsorblock: true,
            eq: EqState::default(),
            volume: 70,
            autoplay: true,
            visualizer: true,
            visualizer_style: "Bars".to_string(),
            saved_playlists: Vec::new(),
        }
    }
}

impl Settings {
    /// Clean Playback is only active when the master switch and the
    /// per-platform switch are both on.
    pub fn spotify_clean_active(&self) -> bool {
        self.clean_playback_master && self.spotify_adblock
    }
    pub fn ytmusic_clean_active(&self) -> bool {
        self.clean_playback_master && self.ytmusic_adblock_sponsorblock
    }
    pub fn any_clean_active(&self) -> bool {
        self.clean_playback_master && (self.spotify_adblock || self.ytmusic_adblock_sponsorblock)
    }

    /// Insert or refresh a saved playlist link (dedupe by
    /// source + kind + id). Returns true when it is new.
    pub fn upsert_saved_playlist(&mut self, entry: SavedPlaylist) -> bool {
        if let Some(existing) = self
            .saved_playlists
            .iter_mut()
            .find(|p| p.source == entry.source && p.kind == entry.kind && p.id == entry.id)
        {
            existing.name = entry.name;
            existing.url = entry.url;
            return false;
        }
        self.saved_playlists.push(entry);
        true
    }

    /// Remove the saved playlist at settings index `idx`.
    pub fn remove_saved_playlist(&mut self, idx: usize) -> Option<SavedPlaylist> {
        if idx < self.saved_playlists.len() {
            Some(self.saved_playlists.remove(idx))
        } else {
            None
        }
    }

    pub fn file_path() -> PathBuf {
        lyre_config_dir().join("settings.json")
    }

    pub fn save_to(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, json).map_err(|e| e.to_string())
    }

    pub fn load_from(path: &Path) -> Settings {
        match std::fs::read_to_string(path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
            Err(_) => Settings::default(),
        }
    }

    pub fn load() -> Settings {
        Self::load_from(&Self::file_path())
    }

    pub fn save(&self) -> Result<(), String> {
        self.save_to(&Self::file_path())
    }
}

// ---------------------------------------------------------------------------
// Trending cache (Feature B): the last successful Home trending list,
// stored with its fetch time so the next start shows it instantly.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrendingCache {
    /// Unix seconds when this list was fetched.
    pub fetched_at: u64,
    pub tracks: Vec<Track>,
}

pub fn trending_cache_path() -> PathBuf {
    lyre_config_dir().join("trending_cache.json")
}

pub fn load_trending_cache() -> Option<TrendingCache> {
    let s = std::fs::read_to_string(trending_cache_path()).ok()?;
    let cache: TrendingCache = serde_json::from_str(&s).ok()?;
    if cache.tracks.is_empty() {
        None
    } else {
        Some(cache)
    }
}

pub fn save_trending_cache(tracks: &[Track], fetched_at: u64) {
    let cache = TrendingCache {
        fetched_at,
        tracks: tracks.to_vec(),
    };
    if let Some(parent) = trending_cache_path().parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(&cache) {
        let _ = std::fs::write(trending_cache_path(), json);
    }
}

/// "HH:MM" (local time) for a unix timestamp - the trending cache
/// footnote ("diperbarui 14:32"). Local offset comes from the TZ
/// environment / system localtime via a tiny civil-time conversion;
/// falls back to UTC when the offset cannot be determined.
pub fn hhmm_of(unix_secs: u64) -> String {
    let offset = local_utc_offset_secs();
    let t = (unix_secs as i64 + offset).rem_euclid(86_400);
    format!("{:02}:{:02}", t / 3600, (t % 3600) / 60)
}

/// "HH:MM:SS" (local time) for a unix timestamp - the Spotify
/// rate-limit note names the exact retry time (v0.1.7).
pub fn hms_of(unix_secs: u64) -> String {
    let offset = local_utc_offset_secs();
    let t = (unix_secs as i64 + offset).rem_euclid(86_400);
    format!("{:02}:{:02}:{:02}", t / 3600, (t % 3600) / 60, t % 60)
}

/// Current unix time in seconds (0 if the clock is unavailable).
pub fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Spotify Top 50 disk cache (v0.1.7): the Spotify half of Home's
// trending list, cached separately from the merged trending cache
// so the anonymous-token API is called at most once per TTL - the
// 429 rate-budget fix. Same on-disk shape as TrendingCache.
// ---------------------------------------------------------------------------

/// How long the cached Spotify Top 50 list stays fresh: 6 hours.
pub const SPOTIFY_TRENDING_TTL_SECS: u64 = 6 * 3600;

pub fn spotify_trending_cache_path() -> PathBuf {
    lyre_config_dir().join("spotify_trending_cache.json")
}

pub fn load_spotify_trending_cache() -> Option<TrendingCache> {
    let s = std::fs::read_to_string(spotify_trending_cache_path()).ok()?;
    let cache: TrendingCache = serde_json::from_str(&s).ok()?;
    if cache.tracks.is_empty() {
        None
    } else {
        Some(cache)
    }
}

pub fn save_spotify_trending_cache(tracks: &[Track], fetched_at: u64) {
    let cache = TrendingCache {
        fetched_at,
        tracks: tracks.to_vec(),
    };
    if let Some(parent) = spotify_trending_cache_path().parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(&cache) {
        let _ = std::fs::write(spotify_trending_cache_path(), json);
    }
}

/// True when a Spotify trending cache exists and is younger than
/// the TTL - in that window the API must not be called at all.
pub fn spotify_trending_cache_fresh(cache: &TrendingCache, now_secs: u64) -> bool {
    now_secs.saturating_sub(cache.fetched_at) < SPOTIFY_TRENDING_TTL_SECS
}

#[cfg(unix)]
fn local_utc_offset_secs() -> i64 {
    // Read the zone offset from `date +%z` once - cheap and correct
    // enough for a display footnote; any failure means UTC.
    use std::sync::OnceLock;
    static CACHE: OnceLock<i64> = OnceLock::new();
    *CACHE.get_or_init(|| {
        let out = std::process::Command::new("date")
            .arg("+%z")
            .output()
            .ok();
        if let Some(out) = out {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if s.len() == 5 {
                let sign: i64 = if s.starts_with('-') { -1 } else { 1 };
                let h: i64 = s[1..3].parse().unwrap_or(0);
                let m: i64 = s[3..5].parse().unwrap_or(0);
                return sign * (h * 3600 + m * 60);
            }
        }
        0
    })
}

#[cfg(not(unix))]
fn local_utc_offset_secs() -> i64 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_roundtrip_and_legacy_files_load() {
        let path = std::env::temp_dir().join(format!("lyre-test-{}-settings.json", std::process::id()));
        let mut s = Settings::default();
        s.saved_playlists.push(SavedPlaylist {
            source: Source::Spotify,
            kind: SavedKind::Playlist,
            id: "pl123".to_string(),
            name: "Playlist Tempel".to_string(),
            url: "https://open.spotify.com/playlist/pl123".to_string(),
        });
        s.save_to(&path).expect("save settings");
        let loaded = Settings::load_from(&path);
        assert_eq!(loaded.saved_playlists.len(), 1);
        assert_eq!(loaded.saved_playlists[0].id, "pl123");
        // A v0.1.4-era file (with an oauth section + account-era
        // keys) still loads; unknown keys are ignored by serde and
        // the account data is simply never used again.
        std::fs::write(&path, "{\"clean_playback_master\":true,\"spotify_adblock\":true,\"ytmusic_adblock_sponsorblock\":true,\"eq\":{\"bands\":[0.0,0.0,0.0,0.0,0.0,0.0,0.0,0.0,0.0,0.0],\"preamp\":0.0,\"preset_name\":\"Flat\",\"custom_presets\":[]},\"volume\":70,\"oauth\":{\"spotify_client_id\":\"spid\",\"google_client_id\":\"gid\"}}").unwrap();
        let legacy = Settings::load_from(&path);
        assert_eq!(legacy.volume, 70);
        assert!(legacy.saved_playlists.is_empty());
        // A minimal/old file without saved_playlists loads too.
        std::fs::write(&path, "{\"volume\":55}").unwrap();
        let minimal = Settings::load_from(&path);
        assert_eq!(minimal.volume, 55);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn autoplay_and_visualizer_settings_persist_and_default_on() {
        // v0.1.6: both new toggles default ON, persist through a
        // save/load roundtrip, and a pre-v0.1.6 settings file (which
        // lacks the keys) loads them as ON via serde defaults.
        let path = std::env::temp_dir().join(format!("lyre-test-{}-toggles.json", std::process::id()));
        let mut s = Settings::default();
        assert!(s.autoplay && s.visualizer, "both default ON");
        s.autoplay = false;
        s.save_to(&path).expect("save settings");
        let loaded = Settings::load_from(&path);
        assert!(!loaded.autoplay);
        assert!(loaded.visualizer);
        std::fs::write(&path, "{\"volume\":55}").unwrap();
        let legacy = Settings::load_from(&path);
        assert!(legacy.autoplay && legacy.visualizer, "legacy file -> defaults ON");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn visualizer_style_persists_and_defaults_to_bars() {
        // v0.1.7: the chosen visualizer template persists; files
        // written before it existed load the "Bars" default.
        let path = std::env::temp_dir().join(format!("lyre-test-{}-vizstyle.json", std::process::id()));
        let mut s = Settings::default();
        assert_eq!(s.visualizer_style, "Bars", "default template");
        s.visualizer_style = "Wave".to_string();
        s.save_to(&path).expect("save settings");
        assert_eq!(Settings::load_from(&path).visualizer_style, "Wave");
        std::fs::write(&path, "{\"volume\":55}").unwrap();
        assert_eq!(Settings::load_from(&path).visualizer_style, "Bars", "legacy file -> Bars");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn spotify_trending_ttl_and_hms_shape() {
        // 6-hour TTL: fresh strictly inside the window.
        let cache = TrendingCache {
            fetched_at: 1_000_000,
            tracks: vec![Track::new("a", "T", "A", 100, Source::Spotify)],
        };
        assert!(spotify_trending_cache_fresh(&cache, 1_000_000 + SPOTIFY_TRENDING_TTL_SECS - 1));
        assert!(!spotify_trending_cache_fresh(&cache, 1_000_000 + SPOTIFY_TRENDING_TTL_SECS));
        assert_eq!(SPOTIFY_TRENDING_TTL_SECS, 6 * 3600);
        // hms_of renders "HH:MM:SS" (local offset applied).
        let h = hms_of(1_760_000_000);
        assert_eq!(h.len(), 8);
        assert_eq!(&h[2..3], ":");
        assert_eq!(&h[5..6], ":");
    }

    #[test]
    fn saved_playlist_upsert_dedupes_and_removes() {
        let mut s = Settings::default();
        let entry = SavedPlaylist {
            source: Source::YtMusic,
            kind: SavedKind::Playlist,
            id: "VLPLx".to_string(),
            name: "Satu".to_string(),
            url: "https://music.youtube.com/playlist?list=PLx".to_string(),
        };
        assert!(s.upsert_saved_playlist(entry.clone()));
        let mut renamed = entry.clone();
        renamed.name = "Satu (baru)".to_string();
        assert!(!s.upsert_saved_playlist(renamed));
        assert_eq!(s.saved_playlists.len(), 1);
        assert_eq!(s.saved_playlists[0].name, "Satu (baru)");
        let removed = s.remove_saved_playlist(0).expect("removed");
        assert_eq!(removed.id, "VLPLx");
        assert!(s.remove_saved_playlist(0).is_none());
    }

    #[test]
    fn trending_cache_roundtrip_with_timestamp() {
        // Exercise the (de)serialization the disk cache relies on.
        let tracks = vec![
            Track::new("a", "Tren Satu", "Artis A", 200, Source::Spotify),
            Track::new("b", "Tren Dua", "Artis B", 180, Source::YtMusic),
        ];
        let cache = TrendingCache {
            fetched_at: 1_760_000_000,
            tracks: tracks.clone(),
        };
        let json = serde_json::to_string(&cache).unwrap();
        let back: TrendingCache = serde_json::from_str(&json).unwrap();
        assert_eq!(back.fetched_at, 1_760_000_000);
        assert_eq!(back.tracks, tracks);
        // hhmm_of renders "HH:MM" (local offset applied); assert shape.
        let h = hhmm_of(1_760_000_000);
        assert_eq!(h.len(), 5);
        assert_eq!(&h[2..3], ":");
    }
}
