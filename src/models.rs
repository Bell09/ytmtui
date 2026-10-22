use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq)]
pub enum Page {
    Library,
    Playlist,
    Search,
    Queue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaylistKind {
    Playlist,
    Album,
    Artist,
    Channel,
}

#[derive(Debug, Clone)]
pub struct Playlist {
    pub id: String,
    pub title: String,
    pub track_count: Option<u32>,
    pub kind: PlaylistKind,
    pub thumbnail_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Track {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub duration: Option<String>,
    pub thumbnail_url: Option<String>,
    pub like: Option<bool>,
    pub downloaded: bool,
}

#[derive(Debug, Clone)]
pub enum SearchHit {
    Track(Track),
    Playlist(Playlist),
    Album(Playlist),
    Artist(Playlist),
}

impl SearchHit {
    pub fn title(&self) -> &str {
        match self {
            SearchHit::Track(t) => &t.title,
            SearchHit::Playlist(p) | SearchHit::Album(p) | SearchHit::Artist(p) => &p.title,
        }
    }
}
