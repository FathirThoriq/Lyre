//! Guest YouTube Music Innertube client - it reads PUBLIC catalog
//! data from the very API the YouTube Music web app itself uses
//! (music.youtube.com/youtubei/v1), the same approach as ytmusicapi.
//!
//! This is YouTube Music's INTERNAL API: unofficial, undocumented,
//! and it may change without notice. Every failure here is a soft
//! failure for Lyre - the status line reports it and yt-dlp search /
//! playback keeps working untouched. Nothing in this module ever
//! fabricates catalog data.
//!
//! v0.1.5: Lyre is guest-only (Fathir's decision - logins removed),
//! so EVERY call here is a guest call: no Authorization header, no
//! token anywhere. The request shape mirrors ytmusicapi's CURRENT
//! source (checked 2026-10-16 against helpers.py / ytmusic.py /
//! constants.py on GitHub):
//! - context: {"client": {"clientName": "WEB_REMIX",
//!   "clientVersion": "1.<UTC YYYYMMDD>.01.00"}, "user": {}} - the
//!   clientVersion is generated from TODAY's date exactly like
//!   ytmusicapi's initialize_context() does; the previous build
//!   pinned a stale static version string.
//! - headers: its initialize_headers() set (Firefox UA, Accept */*,
//!   Accept-Encoding, Content-Type, Origin) plus a fetched
//!   X-Goog-Visitor-Id and X-Goog-Request-Time, and the SOCS=CAI
//!   cookie ytmusicapi's session sends.
//! - URL params: "?alt=json" only - ytmusicapi appends the API key
//!   solely for browser-cookie auth, never for guest/OAuth calls.
//!   One resilience retry with the key appended still exists below.
//!
//! Why guest-only matters (the v0.1.4 HTTP 400): Fathir's build was
//! logged in, so every Innertube call carried the TV device-flow
//! Bearer token, and Google answered the data calls with HTTP 400
//! (the pattern tracked in ytmusicapi issue #676: device-flow TV
//! tokens are rejected by the WEB_REMIX data API). The same request
//! shapes WITHOUT the token were verified live from this VM on
//! 2026-10-16: songs/albums/playlists search, album + playlist
//! browse and the charts browse all returned HTTP 200 with real
//! items. Guest calls are therefore both the fix and the design.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::diag;

/// YouTube Music web app origin / API root.
pub const YTM_ORIGIN: &str = "https://music.youtube.com";
pub const YTM_BASE_API: &str = "https://music.youtube.com/youtubei/v1";
/// Public WEB_REMIX API key, as published in ytmusicapi's constants.
/// Guest calls do NOT send it (matching ytmusicapi); it is used only
/// by the one resilience retry in innertube_post.
pub const WEB_REMIX_API_KEY: &str = "AIzaSyC9XL3ZjWddXya6X74dJoCTL-WEYFDNX30";
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:88.0) Gecko/20100101 Firefox/88.0";

// ---------------------------------------------------------------------------
// Context / payload construction (pure, unit-tested)
// ---------------------------------------------------------------------------

/// Days-from-civil inverse (Howard Hinnant's algorithm): unix day
/// number -> (year, month, day), proleptic Gregorian, UTC.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// WEB_REMIX clientVersion the way ytmusicapi builds it TODAY:
/// "1." + strftime("%Y%m%d") + ".01.00" - i.e. it changes daily and
/// never goes stale. `unix_secs` is a parameter so tests can pin it.
pub fn client_version_at(unix_secs: u64) -> String {
    let days = (unix_secs / 86_400) as i64;
    let (y, m, d) = civil_from_days(days);
    format!("1.{:04}{:02}{:02}.01.00", y, m, d)
}

pub fn client_version() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    client_version_at(now)
}

/// The Innertube context block, shaped exactly like ytmusicapi's
/// initialize_context(): client name + dynamic version, empty user.
pub fn build_context() -> Value {
    json!({
        "context": {
            "client": {
                "clientName": "WEB_REMIX",
                "clientVersion": client_version()
            },
            "user": {}
        }
    })
}

pub fn endpoint_url(endpoint: &str, with_key: bool) -> String {
    // Test-only override (PTY capture harness points this at a local
    // stub); production always uses the real API root.
    let base = std::env::var("LYRE_INNERTUBE_BASE").unwrap_or_else(|_| YTM_BASE_API.to_string());
    if with_key {
        format!("{}/{}?alt=json&key={}", base, endpoint, WEB_REMIX_API_KEY)
    } else {
        format!("{}/{}?alt=json", base, endpoint)
    }
}

/// Search `params` strings, constructed exactly as ytmusicapi's
/// get_search_params does with its default ignore_spelling=True:
/// filtered_param1 "EgWKAQ" + a per-filter param2 + the shared
/// param3; playlists use their special two-part form.
pub const SEARCH_PARAMS_SONGS: &str = "EgWKAQIIAUICCAFqDBAOEAoQAxAEEAkQBQ%3D%3D";
pub const SEARCH_PARAMS_ALBUMS: &str = "EgWKAQIYAUICCAFqDBAOEAoQAxAEEAkQBQ%3D%3D";
pub const SEARCH_PARAMS_PLAYLISTS: &str = "Eg-KAQwIABAAGAAgACgBMABCAggBagoQBBADEAkQBRAK";

/// Payload for a filtered search call.
pub fn search_payload(query: &str, params: &str) -> Value {
    let mut v = build_context();
    v["query"] = json!(query);
    v["params"] = json!(params);
    v
}

/// Payload for browsing an album/playlist by browseId.
pub fn browse_payload(browse_id: &str) -> Value {
    let mut v = build_context();
    v["browseId"] = json!(browse_id);
    v
}

/// Payload for the global charts landing (ytmusicapi get_charts
/// pattern: FEmusic_charts + formData country selection, ZZ =
/// global).
pub fn charts_payload() -> Value {
    let mut v = build_context();
    v["browseId"] = json!("FEmusic_charts");
    v["formData"] = json!({ "selectedValues": ["ZZ"] });
    v
}

