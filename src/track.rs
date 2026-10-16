use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Source {
    Spotify,
    YtMusic,
}

impl Source {
    pub fn label(&self) -> &'static str {
        match self {
            Source::Spotify => "[SPOT]",
            Source::YtMusic => "[YTM]",
        }
    }
    pub fn name(&self) -> &'static str {
        match self {
            Source::Spotify => "Spotify",
            Source::YtMusic => "YouTube Music",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub duration_secs: u64,
    pub source: Source,
    /// "track" for normal music, "ad" for advertisement items.
    pub track_type: String,
    /// Seeded example track: the artist/title metadata is REAL, only
    /// the id is a placeholder, so playback resolves the audio through
    /// the "Artist - Title" YouTube Music match (same path as Spotify
    /// tracks) instead of a direct stream id. Since v0.1.3 seeded
    /// tracks are fully playable - the old "demo tracks are never
    /// played" rule is gone.
    pub demo: bool,
}

impl Track {
    pub fn new(
        id: impl Into<String>,
        title: impl Into<String>,
        artist: impl Into<String>,
        duration_secs: u64,
        source: Source,
    ) -> Self {
        Track {
            id: id.into(),
            title: title.into(),
            artist: artist.into(),
            duration_secs,
            source,
            track_type: "track".to_string(),
            demo: false,
        }
    }
    pub fn is_ad(&self) -> bool {
        self.track_type == "ad"
    }
    /// Query used to find the playable audio match for this track on
    /// YouTube Music, in the Spotube model: "Artist - Title".
    pub fn match_query(&self) -> String {
        format!("{} - {}", self.artist, self.title)
    }
    /// Honest audio provenance shown in Now Playing: Spotify tracks are
    /// metadata from Spotify, but their audio is matched and streamed via
    /// YouTube Music - the audio bytes never come from Spotify's CDN.
    pub fn audio_provenance(&self) -> String {
        match self.source {
            Source::Spotify => "Spotify - audio via YT Music".to_string(),
            Source::YtMusic => "YouTube Music".to_string(),
        }
    }
    pub fn duration_label(&self) -> String {
        format!("{}:{:02}", self.duration_secs / 60, self.duration_secs % 60)
    }
}

/// The stable identity of a track: source + the source's own id +
/// title + artist. Lyrics state and autoplay radio fetches are
/// bound to this key (v0.1.6), so two tracks that merely share a
/// title - or a title+artist with different ids - never collide,
/// and a result fetched for one track can never be applied to
/// another.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TrackKey {
    pub source: Source,
    pub id: String,
    pub title: String,
    pub artist: String,
}

impl TrackKey {
    pub fn of(t: &Track) -> TrackKey {
        TrackKey {
            source: t.source,
            id: t.id.clone(),
            title: t.title.clone(),
            artist: t.artist.clone(),
        }
    }
    /// Loose same-song test for dedupe: same source+id, or the same
    /// normalized title+artist (a radio list may carry the same song
    /// under a different video id).
    pub fn same_song(&self, other: &TrackKey) -> bool {
        (self.source == other.source && self.id == other.id)
            || (norm(&self.title) == norm(&other.title) && norm(&self.artist) == norm(&other.artist))
    }
}

fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// A catalog album (search result / browse target). `id` is the
/// source's own identifier: a Spotify album id, or a YouTube Music
/// browseId (MPREb...). Tracks are listed by browsing it.
#[derive(Debug, Clone, PartialEq)]
pub struct AlbumRef {
    pub id: String,
    pub title: String,
    pub artist: String,
    /// Release year as reported by the source ("" when unknown).
    pub year: String,
    pub source: Source,
}

/// A catalog or library playlist (search result / sidebar entry /
/// browse target). `id` is a Spotify playlist id or a YouTube Music
/// browseId (VL...); it is "" for seeded sidebar entries that have no
/// real counterpart (those open via search matching, labeled as such).
#[derive(Debug, Clone, PartialEq)]
pub struct PlaylistRef {
    pub id: String,
    pub title: String,
    /// One-line source-provided info (owner / track count), shown
    /// verbatim - never invented.
    pub info: String,
    pub source: Source,
}

/// Seeded starter tracks so the UI is navigable before any login.
/// Real artist/title metadata: pressing Enter on one plays it via the
/// YouTube Music match path, exactly like a Spotify track.
pub fn demo_tracks() -> Vec<Track> {
    // Same dataset as the approved design artifact's
    // "Terakhir Diputar - Dari Dua Sumber" box.
    let mut v = vec![
        Track::new("demo-spot-1", "Monokrom", "Tulus", 234, Source::Spotify),
        Track::new("demo-ytm-1", "As It Was", "Harry Styles", 167, Source::YtMusic),
        Track::new("demo-spot-2", "Hati-Hati di Jalan", "Tulus", 242, Source::Spotify),
        Track::new("demo-ytm-2", "Golden Hour", "JVKE", 209, Source::YtMusic),
        Track::new("demo-spot-3", "Sial", "Mahalini", 243, Source::Spotify),
        Track::new("demo-ytm-3", "Creep (Acoustic)", "Radiohead", 238, Source::YtMusic),
    ];
    for t in v.iter_mut() {
        t.demo = true;
    }
    v
}
