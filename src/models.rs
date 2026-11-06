use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Track {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub duration: Option<String>,
    pub thumbnail_url: Option<String>,
    /// Like state from the row's menu button: `Some(true)` liked,
    /// `Some(false)` not liked, `None` unknown (search rows, offline fallbacks).
    #[serde(default)]
    pub like: Option<bool>,
    /// Track exists in the local music folder (draws a check mark). Stamped
    /// from the download registry when lists are stored and when a download
    /// lands, so rows update without re-navigation. Local scans set it directly.
    #[serde(default)]
    pub downloaded: bool,
    /// Playlist membership token (`setVideoId`) parsed from the row's
    /// "remove from playlist" menu entry. `browse/edit_playlist` removal
    /// needs this, not the video id, so rows without it can't be removed.
    #[serde(default)]
    pub set_video_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlaylistKind {
    Playlist,
    Album,
    Artist,
    Channel,
    /// Mood button on the "Moods & genres" page. Its id holds an Innertube
    /// category link, not a playlist id.
    Mood,
    /// Genre button on the "Moods & genres" page.
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

/// One search result: a playable song or an openable collection
/// (album / artist / playlist).
#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// A titled group of rows on the "For you" page.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowseShelf {
    pub title: String,
    pub items: Vec<SearchHit>,
}

/// One line on the browse page. Headers render but the cursor skips them.
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
