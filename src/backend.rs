use serde_json::Value;
use std::process::Command;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::Settings;
use crate::track::{AlbumRef, PlaylistRef, Source, Track};

/// OS-appropriate install hint for a missing external binary.
pub fn missing_binary_hint(name: &str) -> String {
    if cfg!(target_os = "windows") {
        match name {
            "yt-dlp" => "yt-dlp not found in PATH. Install on Windows: winget install yt-dlp.yt-dlp".to_string(),
            "mpv" => "mpv not found in PATH. Install on Windows: winget install mpv-player.mpv (or: scoop install mpv / choco install mpv)".to_string(),
            _ => format!("{} not found in PATH.", name),
        }
    } else {
        match name {
            "yt-dlp" => "yt-dlp not found in PATH. Install it: Fedora: sudo dnf install yt-dlp | Debian: sudo apt install yt-dlp | Arch: sudo pacman -S yt-dlp".to_string(),
            "mpv" => "mpv not found in PATH. Install it: Fedora: sudo dnf install mpv | Debian: sudo apt install mpv | Arch: sudo pacman -S mpv".to_string(),
            _ => format!("{} not found in PATH.", name),
        }
    }
}

pub fn binary_available(name: &str) -> bool {
    // `Command::new(name)` resolves through PATH on both Linux and Windows
    // (where it finds yt-dlp.exe / mpv.exe). `--version` is a harmless probe.
    Command::new(name)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Existence-only PATH scan (no execution). Needed for tools whose
/// `--version` probe is unreliable - cava, for instance, exits 1 on
/// `--version` (its flag is `-v`), so binary_available() reports it
/// missing even when installed.
pub fn binary_on_path(name: &str) -> bool {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let candidates: Vec<String> = if cfg!(target_os = "windows") {
        vec![format!("{}.exe", name), name.to_string()]
    } else {
        vec![name.to_string()]
    };
    for dir in std::env::split_paths(&path) {
        for cand in &candidates {
            let p = dir.join(cand);
            if p.is_file() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Ok(meta) = p.metadata() {
                        if meta.permissions().mode() & 0o111 == 0 {
                            continue;
                        }
                    }
                }
                return true;
            }
        }
    }
    false
}

pub trait PlayerBackend {
    #[allow(dead_code)]
    fn source(&self) -> Source;
    fn search(&self, query: &str) -> Result<Vec<Track>, String>;
    fn resolve_stream(&self, track: &Track, settings: &Settings) -> Result<String, String>;
    /// Spotube model: resolve playable audio for an arbitrary
    /// "Artist - Title" match query. Only the YouTube Music backend
    /// implements this; it is how Spotify tracks (Free or Premium
    /// accounts alike) get their audio.
    fn resolve_match(&self, _query: &str, _settings: &Settings) -> Result<String, String> {
        Err("This backend does not support match resolution.".to_string())
    }
}

// ---------------------------------------------------------------------------
// YouTube Music backend: real, via the external yt-dlp binary.
// ---------------------------------------------------------------------------

pub struct YtMusicBackend;

impl PlayerBackend for YtMusicBackend {
    fn source(&self) -> Source {
        Source::YtMusic
    }

