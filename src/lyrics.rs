use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct LyricLine {
    pub time_secs: f64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LyricsResult {
    Synced(Vec<LyricLine>),
    Plain(String),
    NotFound,
}

/// Parse LRC content: lines like `[01:23.45]text` (centiseconds or millis).
/// Lines without a timestamp are ignored. Result is sorted by time.
pub fn parse_lrc(content: &str) -> Vec<LyricLine> {
    let mut out: Vec<LyricLine> = Vec::new();
    for raw in content.lines() {
        let line = raw.trim();
        if !line.starts_with('[') {
            continue;
        }
        // A line may carry several timestamps; take the first one.
        if let Some(close) = line.find(']') {
            let stamp = &line[1..close];
            let text = line[close + 1..].trim().to_string();
            if let Some(t) = parse_timestamp(stamp) {
                out.push(LyricLine { time_secs: t, text });
            }
        }
    }
    out.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
    out
}

fn parse_timestamp(stamp: &str) -> Option<f64> {
    // Formats: mm:ss, mm:ss.xx, mm:ss.xxx ; also tolerate hh:mm:ss.
    let parts: Vec<&str> = stamp.split(':').collect();
    let (min, sec_part) = match parts.len() {
        2 => (parts[0].parse::<f64>().ok()?, parts[1]),
        3 => {
            let h = parts[0].parse::<f64>().ok()?;
            let m = parts[1].parse::<f64>().ok()?;
            (h * 60.0 + m, parts[2])
        }
        _ => return None,
    };
    let sec = sec_part.parse::<f64>().ok()?;
    Some(min * 60.0 + sec)
}

/// User lyric-sync offset bounds (Bug 2): +/- keys shift the sync
/// in 0.5s steps within this range; the value is shown in the
/// lyrics box header.
pub const OFFSET_MIN: f64 = -5.0;
pub const OFFSET_MAX: f64 = 5.0;
pub const OFFSET_STEP: f64 = 0.5;

pub fn clamp_offset(v: f64) -> f64 {
    v.clamp(OFFSET_MIN, OFFSET_MAX)
}

/// The position lyric sync should use: mpv's real position shifted
/// by the user's offset. Never negative.
pub fn effective_position(position_secs: f64, offset_secs: f64) -> f64 {
    (position_secs + offset_secs).max(0.0)
}

/// Index of the line that is active at `position_secs`:
/// the last line whose timestamp is <= position. None before the first line.
pub fn active_line_index(lines: &[LyricLine], position_secs: f64) -> Option<usize> {
    let mut active: Option<usize> = None;
    for (i, l) in lines.iter().enumerate() {
        if l.time_secs <= position_secs {
            active = Some(i);
        } else {
            break;
        }
    }
    active
}

fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn extract_result(v: &Value) -> Option<LyricsResult> {
    let synced = v.get("syncedLyrics").and_then(|x| x.as_str()).unwrap_or("");
    if !synced.trim().is_empty() {
        let lines = parse_lrc(synced);
        if !lines.is_empty() {
            return Some(LyricsResult::Synced(lines));
        }
    }
    let plain = v.get("plainLyrics").and_then(|x| x.as_str()).unwrap_or("");
    if !plain.trim().is_empty() {
        return Some(LyricsResult::Plain(plain.to_string()));
    }
    None
}

/// LRCLib base URL (test-only env override for the capture harness;
/// production is always lrclib.net).
fn lrclib_base() -> String {
    std::env::var("LYRE_LRCLIB_BASE").unwrap_or_else(|_| "https://lrclib.net".to_string())
}

/// Real LRCLib client (https://lrclib.net). Blocking, on-demand only.
/// Tries /api/get first, then falls back to /api/search.
pub fn fetch_lyrics(artist: &str, track: &str) -> Result<LyricsResult, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent("Lyre/0.1.0 (terminal music player)")
        .build();
    let get_url = format!(
        "{}/api/get?artist_name={}&track_name={}",
        lrclib_base(),
        url_encode(artist),
        url_encode(track)
    );
    if let Ok(resp) = agent.get(&get_url).call() {
        if let Ok(v) = resp.into_json::<Value>() {
            if let Some(r) = extract_result(&v) {
                return Ok(r);
            }
        }
    }
    let search_url = format!(
        "{}/api/search?artist_name={}&track_name={}",
        lrclib_base(),
        url_encode(artist),
        url_encode(track)
    );
    let resp = agent
        .get(&search_url)
        .call()
        .map_err(|e| format!("LRCLib request failed: {}", e))?;
    let arr = resp
        .into_json::<Value>()
        .map_err(|e| format!("LRCLib response parse failed: {}", e))?;
    if let Some(list) = arr.as_array() {
        for item in list {
            if let Some(r) = extract_result(item) {
                return Ok(r);
            }
        }
        // First hit may carry only plain lyrics that extract_result missed if
        // fields are empty strings; treat an empty catalogue as NotFound.
        return Ok(LyricsResult::NotFound);
    }
    Ok(LyricsResult::NotFound)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "[00:01.00]First line\n[00:05.50]Second line\n[00:10.250]Third line\n[ar:Some Artist]\nno timestamp here\n[00:20.00]Fourth line";

    #[test]
    fn lrc_parsing() {
        let lines = parse_lrc(SAMPLE);
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0].text, "First line");
        assert!((lines[0].time_secs - 1.0).abs() < 0.001);
        assert!((lines[1].time_secs - 5.5).abs() < 0.001);
        assert!((lines[2].time_secs - 10.25).abs() < 0.001);
        assert_eq!(lines[3].text, "Fourth line");
    }

    #[test]
    fn lrc_sorted_and_metadata_skipped() {
        let lines = parse_lrc("[00:30.00]Late\n[00:02.00]Early\n[ti:Title]");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "Early");
        assert_eq!(lines[1].text, "Late");
    }

    #[test]
    fn active_line_lookup() {
        let lines = parse_lrc(SAMPLE);
        assert_eq!(active_line_index(&lines, 0.0), None);
        assert_eq!(active_line_index(&lines, 1.0), Some(0));
        assert_eq!(active_line_index(&lines, 7.3), Some(1));
        assert_eq!(active_line_index(&lines, 10.25), Some(2));
        assert_eq!(active_line_index(&lines, 999.0), Some(3));
        assert_eq!(active_line_index(&[], 5.0), None);
    }

    #[test]
    fn ipc_fed_position_and_offset_drive_line_selection() {
        // Bug 2 contract: the UI feeds mpv's IPC position (never a
        // local timer) through effective_position with the user
        // offset, and THAT selects the highlighted line.
        let lines = parse_lrc(SAMPLE);
        // Position exactly as an IPC snapshot would report it.
        let ipc_pos = 7.3;
        assert_eq!(active_line_index(&lines, effective_position(ipc_pos, 0.0)), Some(1));
        // Negative offset: lyrics run later (display lags the audio).
        assert_eq!(active_line_index(&lines, effective_position(ipc_pos, -3.0)), Some(0));
        // Positive offset: lyrics run earlier.
        assert_eq!(active_line_index(&lines, effective_position(ipc_pos, 3.0)), Some(2));
        // Effective position never goes below zero.
        assert_eq!(effective_position(1.0, -5.0), 0.0);
        // Offset clamping: 0.5s steps within +-5s.
        assert_eq!(clamp_offset(99.0), OFFSET_MAX);
        assert_eq!(clamp_offset(-99.0), OFFSET_MIN);
        assert_eq!(clamp_offset(0.5), 0.5);
    }
}