/// The header set for a guest Innertube POST, mirroring
/// ytmusicapi's initialize_headers() + headers property. Pure so the
/// guest invariant is testable: there is NO Authorization entry,
/// ever - guest means guest.
pub fn request_headers(visitor: Option<&str>, now_secs: u64) -> Vec<(String, String)> {
    let mut h = vec![
        ("User-Agent".to_string(), USER_AGENT.to_string()),
        ("Accept".to_string(), "*/*".to_string()),
        ("Accept-Encoding".to_string(), "gzip, deflate".to_string()),
        ("Content-Type".to_string(), "application/json".to_string()),
        ("Origin".to_string(), YTM_ORIGIN.to_string()),
        ("X-Goog-Request-Time".to_string(), now_secs.to_string()),
        // The consent cookie ytmusicapi's session sends with every
        // request (self.cookies = {"SOCS": "CAI"}).
        ("Cookie".to_string(), "SOCS=CAI".to_string()),
    ];
    if let Some(v) = visitor {
        if !v.is_empty() {
            h.push(("X-Goog-Visitor-Id".to_string(), v.to_string()));
        }
    }
    h
}

/// Extract VISITOR_DATA from the music.youtube.com homepage HTML
/// (the ytcfg.set({...}) blob), the way ytmusicapi's get_visitor_id
/// does. Pure; returns None when the blob is absent.
pub fn parse_visitor_data(html: &str) -> Option<String> {
    let key = "\"VISITOR_DATA\":\"";
    let start = html.find(key)? + key.len();
    let rest = &html[start..];
    let end = rest.find('"')?;
    let v = &rest[..end];
    if v.is_empty() {
        None
    } else {
        Some(v.to_string())
    }
}

// ---------------------------------------------------------------------------
// Google error bodies: parsed, logged, surfaced - never just a status
// ---------------------------------------------------------------------------

/// A Google API error body distilled: HTTP code plus the body's
/// status / message / first reason. These are Google's own words -
/// safe to log and show (they never contain tokens).
#[derive(Debug, Clone, PartialEq)]
pub struct GoogleError {
    pub http: u16,
    pub status: String,
    pub message: String,
    pub reason: String,
}

impl GoogleError {
    pub fn describe(&self) -> String {
        let mut s = format!("HTTP {}", self.http);
        if !self.status.is_empty() {
            s.push_str(&format!(" {}", self.status));
        }
        if !self.reason.is_empty() && self.reason != self.status {
            s.push_str(&format!(" ({})", self.reason));
        }
        if !self.message.is_empty() {
            s.push_str(&format!(": {}", self.message));
        }
        s
    }
}

/// Parse a Google error JSON body: {"error": {"status", "message",
/// "errors": [{"reason", "domain", "message"}]}}. None when the body
/// is not that shape.
pub fn parse_google_error(http: u16, body: &Value) -> Option<GoogleError> {
    let err = body.get("error")?;
    let first = err
        .get("errors")
        .and_then(|e| e.as_array())
        .and_then(|a| a.first())
        .cloned()
        .unwrap_or(Value::Null);
    Some(GoogleError {
        http,
        status: err.get("status").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        message: err.get("message").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        reason: first.get("reason").and_then(|x| x.as_str()).unwrap_or("").to_string(),
    })
}

// ---------------------------------------------------------------------------
// Response parsing (pure, defensive, unit-tested on fixtures)
// ---------------------------------------------------------------------------

/// Rendered-text helper: Innertube text is either {"simpleText": "..."}
/// or {"runs": [{"text": ...}, ...]}.
fn text_of(v: &Value) -> Option<String> {
    if let Some(s) = v.get("simpleText").and_then(|x| x.as_str()) {
        return Some(s.to_string());
    }
    if let Some(runs) = v.get("runs").and_then(|x| x.as_array()) {
        let mut out = String::new();
        for r in runs {
            if let Some(t) = r.get("text").and_then(|x| x.as_str()) {
                out.push_str(t);
            }
        }
        if !out.is_empty() {
            return Some(out);
        }
    }
    None
}

/// Recursively walk every object in a JSON tree.
fn walk(v: &Value, f: &mut dyn FnMut(&Value)) {
    match v {
        Value::Object(map) => {
            f(v);
            for val in map.values() {
                walk(val, f);
            }
        }
        Value::Array(items) => {
            for item in items {
                walk(item, f);
            }
        }
        _ => {}
    }
}

use crate::track::{AlbumRef, PlaylistRef, Source, Track};

