use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Track {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub duration: Option<String>,
    pub thumbnail_url: Option<String>,
    /// Like state carried by the row's menu button: `Some(true)` when the
    /// account has the song liked, `Some(false)` when not, `None` when the
    /// response didn't say (search rows, offline fallbacks).
    #[serde(default)]
    pub like: Option<bool>,
    /// Whether a copy of this track sits in the local music folder, drawn as
    /// a check mark. Stamped from the download registry whenever a list is
    /// stored and re-stamped the moment a download lands, so an online row
    /// lights up without a re-navigation. Local scans set it directly: every
    /// file they report is, by definition, already offline.
    #[serde(default)]
    pub downloaded: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlaylistKind {
    Playlist,
    Album,
    Artist,
    Channel,
    /// A mood button on the "Moods & genres" browse page. Its id carries the
    /// Innertube category link, not a playlist id.
    Mood,
    /// A genre button on the "Moods & genres" browse page.
    Genre,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Playlist {
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub track_count: Option<usize>,
    pub kind: PlaylistKind,
}

impl Playlist {
    /// Human-readable label for the entry's kind.
    pub fn kind_label(&self) -> &'static str {
        match self.kind {
            PlaylistKind::Playlist => "Playlist",
            PlaylistKind::Album => "Album",
            PlaylistKind::Artist => "Artist",
            PlaylistKind::Channel => "Channel",
            PlaylistKind::Mood => "Mood",
            PlaylistKind::Genre => "Genre",
        }
    }
}

/// One row in a search result: either a playable song or an openable
/// collection (album / artist / playlist).
#[derive(Debug, Clone)]
pub enum SearchHit {
    Track(Track),
    Collection(Playlist),
}

impl SearchHit {
    pub fn track(&self) -> Option<&Track> {
        match self {
            SearchHit::Track(t) => Some(t),
            SearchHit::Collection(_) => None,
        }
    }

    pub fn collection(&self) -> Option<&Playlist> {
        match self {
            SearchHit::Collection(p) => Some(p),
            SearchHit::Track(_) => None,
        }
    }
}

/// A titled group of rows on the "For you" browse page.
#[derive(Debug, Clone)]
pub struct BrowseShelf {
    pub title: String,
    pub items: Vec<SearchHit>,
}

/// One displayed line on the browse page. Headers are drawn but skipped by
/// the selection cursor.
#[derive(Debug, Clone)]
pub enum BrowseRow {
    Header(String),
    Item(SearchHit),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Search,
    Library,
    Browse,
    Playlist,
    Queue,
}