    fn search(&self, query: &str) -> Result<Vec<Track>, String> {
        if !binary_available("yt-dlp") {
            return Err(missing_binary_hint("yt-dlp"));
        }
        let search_arg = format!("ytsearch10:{}", query);
        let output = Command::new("yt-dlp")
            .args(["--dump-json", "--flat-playlist", "--no-warnings", &search_arg])
            .output()
            .map_err(|e| format!("Failed to run yt-dlp: {}", e))?;
        if !output.status.success() {
            return Err(format!(
                "yt-dlp search failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut tracks = Vec::new();
        for line in stdout.lines() {
            if let Ok(v) = serde_json::from_str::<Value>(line) {
                let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
                if id.is_empty() {
                    continue;
                }
                let raw_title = v.get("title").and_then(|x| x.as_str()).unwrap_or("Unknown title");
                let uploader = v
                    .get("uploader")
                    .and_then(|x| x.as_str())
                    .or_else(|| v.get("channel").and_then(|x| x.as_str()))
                    .unwrap_or("Unknown artist");
                let (title, artist) = clean_video_title(raw_title, uploader);
                let duration = v.get("duration").and_then(|x| x.as_f64()).unwrap_or(0.0) as u64;
                tracks.push(Track::new(id, title, artist, duration, Source::YtMusic));
            }
        }
        Ok(tracks)
    }

    fn resolve_stream(&self, track: &Track, settings: &Settings) -> Result<String, String> {
        if !binary_available("yt-dlp") {
            return Err(missing_binary_hint("yt-dlp"));
        }
        let url = format!("https://music.youtube.com/watch?v={}", track.id);
        let mut args: Vec<String> = vec![
            "-g".into(),
            "--format".into(),
            "bestaudio".into(),
            "--no-warnings".into(),
        ];
        if settings.ytmusic_clean_active() {
            // Clean Playback for YT Music: SponsorBlock segment removal is
            // honoured by yt-dlp-supported players via these options; we pass
            // them during resolution and when downloading segment metadata.
            // Category names must be yt-dlp's: "sponsor" + "music_offtopic"
            // (the non-music category). Verified against a real yt-dlp via
            // `lyre resolve-test` in v0.1.3 - the wrong name made every
            // Clean-Playback resolve fail.
            args.push("--sponsorblock-remove".into());
            args.push("sponsor,music_offtopic".into());
        }
        args.push(url);
        let output = Command::new("yt-dlp")
            .args(&args)
            .output()
            .map_err(|e| format!("Failed to run yt-dlp: {}", e))?;
        if !output.status.success() {
            return Err(format!(
                "yt-dlp could not resolve the stream: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        stdout
            .lines()
            .next()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "yt-dlp returned no stream URL".to_string())
    }

    fn resolve_match(&self, query: &str, settings: &Settings) -> Result<String, String> {
        // Find the best YouTube Music match for "Artist - Title", then
        // resolve that video's audio stream through the normal path
        // (SponsorBlock/Clean Playback handling included).
        let results = self.search(query)?;
        let first = results
            .into_iter()
            .next()
            .ok_or_else(|| format!("No matching YouTube Music result for '{}'", query))?;
        self.resolve_stream(&first, settings)
    }
}

/// Clean a raw yt-dlp (YouTube) video title into (title, artist).
/// Raw titles look like "Rumahsakit - Sandiwara Semu (Official
/// Lyric Video) - Off The Records": bracketed promo suffixes are
/// stripped, the first " - " segment is the artist, and a trailing
/// segment is dropped as the channel/label name when the uploader
/// is recognisably the artist's own channel (Fathir's real example:
/// uploader "officialrumahsakit" for artist "Rumahsakit").
pub fn clean_video_title(raw_title: &str, uploader: &str) -> (String, String) {
    // 1. Strip bracketed promo segments: (Official ...), [Lyric
    // Video], etc. A bracket is promo when its text mentions any of
    // the usual noise words; other brackets (e.g. "(Acoustic)")
    // stay - they are part of the work's name.
    const NOISE: [&str; 10] = [
        "official", "video", "audio", "lyric", "lirik", "klip", "mv", "hd", "hq", "4k",
    ];
    let mut cleaned = String::new();
    let mut depth = 0i32;
    let mut bracket = String::new();
    for c in raw_title.chars() {
        match c {
            '(' | '[' if depth == 0 => {
                depth = 1;
                bracket.clear();
            }
            ')' | ']' if depth > 0 => {
                depth = 0;
                let low = bracket.to_lowercase();
                if !NOISE.iter().any(|n| low.contains(n)) {
                    cleaned.push_str(&format!("({})", bracket.trim()));
                }
                bracket.clear();
            }
            _ => {
                if depth > 0 {
                    bracket.push(c);
                } else {
                    cleaned.push(c);
                }
            }
        }
    }
    if depth > 0 {
        // Unterminated bracket: keep its text rather than eat it.
        cleaned.push_str(bracket.trim());
    }
    let cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");

    // Channel cleanup: "Artist - Topic" -> "Artist".
    let channel = uploader.trim().trim_end_matches(" - Topic").trim().to_string();

    // 2. Split on " - ".
    let parts: Vec<&str> = cleaned.split(" - ").map(|p| p.trim()).filter(|p| !p.is_empty()).collect();
    let norm = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).flat_map(|c| c.to_lowercase()).collect::<String>();
    match parts.len() {
        0 => (cleaned, channel),
        1 => (parts[0].to_string(), channel),
        2 => (parts[1].to_string(), parts[0].to_string()),
        _ => {
            let artist = parts[0].to_string();
            // A trailing segment after artist+title is the channel /
            // label ("Off The Records") when the uploader channel is
            // the artist's own (contains the artist name or the
            // artist contains it) - drop it. Otherwise keep the
            // middle segments joined as the title.
            let up = norm(uploader);
            let ar = norm(&artist);
            let last = parts[parts.len() - 1];
            let last_is_channel = (!ar.is_empty() && (up.contains(&ar) || ar.contains(&up) && !up.is_empty()))
                || norm(last) == up;
            let title_parts = if last_is_channel {
                &parts[1..parts.len() - 1]
            } else {
                &parts[1..]
            };
            (title_parts.join(" - "), artist)
        }
    }
}

// ---------------------------------------------------------------------------
// Spotify backend (v0.1.5, guest-only): catalog data comes from the
// Web API with an ANONYMOUS access token - the same public token
// Spotify's own web player fetches for logged-out browsing
// (GET open.spotify.com/get_access_token?reason=transport&
// productType=embed). No Client ID, no secret, no user credential
// exists anywhere in Lyre. The token lives in memory only, is
// refreshed before expiry and once on any 401. Playback does NOT
// use this backend: a Spotify track's audio is matched on YouTube
// Music and streamed from there (Spotube model), so Spotify catalog
// availability never affects whether music plays.
//
// Honest status: the get_access_token endpoint is Spotify's own
// public web-player mechanism, unofficial for third-party use, and
// may change; if it cannot be fetched, search/browse shows the
// clear guest note below and the YT Music side keeps working.
// ---------------------------------------------------------------------------

/// The exact user-facing note when the Spotify guest catalog cannot
/// be reached (brief-fixed wording; details are appended after it).
pub const SPOTIFY_GUEST_NOTE: &str = "Spotify guest access is unreachable right now";

fn guest_err(detail: &str) -> String {
    format!("{}: {}", SPOTIFY_GUEST_NOTE, detail)
}

/// Spotify's public Top 50 - Global playlist (Feature B trending).
pub const TOP50_GLOBAL_ID: &str = "37i9dQZEVXbMDoHDwVN2tF";

/// Token endpoint (test-only env override points it at a stub).
fn spotify_token_url() -> String {
    std::env::var("LYRE_SPOTIFY_TOKEN_URL").unwrap_or_else(|_| {
        "https://open.spotify.com/get_access_token?reason=transport&productType=embed".to_string()
    })
}

/// Embed page used by the token fallback chain (v0.1.6): when the
/// get_access_token endpoint itself is blocked (HTTP 403), the
/// anonymous token is extracted from this page's embedded JSON
/// state instead. Test-only env override points it at a stub.
fn spotify_embed_url() -> String {
    std::env::var("LYRE_SPOTIFY_EMBED_URL").unwrap_or_else(|_| {
        format!("https://open.spotify.com/embed/playlist/{}", TOP50_GLOBAL_ID)
    })
}

/// One diagnostic line for a blocked Spotify guest step. The log
/// names the STEP, the URL path, the HTTP status and a short body
/// snippet (Akamai error pages carry a "Reference #..." id, kept
/// when present) - never a token value: snippets come only from
/// ERROR responses, and a token is never part of one.
pub fn spotify_block_line(step: &str, path: &str, status: u16, body: &str) -> String {
    let collapsed: String = body
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(160)
        .collect();
    let reference = body
        .find("Reference")
        .map(|i| body[i..].chars().take(28).collect::<String>())
        .unwrap_or_default();
    format!(
        "spotify guest blocked: step={} path={} status={} ref=\"{}\" body=\"{}\"",
        step, path, status, reference, collapsed
    )
}

fn log_spotify_block(step: &str, path: &str, status: u16, body: &str) {
    crate::diag::append_log(&spotify_block_line(step, path, status, body));
}

/// Path portion of a URL for log lines (no query, no host).
fn url_path(url: &str) -> String {
    let after_host = url.split("://").nth(1).unwrap_or(url);
    match after_host.find('/') {
        Some(i) => after_host[i..].split('?').next().unwrap_or("/").to_string(),
        None => "/".to_string(),
    }
}

/// Web API root (test-only env override points it at a stub).
fn spotify_api_base() -> String {
    std::env::var("LYRE_SPOTIFY_API_BASE").unwrap_or_else(|_| "https://api.spotify.com".to_string())
}

const SPOTIFY_UA: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";

/// Parse the anonymous-token response: {accessToken,
/// accessTokenExpirationTimestampMs, isAnonymous, clientId}.
/// Returns (token, expires_at_ms). Pure; tested on fixtures.
pub fn parse_anon_token(v: &Value) -> Option<(String, u64)> {
    let token = v.get("accessToken").and_then(|x| x.as_str()).filter(|s| !s.is_empty())?;
    let exp = v
        .get("accessTokenExpirationTimestampMs")
        .and_then(|x| x.as_u64())
        .unwrap_or(0);
    Some((token.to_string(), exp))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The HTTP surface the guest client needs - an interface so the
/// 401-refresh logic is unit-testable with a scripted stub.
pub trait SpotHttp: Send + Sync {
    /// GET `url`; with a Bearer token when `bearer` is Some.
    /// Returns (HTTP status, JSON body) - error statuses included.
    /// The default drops the response headers via `get_resp`;
    /// scripted stubs may implement either method (each default
    /// delegates to the other, so overriding one is enough).
    fn get(&self, url: &str, bearer: Option<&str>) -> Result<(u16, Value), String> {
        let (status, v, _) = self.get_resp(url, bearer)?;
        Ok((status, v))
    }
    /// Like `get`, plus the raw `Retry-After` response header when
    /// the server sent one (v0.1.7: HTTP 429 handling reads it).
    fn get_resp(&self, url: &str, bearer: Option<&str>) -> Result<(u16, Value, Option<String>), String> {
        let (status, v) = self.get(url, bearer)?;
        Ok((status, v, None))
    }
    /// GET `url` as raw text (the embed page is HTML; the token
    /// endpoint is fetched this way too so a block page's body -
    /// the Akamai reference - survives for the log). The default
    /// serializes `get`'s JSON; the real client overrides it.
    fn get_text(&self, url: &str) -> Result<(u16, String), String> {
        self.get(url, None).map(|(s, v)| (s, v.to_string()))
    }
    /// Like `get_text`, plus the raw `Retry-After` header.
    fn get_text_resp(&self, url: &str) -> Result<(u16, String, Option<String>), String> {
        let (status, t) = self.get_text(url)?;
        Ok((status, t, None))
    }
}

pub struct UreqHttp;

/// Browser-grade header set for an open.spotify.com fetch/XHR, the
/// way the real web player issues it - Akamai fingerprints header
/// consistency, not just the User-Agent.
fn browser_fetch_headers(req: ureq::Request) -> ureq::Request {
    req.set("User-Agent", SPOTIFY_UA)
        .set("Accept", "application/json, text/plain, */*")
        .set("Accept-Language", "en-US,en;q=0.9")
        .set("Referer", "https://open.spotify.com/")
        .set("Sec-Fetch-Dest", "empty")
        .set("Sec-Fetch-Mode", "cors")
        .set("Sec-Fetch-Site", "same-origin")
}

/// Browser-grade header set for an open.spotify.com page
/// navigation (the embed page fetch of the token fallback chain).
fn browser_page_headers(req: ureq::Request) -> ureq::Request {
    req.set("User-Agent", SPOTIFY_UA)
        .set("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8")
        .set("Accept-Language", "en-US,en;q=0.9")
        .set("Referer", "https://open.spotify.com/")
        .set("Sec-Fetch-Dest", "document")
        .set("Sec-Fetch-Mode", "navigate")
        .set("Sec-Fetch-Site", "same-origin")
        .set("Upgrade-Insecure-Requests", "1")
}

impl SpotHttp for UreqHttp {
    fn get_resp(&self, url: &str, bearer: Option<&str>) -> Result<(u16, Value, Option<String>), String> {
        let mut req = ureq::get(url).timeout(std::time::Duration::from_secs(12));
        if url.contains("open.spotify.com") {
            req = browser_fetch_headers(req);
        } else {
            req = req
                .set("User-Agent", SPOTIFY_UA)
                .set("Accept", "application/json")
                .set("Accept-Language", "en-US,en;q=0.9");
        }
        if let Some(t) = bearer {
            req = req.set("Authorization", &format!("Bearer {}", t));
        }
        match req.call() {
            Ok(resp) => {
                let status = resp.status();
                let retry = resp.header("Retry-After").map(|s| s.to_string());
                let v = resp.into_json().unwrap_or(Value::Null);
                Ok((status, v, retry))
            }
            Err(ureq::Error::Status(status, resp)) => {
                let retry = resp.header("Retry-After").map(|s| s.to_string());
                let v = resp.into_json().unwrap_or(Value::Null);
                Ok((status, v, retry))
            }
            Err(e) => Err(format!("{}", e)),
        }
    }

    fn get_text(&self, url: &str) -> Result<(u16, String), String> {
        let (status, text, _) = self.get_text_resp(url)?;
        Ok((status, text))
    }

    fn get_text_resp(&self, url: &str) -> Result<(u16, String, Option<String>), String> {
        let mut req = ureq::get(url).timeout(std::time::Duration::from_secs(12));
        if url.contains("/embed/") {
            req = browser_page_headers(req);
        } else {
            req = browser_fetch_headers(req);
        }
        match req.call() {
            Ok(resp) => {
                let status = resp.status();
                let retry = resp.header("Retry-After").map(|s| s.to_string());
                let text = resp.into_string().unwrap_or_default();
                Ok((status, text, retry))
            }
            Err(ureq::Error::Status(status, resp)) => {
                let retry = resp.header("Retry-After").map(|s| s.to_string());
                let text = resp.into_string().unwrap_or_default();
                Ok((status, text, retry))
            }
            Err(e) => Err(format!("{}", e)),
        }
    }
}

/// Extract the anonymous token from an embed page's HTML: the
/// server-rendered page carries its session as JSON (the
/// __NEXT_DATA__ script), including an "accessToken" field and
/// usually "accessTokenExpirationTimestampMs". Tolerates both raw
/// JSON quotes and &quot;-escaped markup. Pure; fixture-tested
/// with synthetic tokens only.
pub fn parse_embed_token(html: &str) -> Option<(String, u64)> {
    let normalized;
    let text: &str = if html.contains("&quot;") {
        normalized = html.replace("&quot;", "\"");
        &normalized
    } else {
        html
    };
    let key_at = text.find("\"accessToken\"")?;
    let rest = &text[key_at + "\"accessToken\"".len()..];
    let after_colon = rest.trim_start_matches(|c| c == ':' || c == ' ' || c == '\t');
    let after_quote = after_colon.strip_prefix('"')?;
    let end = after_quote.find('"')?;
    let token = &after_quote[..end];
    if token.is_empty() {
        return None;
    }
    let exp = text
        .find("\"accessTokenExpirationTimestampMs\"")
        .and_then(|i| {
            let tail = &text[i + "\"accessTokenExpirationTimestampMs\"".len()..];
            let digits: String = tail
                .trim_start_matches(|c| c == ':' || c == ' ')
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            digits.parse::<u64>().ok()
        })
        .unwrap_or(0);
    Some((token.to_string(), exp))
}

// ---------------------------------------------------------------------------
// Spotify rate-budget state (v0.1.7). Fathir's v0.1.6 home test hit
// HTTP 429 at the API step: the anonymous token's quota is real, so
// Lyre now (a) honors Retry-After with a GLOBAL cooldown - zero
// Spotify calls while it runs, (b) PERSISTS the cooldown + token
// expiry so a restart does not reset the penalty and re-hammer the
// API, (c) routes every api.spotify.com call through one gate with
// >= 1s spacing, and (d) cuts call volume (one combined search
// request, a 6-hour disk cache for the Top 50 trending half).
// ---------------------------------------------------------------------------

/// Default cooldown when a 429 carries no usable Retry-After.
pub const DEFAULT_RETRY_AFTER_SECS: u64 = 60;
/// Upper clamp for a server-provided Retry-After (1 hour).
pub const MAX_RETRY_AFTER_SECS: u64 = 3600;
/// Minimum spacing between two api.spotify.com calls.
pub const API_MIN_SPACING_MS: u64 = 1000;

/// Parse a `Retry-After` header value (delay-seconds form) into a
/// cooldown length: absent/unparseable/zero -> the 60s default;
/// huge values clamp to one hour. Pure; unit-tested.
pub fn parse_retry_after(raw: Option<&str>) -> u64 {
    match raw.and_then(|s| s.trim().parse::<u64>().ok()) {
        Some(secs) if secs > 0 => secs.min(MAX_RETRY_AFTER_SECS),
        _ => DEFAULT_RETRY_AFTER_SECS,
    }
}

/// The user-facing rate-limit note (English, exact wording used by
/// the search view, Home and `lyre doctor`): names the retry time.
pub fn rate_limit_note(cooldown_until_ms: u64) -> String {
    format!(
        "Spotify rate limited — retry at {}",
        crate::config::hms_of(cooldown_until_ms / 1000)
    )
}

/// One log line for a 429: step, path, status and the Retry-After
/// value. Never a token (tokens are never part of these inputs).
pub fn rate_limit_log_line(step: &str, path: &str, status: u16, retry_after_secs: u64, until_ms: u64) -> String {
    format!(
        "spotify rate limit: step={} path={} status={} retry_after={}s cooldown_until={}",
        step,
        path,
        status,
        retry_after_secs,
        crate::config::hms_of(until_ms / 1000)
    )
}

/// On-disk Spotify guest state (`spotify_state.json` in the config
/// dir): the rate-limit cooldown and the working token's expiry -
/// deliberately NOT the token value itself.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
struct SpotifyPersist {
    cooldown_until_ms: u64,
    token_expires_at_ms: u64,
    token_source: Option<String>,
    last_block_step: Option<String>,
    last_block_status: u16,
}

/// Where the persisted Spotify state lives (test-only env override
/// points it at a temp file).
fn spotify_state_path() -> std::path::PathBuf {
    match std::env::var("LYRE_SPOTIFY_STATE_FILE") {
        Ok(p) if !p.is_empty() => std::path::PathBuf::from(p),
        _ => crate::config::lyre_config_dir().join("spotify_state.json"),
    }
}

fn load_spotify_persist(path: &std::path::Path) -> SpotifyPersist {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

#[derive(Default)]
struct AnonState {
    token: Option<String>,
    expires_at_ms: u64,
    /// Where the cached token came from ("get_access_token" or
    /// "embed page") - diagnostics for `lyre doctor`.
    source: Option<&'static str>,
    /// Global rate-limit cooldown: while now < this instant, NO
    /// Spotify call is made at all (v0.1.7).
    cooldown_until_ms: u64,
    /// Last api.spotify.com call time (the 1s spacing gate).
    last_api_call_ms: u64,
    /// Most recent blocked step, for `lyre doctor` (persisted).
    last_block_step: Option<String>,
    last_block_status: u16,
}

/// The shared guest client: one anonymous token, cached in memory,
/// refreshed when expired/near expiry and once after a 401. All
/// api.spotify.com traffic is serialized through `get_json`, which
/// enforces the cooldown and the minimum call spacing (v0.1.7).
pub struct SpotifyGuest {
    http: Box<dyn SpotHttp>,
    state: Mutex<AnonState>,
    /// Serializes api.spotify.com calls (one at a time, spaced).
    api_gate: Mutex<()>,
    /// Where rate-limit state persists across restarts; None in
    /// unit tests unless a test opts in explicitly.
    persist_path: Option<std::path::PathBuf>,
}

impl Default for SpotifyGuest {
    fn default() -> Self {
        SpotifyGuest::new()
    }
}

impl SpotifyGuest {
    pub fn new() -> SpotifyGuest {
        let path = spotify_state_path();
        let persisted = load_spotify_persist(&path);
        let mut state = AnonState::default();
        // A persisted cooldown survives the restart: the app opens
        // in the rate-limited state instead of re-hammering the API.
        state.cooldown_until_ms = persisted.cooldown_until_ms;
        state.expires_at_ms = persisted.token_expires_at_ms;
        state.last_block_step = persisted.last_block_step.clone();
        state.last_block_status = persisted.last_block_status;
        SpotifyGuest {
            http: Box::new(UreqHttp),
            state: Mutex::new(state),
            api_gate: Mutex::new(()),
            persist_path: Some(path),
        }
    }

    #[cfg(test)]
    fn with_http(http: Box<dyn SpotHttp>) -> SpotifyGuest {
        SpotifyGuest {
            http,
            state: Mutex::new(AnonState::default()),
            api_gate: Mutex::new(()),
            persist_path: None,
        }
    }

    #[cfg(test)]
    fn with_http_persist(http: Box<dyn SpotHttp>, path: std::path::PathBuf) -> SpotifyGuest {
        let persisted = load_spotify_persist(&path);
        let mut state = AnonState::default();
        state.cooldown_until_ms = persisted.cooldown_until_ms;
        state.expires_at_ms = persisted.token_expires_at_ms;
        state.last_block_step = persisted.last_block_step;
        state.last_block_status = persisted.last_block_status;
        SpotifyGuest {
            http,
            state: Mutex::new(state),
            api_gate: Mutex::new(()),
            persist_path: Some(path),
        }
    }

    /// Snapshot the persistable state to disk (best effort).
    fn persist(&self) {
        let Some(path) = &self.persist_path else { return };
        let snapshot = {
            let st = self.state.lock().unwrap();
            SpotifyPersist {
                cooldown_until_ms: st.cooldown_until_ms,
                token_expires_at_ms: st.expires_at_ms,
                token_source: st.source.map(|s| s.to_string()),
                last_block_step: st.last_block_step.clone(),
                last_block_status: st.last_block_status,
            }
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string(&snapshot) {
            let _ = std::fs::write(path, json);
        }
    }

    /// The active cooldown deadline, if one is running right now.
    pub fn cooldown_until(&self) -> Option<u64> {
        let until = self.state.lock().unwrap().cooldown_until_ms;
        if until > now_ms() {
            Some(until)
        } else {
            None
        }
    }

    /// Record a blocked step (doctor diagnostics) and persist it.
    fn note_block(&self, step: &str, status: u16) {
        {
            let mut st = self.state.lock().unwrap();
            st.last_block_step = Some(step.to_string());
            st.last_block_status = status;
        }
        self.persist();
    }

    /// Start (or extend) the global cooldown after a 429 and log
    /// it. Returns the user-facing rate-limit note.
    fn start_cooldown(&self, step: &str, path: &str, status: u16, retry_after_secs: u64) -> String {
        let until = {
            let mut st = self.state.lock().unwrap();
            let candidate = now_ms() + retry_after_secs * 1000;
            if candidate > st.cooldown_until_ms {
                st.cooldown_until_ms = candidate;
            }
            st.last_block_step = Some(step.to_string());
            st.last_block_status = status;
            st.cooldown_until_ms
        };
        self.persist();
        crate::diag::append_log(&rate_limit_log_line(step, path, status, retry_after_secs, until));
        rate_limit_note(until)
    }

    /// The 1s minimum spacing between api.spotify.com calls: sleeps
    /// only the remainder, and only when a call actually proceeds.
    fn throttle(&self) {
        let wait_ms = {
            let mut st = self.state.lock().unwrap();
            let now = now_ms();
            let wait = if st.last_api_call_ms == 0 {
                0
            } else {
                (st.last_api_call_ms + API_MIN_SPACING_MS).saturating_sub(now)
            };
            st.last_api_call_ms = now + wait;
            wait
        };
        if wait_ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(wait_ms));
        }
    }

    /// The current anonymous token, fetching/refreshing as needed.
    /// `force` skips the cache (used by the 401 retry).
    pub fn token(&self, force: bool) -> Result<String, String> {
        // Rate-limit cooldown: no Spotify call of any kind while
        // it runs - the embed fallback must not bypass it either.
        if let Some(until) = self.cooldown_until() {
            return Err(rate_limit_note(until));
        }
        if !force {
            let st = self.state.lock().unwrap();
            if let Some(t) = &st.token {
                // Refresh a minute early so a token never dies mid-call.
                if st.expires_at_ms > now_ms() + 60_000 {
                    return Ok(t.clone());
                }
            }
        }
        // The known workaround chain, in order (v0.1.6 - the plain
        // token endpoint answers HTTP 403 from some networks,
        // Fathir's residential Fedora included, not just this VM):
        // (a) the get_access_token endpoint, (b) the token embedded
        // in a public embed page's JSON state. Every blocked step
        // is logged with its status + body snippet, and the error
        // names the failing step - "403" alone is not a diagnosis.
        match self.fetch_token_direct() {
            Ok(t) => Ok(t),
            Err(direct_err) => {
                // A 429 on the token endpoint starts the global
                // cooldown: do NOT escalate to the embed page (that
                // would be exactly the retry storm to avoid).
                if let Some(until) = self.cooldown_until() {
                    return Err(rate_limit_note(until));
                }
                crate::diag::append_log(&format!(
                    "spotify guest: token endpoint failed ({}); trying the embed page",
                    direct_err
                ));
                self.fetch_token_embed().map_err(|embed_err| {
                    crate::diag::append_log(&format!(
                        "spotify guest: embed fallback failed ({})",
                        embed_err
                    ));
                    embed_err
                })
            }
        }
    }

    fn cache_token(&self, token: String, exp_ms: u64, source: &'static str) -> String {
        {
            let mut st = self.state.lock().unwrap();
            st.token = Some(token.clone());
            // Unknown expiry: assume a conservative 30 minutes.
            st.expires_at_ms = if exp_ms > 0 { exp_ms } else { now_ms() + 1_800_000 };
            st.source = Some(source);
        }
        // The token's expiry is part of the persisted state, so a
        // restart knows how long the working token stays valid.
        self.persist();
        token
    }

    /// Chain step (a): GET get_access_token with browser headers.
    fn fetch_token_direct(&self) -> Result<String, String> {
        let url = spotify_token_url();
        let (status, text, retry_after) = self
            .http
            .get_text_resp(&url)
            .map_err(|e| guest_err(&format!("Spotify guest token request failed ({})", e)))?;
        if status != 200 {
            log_spotify_block("token", &url_path(&url), status, &text);
            self.note_block("token", status);
            if status == 429 {
                return Err(self.start_cooldown("token", &url_path(&url), status, parse_retry_after(retry_after.as_deref())));
            }
            return Err(guest_err(&format!(
                "Spotify guest token request blocked (HTTP {})",
                status
            )));
        }
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let (token, exp) = parse_anon_token(&v)
            .ok_or_else(|| guest_err("Spotify guest token response had an unexpected shape"))?;
        Ok(self.cache_token(token, exp, "get_access_token"))
    }

    /// Chain step (b): GET an embed playlist page and extract the
    /// accessToken from its embedded JSON state.
    fn fetch_token_embed(&self) -> Result<String, String> {
        let url = spotify_embed_url();
        let (status, html, retry_after) = self
            .http
            .get_text_resp(&url)
            .map_err(|e| guest_err(&format!("Spotify guest token via embed page failed ({})", e)))?;
        if status != 200 {
            log_spotify_block("embed", &url_path(&url), status, &html);
            self.note_block("embed", status);
            if status == 429 {
                return Err(self.start_cooldown("embed", &url_path(&url), status, parse_retry_after(retry_after.as_deref())));
            }
            return Err(guest_err(&format!(
                "Spotify guest token via embed page blocked (HTTP {})",
                status
            )));
        }
        match parse_embed_token(&html) {
            Some((token, exp)) => Ok(self.cache_token(token, exp, "embed page")),
            None => {
                log_spotify_block("embed", &url_path(&url), status, "no accessToken field in the page");
                self.note_block("embed", status);
                Err(guest_err("Spotify guest token not found in the embed page"))
            }
        }
    }

    /// Handle a non-200 API response: a 429 starts/extends the
    /// global cooldown and yields the rate-limit note; anything
    /// else keeps the v0.1.6 behavior (403 logged, no retry storm).
    fn api_failure(&self, path: &str, status: u16, v: &Value, retry_after: Option<String>) -> String {
        if status == 429 {
            log_spotify_block("api", path, status, &v.to_string());
            return self.start_cooldown("api", path, status, parse_retry_after(retry_after.as_deref()));
        }
        if status == 403 {
            log_spotify_block("api", path, status, &v.to_string());
            self.note_block("api", status);
        }
        api_error(status, v)
    }

    /// GET a Web API path as guest; on 401 the token is refreshed
    /// and the call retried exactly once. THE choke point for all
    /// api.spotify.com traffic (v0.1.7): calls are serialized,
    /// spaced >= 1s apart, and short-circuited entirely while a
    /// rate-limit cooldown runs - zero network traffic then, just
    /// the note naming the retry time.
    pub fn get_json(&self, path: &str) -> Result<Value, String> {
        let _gate = self.api_gate.lock().unwrap();
        if let Some(until) = self.cooldown_until() {
            return Err(rate_limit_note(until));
        }
        self.throttle();
        let token = self.token(false)?;
        let url = format!("{}{}", spotify_api_base(), path);
        let (status, v, retry_after) = self
            .http
            .get_resp(&url, Some(&token))
            .map_err(|e| guest_err(&format!("Spotify API request failed ({})", e)))?;
        match status {
            200 => Ok(v),
            401 => {
                // Stale/rotated anonymous token: refresh + one retry.
                let fresh = self.token(true)?;
                let (status2, v2, retry2) = self
                    .http
                    .get_resp(&url, Some(&fresh))
                    .map_err(|e| guest_err(&format!("Spotify API request failed ({})", e)))?;
                if status2 == 200 {
                    Ok(v2)
                } else {
                    Err(self.api_failure(path, status2, &v2, retry2))
                }
            }
            _ => Err(self.api_failure(path, status, &v, retry_after)),
        }
    }

    /// Doctor-facing snapshot: (token source, token expiry ms,
    /// last blocked step + status) - no token value, ever.
    pub fn doctor_state(&self) -> (Option<String>, u64, Option<String>, u16) {
        let st = self.state.lock().unwrap();
        (
            st.source.map(|s| s.to_string()),
            st.expires_at_ms,
            st.last_block_step.clone(),
            st.last_block_status,
        )
    }
}

/// Human-readable API error: Spotify's own error.message when the
/// body carries one, plus the guest note and the HTTP code.
fn api_error(status: u16, v: &Value) -> String {
    let msg = v
        .get("error")
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_str())
        .unwrap_or("");
    if status == 429 {
        guest_err("Spotify rate limit reached (HTTP 429) - try again later")
    } else if !msg.is_empty() {
        guest_err(&format!("Spotify answered HTTP {}: {}", status, msg))
    } else {
        guest_err(&format!("Spotify answered HTTP {}", status))
    }
}

/// Thin PlayerBackend-shaped facade over the shared guest client.
#[derive(Clone)]
pub struct SpotifyBackend {
    pub guest: std::sync::Arc<SpotifyGuest>,
}

fn encode_query(query: &str) -> String {
    let mut encoded = String::new();
    for b in query.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => encoded.push(b as char),
            b' ' => encoded.push_str("%20"),
            _ => encoded.push_str(&format!("%{:02X}", b)),
        }
    }
    encoded
}

/// Parse one Spotify track object (search result, album track, or
/// playlist item's inner `track`). Defensive: no id -> no track.
pub fn parse_spotify_track(item: &Value) -> Option<Track> {
    let id = item.get("id").and_then(|x| x.as_str()).unwrap_or("");
    if id.is_empty() {
        return None;
    }
    let title = item.get("name").and_then(|x| x.as_str()).unwrap_or("Unknown title").to_string();
    let artist = item
        .get("artists")
        .and_then(|a| a.as_array())
        .and_then(|a| a.first())
        .and_then(|a| a.get("name"))
        .and_then(|n| n.as_str())
        .unwrap_or("Unknown artist")
        .to_string();
    let duration_ms = item.get("duration_ms").and_then(|x| x.as_u64()).unwrap_or(0);
    Some(Track::new(id.to_string(), title, artist, duration_ms / 1000, Source::Spotify))
}

/// Parse a Spotify search response's track section.
pub fn parse_spotify_search_tracks(v: &Value) -> Vec<Track> {
    let mut out = Vec::new();
    if let Some(items) = v.get("tracks").and_then(|t| t.get("items")).and_then(|i| i.as_array()) {
        for item in items {
            if let Some(t) = parse_spotify_track(item) {
                out.push(t);
            }
        }
    }
    out
}

/// Parse a Spotify search response's album section.
pub fn parse_spotify_albums(v: &Value) -> Vec<AlbumRef> {
    let mut out = Vec::new();
    if let Some(items) = v.get("albums").and_then(|a| a.get("items")).and_then(|i| i.as_array()) {
        for item in items {
            let id = item.get("id").and_then(|x| x.as_str()).unwrap_or("");
            let title = item.get("name").and_then(|x| x.as_str()).unwrap_or("");
            if id.is_empty() || title.is_empty() {
                continue;
            }
            let artist = item
                .get("artists")
                .and_then(|a| a.as_array())
                .and_then(|a| a.first())
                .and_then(|a| a.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let year = item
                .get("release_date")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .chars()
                .take(4)
                .collect();
            out.push(AlbumRef { id: id.to_string(), title: title.to_string(), artist, year, source: Source::Spotify });
        }
    }
    out
}

/// Parse playlist objects (search section or /me/playlists): the
/// info line is owner + total tracks, from the source itself.
pub fn parse_spotify_playlists(v: &Value, section: &str) -> Vec<PlaylistRef> {
    let mut out = Vec::new();
    let items = match section {
        "items" => v.get("items").and_then(|i| i.as_array()),
        _ => v.get("playlists").and_then(|p| p.get("items")).and_then(|i| i.as_array()),
    };
    if let Some(items) = items {
        for item in items {
            // Playlist search results may contain null entries.
            let id = item.get("id").and_then(|x| x.as_str()).unwrap_or("");
            let title = item.get("name").and_then(|x| x.as_str()).unwrap_or("");
            if id.is_empty() || title.is_empty() {
                continue;
            }
            let owner = item
                .get("owner")
                .and_then(|o| o.get("display_name"))
                .and_then(|n| n.as_str())
                .unwrap_or("");
            let total = item.get("tracks").and_then(|t| t.get("total")).and_then(|t| t.as_u64());
            let info = match (owner.is_empty(), total) {
                (false, Some(n)) => format!("by {} • {} tracks", owner, n),
                (false, None) => format!("by {}", owner),
                (true, Some(n)) => format!("{} tracks", n),
                (true, None) => String::new(),
            };
            out.push(PlaylistRef { id: id.to_string(), title: title.to_string(), info, source: Source::Spotify });
        }
    }
    out
}

/// Parse an album-tracks response (items are simplified tracks).
pub fn parse_spotify_album_tracks(v: &Value) -> Vec<Track> {
    let mut out = Vec::new();
    if let Some(items) = v.get("items").and_then(|i| i.as_array()) {
        for item in items {
            if let Some(t) = parse_spotify_track(item) {
                out.push(t);
            }
        }
    }
    out
}

/// Parse a playlist-items response (items wrap the track; removed
/// tracks arrive as null and are skipped).
pub fn parse_spotify_playlist_items(v: &Value) -> Vec<Track> {
    let mut out = Vec::new();
    if let Some(items) = v.get("items").and_then(|i| i.as_array()) {
        for item in items {
            if let Some(track) = item.get("track") {
                if let Some(t) = parse_spotify_track(track) {
                    out.push(t);
                }
            }
        }
    }
    out
}

impl SpotifyBackend {
    pub fn new() -> SpotifyBackend {
        SpotifyBackend {
            guest: std::sync::Arc::new(SpotifyGuest::new()),
        }
    }

    fn get_json(&self, path: &str) -> Result<Value, String> {
        self.guest.get_json(path)
    }

    /// ONE Web API search request for a query (v0.1.7 call-budget
    /// fix - v0.1.6 made two calls per search, tracks then
    /// albums+playlists). `kind` is the Search kind filter:
    /// 0 = All -> a single multi-type request
    /// (type=track,album,playlist); 1/2/3 = a single-type request
    /// for tracks/albums/playlists. Sections absent from the
    /// response parse as empty.
    pub fn search_catalog(
        &self,
        query: &str,
        kind: u8,
    ) -> Result<(Vec<Track>, Vec<AlbumRef>, Vec<PlaylistRef>), String> {
        let (types, limit) = match kind {
            1 => ("track", 10),
            2 => ("album", 8),
            3 => ("playlist", 8),
            _ => ("track,album,playlist", 10),
        };
        let v = self.get_json(&format!(
            "/v1/search?q={}&type={}&limit={}",
            encode_query(query),
            types,
            limit
        ))?;
        Ok((
            parse_spotify_search_tracks(&v),
            parse_spotify_albums(&v),
            parse_spotify_playlists(&v, "playlists"),
        ))
    }

    /// Tracks of one album (GET /v1/albums/{id}/tracks). Browse is
    /// deliberately a single page of up to 100 - no prefetching.
    pub fn album_tracks(&self, album_id: &str) -> Result<Vec<Track>, String> {
        let v = self.get_json(&format!("/v1/albums/{}/tracks?limit=100", album_id))?;
        Ok(parse_spotify_album_tracks(&v))
    }

    /// Tracks of one playlist (GET /v1/playlists/{id}/tracks).
    pub fn playlist_tracks(&self, playlist_id: &str) -> Result<Vec<Track>, String> {
        let v = self.get_json(&format!("/v1/playlists/{}/tracks?limit=100", playlist_id))?;
        Ok(parse_spotify_playlist_items(&v))
    }

    /// The display name of a playlist or album (Feature A: a pasted
    /// link opens under its real name, not under the raw URL/id).
    pub fn catalog_name(&self, kind: &str, id: &str) -> Result<String, String> {
        let v = self.get_json(&format!("/v1/{}s/{}?fields=name", kind, id))?;
        v.get("name")
            .and_then(|n| n.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .ok_or_else(|| guest_err("catalog name not found in the Spotify response"))
    }

    /// Trending for Home (Feature B): the public Top 50 - Global
    /// playlist's tracks, read with the anonymous token.
    pub fn trending_tracks(&self) -> Result<Vec<Track>, String> {
        self.playlist_tracks(TOP50_GLOBAL_ID)
    }

    /// The rate-budget-aware trending fetch (v0.1.7): the Top 50
    /// list is served from its disk cache while that cache is
    /// younger than the 6-hour TTL - ZERO API calls in that
    /// window, startup included. Past the TTL it refetches (unless
    /// a cooldown is running); a failed/rate-limited refresh falls
    /// back to the stale cache and says so in the returned note.
    /// Ok((tracks, note)): note is Some when the list shown is
    /// stale or rate-limited; Err only when nothing exists at all.
    pub fn trending_tracks_cached(&self) -> Result<(Vec<Track>, Option<String>), String> {
        let now = crate::config::unix_now_secs();
        let cached = crate::config::load_spotify_trending_cache();
        if let Some(c) = &cached {
            if crate::config::spotify_trending_cache_fresh(c, now) {
                return Ok((c.tracks.clone(), None));
            }
        }
        if let Some(until) = self.guest.cooldown_until() {
            if let Some(c) = &cached {
                return Ok((
                    c.tracks.clone(),
                    Some(format!(
                        "{} — Spotify Top 50 shown from the cache updated {}",
                        rate_limit_note(until),
                        crate::config::hhmm_of(c.fetched_at)
                    )),
                ));
            }
            return Err(rate_limit_note(until));
        }
        match self.trending_tracks() {
            Ok(tracks) => {
                if !tracks.is_empty() {
                    crate::config::save_spotify_trending_cache(&tracks, now);
                }
                Ok((tracks, None))
            }
            Err(e) => {
                if let Some(c) = &cached {
                    return Ok((
                        c.tracks.clone(),
                        Some(format!(
                            "Spotify Top 50 refresh failed — showing the cache updated {} ({})",
                            crate::config::hhmm_of(c.fetched_at),
                            e
                        )),
                    ));
                }
                Err(e)
            }
        }
    }

    /// Health probe for `lyre doctor`: can an anonymous token be
    /// fetched right now, and through which chain step? During a
    /// rate-limit cooldown the probe makes NO call and reports
    /// the cooldown instead (the doctor must not break the budget
    /// it reports on).
    pub fn probe(&self) -> Result<String, String> {
        if let Some(until) = self.guest.cooldown_until() {
            return Err(rate_limit_note(until));
        }
        let _ = self.guest.token(true)?;
        let (source, exp_ms, _, _) = self.guest.doctor_state();
        let source = source.unwrap_or_else(|| "get_access_token".to_string());
        if exp_ms > 0 {
            Ok(format!(
                "anonymous token fetched via {} (valid until {})",
                source,
                crate::config::hms_of(exp_ms / 1000)
            ))
        } else {
            Ok(format!("anonymous token fetched via {}", source))
        }
    }
}

impl PlayerBackend for SpotifyBackend {
    fn source(&self) -> Source {
        Source::Spotify
    }

    fn search(&self, query: &str) -> Result<Vec<Track>, String> {
        let (tracks, _, _) = self.search_catalog(query, 1)?;
        Ok(tracks)
    }

    fn resolve_stream(&self, _track: &Track, _settings: &Settings) -> Result<String, String> {
        // Not a gate: Spotify audio in Lyre is resolved through the
        // YouTube Music match path (PlayerBackend::resolve_match on the
        // YT backend), so direct Spotify stream resolution is simply
        // never used. The player routes Spotify tracks there instead.
        Err(
            "Spotify tracks are played via a YouTube Music audio match in Lyre; direct Spotify stream resolution is not used."
                .to_string(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn spotify_mixed_search_sections_parsing() {
        // One search response carrying the album + playlist sections.
        let v = json!({
            "albums": {"items": [
                {"id": "alb1", "name": "Monokrom", "artists": [{"name": "Tulus"}], "release_date": "2016-08-03"},
                {"id": "alb2", "name": "Manusia", "artists": [{"name": "Tulus"}], "release_date": "2022-03-03"}
            ]},
            "playlists": {"items": [
                {"id": "pl1", "name": "Senja & Kopi", "owner": {"display_name": "fathir"}, "tracks": {"total": 25}},
                null,
                {"id": "pl2", "name": "Indie Lokal", "owner": {"display_name": "Spotify"}, "tracks": {"total": 50}}
            ]}
        });
        let albums = parse_spotify_albums(&v);
        assert_eq!(albums.len(), 2);
        assert_eq!(albums[0].title, "Monokrom");
        assert_eq!(albums[0].artist, "Tulus");
        assert_eq!(albums[0].year, "2016");
        assert_eq!(albums[0].source, Source::Spotify);
        let pls = parse_spotify_playlists(&v, "playlists");
        assert_eq!(pls.len(), 2, "null entries are skipped: {:?}", pls);
        assert_eq!(pls[0].info, "by fathir • 25 tracks");
        // /me/playlists shape (items at the top level).
        let mine = parse_spotify_playlists(&json!({"items": [
            {"id": "pl9", "name": "Fokus Coding", "owner": {"display_name": "fathir"}, "tracks": {"total": 12}}
        ]}), "items");
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].title, "Fokus Coding");
    }

    #[test]
    fn anon_token_response_parsing() {
        // Real shape of open.spotify.com/get_access_token.
        let v = json!({
            "clientId": "65b708073fc0480ea92a077233ca87bd",
            "accessToken": "BQC-anonymous-token-value",
            "accessTokenExpirationTimestampMs": 1791800000000u64,
            "isAnonymous": true
        });
        let (tok, exp) = parse_anon_token(&v).expect("token parses");
        assert_eq!(tok, "BQC-anonymous-token-value");
        assert_eq!(exp, 1791800000000u64);
        // Missing/empty token -> None (caller shows the guest note).
        assert!(parse_anon_token(&json!({"isAnonymous": true})).is_none());
        assert!(parse_anon_token(&json!({"accessToken": ""})).is_none());
        assert!(parse_anon_token(&json!({})).is_none());
    }

    /// Scripted SpotHttp: serves queued (status, body) responses and
    /// records every (url, bearer) call so the 401-refresh flow can
    /// be asserted exactly.
    struct StubHttp {
        script: Mutex<Vec<(u16, Value)>>,
    }

    impl StubHttp {
        fn new(script: Vec<(u16, Value)>) -> StubHttp {
            StubHttp {
                script: Mutex::new(script),
            }
        }
    }

    impl SpotHttp for StubHttp {
        fn get(&self, _url: &str, _bearer: Option<&str>) -> Result<(u16, Value), String> {
            let mut script = self.script.lock().unwrap();
            if script.is_empty() {
                return Err("stub: no scripted response left".to_string());
            }
            Ok(script.remove(0))
        }
    }

    fn far_future_ms() -> u64 {
        now_ms() + 3_600_000
    }

    #[test]
    fn guest_client_401_refresh_then_retry_once() {
        let stub = StubHttp::new(vec![
            // 1: token fetch -> token A
            (200, json!({"accessToken": "TOKEN-A", "accessTokenExpirationTimestampMs": far_future_ms(), "isAnonymous": true})),
            // 2: API call with A -> 401
            (401, json!({"error": {"status": 401, "message": "The access token expired"}})),
            // 3: forced token fetch -> token B
            (200, json!({"accessToken": "TOKEN-B", "accessTokenExpirationTimestampMs": far_future_ms(), "isAnonymous": true})),
            // 4: retry with B -> 200 payload
            (200, json!({"tracks": {"items": []}})),
        ]);
        let calls = std::sync::Arc::new(Mutex::new(Vec::new()));
        // Wrap the stub so the test can inspect the call log.
        struct Shared(std::sync::Arc<StubHttp>, std::sync::Arc<Mutex<Vec<(String, Option<String>)>>>);
        impl SpotHttp for Shared {
            fn get(&self, url: &str, bearer: Option<&str>) -> Result<(u16, Value), String> {
                self.1.lock().unwrap().push((url.to_string(), bearer.map(|b| b.to_string())));
                let mut script = self.0.script.lock().unwrap();
                if script.is_empty() {
                    return Err("stub: exhausted".to_string());
                }
                Ok(script.remove(0))
            }
        }
        let stub = std::sync::Arc::new(stub);
        let guest = SpotifyGuest::with_http(Box::new(Shared(stub, calls.clone())));
        let v = guest.get_json("/v1/search?q=x&type=track").expect("401 then retry must succeed");
        assert_eq!(v["tracks"]["items"], json!([]));
        let log = calls.lock().unwrap();
        assert_eq!(log.len(), 4, "token, api(401), token(refresh), api(retry): {:?}", *log);
        assert!(log[0].0.contains("get_access_token"), "first call is the token fetch: {}", log[0].0);
        assert_eq!(log[1].1.as_deref(), Some("TOKEN-A"));
        assert_eq!(log[3].1.as_deref(), Some("TOKEN-B"), "retry carries the refreshed token");
        // A second 401 is NOT retried forever: it surfaces as an error.
        let stub2 = StubHttp::new(vec![
            (200, json!({"accessToken": "T1", "accessTokenExpirationTimestampMs": far_future_ms()})),
            (401, json!({"error": {"status": 401, "message": "bad"}})),
            (200, json!({"accessToken": "T2", "accessTokenExpirationTimestampMs": far_future_ms()})),
            (401, json!({"error": {"status": 401, "message": "still bad"}})),
        ]);
        let guest2 = SpotifyGuest::with_http(Box::new(stub2));
        let err = guest2.get_json("/v1/search?q=x").unwrap_err();
        assert!(err.contains(SPOTIFY_GUEST_NOTE), "error carries the guest note: {}", err);
    }

    #[test]
    fn embed_token_extraction_fixtures() {
        // Synthetic embed page (NEVER a real token): the session
        // JSON sits in a __NEXT_DATA__-style script block.
        let html = r#"<html><head><script id="__NEXT_DATA__" type="application/json">{"props":{"pageProps":{"state":{"data":{"session":{"accessToken":"BQSyntheticEmbedToken123","accessTokenExpirationTimestampMs":1791900000000,"isAnonymous":true}}}}}}</script></head></html>"#;
        let (tok, exp) = parse_embed_token(html).expect("embed token parses");
        assert_eq!(tok, "BQSyntheticEmbedToken123");
        assert_eq!(exp, 1791900000000u64);
        // &quot;-escaped variant parses too.
        let escaped = "&quot;accessToken&quot;:&quot;BQEscapedToken&quot;,&quot;accessTokenExpirationTimestampMs&quot;:1791900000000";
        let (tok2, exp2) = parse_embed_token(escaped).expect("escaped embed token parses");
        assert_eq!(tok2, "BQEscapedToken");
        assert_eq!(exp2, 1791900000000u64);
        // No token field -> None; empty token -> None.
        assert!(parse_embed_token("<html>no session here</html>").is_none());
        assert!(parse_embed_token(r#"{"accessToken":""}"#).is_none());
    }

    /// SpotHttp stub for the token chain: text fetches are answered
    /// per URL kind and the call order is recorded.
    struct ChainHttp {
        calls: std::sync::Arc<Mutex<Vec<String>>>,
        token_status: u16,
        embed_html: String,
    }

    impl ChainHttp {
        fn new(token_status: u16, embed_html: String) -> (ChainHttp, std::sync::Arc<Mutex<Vec<String>>>) {
            let calls = std::sync::Arc::new(Mutex::new(Vec::new()));
            (
                ChainHttp {
                    calls: calls.clone(),
                    token_status,
                    embed_html,
                },
                calls,
            )
        }
    }

    impl SpotHttp for ChainHttp {
        fn get(&self, _url: &str, _bearer: Option<&str>) -> Result<(u16, Value), String> {
            Err("unused in the chain test".to_string())
        }
        fn get_text(&self, url: &str) -> Result<(u16, String), String> {
            self.calls.lock().unwrap().push(url.to_string());
            if url.contains("get_access_token") {
                Ok((self.token_status, "Access Denied - Reference #18.abc123".to_string()))
            } else {
                Ok((200, self.embed_html.clone()))
            }
        }
    }

    #[test]
    fn token_chain_falls_back_to_embed_on_403() {
        let html = r#"{"accessToken":"BQEmbedChainToken","accessTokenExpirationTimestampMs":1791900000000}"#.to_string();
        let (stub, calls) = ChainHttp::new(403, html);
        let guest = SpotifyGuest::with_http(Box::new(stub));
        let tok = guest.token(false).expect("embed fallback must yield a token");
        assert_eq!(tok, "BQEmbedChainToken");
        let log = calls.lock().unwrap();
        assert_eq!(log.len(), 2, "direct first, embed second: {:?}", *log);
        assert!(log[0].contains("get_access_token"), "first call: {}", log[0]);
        assert!(log[1].contains("/embed/"), "second call: {}", log[1]);
        drop(log);
        // Source recorded for the doctor probe.
        assert_eq!(guest.state.lock().unwrap().source, Some("embed page"));
        // Direct success: the embed page is never fetched.
        let (stub_ok, calls_ok) = ChainHttp::new(200, String::new());
        // token_status 200 returns the Akamai text body, not JSON -
        // use the JSON-scripted stub for the direct-success shape:
        drop(stub_ok);
        drop(calls_ok);
        let stub_json = StubHttp::new(vec![(
            200,
            json!({"accessToken": "BQDirectToken", "accessTokenExpirationTimestampMs": far_future_ms()}),
        )]);
        let guest_ok = SpotifyGuest::with_http(Box::new(stub_json));
        assert_eq!(guest_ok.token(false).unwrap(), "BQDirectToken");
        assert_eq!(guest_ok.state.lock().unwrap().source, Some("get_access_token"));
        // Both steps blocked -> the error names the failing step.
        let (stub2, _) = ChainHttp::new(403, "Access Denied".to_string());
        let guest2 = SpotifyGuest::with_http(Box::new(stub2));
        let err = guest2.token(false).unwrap_err();
        assert!(err.contains("embed page"), "error names the step: {}", err);
    }

    #[test]
    fn spotify_block_log_names_step_and_status_without_token() {
        let _env_guard = crate::diag::env_lock();
        let line = spotify_block_line(
            "token",
            "/get_access_token",
            403,
            "<html>Access Denied Reference #18.abc123 xyz</html>",
        );
        assert!(line.contains("step=token"), "line: {}", line);
        assert!(line.contains("status=403"), "line: {}", line);
        assert!(line.contains("Reference #18.abc123"), "line: {}", line);
        assert!(!line.contains("BQ"), "no token-shaped value in the line: {}", line);
        // End-to-end: with a sandboxed HOME, a blocked token fetch
        // lands exactly this kind of line in lyre.log, and the
        // WORKING embed token never appears in the file.
        let home = std::env::temp_dir().join(format!("lyre-log-home-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let prev_home = std::env::var("HOME").ok();
        let prev_xdg = std::env::var("XDG_CONFIG_HOME").ok();
        std::env::set_var("HOME", &home);
        std::env::remove_var("XDG_CONFIG_HOME");
        let (stub, _calls) = ChainHttp::new(
            403,
            r#"{"accessToken":"BQSecretWorkingToken","accessTokenExpirationTimestampMs":1791900000000}"#.to_string(),
        );
        let guest = SpotifyGuest::with_http(Box::new(stub));
        let tok = guest.token(false).expect("embed token works");
        assert_eq!(tok, "BQSecretWorkingToken");
        let log_text = std::fs::read_to_string(home.join(".config").join("lyre").join("lyre.log"))
            .expect("lyre.log written under the sandbox HOME");
        match prev_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        match prev_xdg {
            Some(x) => std::env::set_var("XDG_CONFIG_HOME", x),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        assert!(log_text.contains("step=token"), "log: {}", log_text);
        assert!(log_text.contains("status=403"), "log: {}", log_text);
        assert!(
            !log_text.contains("BQSecretWorkingToken"),
            "the working token must never be logged: {}",
            log_text
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn ytdlp_fallback_title_cleanup() {
        // Fathir's exact example (v0.1.4 screenshot): artist first,
        // promo bracket stripped, trailing label dropped because
        // the uploader is the artist's own channel.
        let (title, artist) = clean_video_title(
            "Rumahsakit - Sandiwara Semu (Official Lyric Video) - Off The Records",
            "officialrumahsakit",
        );
        assert_eq!(artist, "Rumahsakit");
        assert_eq!(title, "Sandiwara Semu");
        // More of his real result rows:
        let (t, a) = clean_video_title("Rumahsakit - Panasea (Official Music Video)", "officialrumahsakit");
        assert_eq!((t.as_str(), a.as_str()), ("Panasea", "Rumahsakit"));
        let (t, a) = clean_video_title("Rumahsakit - Kuning (Official Lyric Video)", "Off The Records");
        assert_eq!((t.as_str(), a.as_str()), ("Kuning", "Rumahsakit"));
        let (t, a) = clean_video_title("Panasea - Rumahsakit (Lirik Lagu)", "DeLirik");
        assert_eq!((t.as_str(), a.as_str()), ("Rumahsakit", "Panasea"));
        // No separator: the whole thing is the title, artist = the
        // channel (with " - Topic" stripped).
        let (t, a) = clean_video_title("Sandiwara Semu", "Rumahsakit - Topic");
        assert_eq!((t.as_str(), a.as_str()), ("Sandiwara Semu", "Rumahsakit"));
        // Meaningful brackets are NOT promo and survive.
        let (t, a) = clean_video_title("Radiohead - Creep (Acoustic)", "Radiohead");
        assert_eq!((t.as_str(), a.as_str()), ("Creep (Acoustic)", "Radiohead"));
        // Three segments where the last is part of the title (the
        // uploader is unrelated): middle+last stay joined.
        let (t, a) = clean_video_title("Artis - Judul - Bagian Dua", "somechannel");
        assert_eq!((t.as_str(), a.as_str()), ("Judul - Bagian Dua", "Artis"));
    }

    // -----------------------------------------------------------------
    // v0.1.7: rate-budget discipline (Retry-After cooldown, call
    // volume, persistence, trending TTL cache)
    // -----------------------------------------------------------------

    #[test]
    fn retry_after_parsing_defaults_and_clamps() {
        assert_eq!(parse_retry_after(None), 60, "absent -> 60s default");
        assert_eq!(parse_retry_after(Some("120")), 120);
        assert_eq!(parse_retry_after(Some(" 45 ")), 45, "whitespace tolerated");
        assert_eq!(parse_retry_after(Some("soon")), 60, "unparseable -> default");
        assert_eq!(parse_retry_after(Some("0")), 60, "zero -> default");
        assert_eq!(parse_retry_after(Some("99999")), 3600, "huge -> 1h clamp");
        let note = rate_limit_note(now_ms() + 120_000);
        assert!(note.starts_with("Spotify rate limited — retry at "), "note: {}", note);
        let line = rate_limit_log_line("api", "/v1/search", 429, 120, now_ms() + 120_000);
        assert!(line.contains("step=api") && line.contains("status=429") && line.contains("retry_after=120"), "line: {}", line);
    }

    /// Counting SpotHttp for the rate-limit tests: token fetches
    /// (no Bearer) always succeed; API calls follow the scripted
    /// mode - 0 = HTTP 429 with the configured Retry-After,
    /// 1 = HTTP 200 with a combined search payload.
    struct RateStub {
        api_calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        mode: std::sync::Arc<std::sync::atomic::AtomicU8>,
        retry_after: Option<String>,
        last_api_url: std::sync::Arc<Mutex<String>>,
    }

    impl RateStub {
        fn new(
            mode: u8,
            retry_after: Option<&str>,
        ) -> (RateStub, std::sync::Arc<std::sync::atomic::AtomicUsize>, std::sync::Arc<std::sync::atomic::AtomicU8>, std::sync::Arc<Mutex<String>>) {
            let api_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let mode_a = std::sync::Arc::new(std::sync::atomic::AtomicU8::new(mode));
            let last_api_url = std::sync::Arc::new(Mutex::new(String::new()));
            (
                RateStub {
                    api_calls: api_calls.clone(),
                    mode: mode_a.clone(),
                    retry_after: retry_after.map(|s| s.to_string()),
                    last_api_url: last_api_url.clone(),
                },
                api_calls,
                mode_a,
                last_api_url,
            )
        }

        fn handle(&self, url: &str, bearer: Option<&str>) -> (u16, Value, Option<String>) {
            if bearer.is_none() {
                // Token endpoint (fetched as text; the JSON value
                // serializes through the trait's default get_text).
                return (
                    200,
                    json!({"accessToken": "RATE-STUB-TOKEN", "accessTokenExpirationTimestampMs": far_future_ms(), "isAnonymous": true}),
                    None,
                );
            }
            self.api_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            *self.last_api_url.lock().unwrap() = url.to_string();
            if url.contains("/v1/playlists/") {
                return (
                    200,
                    json!({"items": [{"track": {"id": "tp1", "name": "Top One", "artists": [{"name": "A"}], "duration_ms": 1000}}]}),
                    None,
                );
            }
            match self.mode.load(std::sync::atomic::Ordering::SeqCst) {
                0 => (
                    429,
                    json!({"error": {"status": 429, "message": "rate limited"}}),
                    self.retry_after.clone(),
                ),
                _ => (
                    200,
                    json!({
                        "tracks": {"items": [{"id": "st1", "name": "Stub Track", "artists": [{"name": "Stub Artist"}], "duration_ms": 1000}]},
                        "albums": {"items": [{"id": "sa1", "name": "Stub Album", "artists": [{"name": "Stub Artist"}], "release_date": "2024-01-01"}]},
                        "playlists": {"items": [{"id": "sp1", "name": "Stub Playlist", "owner": {"display_name": "stub"}, "tracks": {"total": 3}}]}
                    }),
                    None,
                ),
            }
        }
    }

    impl SpotHttp for RateStub {
        fn get(&self, url: &str, bearer: Option<&str>) -> Result<(u16, Value), String> {
            let (s, v, _) = self.handle(url, bearer);
            Ok((s, v))
        }
        fn get_resp(&self, url: &str, bearer: Option<&str>) -> Result<(u16, Value, Option<String>), String> {
            Ok(self.handle(url, bearer))
        }
    }

    #[test]
    fn rate_limit_429_sets_cooldown_and_blocks_further_calls() {
        // (a) 429 + Retry-After: 120 -> cooldown recorded, the note
        // names the retry time, and later searches make ZERO calls.
        let (stub, api_calls, _mode, _url) = RateStub::new(0, Some("120"));
        let backend = SpotifyBackend {
            guest: std::sync::Arc::new(SpotifyGuest::with_http(Box::new(stub))),
        };
        let err = backend.search_catalog("anything", 0).unwrap_err();
        assert!(err.contains("Spotify rate limited — retry at "), "note: {}", err);
        assert_eq!(api_calls.load(std::sync::atomic::Ordering::SeqCst), 1, "exactly one API call");
        let until = backend.guest.cooldown_until().expect("cooldown recorded");
        let remain = until - now_ms();
        assert!(remain > 110_000 && remain <= 120_000, "cooldown ~120s: {}ms", remain);
        let err2 = backend.search_catalog("anything", 0).unwrap_err();
        assert!(err2.contains("Spotify rate limited — retry at "), "note: {}", err2);
        assert_eq!(
            api_calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "ZERO api calls while the cooldown runs"
        );
        // Browse is gated by the same choke point.
        assert!(backend.playlist_tracks("x").is_err());
        assert_eq!(api_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn cooldown_survives_a_simulated_restart() {
        // (b) state file round-trip: a fresh guest over the same
        // state file starts in cooldown and makes no calls.
        let dir = std::env::temp_dir().join(format!("lyre-spotify-state-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("spotify_state.json");
        let (stub, _calls, _m, _u) = RateStub::new(0, Some("120"));
        let guest1 = SpotifyGuest::with_http_persist(Box::new(stub), path.clone());
        let backend1 = SpotifyBackend {
            guest: std::sync::Arc::new(guest1),
        };
        assert!(backend1.search_catalog("x", 0).is_err());
        let saved = std::fs::read_to_string(&path).expect("state file written");
        assert!(saved.contains("cooldown_until_ms"), "state: {}", saved);
        assert!(
            !saved.contains("RATE-STUB-TOKEN"),
            "the token VALUE is never persisted: {}",
            saved
        );
        drop(backend1);
        // "Restart": a brand-new guest over the same file.
        let (stub2, api_calls2, _m2, _u2) = RateStub::new(1, None);
        let guest2 = SpotifyGuest::with_http_persist(Box::new(stub2), path.clone());
        assert!(guest2.cooldown_until().is_some(), "cooldown loaded from disk");
        let backend2 = SpotifyBackend {
            guest: std::sync::Arc::new(guest2),
        };
        let err = backend2.search_catalog("x", 0).unwrap_err();
        assert!(err.contains("Spotify rate limited"), "err: {}", err);
        assert_eq!(
            api_calls2.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "no API call after restart while cooling down"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn all_kinds_search_is_exactly_one_request() {
        // (c) kind = All -> ONE multi-type request; a specific kind
        // -> ONE single-type request.
        let (stub, api_calls, _m, last_url) = RateStub::new(1, None);
        let backend = SpotifyBackend {
            guest: std::sync::Arc::new(SpotifyGuest::with_http(Box::new(stub))),
        };
        let (tracks, albums, playlists) = backend.search_catalog("stub", 0).expect("search works");
        assert_eq!(api_calls.load(std::sync::atomic::Ordering::SeqCst), 1, "All = exactly 1 request");
        assert!(last_url.lock().unwrap().contains("type=track,album,playlist"), "url: {}", last_url.lock().unwrap());
        assert_eq!(tracks.len(), 1);
        assert_eq!(albums.len(), 1);
        assert_eq!(playlists.len(), 1);

        let (stub2, api_calls2, _m2, last_url2) = RateStub::new(1, None);
        let backend2 = SpotifyBackend {
            guest: std::sync::Arc::new(SpotifyGuest::with_http(Box::new(stub2))),
        };
        let (tracks2, albums2, playlists2) = backend2.search_catalog("stub", 2).expect("album search works");
        assert_eq!(api_calls2.load(std::sync::atomic::Ordering::SeqCst), 1, "Albums = exactly 1 request");
        assert!(last_url2.lock().unwrap().contains("type=album&"), "url: {}", last_url2.lock().unwrap());
        assert_eq!(albums2.len(), 1);
        // The stub answers all sections regardless; the app only
        // displays the requested kind. Tracks parse from the same
        // single response (no extra call was needed).
        assert_eq!(tracks2.len(), 1);
        assert_eq!(playlists2.len(), 1);
    }

    #[test]
    fn search_after_cooldown_expiry_calls_once_and_succeeds() {
        // (f) Retry-After: 1 -> wait out the cooldown -> the next
        // search probes exactly once and succeeds on the flipped stub.
        let (stub, api_calls, mode, _u) = RateStub::new(0, Some("1"));
        let backend = SpotifyBackend {
            guest: std::sync::Arc::new(SpotifyGuest::with_http(Box::new(stub))),
        };
        assert!(backend.search_catalog("x", 0).is_err());
        assert_eq!(api_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        std::thread::sleep(std::time::Duration::from_millis(1300));
        assert!(backend.guest.cooldown_until().is_none(), "cooldown expired");
        mode.store(1, std::sync::atomic::Ordering::SeqCst);
        let (tracks, _, _) = backend.search_catalog("x", 0).expect("post-cooldown search succeeds");
        assert_eq!(tracks.len(), 1);
        assert_eq!(
            api_calls.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "exactly one probe call after expiry - no retry storm"
        );
    }

    #[test]
    fn trending_cache_ttl_zero_calls_and_stale_during_cooldown() {
        // (d) within the 6h TTL: zero requests. Stale + cooldown:
        // the stale cache is served, still zero requests.
        let _env_guard = crate::diag::env_lock();
        let home = std::env::temp_dir().join(format!("lyre-trending-home-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let prev_home = std::env::var("HOME").ok();
        let prev_xdg = std::env::var("XDG_CONFIG_HOME").ok();
        std::env::set_var("HOME", &home);
        std::env::remove_var("XDG_CONFIG_HOME");
        let restore = |prev_home: &Option<String>, prev_xdg: &Option<String>| {
            match prev_home {
                Some(h) => std::env::set_var("HOME", h),
                None => std::env::remove_var("HOME"),
            }
            match prev_xdg {
                Some(x) => std::env::set_var("XDG_CONFIG_HOME", x),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
        };
        let fixture = vec![
            Track::new("ct1", "Cached One", "Artist A", 100, Source::Spotify),
            Track::new("ct2", "Cached Two", "Artist B", 100, Source::Spotify),
        ];
        // Fresh cache (fetched now): no API call at all.
        crate::config::save_spotify_trending_cache(&fixture, crate::config::unix_now_secs());
        let (stub, api_calls, _m, _u) = RateStub::new(1, None);
        let backend = SpotifyBackend {
            guest: std::sync::Arc::new(SpotifyGuest::with_http(Box::new(stub))),
        };
        let (tracks, note) = backend.trending_tracks_cached().expect("fresh cache serves");
        assert_eq!(tracks.len(), 2);
        assert!(note.is_none(), "fresh cache needs no note: {:?}", note);
        assert_eq!(api_calls.load(std::sync::atomic::Ordering::SeqCst), 0, "TTL window = zero requests");
        // Stale cache (7h old) + a live cooldown: serve stale, no call.
        crate::config::save_spotify_trending_cache(&fixture, crate::config::unix_now_secs() - 7 * 3600);
        {
            let mut st = backend.guest.state.lock().unwrap();
            st.cooldown_until_ms = now_ms() + 600_000;
        }
        let (tracks2, note2) = backend.trending_tracks_cached().expect("stale cache serves during cooldown");
        assert_eq!(tracks2.len(), 2);
        let note2 = note2.expect("stale + cooldown carries a note");
        assert!(note2.contains("Spotify rate limited — retry at "), "note: {}", note2);
        assert!(note2.contains("cache updated"), "note names the cache time: {}", note2);
        assert_eq!(api_calls.load(std::sync::atomic::Ordering::SeqCst), 0, "cooldown = zero requests");
        restore(&prev_home, &prev_xdg);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn spotify_album_and_playlist_browse_parsing() {
        let album = json!({"items": [
            {"id": "t1", "name": "Sepatu", "artists": [{"name": "Tulus"}], "duration_ms": 189000},
            {"id": "t2", "name": "Monokrom", "artists": [{"name": "Tulus"}], "duration_ms": 234000}
        ]});
        let tracks = parse_spotify_album_tracks(&album);
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[1].title, "Monokrom");
        assert_eq!(tracks[1].duration_secs, 234);
        assert_eq!(tracks[1].source, Source::Spotify);

        let playlist = json!({"items": [
            {"track": {"id": "t9", "name": "As It Was", "artists": [{"name": "Harry Styles"}], "duration_ms": 167000}},
            {"track": null},
            {"added_at": "2026-01-01"}
        ]});
        let ptracks = parse_spotify_playlist_items(&playlist);
        assert_eq!(ptracks.len(), 1, "null/removed tracks skipped: {:?}", ptracks);
        assert_eq!(ptracks[0].title, "As It Was");
        // Empty/garbage responses are empty lists, never panics.
        assert!(parse_spotify_album_tracks(&json!({})).is_empty());
        assert!(parse_spotify_playlist_items(&json!({"items": []})).is_empty());
    }
}