/// Collect every musicResponsiveListItemRenderer object in encounter
/// order (arrays keep their order; this walks the tree depth-first).
fn collect_list_items(v: &Value, out: &mut Vec<Value>) {
    match v {
        Value::Object(map) => {
            if let Some(item) = map.get("musicResponsiveListItemRenderer") {
                out.push(item.clone());
            }
            for val in map.values() {
                collect_list_items(val, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_list_items(item, out);
            }
        }
        _ => {}
    }
}

fn list_items(v: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    collect_list_items(v, &mut out);
    out
}

/// Text of flex column `i` of a list item ("" when absent).
fn flex_text(item: &Value, i: usize) -> String {
    item.get("flexColumns")
        .and_then(|c| c.as_array())
        .and_then(|c| c.get(i))
        .and_then(|c| c.get("musicResponsiveListItemFlexColumnRenderer"))
        .and_then(|c| c.get("text"))
        .and_then(text_of)
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// The non-separator run texts of flex column `i`.
fn flex_run_texts(item: &Value, i: usize) -> Vec<String> {
    let runs = item
        .get("flexColumns")
        .and_then(|c| c.as_array())
        .and_then(|c| c.get(i))
        .and_then(|c| c.get("musicResponsiveListItemFlexColumnRenderer"))
        .and_then(|c| c.get("text"))
        .and_then(|t| t.get("runs"))
        .and_then(|r| r.as_array());
    match runs {
        Some(runs) => runs
            .iter()
            .filter_map(|r| r.get("text").and_then(|t| t.as_str()))
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty() && t != "•" && t != "·")
            .collect(),
        None => Vec::new(),
    }
}

/// The videoId a list item plays: overlay play button first (the
/// path ytmusicapi uses), then any flex-column run's watch endpoint,
/// then playlistItemData.
fn item_video_id(item: &Value) -> Option<String> {
    let direct = item
        .get("overlay")
        .and_then(|o| o.get("musicItemThumbnailOverlayRenderer"))
        .and_then(|o| o.get("content"))
        .and_then(|c| c.get("musicPlayButtonRenderer"))
        .and_then(|b| b.get("playNavigationEndpoint"))
        .and_then(|e| e.get("watchEndpoint"))
        .and_then(|w| w.get("videoId"))
        .and_then(|v| v.as_str());
    if let Some(id) = direct {
        if !id.is_empty() {
            return Some(id.to_string());
        }
    }
    let mut found: Option<String> = None;
    walk(item, &mut |node| {
        if found.is_none() {
            if let Some(id) = node
                .get("watchEndpoint")
                .and_then(|w| w.get("videoId"))
                .and_then(|v| v.as_str())
            {
                if !id.is_empty() {
                    found = Some(id.to_string());
                }
            }
        }
    });
    if found.is_some() {
        return found;
    }
    item.get("playlistItemData")
        .and_then(|p| p.get("videoId"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// The item's own browse target (albums: MPRE..., playlists: VL...).
fn item_browse_id(item: &Value) -> Option<String> {
    item.get("navigationEndpoint")
        .and_then(|n| n.get("browseEndpoint"))
        .and_then(|b| b.get("browseId"))
        .and_then(|b| b.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// Parse an "m:ss" / "h:mm:ss" label into seconds.
fn parse_time_label(s: &str) -> Option<u64> {
    let parts: Vec<&str> = s.trim().split(':').collect();
    if parts.len() < 2 || parts.len() > 3 {
        return None;
    }
    let mut total: u64 = 0;
    for p in &parts {
        if p.is_empty() || !p.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        total = total * 60 + p.parse::<u64>().ok()?;
    }
    Some(total)
}

/// A track's duration: fixedColumns first (browse results), then a
/// time-shaped run in the info columns (search results).
fn item_duration(item: &Value) -> u64 {
    if let Some(fixed) = item.get("fixedColumns").and_then(|c| c.as_array()) {
        for col in fixed {
            if let Some(t) = col
                .get("musicResponsiveListItemFixedColumnRenderer")
                .and_then(|c| c.get("text"))
                .and_then(text_of)
            {
                if let Some(secs) = parse_time_label(&t) {
                    return secs;
                }
            }
        }
    }
    for col in 1..=3 {
        for t in flex_run_texts(item, col) {
            if let Some(secs) = parse_time_label(&t) {
                return secs;
            }
        }
    }
    0
}

/// The artist-ish text of a song item: the info column's runs minus
/// type labels ("Song"/"Video") and the duration; the first
/// remaining run is the artist (ytmusicapi's parse_song_runs order).
fn item_artist(item: &Value) -> String {
    for t in flex_run_texts(item, 1) {
        let low = t.to_lowercase();
        if low == "song" || low == "video" || low == "album" || low == "single" || low == "ep" {
            continue;
        }
        if parse_time_label(&t).is_some() {
            continue;
        }
        return t;
    }
    String::new()
}

/// Parse the songs out of a songs-filtered search response.
pub fn parse_search_songs(v: &Value) -> Vec<Track> {
    let mut out: Vec<Track> = Vec::new();
    for item in list_items(v) {
        let title = flex_text(&item, 0);
        let video_id = item_video_id(&item);
        let (title, video_id) = match (title.is_empty(), video_id) {
            (false, Some(id)) => (title, id),
            _ => continue,
        };
        if out.iter().any(|t: &Track| t.id == video_id) {
            continue;
        }
        out.push(Track::new(
            video_id,
            title,
            item_artist(&item),
            item_duration(&item),
            Source::YtMusic,
        ));
    }
    out
}

/// Parse the albums out of an albums-filtered search response:
/// items whose browseId starts with MPRE (ytmusicapi's mapping).
pub fn parse_search_albums(v: &Value) -> Vec<AlbumRef> {
    let mut out: Vec<AlbumRef> = Vec::new();
    for item in list_items(v) {
        let browse_id = match item_browse_id(&item) {
            Some(b) if b.starts_with("MPRE") => b,
            _ => continue,
        };
        let title = flex_text(&item, 0);
        if title.is_empty() || out.iter().any(|a: &AlbumRef| a.id == browse_id) {
            continue;
        }
        let texts = flex_run_texts(&item, 1);
        // Shape: [type, artist, year] after separator removal.
        let mut artist = String::new();
        let mut year = String::new();
        for (i, t) in texts.iter().enumerate() {
            if t.len() == 4 && t.chars().all(|c| c.is_ascii_digit()) {
                year = t.clone();
            } else if i > 0 && artist.is_empty() {
                artist = t.clone();
            }
        }
        out.push(AlbumRef {
            id: browse_id,
            title,
            artist,
            year,
            source: Source::YtMusic,
        });
    }
    out
}

/// Parse the playlists out of a playlists-filtered search response:
/// items whose browseId starts with VL. The info line is the
/// source's own subtitle text, kept verbatim.
pub fn parse_search_playlists(v: &Value) -> Vec<PlaylistRef> {
    let mut out: Vec<PlaylistRef> = Vec::new();
    for item in list_items(v) {
        let browse_id = match item_browse_id(&item) {
            Some(b) if b.starts_with("VL") => b,
            _ => continue,
        };
        let title = flex_text(&item, 0);
        if title.is_empty() || out.iter().any(|p: &PlaylistRef| p.id == browse_id) {
            continue;
        }
        out.push(PlaylistRef {
            id: browse_id,
            title,
            info: flex_text(&item, 1),
            source: Source::YtMusic,
        });
    }
    out
}

/// Parse the track list of an album/playlist browse response
/// (musicShelfRenderer / musicPlaylistShelfRenderer contents arrive
/// as list items with videoIds and fixed-column durations).
pub fn parse_browse_tracks(v: &Value) -> Vec<Track> {
    let mut out: Vec<Track> = Vec::new();
    for item in list_items(v) {
        let title = flex_text(&item, 0);
        let video_id = match item_video_id(&item) {
            Some(id) => id,
            None => continue,
        };
        if title.is_empty() || out.iter().any(|t: &Track| t.id == video_id) {
            continue;
        }
        out.push(Track::new(
            video_id,
            title,
            item_artist(&item),
            item_duration(&item),
            Source::YtMusic,
        ));
    }
    out
}

/// The display title of a playlist/album browse response: the
/// detail/immersive header's title, when present.
pub fn parse_browse_title(v: &Value) -> Option<String> {
    let mut title: Option<String> = None;
    walk(v, &mut |node| {
        if title.is_none() {
            for key in ["musicDetailHeaderRenderer", "musicImmersiveHeaderRenderer"] {
                if let Some(h) = node.get(key) {
                    if let Some(t) = h.get("title").and_then(text_of) {
                        if !t.trim().is_empty() {
                            title = Some(t.trim().to_string());
                            return;
                        }
                    }
                }
            }
        }
    });
    title
}

/// Chart playlists on the global charts landing: musicTwoRowItem
/// entries with VL browse targets: (title, browseId) in page order.
pub fn parse_chart_playlists(v: &Value) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    walk(v, &mut |node| {
        let renderer = match node.get("musicTwoRowItemRenderer") {
            Some(r) => r,
            None => return,
        };
        let browse_id = renderer
            .get("navigationEndpoint")
            .and_then(|n| n.get("browseEndpoint"))
            .and_then(|b| b.get("browseId"))
            .and_then(|b| b.as_str())
            .unwrap_or("")
            .to_string();
        if !browse_id.starts_with("VL") {
            return;
        }
        let title = renderer.get("title").and_then(text_of).unwrap_or_default().trim().to_string();
        if title.is_empty() || out.iter().any(|(_, b)| *b == browse_id) {
            return;
        }
        out.push((title, browse_id));
    });
    out
}

/// Pick the songs chart from the charts landing: prefer a "Top 100"
/// / top-songs title, else the first chart playlist (on the global
/// landing that is the daily top music videos chart).
pub fn pick_chart_playlist(charts: &[(String, String)]) -> Option<(String, String)> {
    for (title, id) in charts {
        let low = title.to_lowercase();
        if low.contains("top 100") || (low.contains("top") && low.contains("song")) {
            return Some((title.clone(), id.clone()));
        }
    }
    charts.first().cloned()
}

// ---------------------------------------------------------------------------
// Network (guest only)
// ---------------------------------------------------------------------------

/// The process-wide visitor id, fetched once from the homepage
/// (best-effort: a missing visitor id never blocks a call).
fn visitor_id() -> Option<String> {
    static CACHE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| {
            let root = match std::env::var("LYRE_INNERTUBE_BASE") {
                Ok(base) => {
                    // Scheme + host of the override (stub server).
                    let parts: Vec<&str> = base.split('/').collect();
                    if parts.len() >= 3 {
                        format!("{}//{}", parts[0], parts[2])
                    } else {
                        base
                    }
                }
                Err(_) => YTM_ORIGIN.to_string(),
            };
            let resp = ureq::get(&format!("{}/", root))
                .set("User-Agent", USER_AGENT)
                .set("Accept", "*/*")
                .timeout(Duration::from_secs(8))
                .call()
                .ok()?;
            let html = resp.into_string().ok()?;
            parse_visitor_data(&html)
        })
        .clone()
}

fn post_attempt(endpoint: &str, payload: &Value, with_key: bool) -> Result<Value, String> {
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut req = ureq::post(&endpoint_url(endpoint, with_key));
    for (k, v) in request_headers(visitor_id().as_deref(), now_secs) {
        req = req.set(&k, &v);
    }
    match req.timeout(Duration::from_secs(15)).send_json(payload.clone()) {
        Ok(resp) => resp
            .into_json()
            .map_err(|e| format!("Innertube {} response could not be read: {}", endpoint, e)),
        Err(ureq::Error::Status(code, resp)) => {
            // Google explains itself in the body - read it, distill
            // it, log it, surface it. "status code 400" alone was
            // the v0.1.4 diagnostic dead end.
            let body_text = resp.into_string().unwrap_or_default();
            let body: Value = serde_json::from_str(&body_text).unwrap_or(Value::Null);
            match parse_google_error(code, &body) {
                Some(ge) => Err(format!("Innertube {}: {}", endpoint, ge.describe())),
                None => Err(format!(
                    "Innertube {}: HTTP {} (error body is not Google API shaped: {})",
                    endpoint,
                    code,
                    body_text.chars().take(120).collect::<String>()
                )),
            }
        }
        Err(e) => Err(format!("Innertube {} failed: {}", endpoint, e)),
    }
}

/// One guest Innertube POST. If the plain ytmusicapi-style call
/// fails, ONE resilience retry goes out with the public API key
/// appended (the pre-2024 ytmusicapi URL shape) before the first
/// error is surfaced; both attempts' errors land in lyre.log with
/// Google's own reason text.
fn innertube_post(endpoint: &str, payload: &Value) -> Result<Value, String> {
    match post_attempt(endpoint, payload, false) {
        Ok(v) => Ok(v),
        Err(first) => {
            diag::append_log(&format!("innertube {} (guest): {}", endpoint, first));
            match post_attempt(endpoint, payload, true) {
                Ok(v) => {
                    diag::append_log(&format!(
                        "innertube {}: retry with key succeeded",
                        endpoint
                    ));
                    Ok(v)
                }
                Err(second) => {
                    diag::append_log(&format!("innertube {} (guest+key): {}", endpoint, second));
                    Err(first)
                }
            }
        }
    }
}

/// Everything one unified guest YTM search found. `notes` carries
/// inline, per-filter failure notes (a filter failing never fails
/// the whole search and never fabricates entries).
#[derive(Debug, Clone, Default)]
pub struct YtmSearch {
    pub tracks: Vec<Track>,
    pub albums: Vec<AlbumRef>,
    pub playlists: Vec<PlaylistRef>,
    pub notes: Vec<String>,
}

/// Unified YouTube Music search: three filtered guest Innertube
/// calls (ytmusicapi's approach). Fully public catalog data - no
/// token exists in guest-only Lyre.
pub fn search_all(query: &str) -> Result<YtmSearch, String> {
    let mut out = YtmSearch::default();
    let mut errors: Vec<String> = Vec::new();
    let calls: [(&str, &str); 3] = [
        ("songs", SEARCH_PARAMS_SONGS),
        ("albums", SEARCH_PARAMS_ALBUMS),
        ("playlists", SEARCH_PARAMS_PLAYLISTS),
    ];
    for (label, params) in calls {
        match innertube_post("search", &search_payload(query, params)) {
            Ok(v) => match label {
                "songs" => out.tracks = parse_search_songs(&v),
                "albums" => out.albums = parse_search_albums(&v),
                _ => out.playlists = parse_search_playlists(&v),
            },
            Err(e) => {
                out.notes.push(format!("YT Music ({}): {}", label, e));
                errors.push(e);
            }
        }
    }
    if out.tracks.is_empty() && out.albums.is_empty() && out.playlists.is_empty() && !errors.is_empty() {
        return Err(errors.remove(0));
    }
    Ok(out)
}

/// Browse an album/playlist browseId into its track list (guest).
pub fn browse_tracks(browse_id: &str) -> Result<Vec<Track>, String> {
    let v = innertube_post("browse", &browse_payload(browse_id))?;
    Ok(parse_browse_tracks(&v))
}

/// Browse a playlist/album and also return its real display title
/// (Feature A: a pasted link must open under the playlist's own
/// name, not under the URL).
pub fn browse_playlist(browse_id: &str) -> Result<(Option<String>, Vec<Track>), String> {
    let v = innertube_post("browse", &browse_payload(browse_id))?;
    Ok((parse_browse_title(&v), parse_browse_tracks(&v)))
}

/// Trending songs for Home (Feature B): the ytmusicapi get_charts
/// pattern (FEmusic_charts, global) finds the chart playlists; the
/// picked chart is then browsed into real tracks. Guest, public.
pub fn fetch_trending_tracks() -> Result<Vec<Track>, String> {
    let v = innertube_post("browse", &charts_payload())?;
    let charts = parse_chart_playlists(&v);
    let (_, browse_id) = pick_chart_playlist(&charts)
        .ok_or_else(|| "The YouTube Music charts page holds no trending playlist.".to_string())?;
    let mut tracks = browse_tracks(&browse_id)?;
    if tracks.is_empty() {
        return Err("The YouTube Music trending playlist read as empty.".to_string());
    }
    tracks.truncate(25);
    Ok(tracks)
}

/// One-call guest health probe for `lyre doctor`.
pub fn probe() -> Result<String, String> {
    let v = innertube_post("search", &search_payload("a", SEARCH_PARAMS_SONGS))?;
    Ok(format!("reachable ({} test search results)", parse_search_songs(&v).len()))
}

// ---------------------------------------------------------------------------
// Autoplay radio (v0.1.6): the YT Music radio of the playing song
// ---------------------------------------------------------------------------

/// Payload for the guest `next` call that returns a song's radio
/// (watch playlist). Mirrors ytmusicapi's get_watch_playlist radio
/// construction: the videoId plus its RDAMVM radio playlist id.
pub fn next_payload(video_id: &str) -> Value {
    let mut v = build_context();
    v["videoId"] = json!(video_id);
    v["playlistId"] = json!(format!("RDAMVM{}", video_id));
    v["isAudioOnly"] = json!(true);
    v
}

/// Collect every playlistPanelVideoRenderer object in encounter
/// order (the watch playlist's item shape).
fn collect_panel_items(v: &Value, out: &mut Vec<Value>) {
    match v {
        Value::Object(map) => {
            if let Some(item) = map.get("playlistPanelVideoRenderer") {
                out.push(item.clone());
            }
            for val in map.values() {
                collect_panel_items(val, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_panel_items(item, out);
            }
        }
        _ => {}
    }
}

/// Parse a `next` (watch playlist / radio) response into tracks.
/// Item shape: {videoId, title (runs), longBylineText (runs:
/// artist - album - ...), lengthText (simpleText "m:ss")}. Pure;
/// fixture-tested.
pub fn parse_watch_tracks(v: &Value) -> Vec<Track> {
    let mut items = Vec::new();
    collect_panel_items(v, &mut items);
    let mut out: Vec<Track> = Vec::new();
    for item in items {
        let video_id = match item.get("videoId").and_then(|x| x.as_str()) {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => continue,
        };
        // Unplayable / ad-style entries carry no real title.
        if item.get("isPlayable").and_then(|x| x.as_bool()) == Some(false) {
            continue;
        }
        let title = item
            .get("title")
            .and_then(text_of)
            .unwrap_or_default()
            .trim()
            .to_string();
        if title.is_empty() {
            continue;
        }
        // Artist: the first meaningful run of the byline (the rest
        // is album / view counts, separated by " - " style runs).
        let byline_runs = item
            .get("longBylineText")
            .and_then(|t| t.get("runs"))
            .and_then(|r| r.as_array());
        let artist = byline_runs
            .map(|runs| {
                runs.iter()
                    .filter_map(|r| r.get("text").and_then(|t| t.as_str()))
                    .map(|t| t.trim().to_string())
                    .find(|t| !t.is_empty() && t != "•" && t != "·")
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        let duration = item
            .get("lengthText")
            .and_then(text_of)
            .and_then(|s| parse_time_label(&s))
            .unwrap_or(0);
        out.push(Track::new(video_id, title, artist, duration, Source::YtMusic));
    }
    out
}

/// The YT Music radio picks for a YouTube video id (guest
/// Innertube `next`). Deliberately NOT Spotify's recommendations
/// API: every Lyre track resolves to a YouTube video for its
/// audio, so the playing video's own radio is the honest source.
pub fn fetch_radio(video_id: &str) -> Result<Vec<Track>, String> {
    let v = innertube_post("next", &next_payload(video_id))?;
    let tracks = parse_watch_tracks(&v);
    if tracks.is_empty() {
        return Err("YT Music radio returned no playable tracks.".to_string());
    }
    Ok(tracks)
}

/// Radio picks for a Lyre track. A YT Music track's id IS its
/// video id; anything else (Spotify tracks, demo tracks) first
/// resolves its video id the way playback finds its audio - a
/// guest search for the "Artist - Title" match, first hit.
pub fn fetch_radio_for_track(track: &Track) -> Result<Vec<Track>, String> {
    let video_id = if track.source == Source::YtMusic && !track.demo {
        track.id.clone()
    } else {
        let res = search_all(&track.match_query())?;
        res.tracks
            .into_iter()
            .next()
            .map(|t| t.id)
            .ok_or_else(|| format!("No YT Music match found for '{}'", track.match_query()))?
    };
    fetch_radio(&video_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn client_version_is_dynamic_ytmusicapi_style() {
        // 2026-10-16 00:00:00 UTC = 1791763200 (verified with
        // `date -u -d 2026-10-16 +%s`).
        assert_eq!(client_version_at(1_792_108_800), "1.20261016.01.00");
        // 2025-01-01 00:00:00 UTC = 1735689600.
        assert_eq!(client_version_at(1_735_689_600), "1.20250101.01.00");
        // The pinned v0.1.4 static string would be a stale date;
        // today's generated value always carries today's UTC date.
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        assert_eq!(client_version(), client_version_at(now));
    }

    #[test]
    fn context_shape_matches_current_ytmusicapi() {
        let ctx = build_context();
        assert_eq!(ctx["context"]["client"]["clientName"], "WEB_REMIX");
        assert_eq!(
            ctx["context"]["client"]["clientVersion"],
            json!(client_version())
        );
        // initialize_context() sends exactly client + empty user:
        // no hl/gl by default.
        assert!(ctx["context"].get("user").is_some());
        assert!(ctx["context"]["client"].get("hl").is_none());
        assert!(ctx["context"]["client"].get("gl").is_none());
        // Guest URL: alt=json only (key is a browser-auth-only
        // addition in ytmusicapi); the retry variant appends it.
        assert!(endpoint_url("search", false).ends_with("/search?alt=json"));
        assert!(!endpoint_url("search", false).contains("key="));
        assert!(endpoint_url("search", true).contains("&key="));
    }

    #[test]
    fn guest_headers_never_carry_authorization() {
        let h = request_headers(Some("visitor-xyz"), 1_700_000_000);
        let keys: Vec<&str> = h.iter().map(|(k, _)| k.as_str()).collect();
        assert!(!keys.iter().any(|k| k.eq_ignore_ascii_case("authorization")), "headers: {:?}", keys);
        let get = |k: &str| h.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        assert_eq!(get("User-Agent"), Some(USER_AGENT));
        assert_eq!(get("Origin"), Some(YTM_ORIGIN));
        assert_eq!(get("X-Goog-Visitor-Id"), Some("visitor-xyz"));
        assert_eq!(get("X-Goog-Request-Time"), Some("1700000000"));
        assert_eq!(get("Cookie"), Some("SOCS=CAI"));
        // No visitor id fetched -> header absent, call still shaped.
        let h2 = request_headers(None, 1);
        assert!(!h2.iter().any(|(k, _)| k == "X-Goog-Visitor-Id"));
    }

    #[test]
    fn visitor_data_parsing() {
        let html = r#"<script>ytcfg.set({"INNERTUBE_API_KEY":"x","VISITOR_DATA":"Cgt2aXNpdG9yLTEyMw%3D%3D","OTHER":1});</script>"#;
        assert_eq!(parse_visitor_data(html).as_deref(), Some("Cgt2aXNpdG9yLTEyMw%3D%3D"));
        assert_eq!(parse_visitor_data("<html>nothing</html>"), None);
        assert_eq!(parse_visitor_data("\"VISITOR_DATA\":\"\""), None);
    }

    #[test]
    fn google_error_body_parsing_and_description() {
        let body = json!({
            "error": {
                "code": 400,
                "message": "Request contains an invalid argument.",
                "status": "INVALID_ARGUMENT",
                "errors": [{"message": "Request contains an invalid argument.", "domain": "global", "reason": "badRequest"}]
            }
        });
        let ge = parse_google_error(400, &body).expect("parsed");
        assert_eq!(ge.http, 400);
        assert_eq!(ge.status, "INVALID_ARGUMENT");
        assert_eq!(ge.reason, "badRequest");
        let d = ge.describe();
        assert!(d.contains("HTTP 400") && d.contains("INVALID_ARGUMENT") && d.contains("invalid argument"), "desc: {}", d);
        // Non-Google bodies do not parse (caller falls back).
        assert!(parse_google_error(502, &json!({"x": 1})).is_none());
        assert!(parse_google_error(400, &json!("plain text")).is_none());
    }

    /// A helper building one search-result list item in the real
    /// (trimmed) response shape.
    fn shelf(items: Vec<Value>) -> Value {
        json!({
            "contents": {
                "tabbedSearchResultsRenderer": {
                    "tabs": [{
                        "tabRenderer": {
                            "content": {
                                "sectionListRenderer": {
                                    "contents": [{
                                        "musicShelfRenderer": {
                                            "title": {"runs": [{"text": "Results"}]},
                                            "contents": items
                                        }
                                    }]
                                }
                            }
                        }
                    }]
                }
            }
        })
    }

    fn song_item() -> Value {
        json!({
            "musicResponsiveListItemRenderer": {
                "flexColumns": [
                    {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [
                        {"text": "Monokrom", "navigationEndpoint": {"watchEndpoint": {"videoId": "vid-monokrom"}}}
                    ]}}},
                    {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [
                        {"text": "Song"}, {"text": " • "}, {"text": "Tulus"}, {"text": " • "},
                        {"text": "Monokrom"}, {"text": " • "}, {"text": "3:54"}
                    ]}}}
                ],
                "overlay": {"musicItemThumbnailOverlayRenderer": {"content": {"musicPlayButtonRenderer": {
                    "playNavigationEndpoint": {"watchEndpoint": {"videoId": "vid-monokrom"}}
                }}}}
            }
        })
    }

    #[test]
    fn search_songs_fixture_parsing() {
        let v = shelf(vec![song_item()]);
        let tracks = parse_search_songs(&v);
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].id, "vid-monokrom");
        assert_eq!(tracks[0].title, "Monokrom");
        assert_eq!(tracks[0].artist, "Tulus");
        assert_eq!(tracks[0].duration_secs, 234);
        assert_eq!(tracks[0].source, crate::track::Source::YtMusic);
        // Garbage yields nothing, never a panic or a phantom track.
        assert!(parse_search_songs(&json!({"x": 1})).is_empty());
    }

    #[test]
    fn search_albums_and_playlists_fixture_parsing() {
        let album_item = json!({
            "musicResponsiveListItemRenderer": {
                "navigationEndpoint": {"browseEndpoint": {"browseId": "MPREb_album1"}},
                "flexColumns": [
                    {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "Monokrom"}]}}},
                    {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [
                        {"text": "Album"}, {"text": " • "}, {"text": "Tulus"}, {"text": " • "}, {"text": "2016"}
                    ]}}}
                ]
            }
        });
        let pl_item = json!({
            "musicResponsiveListItemRenderer": {
                "navigationEndpoint": {"browseEndpoint": {"browseId": "VLPLxyz"}},
                "flexColumns": [
                    {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "Senja & Kopi"}]}}},
                    {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [
                        {"text": "Community playlist"}, {"text": " • "}, {"text": "25 songs"}
                    ]}}}
                ]
            }
        });
        // Artist/channel items (UC...) are not albums or playlists.
        let artist_item = json!({
            "musicResponsiveListItemRenderer": {
                "navigationEndpoint": {"browseEndpoint": {"browseId": "UCabc"}},
                "flexColumns": [
                    {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "Tulus"}]}}}
                ]
            }
        });
        let v = shelf(vec![album_item, pl_item, artist_item]);
        let albums = parse_search_albums(&v);
        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].id, "MPREb_album1");
        assert_eq!(albums[0].title, "Monokrom");
        assert_eq!(albums[0].artist, "Tulus");
        assert_eq!(albums[0].year, "2016");
        let pls = parse_search_playlists(&v);
        assert_eq!(pls.len(), 1);
        assert_eq!(pls[0].id, "VLPLxyz");
        assert_eq!(pls[0].title, "Senja & Kopi");
        assert!(pls[0].info.contains("25 songs"), "info: {}", pls[0].info);
    }

    #[test]
    fn browse_tracks_fixture_parsing() {
        // Playlist browse shape: musicPlaylistShelfRenderer with
        // fixed-column durations and playlistItemData video ids.
        let v = json!({
            "contents": {
                "singleColumnBrowseResultsRenderer": {
                    "tabs": [{
                        "tabRenderer": {"content": {"sectionListRenderer": {"contents": [{
                            "musicPlaylistShelfRenderer": {
                                "contents": [
                                    {
                                        "musicResponsiveListItemRenderer": {
                                            "playlistItemData": {"videoId": "vid-a"},
                                            "flexColumns": [
                                                {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "Lagu A"}]}}},
                                                {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "Artis A"}]}}}
                                            ],
                                            "fixedColumns": [
                                                {"musicResponsiveListItemFixedColumnRenderer": {"text": {"runs": [{"text": "4:05"}]}}}
                                            ]
                                        }
                                    },
                                    {
                                        "musicResponsiveListItemRenderer": {
                                            "playlistItemData": {"videoId": "vid-b"},
                                            "flexColumns": [
                                                {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "Lagu B"}]}}},
                                                {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "Artis B"}]}}}
                                            ],
                                            "fixedColumns": [
                                                {"musicResponsiveListItemFixedColumnRenderer": {"text": {"simpleText": "2:30"}}}
                                            ]
                                        }
                                    }
                                ]
                            }
                        }]}}}
                    }]
                }
            }
        });
        let tracks = parse_browse_tracks(&v);
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[0].title, "Lagu A");
        assert_eq!(tracks[0].artist, "Artis A");
        assert_eq!(tracks[0].duration_secs, 245);
        assert_eq!(tracks[1].duration_secs, 150);
        // Album browse (same item shape) parses identically, and an
        // item without a videoId is dropped, not invented.
        assert!(parse_browse_tracks(&json!({"contents": {}})).is_empty());
    }

    #[test]
    fn charts_fixture_parsing_and_pick() {
        // Trimmed shape of the real FEmusic_charts (global) landing:
        // carousels of musicTwoRowItemRenderer chart playlists plus
        // an artist entry that must be ignored.
        let v = json!({
            "contents": {"singleColumnBrowseResultsRenderer": {"tabs": [{"tabRenderer": {"content": {"sectionListRenderer": {"contents": [
                {"musicCarouselShelfRenderer": {"contents": [
                    {"musicTwoRowItemRenderer": {
                        "title": {"runs": [{"text": "Daily Top Music Videos - Global"}]},
                        "navigationEndpoint": {"browseEndpoint": {"browseId": "VLPLdaily"}}
                    }},
                    {"musicTwoRowItemRenderer": {
                        "title": {"runs": [{"text": "Top 100 Music Videos Global"}]},
                        "navigationEndpoint": {"browseEndpoint": {"browseId": "VLPLtop100"}}
                    }},
                    {"musicTwoRowItemRenderer": {
                        "title": {"runs": [{"text": "Some Artist"}]},
                        "navigationEndpoint": {"browseEndpoint": {"browseId": "UCartist"}}
                    }}
                ]}}
            ]}}}}]}}
        });
        let charts = parse_chart_playlists(&v);
        assert_eq!(charts.len(), 2, "artist UC entry excluded: {:?}", charts);
        assert_eq!(charts[0].0, "Daily Top Music Videos - Global");
        let picked = pick_chart_playlist(&charts).expect("pick");
        assert_eq!(picked.1, "VLPLtop100", "Top 100 wins over the daily chart");
        // No charts at all -> None (caller reports honestly).
        assert!(pick_chart_playlist(&[]).is_none());
        // Charts payload carries the global formData selection.
        let p = charts_payload();
        assert_eq!(p["browseId"], "FEmusic_charts");
        assert_eq!(p["formData"]["selectedValues"][0], "ZZ");
    }

    #[test]
    fn browse_title_parsing() {
        let v = json!({
            "header": {"musicDetailHeaderRenderer": {
                "title": {"runs": [{"text": "Playlist Dari Link"}]},
                "subtitle": {"runs": [{"text": "25 songs"}]}
            }},
            "contents": {}
        });
        assert_eq!(parse_browse_title(&v).as_deref(), Some("Playlist Dari Link"));
        assert_eq!(parse_browse_title(&json!({"contents": {}})), None);
    }

    #[test]
    fn search_payload_and_params_shape() {
        let p = search_payload("tulus", SEARCH_PARAMS_ALBUMS);
        assert_eq!(p["query"], "tulus");
        assert_eq!(p["params"], SEARCH_PARAMS_ALBUMS);
        assert_eq!(p["context"]["client"]["clientName"], "WEB_REMIX");
        // The three filters are distinct, ytmusicapi-shaped params.
        assert_ne!(SEARCH_PARAMS_SONGS, SEARCH_PARAMS_ALBUMS);
        assert_ne!(SEARCH_PARAMS_ALBUMS, SEARCH_PARAMS_PLAYLISTS);
        assert!(SEARCH_PARAMS_SONGS.starts_with("EgWKAQII"));
        assert!(SEARCH_PARAMS_ALBUMS.starts_with("EgWKAQIY"));
        let b = browse_payload("VLPLxyz");
        assert_eq!(b["browseId"], "VLPLxyz");
    }

    #[test]
    fn watch_playlist_radio_parsing_fixture() {
        // The `next` response shape (ytmusicapi get_watch_playlist):
        // playlistPanelVideoRenderer items under the panel renderer.
        let v = json!({
            "contents": {"singleColumnMusicWatchNextResultsRenderer": {"tabbedRenderer": {"watchNextTabbedResultsRenderer": {"tabs": [{"tabRenderer": {"content": {"musicQueueRenderer": {"content": {"playlistPanelRenderer": {"contents": [
                {"playlistPanelVideoRenderer": {
                    "videoId": "vidSEED01",
                    "title": {"runs": [{"text": "Seed Song"}]},
                    "longBylineText": {"runs": [{"text": "Seed Artist"}, {"text": " • "}, {"text": "2.2B views"}]},
                    "lengthText": {"runs": [{"text": "3:30"}]},
                    "isPlayable": true
                }},
                {"playlistPanelVideoRenderer": {
                    "videoId": "vidRADIO2",
                    "title": {"runs": [{"text": "Radio Pick"}]},
                    "longBylineText": {"runs": [{"text": "Other Artist"}]},
                    "lengthText": {"simpleText": "4:05"},
                    "isPlayable": true
                }},
                {"playlistPanelVideoRenderer": {
                    "videoId": "vidSKIP33",
                    "title": {"runs": [{"text": "Unplayable"}]},
                    "longBylineText": {"runs": [{"text": "Nobody"}]},
                    "lengthText": {"simpleText": "1:00"},
                    "isPlayable": false
                }}
            ]}}}}}}]}}}}
        });
        let tracks = parse_watch_tracks(&v);
        assert_eq!(tracks.len(), 2, "unplayable entries are skipped");
        assert_eq!(tracks[0].id, "vidSEED01");
        assert_eq!(tracks[0].title, "Seed Song");
        assert_eq!(tracks[0].artist, "Seed Artist");
        assert_eq!(tracks[0].duration_secs, 210);
        assert_eq!(tracks[0].source, Source::YtMusic);
        assert_eq!(tracks[1].id, "vidRADIO2");
        assert_eq!(tracks[1].duration_secs, 245);
        // The radio payload carries the RDAMVM radio playlist id.
        let p = next_payload("vidSEED01");
        assert_eq!(p["videoId"], "vidSEED01");
        assert_eq!(p["playlistId"], "RDAMVMvidSEED01");
        assert!(parse_watch_tracks(&json!({"contents": {}})).is_empty());
    }
}
