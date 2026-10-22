use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Track {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub duration: Option<String>,
    pub thumbnail_url: Option<String>,

    #[serde(default)]
    pub like: Option<bool>,

    #[serde(default)]
    pub downloaded: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlaylistKind {
    Playlist,
    Album,
    Artist,
    Channel,
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
    pub fn kind_label(&self) -> &'static str {
        match self.kind {
            PlaylistKind::Playlist => "Playlist",
            PlaylistKind::Album => "Album",
            PlaylistKind::Artist => "Artist",
            PlaylistKind::Channel => "Channel",
        }
    }
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Search,
    Library,
    Playlist,
    Queue,
}
