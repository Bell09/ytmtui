use crate::models::{BrowseRow, BrowseShelf, Page, Playlist, PlaylistKind, SearchHit, Track};
use crate::api::{SearchFilter, YtmApi};
use crate::player::Player;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

type PreviewResult = (String, Result<Vec<Track>, String>);

/// A finished library fetch: the generation it was started for, and the
/// playlists (or an error). The generation lets a newer request supersede one
/// that is still in flight, e.g. when switching back online.
type LibraryResult = (u64, Result<Vec<Playlist>, String>);

/// The heart key's outcome: video id, the rating that was requested,
/// which press asked for it (so an older reply can't clobber a newer
/// press), and whether the API call succeeded.
type LikeResult = (String, bool, u64, Result<(), String>);

/// One queued "add to playlist" command: the server's playlist id, the
/// playlist title (for the status line), the track's video id and title.
type AddCmd = (String, String, String, String);

/// A finished add-to-playlist: the playlist title, the track title, and
/// whether the API call succeeded.
type AddResult = (String, String, Result<(), String>);

/// One queued "remove from playlist" command: the playlist id and name
/// (status), the track with the row it occupied (for the rollback breath
/// if the server refuses), and its video id under the hood.
type RmCmd = (String, String, Track, usize);
type RmResult = (String, String, Track, usize, Result<(), String>);

/// Labels for the browse sections, left to right. The matching shelves are
/// found by title, so the API can return them in any order.
pub const BROWSE_SECTIONS: [&str; 5] = ["TRENDING", "MIX", "COMMUNITY", "MOODS", "GENRES"];

/// Which browse section a shelf header belongs to.
fn browse_section_for_title(title: &str) -> Option<usize> {
    let title = title.to_lowercase();
    if title.contains("trending") {
        Some(0)
    } else if title.contains("mix") {
        Some(1)
    } else if title.contains("community") {
        Some(2)
    } else if title.contains("mood") {
        Some(3)
    } else if title.contains("genre") {
        Some(4)
    } else {
        None
    }
}

/// A queued `L` press: (video id, want like, generation).
type LikeRequest = (String, bool, u64);

/// A background re-fetch of the liked-songs list, for the delayed
/// server re-sync after a rating change.
type LikeRefreshResult = Result<Vec<Track>, String>;

/// A finished (or failed) `d` download: the video id it was for, and the
/// registry entry to save — or why it couldn't.
type DownloadResult = (String, Result<crate::offline::Entry, String>);

/// Filters shown in the search bar, in display order. Only the ones the
/// Innertube response actually advertises are kept.
const SEARCH_FILTER_ORDER: [&str; 5] = [
    "Songs",
    "Albums",
    "Artists",
    "Community playlists",
    "Featured playlists",
];

/// The "Liked Music" auto-playlist's id — the list the `L` heart keeps
/// in step with the server while it's on screen.
const LIKED_PLAYLIST: &str = "VLLM";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaybackState {
    Stopped,
    Buffering,
    Playing,
    Paused,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputMode {
    Normal,
    Search,
}

/// Repeat behaviour for queue advancement (cycled with Ctrl+R,
/// honoured at end-of-file).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepeatMode {
    Off,
    All,
    One,
}

impl RepeatMode {
    pub fn next(self) -> Self {
        match self {
            RepeatMode::Off => RepeatMode::All,
            RepeatMode::All => RepeatMode::One,
            RepeatMode::One => RepeatMode::Off,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            RepeatMode::Off => "off",
            RepeatMode::All => "all",
            RepeatMode::One => "one",
        }
    }
}

pub struct App {
    pub current_page: Page,
    pub input_mode: InputMode,
    pub search_query: String,
    pub search_results: Vec<SearchHit>,
    /// Index 0 is always "Top" (default search); the rest mirror
    /// `search_filters`.
    pub search_filter: usize,
    pub search_filters: Vec<SearchFilter>,

    pub playlists: Vec<Playlist>,
    pub library_loaded: bool,
    pub current_playlist: Option<Playlist>,
    pub playlist_tracks: Vec<Track>,
    /// Where `Esc` returns to after viewing a collection.
    pub playlist_return_page: Page,
    /// The row to reselect when returning from a collection.
    pub playlist_return_index: usize,

    /// The "For you" browse page: flattened section headers and rows. The
    /// selection cursor only lands on `BrowseRow::Item`.
    pub browse_rows: Vec<BrowseRow>,
    pub browse_loaded: bool,
    /// Root shelves, indexed by section; `None` if that section was not
    /// present in the feed.
    pub browse_shelves: Vec<Option<BrowseShelf>>,
    /// Active section while browsing.
    pub browse_section: Option<usize>,
    browse_return_page: Page,
    browse_return_index: usize,
    /// One level of history for opening a mood/genre category: the root
    /// rows and the row that opened it.
    browse_back: Option<(Vec<BrowseRow>, usize)>,

    /// Lazily-fetched preview of the highlighted library entry, plus a cache so
    /// moving back to a previously seen entry is instant.
    pub preview_tracks: Vec<Track>,
    pub preview_for: Option<String>,
    preview_cache: HashMap<String, Vec<Track>>,
    preview_tx: tokio::sync::mpsc::UnboundedSender<PreviewResult>,
    preview_rx: tokio::sync::mpsc::UnboundedReceiver<PreviewResult>,

    /// Library fetches run in the background so the first frame is never
    /// blocked on the network. Only the newest generation may apply.
    library_tx: tokio::sync::mpsc::UnboundedSender<LibraryResult>,
    library_rx: tokio::sync::mpsc::UnboundedReceiver<LibraryResult>,
    library_generation: u64,

    pub selected_index: usize,
    pub current_track: Option<Track>,
    /// The playback queue: every track queued for playback (playing one from
    /// a page replaces the queue with that page's list).
    pub queue: Vec<Track>,
    /// Position of the current track inside `queue`.
    pub queue_index: usize,
    pub repeat: RepeatMode,
    pub shuffle: bool,
    /// Playback order when shuffle is on: positions into `queue`, with the
    /// current track first. Rebuilt on every queue change.
    shuffle_order: Vec<usize>,
    /// Bumped whenever the queue contents change. `sync_ipc` rebuilds its
    /// mirrored rows only when this differs from the last synced revision.
    queue_version: u64,
    ipc_queue_version: u64,
    /// Where `Esc` returns to after viewing the queue page.
    pub queue_return_page: Page,
    pub queue_return_index: usize,
    pub playback_state: PlaybackState,
    pub api: YtmApi,
    pub player: Player,
    pub should_quit: bool,
    pub status_message: String,
    /// Latest position mpv reported, plus when it was sampled. The UI clock
    /// follows mpv instead of a fixed startup timer, so the progress bar and
    /// lyrics start with the audible audio.
    playback_position: f64,
    position_sampled_at: Option<Instant>,
    /// Whether mpv answered at least one timeline query for this track. A
    /// healthy IPC channel that reports no position yet means "still
    /// buffering", not "fall back to a guessed start time".
    mpv_seen: bool,
    pub playback_duration_secs: u64,
    pub buffering_timer: Option<Instant>,
    /// Colour theme (from pywal, wallust or matugen, reloaded live when the
    /// wallpaper tool rewrites its palette).
    pub theme: crate::theme::Theme,
    /// The palette file the theme came from and the mtime we last read it
    /// at — the whole live-reload state. A `stat()` per tick (~1µs) is
    /// enough to notice a wallpaper change; the palette is only re-parsed
    /// when the mtime actually moves.
    theme_src: Option<PathBuf>,
    theme_mtime: Option<SystemTime>,
    /// Wallpaper palettes change at human speed; checking their mtimes once a
    /// second is plenty and keeps the 10Hz UI tick free of filesystem scans.
    theme_poll_at: Instant,
    /// Signed-in account name, fetched in the background for the header.
    pub account_name: Option<String>,
    account_rx: tokio::sync::mpsc::UnboundedReceiver<String>,
    /// Album art for the current track: which track the art belongs to (or
    /// is being fetched for), its decoded square source, and the fitted
    /// canvas cache keyed by cell size.
    pub art_track: Option<String>,
    pub art_source: Option<image::RgbaImage>,
    pub art_fitted: Option<((u16, u16), image::RgbaImage)>,
    /// Kitty graphics state: escape bytes waiting to be written after the
    /// next frame, and what was last transmitted (track, art cells, and
    /// terminal size — kitty drops image placements on window resizes, so
    /// a size change must retransmit).
    pub art_output: Vec<u8>,
    pub art_kitty_sent: Option<(String, u16, u16, u16, u16)>,
    art_tx: tokio::sync::mpsc::UnboundedSender<(String, Option<image::RgbaImage>)>,
    art_rx: tokio::sync::mpsc::UnboundedReceiver<(String, Option<image::RgbaImage>)>,
    /// MPRIS visibility for desktop media widgets (Quickshell etc.):
    /// playback state mirrored in every tick, widget commands drained back.
    mpris: Arc<crate::mpris::MprisHandle>,
    mpris_rx: tokio::sync::mpsc::UnboundedReceiver<crate::mpris::MprisCmd>,
    /// The same for the Unix-socket bridge (`ytmtui queue` / `jump`, and
    /// Quickshell's queue list via the IPC bridge).
    ipc: crate::ipc::IpcHandle,
    ipc_rx: tokio::sync::mpsc::UnboundedReceiver<crate::ipc::IpcCmd>,
    /// Vertical scroll offset for the Library details pane.
    pub details_scroll: u16,
    /// Largest valid `details_scroll`, recomputed on each render.
    pub details_scroll_max: u16,
    /// Height (rows) of the details viewport, recomputed on each render.
    pub details_viewport: u16,
    /// Lyrics pane (the right sidebar), toggled with `l`.
    pub lyrics_open: bool,
    /// Video id the current lyrics state belongs to.
    pub lyrics_track: Option<String>,
    pub lyrics_state: crate::lyrics::LyricsState,
    /// Manual scroll (row offset + when), overriding autoscroll for 3s.
    pub lyrics_scroll: Option<(u16, Instant)>,
    /// Last rendered row offset — the base PgUp/PgDn jump from.
    pub lyrics_offset: u16,
    lyrics_tx: tokio::sync::mpsc::UnboundedSender<crate::lyrics::LyricsMsg>,
    lyrics_rx: tokio::sync::mpsc::UnboundedReceiver<crate::lyrics::LyricsMsg>,
    /// Queued `L` heart toggles. One worker applies them strictly in
    /// press order — concurrent rate calls race server-side, and a rapid
    /// `L L` could otherwise land the unlike after the like and drop the
    /// song from the account entirely.
    like_cmd_tx: tokio::sync::mpsc::UnboundedSender<LikeRequest>,
    like_rx: tokio::sync::mpsc::UnboundedReceiver<LikeResult>,
    /// Bumped on every `L` press — only the latest press's reply may
    /// change the state back.
    like_generation: u64,
    /// When to re-sync the visible liked-songs list with the server
    /// (VLLM's listing converges ~5-6s after a rating change), and the
    /// press generation that scheduled it.
    like_refresh_at: Option<(Instant, u64)>,
    like_refresh_tx: tokio::sync::mpsc::UnboundedSender<LikeRefreshResult>,
    like_refresh_rx: tokio::sync::mpsc::UnboundedReceiver<LikeRefreshResult>,
    /// Queued `a` adds. One worker applies them in order, mirroring the
    /// like queue — two quick `a` presses can't race the edit endpoint.
    add_cmd_tx: tokio::sync::mpsc::UnboundedSender<AddCmd>,
    add_rx: tokio::sync::mpsc::UnboundedReceiver<AddResult>,
    /// True while the add-to-playlist picker overlays the screen. The
    /// key loop routes navigation straight to the picker while it's up.
    pub picker_open: bool,
    /// The picker's own cursor into `self.playlists`.
    pub picker_index: usize,
    /// Queued `r` removals — in-order like the like and add queues.
    rm_cmd_tx: tokio::sync::mpsc::UnboundedSender<RmCmd>,
    rm_rx: tokio::sync::mpsc::UnboundedReceiver<RmResult>,
    /// Offline mode, toggled with `o`. While set the sidebar lists the local
    /// music folder instead of the account's and rows play files from disk.
    /// Playback is untouched by the toggle, so a queue spanning both kinds
    /// of track keeps working across it.
    pub offline: bool,
    /// Which video ids are saved on disk, and where — the thing that makes a
    /// check mark possible for a row still showing as an online track.
    pub registry: crate::offline::Registry,
    /// Where offline files live, re-read from `config.json` on each toggle.
    pub music_dir: PathBuf,
    /// Every file under `music_dir`, rebuilt whenever offline mode opens.
    pub offline_tracks: Vec<Track>,
    /// Video ids currently being saved, so a second `d` can't start the same
    /// song twice while yt-dlp is still running.
    downloading: HashSet<String>,
    /// Whole-percent progress for each active download, keyed by video id.
    /// The row's check area shows this until the ✓ replaces it.
    dl_progress: HashMap<String, u8>,
    /// Downloads reporting back; drained once per tick like the art worker.
    dl_tx: tokio::sync::mpsc::UnboundedSender<DownloadResult>,
    dl_rx: tokio::sync::mpsc::UnboundedReceiver<DownloadResult>,
    /// Percentage updates from those same downloads.
    dl_pct_tx: tokio::sync::mpsc::UnboundedSender<(String, u8)>,
    dl_pct_rx: tokio::sync::mpsc::UnboundedReceiver<(String, u8)>,
}

impl App {
    pub fn new() -> Self {
        let api = YtmApi::new();
        // The detected palette, plus the file it came from and that file's
        // mtime — together these let `poll_theme` notice a wallpaper
        // change and repaint without re-reading the palette every tick.
        let (theme, theme_src) = match crate::theme::Theme::load_with_source() {
            Some((theme, path)) => (theme, Some(path)),
            None => (crate::theme::Theme::fallback(), None),
        };
        let theme_mtime = theme_src
            .as_ref()
            .and_then(|p| std::fs::metadata(p).ok())
            .and_then(|m| m.modified().ok());
        let auth_status = if api.is_authenticated() {
            "↑↓ move · ←→ seek · enter open · space pause · n p skip · / search · z queue · q quit"
        } else {
            "Not signed in — run `ytmtui setup` · Press 'q' to quit."
        };
        let (preview_tx, preview_rx) = tokio::sync::mpsc::unbounded_channel();
        let (library_tx, library_rx) = tokio::sync::mpsc::unbounded_channel();
        let (account_tx, account_rx) = tokio::sync::mpsc::unbounded_channel();
        let (art_tx, art_rx) = tokio::sync::mpsc::unbounded_channel();
        let (lyrics_tx, lyrics_rx) = tokio::sync::mpsc::unbounded_channel();
        let (like_tx, like_rx) = tokio::sync::mpsc::unbounded_channel();
        let (like_cmd_tx, mut like_cmd_rx) = tokio::sync::mpsc::unbounded_channel::<LikeRequest>();
        // One worker drains the toggle queue in press order, so rapid
        // `h h` presses can't race each other on the server.
        {
            let api = api.clone();
            tokio::spawn(async move {
                while let Some((id, want, generation)) = like_cmd_rx.recv().await {
                    let result = api.rate(&id, want).await.map_err(|e| e.to_string());
                    let _ = like_tx.send((id, want, generation, result));
                }
            });
        }
        let (like_refresh_tx, like_refresh_rx) = tokio::sync::mpsc::unbounded_channel();
        let (add_cmd_tx, mut add_cmd_rx) = tokio::sync::mpsc::unbounded_channel::<AddCmd>();
        let (add_tx, add_rx) = tokio::sync::mpsc::unbounded_channel::<AddResult>();
        // Adds flow through the same in-order queue as likes: the picker
        // hands off the command and the footer reports the outcome.
        {
            let api = api.clone();
            tokio::spawn(async move {
                while let Some((playlist_id, playlist_name, video_id, track_title)) =
                    add_cmd_rx.recv().await
                {
                    let result = api
                        .add_to_playlist(&playlist_id, &video_id)
                        .await
                        .map_err(|e| e.to_string());
                    let _ = add_tx.send((playlist_name, track_title, result));
                }
            });
        }
        // Removals ride the same in-order discipline. Liked Music is the
        // exception: dropping a song there *is* an unlike, so its request
        // roads through the rating endpoint.
        let (rm_cmd_tx, mut rm_cmd_rx) = tokio::sync::mpsc::unbounded_channel::<RmCmd>();
        let (rm_tx, rm_rx) = tokio::sync::mpsc::unbounded_channel::<RmResult>();
        {
            let api = api.clone();
            tokio::spawn(async move {
                while let Some((playlist_id, playlist_name, track, index)) = rm_cmd_rx.recv().await {
                    let result = if playlist_id == LIKED_PLAYLIST {
                        // Dropping a row from Liked Music *is* an unlike,
                        // so it goes through the rating endpoint instead.
                        api.rate(&track.id, false).await
                    } else {
                        match track.set_video_id.as_deref() {
                            // Modern `browse/edit_playlist` removals are
                            // addressed by the row's own setVideoId —
                            // rows without the token have no authority
                            // to edit and are refused without a call.
                            Some(token) => {
                                api.remove_from_playlist(&playlist_id, &track.id, token).await
                            }
                            None => Err(anyhow::anyhow!(
                                "row has no setVideoId — this playlist may not be yours"
                            )),
                        }
                    }
                    .map_err(|e| e.to_string());
                    let _ = rm_tx.send((playlist_id, playlist_name, track, index, result));
                }
            });
        }
        let (dl_tx, dl_rx) = tokio::sync::mpsc::unbounded_channel();
        let (dl_pct_tx, dl_pct_rx) = tokio::sync::mpsc::unbounded_channel();
        let (mpris_tx, mpris_rx) = tokio::sync::mpsc::unbounded_channel();
        let (ipc_tx, ipc_rx) = tokio::sync::mpsc::unbounded_channel();
        // Announce ourselves on the MPRIS bus so desktop media widgets
        // (Quickshell & co.) can see and control playback. Silent when
        // there is no session bus.
        let mpris = Arc::new(crate::mpris::MprisHandle::default());
        {
            let handle = mpris.clone();
            tokio::spawn(async move {
                if let Ok(conn) = crate::mpris::serve(handle.clone(), mpris_tx).await {
                    handle.adopt(conn);
                }
            });
        }
        // Unix-socket bridge for the queue CLI and Quickshell's queue list.
        let ipc = Arc::new(std::sync::RwLock::new(crate::ipc::IpcState::default()));
        {
            let handle = ipc.clone();
            tokio::spawn(async move {
                crate::ipc::serve(handle, ipc_tx).await;
            });
        }
        // Fetch the signed-in account name in the background for the header.
        // Reuse the API client so this and the library fetch share one
        // connection pool rather than racing two TLS handshakes at startup.
        let account_api = api.clone();
        tokio::spawn(async move {
            if let Some(name) = account_api.current_account_name().await {
                let _ = account_tx.send(name);
            }
        });

        Self {
            current_page: Page::Library,
            input_mode: InputMode::Normal,
            search_query: String::new(),
            search_results: Vec::new(),
            search_filter: 0,
            search_filters: Vec::new(),
            playlists: Vec::new(),
            library_loaded: false,
            current_playlist: None,
            playlist_tracks: Vec::new(),
            playlist_return_page: Page::Library,
            playlist_return_index: 0,
            browse_rows: Vec::new(),
            browse_loaded: false,
            browse_shelves: vec![None; BROWSE_SECTIONS.len()],
            browse_section: None,
            browse_return_page: Page::Library,
            browse_return_index: 0,
            browse_back: None,
            preview_tracks: Vec::new(),
            preview_for: None,
            preview_cache: HashMap::new(),
            preview_tx,
            preview_rx,
            library_tx,
            library_rx,
            library_generation: 0,
            selected_index: 0,
            current_track: None,
            queue: Vec::new(),
            queue_index: 0,
            repeat: RepeatMode::Off,
            shuffle: false,
            shuffle_order: Vec::new(),
            queue_version: 1,
            ipc_queue_version: 0,
            queue_return_page: Page::Library,
            queue_return_index: 0,
            playback_state: PlaybackState::Stopped,
            api,
            player: Player::new(),
            should_quit: false,
            status_message: auth_status.to_string(),
            playback_position: 0.0,
            position_sampled_at: None,
            mpv_seen: false,
            playback_duration_secs: 210,
            buffering_timer: None,
            theme,
            theme_src,
            theme_mtime,
            theme_poll_at: Instant::now(),
            account_name: None,
            account_rx,
            art_track: None,
            art_source: None,
            art_fitted: None,
            art_output: Vec::new(),
            art_kitty_sent: None,
            art_tx,
            art_rx,
            mpris,
            mpris_rx,
            ipc,
            ipc_rx,
            details_scroll: 0,
            details_scroll_max: 0,
            details_viewport: 0,
            lyrics_open: false,
            lyrics_track: None,
            lyrics_state: crate::lyrics::LyricsState::default(),
            lyrics_scroll: None,
            lyrics_offset: 0,
            lyrics_tx,
            lyrics_rx,
            like_cmd_tx,
            like_rx,
            like_generation: 0,
            like_refresh_at: None,
            like_refresh_tx,
            like_refresh_rx,
            add_cmd_tx,
            add_rx,
            picker_open: false,
            picker_index: 0,
            rm_cmd_tx,
            rm_rx,
            offline: false,
            registry: crate::offline::Registry::load(),
            music_dir: crate::offline::music_dir(),
            offline_tracks: Vec::new(),
            downloading: HashSet::new(),
            dl_progress: HashMap::new(),
            dl_tx,
            dl_rx,
            dl_pct_tx,
            dl_pct_rx,
        }
    }

    pub fn update(&mut self) {
        self.poll_theme();
        self.poll_library();
        self.poll_preview();
        while let Ok(name) = self.account_rx.try_recv() {
            self.account_name = Some(name);
        }
        // Drain finished cover-art fetches for the current track.
        while let Ok((id, art)) = self.art_rx.try_recv() {
            if self.art_track.as_deref() == Some(id.as_str()) {
                self.art_fitted = None;
                self.art_source = art;
                if self.art_source.is_none() {
                    // Fetch failed — drop the panel so the list reclaims it.
                    self.art_track = None;
                }
            }
        }
        // Drain finished lyrics fetches for the current track.
        while let Ok((id, result)) = self.lyrics_rx.try_recv() {
            if self.lyrics_track.as_deref() == Some(id.as_str()) {
                self.lyrics_state = match result {
                    Ok(Some(lyrics)) => crate::lyrics::LyricsState::Ready(lyrics),
                    Ok(None) => crate::lyrics::LyricsState::Missing,
                    Err(()) => crate::lyrics::LyricsState::Failed,
                };
                self.lyrics_scroll = None;
                self.lyrics_offset = 0;
            }
        }
        // A failed heart toggle rolls its optimistic flip back — unless a
        // newer press owns the state now (rapid `h h` double-toggles).
        while let Ok((id, want, generation, result)) = self.like_rx.try_recv() {
            if generation != self.like_generation {
                continue;
            }
            if self.current_track.as_ref().is_none_or(|t| t.id != id) {
                continue;
            }
            if let Err(e) = result {
                if let Some(track) = self.current_track.as_mut() {
                    track.like = Some(!want);
                }
                self.status_message = format!("Failed to update like — {e}");
            }
        }
        // Finished "add to playlist" — success or error lands in the
        // footer. The song shows up the next time the playlist is opened.
        while let Ok((playlist_name, track_title, result)) = self.add_rx.try_recv() {
            self.status_message = match result {
                Ok(()) => format!("Added '{}' to {}", track_title, playlist_name),
                Err(e) => format!("Failed to add '{}' to {} — {e}", track_title, playlist_name),
            };
        }
        // Finished "remove from playlist": on success the row is already
        // gone (the key handler pulled it eagerly); on failure the exact
        // row rides back and is slotted into its old place, so a refusal
        // leaves the list exactly as it was.
        while let Ok((playlist_id, playlist_name, track, index, result)) = self.rm_rx.try_recv() {
            match result {
                Ok(()) => {
                    self.status_message = format!("Removed '{}' from {}", track.title, playlist_name);
                }
                Err(e) => {
                    if self.current_page == Page::Playlist
                        && self
                            .current_playlist
                            .as_ref()
                            .is_some_and(|p| p.id == playlist_id)
                    {
                        let at = index.min(self.playlist_tracks.len());
                        self.playlist_tracks.insert(at, track.clone());
                        self.status_message =
                            format!("Failed to remove '{}' from {} — {e}", track.title, playlist_name);
                    } else {
                        self.status_message = format!("Failed to remove '{}' — {e}", track.title);
                    }
                    // The playback bar's heart reads the liked state off
                    // this track; a failed unlike must restore it.
                    if self.current_track.as_ref().is_some_and(|t| t.id == track.id) {
                        self.current_track = Some(track);
                    }
                }
            }
        }
        // VLLM's listing converges ~5-6s after a rating change; when that
        // deadline lands, refresh whatever liked-songs view is on screen.
        // A superseded press (a newer `L` bumped the generation) is skipped
        // — its successor owns the state and sets its own deadline.
        let due = self
            .like_refresh_at
            .filter(|(at, _)| Instant::now() >= *at)
            .map(|(_, generation)| generation);
        if let Some(generation) = due {
            self.like_refresh_at = None;
            let on_open = self.current_page == Page::Playlist
                && self.current_playlist.as_ref().is_some_and(|p| p.id == LIKED_PLAYLIST);
            if generation == self.like_generation
                && (on_open || self.preview_for.as_deref() == Some(LIKED_PLAYLIST))
            {
                let api = self.api.clone();
                let tx = self.like_refresh_tx.clone();
                tokio::spawn(async move {
                    let result = api
                        .playlist_tracks(LIKED_PLAYLIST)
                        .await
                        .map_err(|e| e.to_string());
                    let _ = tx.send(result);
                });
            }
        }
        while let Ok(result) = self.like_refresh_rx.try_recv() {
            if let Ok(tracks) = result {
                self.apply_like_refresh(tracks);
            }
        }
        // yt-dlp reports whole percents while it runs; the row's check area
        // shows them until the download lands and the ✓ takes over.
        while let Ok((id, pct)) = self.dl_pct_rx.try_recv() {
            self.dl_progress.insert(id, pct);
        }
        // A finished download lands its file on the registry, which is what
        // the check mark reads — so every list already showing that song
        // lights up without anyone navigating away and back.
        while let Ok((id, result)) = self.dl_rx.try_recv() {
            self.downloading.remove(&id);
            self.dl_progress.remove(&id);
            match result {
                Ok(entry) => {
                    let title = entry.title.clone();
                    self.registry.insert(entry);
                    if self.offline {
                        self.offline_tracks =
                            crate::offline::scan(&self.music_dir, &self.registry);
                    }
                    self.stamp_all();
                    self.status_message = format!("Downloaded ✓ {}", title);
                }
                Err(e) => self.status_message = format!("Download failed: {}", e),
            }
        }
        // Widget actions (MPRIS + queue IPC) apply before this tick's own
        // transitions.
        self.apply_mpris_commands();
        self.apply_ipc_commands();
        // Follow mpv's own clock. A file is only marked playing once mpv
        // reports a positive time, so the progress bar and lyrics begin with
        // the audible audio instead of a fixed 1.5s guess. The timer remains
        // only as a fallback for an unreachable IPC socket.
        self.poll_player_timeline();
        if self.playback_state == PlaybackState::Buffering && !self.mpv_seen {
            if let Some(timer) = self.buffering_timer {
                if timer.elapsed() >= Duration::from_millis(1500) {
                    self.playback_position = 0.0;
                    self.position_sampled_at = Some(Instant::now());
                    self.finish_buffering();
                }
            }
        }
        // mpv ending on its own means the track finished (or the stream
        // failed): step the queue forward.
        if let Some(status) = self.player.poll_exited() {
            let was_active = matches!(
                self.playback_state,
                PlaybackState::Playing | PlaybackState::Buffering | PlaybackState::Paused
            );
            if was_active {
                if status.success() {
                    self.advance_on_end();
                } else {
                    self.position_sampled_at = None;
                    self.playback_state = PlaybackState::Stopped;
                    self.status_message = "Playback error: stream failed".to_string();
                }
            }
        }
        // Mirror the resulting state to desktop media widgets.
        self.sync_mpris();
        self.sync_ipc();
    }

    /// Repaint when a wallpaper tool rewrites one of its palette files.
    ///
    /// Wallpaper changes happen at human speed, so the filesystem is checked
    /// once a second rather than on every 10Hz tick. In the common case the
    /// currently watched file is still the newest one, which lets us skip the
    /// second candidate scan entirely. Successful parses are cached by mtime,
    /// so a stable palette costs one `stat()`. If a writer is truncating the
    /// newest file, it is skipped until the write lands and the current
    /// colours stay on screen.
    fn poll_theme(&mut self) {
        const POLL_INTERVAL: Duration = Duration::from_secs(1);
        if self.theme_poll_at.elapsed() < POLL_INTERVAL {
            return;
        }
        self.theme_poll_at = Instant::now();

        // No theming tool at all: drop back to the terminal's own colours.
        let Some(latest) = crate::theme::Theme::latest_file() else {
            if self.theme_src.is_some() {
                self.theme_src = None;
                self.theme_mtime = None;
                self.theme = crate::theme::Theme::fallback();
            }
            return;
        };

        // Steady state: the file we already follow is still the newest. If
        // its mtime has not moved there is nothing else to inspect, and if it
        // is mid-write we keep the last good theme instead of falling back to
        // an older wallpaper palette.
        if self.theme_src.as_ref() == Some(&latest) {
            let Ok(meta) = std::fs::metadata(&latest) else {
                return;
            };
            let mtime = meta.modified().ok();
            if self.theme_mtime == mtime {
                return;
            }
            if let Some(theme) = crate::theme::Theme::reload_from(&latest) {
                self.theme = theme;
                self.theme_mtime = mtime;
            }
            return;
        }

        let Some(path) = crate::theme::Theme::source_file() else {
            // A palette exists but none parses yet: a writer is probably
            // mid-update. Keep the last good theme unless its file is gone.
            if self.theme_src.as_ref().is_some_and(|src| !src.exists()) {
                self.theme_src = None;
                self.theme_mtime = None;
                self.theme = crate::theme::Theme::fallback();
            }
            return;
        };

        // If we can't stat it, leave the watched state alone and try again
        // next second rather than latching on a missing mtime.
        let Ok(meta) = std::fs::metadata(&path) else {
            return;
        };
        let mtime = meta.modified().ok();
        if self.theme_src.as_ref() == Some(&path) && self.theme_mtime == mtime {
            return;
        }
        // Only latch the path/mtime once the palette parsed. A failed read
        // means the writer is still going; the next poll retries.
        if let Some(theme) = crate::theme::Theme::reload_from(&path) {
            self.theme = theme;
            self.theme_src = Some(path);
            self.theme_mtime = mtime;
        }
    }

    /// Drain a finished background library fetch. Results from a superseded
    /// request are ignored so switching offline/online quickly can't let an
    /// older response overwrite the current view.
    fn poll_library(&mut self) {
        while let Ok((generation, result)) = self.library_rx.try_recv() {
            if generation != self.library_generation {
                continue;
            }
            match result {
                Ok(mut playlists) => {
                    // Group the list by kind: playlists, then albums (artists
                    // and channels are already dropped by the API).
                    playlists.sort_by_key(|p| match p.kind {
                        PlaylistKind::Playlist => 0,
                        PlaylistKind::Album => 1,
                        PlaylistKind::Artist => 2,
                        PlaylistKind::Channel => 3,
                        PlaylistKind::Mood | PlaylistKind::Genre => 4,
                    });
                    self.library_loaded = true;
                    self.selected_index = 0;
                    self.status_message = if playlists.is_empty() {
                        "Your library is empty.".to_string()
                    } else {
                        format!("{} items · Enter to open", playlists.len())
                    };
                    self.playlists = playlists;
                    self.preview_for = None;
                    self.sync_preview();
                }
                Err(e) => self.status_message = format!("Failed to load library: {}", e),
            }
        }
    }

    /// Drain any finished background preview fetches.
    fn poll_preview(&mut self) {
        while let Ok((id, result)) = self.preview_rx.try_recv() {
            if let Ok(tracks) = result {
                let mut tracks = tracks;
                Self::mark(&self.registry, &mut tracks);
                self.preview_cache.insert(id.clone(), tracks.clone());
                if self.preview_for.as_deref() == Some(id.as_str()) {
                    self.preview_tracks = tracks;
                }
            }
        }
    }

    /// Ensure the highlighted library entry has a (cached or in-flight) preview.
    pub fn sync_preview(&mut self) {
        if self.current_page != Page::Library {
            return;
        }
        let id = match self.playlists.get(self.selected_index) {
            Some(playlist) => playlist.id.clone(),
            None => {
                self.preview_for = None;
                self.preview_tracks.clear();
                self.details_scroll = 0;
                return;
            }
        };
        if self.preview_for.as_deref() == Some(id.as_str()) {
            return;
        }
        self.preview_for = Some(id.clone());
        self.details_scroll = 0;
        // A local folder lists straight off disk: no request to make, and
        // nothing worth caching either.
        if crate::offline::is_local(&id) {
            let tracks = crate::offline::tracks_for(&self.offline_tracks, &id, &self.music_dir);
            self.preview_tracks = tracks;
            return;
        }
        if let Some(cached) = self.preview_cache.get(&id) {
            let mut tracks = cached.clone();
            Self::mark(&self.registry, &mut tracks);
            self.preview_tracks = tracks;
            return;
        }
        self.preview_tracks.clear();

        let api = self.api.clone();
        let tx = self.preview_tx.clone();
        tokio::spawn(async move {
            let result = api.playlist_tracks(&id).await.map_err(|e| e.to_string());
            let _ = tx.send((id, result));
        });
    }

    fn list_len(&self) -> usize {
        match self.current_page {
            Page::Search => self.search_results.len(),
            Page::Library => self.playlists.len(),
            Page::Browse => self.browse_rows.len(),
            Page::Playlist => self.playlist_tracks.len(),
            Page::Queue => self.queue.len(),
        }
    }

    pub fn next(&mut self) {
        if self.current_page == Page::Browse {
            self.browse_step(true);
            return;
        }
        let len = self.list_len();
        if len > 0 {
            self.selected_index = (self.selected_index + 1) % len;
        }
        self.sync_preview();
    }

    pub fn previous(&mut self) {
        if self.current_page == Page::Browse {
            self.browse_step(false);
            return;
        }
        let len = self.list_len();
        if len > 0 {
            self.selected_index = if self.selected_index == 0 {
                len - 1
            } else {
                self.selected_index - 1
            };
        }
        self.sync_preview();
    }

    /// Scroll the Library details pane by `pages` viewport heights.
    pub fn scroll_details(&mut self, pages: isize) {
        let step = self.details_viewport.saturating_sub(1).max(1) as isize;
        let max = self.details_scroll_max as isize;
        let next = self.details_scroll as isize + pages * step;
        self.details_scroll = next.clamp(0, max) as u16;
    }

    pub fn selected_track(&self) -> Option<Track> {
        match self.current_page {
            Page::Search => self
                .search_results
                .get(self.selected_index)
                .and_then(|h| h.track().cloned()),
            Page::Playlist => self.playlist_tracks.get(self.selected_index).cloned(),
            Page::Queue => self.queue.get(self.selected_index).cloned(),
            Page::Browse => self
                .browse_rows
                .get(self.selected_index)
                .and_then(|row| match row {
                    BrowseRow::Item(SearchHit::Track(track)) => Some(track.clone()),
                    _ => None,
                }),
            Page::Library => None,
        }
    }

    /// `a` on a highlighted song: overlay the add-to-playlist picker.
    /// Needs a signed-in library to scroll through and a track to give.
    pub fn open_playlist_picker(&mut self) {
        if self.picker_open {
            return;
        }
        if !self.api.is_authenticated() {
            self.status_message = "Sign in first — run `ytmtui setup`.".into();
            return;
        }
        if self.selected_track().is_none() {
            self.status_message = "Highlight a song to add it to a playlist.".into();
            return;
        }
        if self.playlists.is_empty() {
            self.status_message = if self.library_loaded {
                "No editable playlists in your library.".into()
            } else {
                "Your library is still loading — press 'a' again in a moment.".into()
            };
            return;
        }
        self.picker_open = true;
        self.picker_index = 0;
    }

    /// The picker's wrap-around cursor, same style as the page lists.
    pub fn picker_next(&mut self) {
        let len = self.playlists.len();
        if self.picker_open && len > 0 {
            self.picker_index = (self.picker_index + 1) % len;
        }
    }

    pub fn picker_previous(&mut self) {
        let len = self.playlists.len();
        if self.picker_open && len > 0 {
            self.picker_index = if self.picker_index == 0 {
                len - 1
            } else {
                self.picker_index - 1
            };
        }
    }

    /// `Enter` in the picker: hand the highlighted playlist the song the
    /// `a` press started with, then fold up and let the footer report.
    pub fn picker_add_selected(&mut self) {
        self.picker_open = false;
        let Some(track) = self.selected_track() else {
            return;
        };
        let Some(playlist) = self.playlists.get(self.picker_index) else {
            return;
        };
        self.status_message = format!("Adding '{}' to {}...", track.title, playlist.title);
        let _ = self.add_cmd_tx.send((
            playlist.id.clone(),
            playlist.title.clone(),
            track.id.clone(),
            track.title.clone(),
        ));
    }

    pub fn close_playlist_picker(&mut self) {
        self.picker_open = false;
    }

    /// `r` in an open playlist: drop the highlighted song from it.
    /// The row leaves the list at once (with the cursor staying on the
    /// same place) and the queued edit travels to the server; a refusal
    /// puts the row straight back via the result drain above.
    pub fn remove_selected_from_playlist(&mut self) {
        if self.picker_open || self.current_page != Page::Playlist {
            return;
        }
        if self.offline {
            self.status_message = "Offline mode — 'r' only removes from server playlists.".into();
            return;
        }
        let Some(playlist) = self.current_playlist.clone() else {
            return;
        };
        let index = self.selected_index;
        let Some(track) = self.playlist_tracks.get(index).cloned() else {
            return;
        };
        self.playlist_tracks.remove(index);
        // The cursor stays on the same logical spot: the row above the
        // removed one slides into it, the last row just closes up.
        let len = self.playlist_tracks.len();
        self.selected_index = self.selected_index.min(len.saturating_sub(1));
        self.status_message =
            format!("Removing '{}' from {}...", track.title, playlist.title);
        let _ = self.rm_cmd_tx.send((playlist.id.clone(), playlist.title.clone(), track, index));
    }

    /// The currently selected search collection, if any.
    fn selected_collection(&self) -> Option<Playlist> {
        if self.current_page != Page::Search {
            return None;
        }
        self.search_results
            .get(self.selected_index)
            .and_then(|h| h.collection().cloned())
    }

    /// The current filter's `params`, or `None` for the default "Top" search.
    fn current_search_params(&self) -> Option<&str> {
        if self.search_filter == 0 {
            None
        } else {
            self.search_filters
                .get(self.search_filter - 1)
                .map(|f| f.params.as_str())
        }
    }

    pub async fn perform_search(&mut self) {
        let query = self.search_query.trim().to_string();
        if query.is_empty() {
            return;
        }
        self.status_message = format!("Searching for '{}'...", query);
        // Offline, "search" means "find it in the folder" — the network is
        // not consulted, so the page keeps working with no connection.
        if self.offline {
            let needle = query.to_lowercase();
            let mut hits: Vec<SearchHit> = self
                .offline_tracks
                .iter()
                .filter(|t| {
                    t.title.to_lowercase().contains(&needle)
                        || t.artist.to_lowercase().contains(&needle)
                        || t.album
                            .as_deref()
                            .unwrap_or_default()
                            .to_lowercase()
                            .contains(&needle)
                })
                .cloned()
                .map(SearchHit::Track)
                .collect();
            Self::mark_hits(&self.registry, &mut hits);
            self.search_results = hits;
            self.selected_index = 0;
            self.status_message = if self.search_results.is_empty() {
                format!("No local match for '{}'", query)
            } else {
                format!(
                    "{} local results for '{}'",
                    self.search_results.len(),
                    query
                )
            };
            return;
        }
        match self.api.search(&query, self.current_search_params()).await {
            Ok(results) => {
                if !results.filters.is_empty() {
                    self.search_filters = curate_filters(results.filters);
                }
                if results.hits.is_empty() {
                    self.status_message = format!("No results for '{}'", query);
                } else {
                    let mut hits = results.hits;
                    Self::mark_hits(&self.registry, &mut hits);
                    self.search_results = hits;
                    self.selected_index = 0;
                    let filter = self.search_filter_label();
                    self.status_message =
                        format!("Found {} results ({}) for '{}'", self.search_results.len(), filter, query);
                }
            }
            Err(e) => self.status_message = format!("Search failed: {}", e),
        }
    }

    /// Human-readable label of the active search filter.
    pub fn search_filter_label(&self) -> &str {
        if self.search_filter == 0 {
            "Top"
        } else {
            self.search_filters
                .get(self.search_filter - 1)
                .map(|f| f.title.as_str())
                .unwrap_or("Top")
        }
    }

    /// All filter labels including the leading "Top".
    pub fn search_filter_labels(&self) -> Vec<String> {
        let mut labels = vec!["Top".to_string()];
        labels.extend(self.search_filters.iter().map(|f| f.title.clone()));
        labels
    }

    /// Switch to the next (or previous) search filter and re-run the query.
    pub async fn cycle_search_filter(&mut self, forward: bool) {
        let count = self.search_filters.len() + 1;
        if count <= 1 || self.search_query.trim().is_empty() {
            return;
        }
        self.search_filter = if forward {
            (self.search_filter + 1) % count
        } else {
            (self.search_filter + count - 1) % count
        };
        self.perform_search().await;
    }

    /// Fetch the account's library in the background. The TUI keeps drawing
    /// and accepting input immediately; `poll_library` applies the result on
    /// the next tick. Every call gets a new generation so an older, slower
    /// response cannot overwrite a newer one.
    pub fn start_library_load(&mut self) {
        self.library_generation = self.library_generation.wrapping_add(1);
        let generation = self.library_generation;
        self.status_message = "Loading your library...".to_string();
        let api = self.api.clone();
        let tx = self.library_tx.clone();
        tokio::spawn(async move {
            let result = api.library_playlists().await.map_err(|e| e.to_string());
            let _ = tx.send((generation, result));
        });
    }

    /// Open the personalised browse page. It is cached for the session, so
    /// returning to it is instant.
    pub async fn open_browse(&mut self) {
        if self.current_page == Page::Browse {
            return;
        }
        if self.offline {
            self.status_message = "Go online to browse mixes and charts".to_string();
            return;
        }
        let return_page = self.current_page;
        let return_index = self.selected_index;
        self.browse_return_page = return_page;
        self.browse_return_index = return_index;
        // Entering from another page always shows the top-level browse. If
        // the last visit ended inside a mood/genre category, restore its
        // root and select the button that opened it.
        if let Some((rows, index)) = self.browse_back.take() {
            self.browse_rows = rows;
            self.selected_index = index;
            self.status_message = "Browse".to_string();
        }
        self.current_page = Page::Browse;
        if self.browse_loaded {
            if !matches!(
                self.browse_rows.get(self.selected_index),
                Some(BrowseRow::Item(_))
            ) {
                self.selected_index = self.first_browse_item();
            }
            return;
        }

        self.status_message = "Loading mixes, trending and categories...".to_string();
        match self.api.for_you().await {
            Ok(shelves) => {
                let mut slots: Vec<Option<BrowseShelf>> = vec![None; BROWSE_SECTIONS.len()];
                for shelf in shelves {
                    if let Some(section) = browse_section_for_title(&shelf.title) {
                        slots[section] = Some(shelf);
                    }
                }
                self.browse_shelves = slots;
                self.browse_loaded = true;
                self.select_browse_section(0);
            }
            Err(e) => {
                // Keep the user where they were rather than parking them on
                // an empty page; the footer still explains why.
                self.current_page = return_page;
                self.selected_index = return_index;
                self.status_message = format!("Failed to load browse: {}", e);
            }
        }
    }

    /// Open the browse page at a section (by its index), loading the
    /// sections first when needed. Used by the 1–5 shortcuts.
    pub async fn open_browse_section(&mut self, section: usize) {
        let section = section.min(BROWSE_SECTIONS.len() - 1);
        self.input_mode = InputMode::Normal;
        if self.current_page != Page::Browse {
            self.open_browse().await;
        } else if self.browse_back.is_some() {
            // A category is open; selecting a section returns to the root view.
            self.browse_back = None;
        }
        if self.current_page == Page::Browse && self.browse_loaded {
            self.select_browse_section(section);
        }
    }

    /// Show one root shelf under the active section. The section titles
    /// live in the content header, so the list starts directly with its
    /// items.
    fn select_browse_section(&mut self, section: usize) {
        self.browse_section = Some(section);
        self.browse_back = None;
        let items = self
            .browse_shelves
            .get(section)
            .and_then(Option::as_ref)
            .map(|shelf| shelf.items.clone());
        if let Some(items) = items {
            let count = items.len();
            self.browse_rows = items.into_iter().map(BrowseRow::Item).collect();
            self.selected_index = self.first_browse_item();
            self.status_message = format!("{count} items");
        } else {
            self.browse_rows.clear();
            self.selected_index = 0;
            self.status_message = "nothing here right now".to_string();
        }
    }

    fn set_browse_rows(&mut self, rows: Vec<BrowseRow>) {
        self.browse_rows = rows;
        self.browse_loaded = true;
        self.selected_index = self.first_browse_item();
    }

    fn first_browse_item(&self) -> usize {
        self.browse_rows
            .iter()
            .position(|row| matches!(row, BrowseRow::Item(_)))
            .unwrap_or(0)
    }

    /// Move the cursor through item rows, skipping section headers.
    fn browse_step(&mut self, forward: bool) {
        let len = self.browse_rows.len();
        if len == 0 {
            return;
        }
        let mut index = self.selected_index.min(len - 1);
        for _ in 0..len {
            index = if forward {
                (index + 1) % len
            } else {
                (index + len - 1) % len
            };
            if matches!(self.browse_rows[index], BrowseRow::Item(_)) {
                self.selected_index = index;
                return;
            }
        }
    }

    /// Esc on the browse page: category history first, then wherever the
    /// browse page was opened from.
    pub fn leave_browse(&mut self) {
        if let Some((rows, index)) = self.browse_back.take() {
            self.browse_rows = rows;
            self.selected_index = index;
            self.status_message = "Browse".to_string();
        } else {
            self.current_page = self.browse_return_page;
            self.selected_index = self.browse_return_index;
            self.sync_preview();
        }
    }

    async fn open_browse_selected(&mut self) {
        let Some(BrowseRow::Item(item)) = self.browse_rows.get(self.selected_index).cloned() else {
            return;
        };
        match item {
            SearchHit::Track(track) => {
                let tracks = self.browse_track_list();
                let index = tracks
                    .iter()
                    .position(|t| t.id == track.id)
                    .unwrap_or(0);
                self.queue = tracks;
                self.queue_index = index;
                self.rebuild_shuffle_order(None);
                self.play_track(track);
            }
            SearchHit::Collection(playlist) => {
                let Some((browse_id, params)) = crate::api::category_parts(&playlist.id) else {
                    self.open_collection(playlist).await;
                    return;
                };
                let browse_id = browse_id.to_string();
                let params = params.to_string();
                let title = playlist.title.clone();
                self.status_message = format!("Loading {}...", title);
                let previous_rows = self.browse_rows.clone();
                let previous_index = self.selected_index;
                match self.api.category_playlists(&browse_id, &params).await {
                    Ok(items) if !items.is_empty() => {
                        let count = items.len();
                        let mut rows = vec![BrowseRow::Header(title.clone())];
                        rows.extend(items.into_iter().map(|p| {
                            BrowseRow::Item(SearchHit::Collection(p))
                        }));
                        self.browse_back = Some((previous_rows, previous_index));
                        self.set_browse_rows(rows);
                        self.status_message = format!("{title} · {count} collections");
                    }
                    Ok(_) => {
                        self.status_message = format!("Nothing found in {title}");
                    }
                    Err(e) => {
                        self.status_message = format!("Failed to load {title}: {}", e);
                    }
                }
            }
        }
    }

    /// Tracks in the same section as the highlighted browse row, used as the
    /// playback queue when a trending song starts.
    fn browse_track_list(&self) -> Vec<Track> {
        if self.browse_rows.is_empty() {
            return Vec::new();
        }
        let mut start = 0;
        for i in (0..=self.selected_index.min(self.browse_rows.len() - 1)).rev() {
            if matches!(self.browse_rows[i], BrowseRow::Header(_)) {
                start = i + 1;
                break;
            }
        }
        let mut end = self.browse_rows.len();
        for i in start..self.browse_rows.len() {
            if matches!(self.browse_rows[i], BrowseRow::Header(_)) {
                end = i;
                break;
            }
        }
        self.browse_rows[start..end]
            .iter()
            .filter_map(|row| match row {
                BrowseRow::Item(SearchHit::Track(track)) => Some(track.clone()),
                _ => None,
            })
            .collect()
    }

    /// Whether a track is available without the network: a local file is so
    /// by definition, anything else once a download tied it to a video id.
    fn saved(registry: &crate::offline::Registry, id: &str) -> bool {
        crate::offline::is_local(id) || registry.contains(id)
    }

    /// Stamp the check mark onto a list from the registry.
    ///
    /// Kept free of `&self` so it can be aimed at any of the several track
    /// lists this struct owns — the registry and a list are disjoint fields,
    /// but a `&self` method call would borrow all of both.
    fn mark(registry: &crate::offline::Registry, tracks: &mut [Track]) {
        for t in tracks.iter_mut() {
            t.downloaded = Self::saved(registry, &t.id);
        }
    }

    /// [`mark`](Self::mark) for search hits, which are half collections.
    fn mark_hits(registry: &crate::offline::Registry, hits: &mut [SearchHit]) {
        for hit in hits.iter_mut() {
            if let SearchHit::Track(t) = hit {
                t.downloaded = Self::saved(registry, &t.id);
            }
        }
    }

    /// Re-stamp every list that could be showing, plus the preview cache and
    /// the now-playing row. Runs when a download lands so the tick's very
    /// next frame already has it.
    fn stamp_all(&mut self) {
        let registry = &self.registry;
        Self::mark(registry, &mut self.playlist_tracks);
        Self::mark(registry, &mut self.preview_tracks);
        Self::mark_hits(registry, &mut self.search_results);
        Self::mark(registry, &mut self.queue);
        if let Some(t) = self.current_track.as_mut() {
            t.downloaded = Self::saved(registry, &t.id);
        }
        for tracks in self.preview_cache.values_mut() {
            Self::mark(registry, tracks);
        }
    }

    /// `o`: flip between the account's library and the local music folder.
    ///
    /// Going back refetches the real library rather than restoring a
    /// snapshot — it is one request, and it can't hand back a list that went
    /// stale while we were away. Playback is deliberately left alone: a
    /// queue mixing local files and online tracks keeps running either way.
    pub fn toggle_offline(&mut self) {
        if self.offline {
            self.offline = false;
            self.preview_cache.clear();
            self.current_page = Page::Library;
            self.start_library_load();
            return;
        }
        self.offline = true;
        // Any library request still in flight is for the online view now.
        self.library_generation = self.library_generation.wrapping_add(1);
        // Re-read `config.json` so an edited path applies from here on.
        self.music_dir = crate::offline::music_dir();
        self.offline_tracks = crate::offline::scan(&self.music_dir, &self.registry);
        self.preview_cache.clear();
        self.load_offline_library();
    }

    /// Fill the sidebar with the local folder's structure.
    fn load_offline_library(&mut self) {
        self.playlists = crate::offline::folders(&self.offline_tracks, &self.music_dir);
        self.library_loaded = true;
        self.current_page = Page::Library;
        self.selected_index = 0;
        self.preview_for = None;
        self.preview_tracks.clear();
        self.details_scroll = 0;
        self.status_message = if self.offline_tracks.is_empty() {
            format!(
                "Offline · {} holds no music — press d online to save a song",
                self.music_dir.display()
            )
        } else {
            format!("Offline · {} files · Enter to open", self.offline_tracks.len())
        };
        self.sync_preview();
    }

    /// Percent complete for a song yt-dlp is currently saving. The track
    /// row shows it where the ✓ will land; `None` when the song isn't
    /// downloading.
    pub fn download_pct(&self, id: &str) -> Option<u8> {
        self.dl_progress.get(id).copied()
    }

    /// `d`: download the highlighted song into the music folder — its cover
    /// embedded in the audio — so it plays offline and shows a check mark in
    /// online lists.
    pub fn download_selected(&mut self) {
        if self.offline {
            self.status_message = "Already offline".to_string();
            return;
        }
        let Some(track) = self.selected_track() else {
            self.status_message = "Highlight a song first".to_string();
            return;
        };
        if crate::offline::is_local(&track.id) {
            self.status_message = "Already on disk".to_string();
            return;
        }
        if self.registry.contains(&track.id) {
            self.status_message = "Already downloaded".to_string();
            return;
        }
        if !self.downloading.insert(track.id.clone()) {
            self.status_message = format!("Downloading '{}'…", track.title);
            return;
        }
        let id = track.id.clone();
        let url = format!("https://www.youtube.com/watch?v={}", id);
        let dir = self.music_dir.clone();
        let tx = self.dl_tx.clone();
        let ptx = self.dl_pct_tx.clone();
        let api = self.api.clone();
        let lyrics_id = id.clone();
        let title = track.title.clone();
        let artist = track.artist.clone();
        let duration = parse_duration(&track.duration);
        self.dl_progress.insert(id.clone(), 0);
        self.status_message = format!("Downloading '{}'…", track.title);
        tokio::spawn(async move {
            // Fetch lyrics while yt-dlp works. The audio still reports back
            // the moment it lands; the `.lrc` sidecar follows as soon as
            // both halves finish, so lyrics never delay the ✓.
            let lyrics = tokio::spawn(async move {
                crate::lyrics::fetch_for_track(&api, &lyrics_id, &title, &artist, duration).await
            });
            let pid = id.clone();
            let result = crate::offline::download(&url, &dir, &track, move |pct| {
                let _ = ptx.send((pid.clone(), pct));
            })
            .await
            .map_err(|e| e.to_string());
            let audio = result.as_ref().ok().map(|entry| PathBuf::from(&entry.path));
            let _ = tx.send((id, result));
            match audio {
                Some(path) => {
                    if let Ok(Ok(Some(lyrics))) = lyrics.await {
                        let _ = crate::lyrics::write_sidecar(&path, &lyrics);
                    }
                }
                None => lyrics.abort(),
            }
        });
    }

    pub async fn activate_selected(&mut self) {
        match self.current_page {
            Page::Library => self.open_selected_playlist().await,
            Page::Browse => self.open_browse_selected().await,
            Page::Search => {
                if let Some(collection) = self.selected_collection() {
                    self.open_collection(collection).await;
                } else {
                    self.play_selected();
                }
            }
            Page::Queue => self.jump_to_selected(),
            _ => self.play_selected(),
        }
    }

    async fn open_selected_playlist(&mut self) {
        let playlist = match self.playlists.get(self.selected_index).cloned() {
            Some(p) => p,
            None => return,
        };
        self.open_collection(playlist).await;
    }

    /// Open any collection (playlist / album / artist) as a track list.
    async fn open_collection(&mut self, playlist: Playlist) {
        // A local folder is already on disk — listing it is a filter, not a
        // request, so it opens with no round trip and works with no network.
        if crate::offline::is_local(&playlist.id) {
            let tracks =
                crate::offline::tracks_for(&self.offline_tracks, &playlist.id, &self.music_dir);
            if tracks.is_empty() {
                self.status_message = format!("'{}' is empty", playlist.title);
                return;
            }
            self.show_collection(playlist, tracks);
            return;
        }
        self.status_message = format!("Loading '{}'...", playlist.title);
        match self.api.playlist_tracks(&playlist.id).await {
            Ok(mut tracks) if !tracks.is_empty() => {
                Self::mark(&self.registry, &mut tracks);
                self.show_collection(playlist, tracks);
            }
            Ok(_) => self.status_message = format!("'{}' is empty", playlist.title),
            Err(e) => self.status_message = format!("Failed to load: {}", e),
        }
    }

    /// Swap into the collection view with a track list that's already loaded.
    fn show_collection(&mut self, playlist: Playlist, tracks: Vec<Track>) {
        self.status_message = format!("{} · {} tracks", playlist.title, tracks.len());
        self.playlist_tracks = tracks;
        self.current_playlist = Some(playlist);
        self.playlist_return_page = self.current_page;
        self.playlist_return_index = self.selected_index;
        self.current_page = Page::Playlist;
        self.selected_index = 0;
    }

    pub fn play_selected(&mut self) {
        if let Some(t) = self.selected_track() {
            // Playing from a page replaces the queue with that page's list,
            // the way playing a collection does. Find the track's position in
            // that list instead of assuming the page row and queue indices
            // line up (browse sections contain headers and collections).
            self.queue = self.current_track_list();
            let index = self
                .queue
                .iter()
                .position(|queued| queued.id == t.id)
                .unwrap_or(0);
            self.queue_index = index.min(self.queue.len().saturating_sub(1));
            self.rebuild_shuffle_order(None);
            self.play_track(t);
        }
    }

    /// Start playback of `t`. The clock stays at zero until mpv reports
    /// that audio is actually running; the duration is corrected from mpv
    /// once the file is loaded.
    fn play_track(&mut self, t: Track) {
        self.status_message = "buffer♪ng".to_string();
        if let Err(e) = self.player.play(&t.id) {
            self.status_message = format!("Playback error: {}", e);
            self.playback_state = PlaybackState::Stopped;
            return;
        }
        self.playback_position = 0.0;
        self.position_sampled_at = None;
        self.mpv_seen = false;
        self.playback_duration_secs = parse_duration(&t.duration).unwrap_or(210);
        self.current_track = Some(t);
        self.playback_state = PlaybackState::Buffering;
        self.buffering_timer = Some(Instant::now());
        self.sync_art();
        self.sync_lyrics();
    }

    /// Read mpv's authoritative timeline. While buffering, the first
    /// positive position is the moment audio started — the state flips to
    /// Playing on that same tick, so lyrics and progress are never ahead of
    /// the sound or trailing behind it.
    fn poll_player_timeline(&mut self) {
        if !matches!(
            self.playback_state,
            PlaybackState::Buffering | PlaybackState::Playing | PlaybackState::Paused
        ) {
            return;
        }
        let Some(timeline) = self.player.timeline() else {
            return;
        };
        self.mpv_seen = true;
        if let Some(position) = timeline.position.filter(|p| p.is_finite()) {
            self.playback_position = position.max(0.0);
            self.position_sampled_at = Some(Instant::now());
            if self.playback_state == PlaybackState::Buffering && position > 0.0 {
                self.finish_buffering();
            }
        }
        if let Some(duration) = timeline.duration.filter(|d| d.is_finite() && *d > 0.0) {
            let secs = duration.round() as u64;
            if secs > 0 && secs != self.playback_duration_secs {
                self.playback_duration_secs = secs;
                if let Some(track) = self.current_track.as_mut() {
                    let label = format_duration(secs);
                    if track.duration.as_deref() != Some(label.as_str()) {
                        track.duration = Some(label);
                    }
                }
            }
        }
    }

    /// Buffering is over: audio is running (or the IPC fallback expired).
    fn finish_buffering(&mut self) {
        self.playback_state = PlaybackState::Playing;
        self.buffering_timer = None;
        self.status_message = "Play♪ng".to_string();
    }

    /// Start (or reuse) the background cover-art fetch for the current track.
    fn sync_art(&mut self) {
        let Some(track) = self.current_track.clone() else {
            self.art_track = None;
            self.art_source = None;
            self.art_fitted = None;
            return;
        };
        if self.art_track.as_deref() == Some(track.id.as_str()) {
            return; // already loaded or in flight
        }
        self.art_track = Some(track.id.clone());
        self.art_source = None;
        self.art_fitted = None;
        let tx = self.art_tx.clone();
        let urls = crate::art::cover_urls(&track);
        let id = track.id;
        tokio::spawn(async move {
            let art = crate::art::fetch_cover(urls).await;
            let _ = tx.send((id, art));
        });
    }

    /// Toggle the lyrics pane (the `l` sidebar) and (re)fetch when it
    /// opens on a new track.
    pub fn toggle_lyrics(&mut self) {
        self.lyrics_open = !self.lyrics_open;
        if self.lyrics_open {
            self.lyrics_scroll = None;
            self.lyrics_offset = 0;
            self.sync_lyrics();
        }
    }

    /// `L`: like the currently-playing song — cycle between liked and no
    /// rating (a disliked track lands on LIKE). The flip is optimistic: the
    /// playback bar's heart and the status message change on the keypress
    /// itself, and the network call only rolls them back if it fails.
    pub fn toggle_like(&mut self) {
        let Some(track) = self.current_track.clone() else {
            return;
        };
        if track.id.is_empty() {
            return;
        }
        // A file on disk has no video id to rate against.
        if crate::offline::is_local(&track.id) {
            self.status_message = "Local file — no rating to change".to_string();
            return;
        }
        // Unknown counts as not-liked: the first press likes.
        let want = track.like != Some(true);
        // Instant feedback — the request confirms (or reverts) behind it.
        if let Some(t) = self.current_track.as_mut() {
            t.like = Some(want);
        }
        self.status_message = if want {
            "Added to Liked songs".to_string()
        } else {
            "Removed from Liked songs".to_string()
        };
        // The liked-songs list on screen updates with the press too, and
        // re-syncs with the server once VLLM's listing has converged.
        self.like_view_edit(&track.id, want);
        self.preview_cache.remove(LIKED_PLAYLIST);

        self.like_generation += 1;
        let generation = self.like_generation;
        self.like_refresh_at = Some((Instant::now() + Duration::from_secs(9), generation));
        // Queued, not spawned: presses apply one at a time in order.
        let _ = self
            .like_cmd_tx
            .send((track.id.clone(), want, generation));
    }

    /// Live-update the liked-songs list on screen for an `L` press:
    /// un-liking drops the row, liking puts it at the top (VLLM lists
    /// newest-first, so a fresh like lands on index 0). The selection
    /// sticks to its track as rows move.
    fn like_view_edit(&mut self, vid: &str, want: bool) {
        let on_open = self.current_page == Page::Playlist
            && self.current_playlist.as_ref().is_some_and(|p| p.id == LIKED_PLAYLIST);
        let on_preview = self.current_page == Page::Library
            && self.preview_for.as_deref() == Some(LIKED_PLAYLIST);
        if !on_open && !on_preview {
            return;
        }
        let Some(track) = self.current_track.clone() else {
            return;
        };
        if on_open {
            match self.playlist_tracks.iter().position(|t| t.id == vid) {
                Some(i) if !want => {
                    self.playlist_tracks.remove(i);
                    if i < self.selected_index {
                        self.selected_index -= 1;
                    } else if i == self.selected_index {
                        self.selected_index =
                            self.selected_index.min(self.playlist_tracks.len().saturating_sub(1));
                    }
                }
                // Listed and staying listed: the server owns the ordering —
                // the delayed re-sync reconciles anything subtle.
                None if want => {
                    self.playlist_tracks.insert(0, track.clone());
                    // Every row below shifted down one — carry the
                    // selection along so it stays on the same track.
                    self.selected_index = (self.selected_index + 1)
                        .min(self.playlist_tracks.len().saturating_sub(1));
                }
                _ => {}
            }
        }
        if on_preview {
            match self.preview_tracks.iter().position(|t| t.id == vid) {
                Some(i) if !want => {
                    self.preview_tracks.remove(i);
                }
                None if want => self.preview_tracks.insert(0, track),
                _ => {}
            }
        }
    }

    /// Swap the on-screen liked-songs rows for the server's latest cut,
    /// keeping the selection on the same track where it still exists.
    fn apply_like_refresh(&mut self, mut tracks: Vec<Track>) {
        // This refresh rewrites the visible liked list wholesale, so it has
        // to carry the check marks too or downloading a liked song would
        // erase its ✓ until the next reload.
        Self::mark(&self.registry, &mut tracks);
        // The refreshed VLLM listing is the authority on what's liked
        // (fully paginated), so re-derive the heart from it: a rate call
        // can report success and still not stick, and without this the
        // playback bar would keep a lie until the next press.
        if let Some(track) = self.current_track.as_mut() {
            if !track.id.is_empty() {
                track.like = Some(tracks.iter().any(|t| t.id == track.id));
            }
        }
        if self.current_page == Page::Playlist
            && self.current_playlist.as_ref().is_some_and(|p| p.id == LIKED_PLAYLIST)
        {
            let selected = self
                .playlist_tracks
                .get(self.selected_index)
                .map(|t| t.id.clone());
            self.playlist_tracks = tracks.clone();
            if let Some(sid) = selected {
                self.selected_index = self
                    .playlist_tracks
                    .iter()
                    .position(|t| t.id == sid)
                    .unwrap_or_else(|| {
                        self.selected_index.min(self.playlist_tracks.len().saturating_sub(1))
                    });
            }
        }
        if self.preview_for.as_deref() == Some(LIKED_PLAYLIST) {
            self.preview_tracks = tracks.clone();
            self.preview_cache
                .insert(LIKED_PLAYLIST.to_string(), tracks);
        }
    }

    /// Start (or reuse) the background lyrics fetch for the pane.
    fn sync_lyrics(&mut self) {
        if !self.lyrics_open {
            return;
        }
        let Some(track) = self.current_track.clone() else {
            self.lyrics_track = None;
            self.lyrics_state = crate::lyrics::LyricsState::Missing;
            return;
        };
        if self.lyrics_track.as_deref() == Some(track.id.as_str()) {
            return; // already loaded or in flight
        }
        self.lyrics_track = Some(track.id.clone());
        self.lyrics_state = crate::lyrics::LyricsState::Loading;
        self.lyrics_scroll = None;
        self.lyrics_offset = 0;
        let tx = self.lyrics_tx.clone();
        let api = self.api.clone();
        let id = track.id;
        let title = track.title.clone();
        let artist = track.artist.clone();
        let duration = parse_duration(&track.duration);
        tokio::spawn(async move {
            let result = crate::lyrics::fetch_for_track(&api, &id, &title, &artist, duration).await;
            let _ = tx.send((id, result));
        });
    }

    /// PgUp/PgDn on the open lyrics pane: shift the view by `delta` rows
    /// from the currently rendered offset; autoscroll resumes after 3s.
    pub fn scroll_lyrics(&mut self, delta: i32) {
        if !self.lyrics_open {
            return;
        }
        let base = self.lyrics_offset;
        let target = if delta >= 0 {
            base.saturating_add(delta as u16)
        } else {
            base.saturating_sub((-delta) as u16)
        };
        self.lyrics_scroll = Some((target, Instant::now()));
    }

    /// The playable tracks on the current page, in display order. Used to set up
    /// the playback context and for skipping tracks.
    fn current_track_list(&self) -> Vec<Track> {
        match self.current_page {
            Page::Search => self
                .search_results
                .iter()
                .filter_map(|h| h.track().cloned())
                .collect(),
            Page::Playlist => self.playlist_tracks.clone(),
            Page::Browse => self.browse_track_list(),
            _ => Vec::new(),
        }
    }

    /// Whether a track is loaded and currently playing or paused.
    pub fn is_music_active(&self) -> bool {
        self.current_track.is_some()
            && matches!(
                self.playback_state,
                PlaybackState::Playing | PlaybackState::Paused
            )
    }

    /// Seek the current track by `delta` seconds (negative rewinds). No-op
    /// unless music is actively playing or paused.
    pub fn seek_relative(&mut self, delta: i64) {
        if !self.is_music_active() {
            return;
        }
        let current = self.position_secs_f64();
        let total = self.playback_duration_secs as f64;
        let target = (current + delta as f64).clamp(0.0, total.max(0.0));
        let effective = (target - current).round() as i64;
        self.playback_position = target;
        self.position_sampled_at = if self.playback_state == PlaybackState::Playing {
            Some(Instant::now())
        } else {
            None
        };
        // Compare the real positions, not the rounded label: a -10s press
        // from 0.2s clamps to zero and must still reach mpv.
        if (target - current).abs() > 0.01 {
            self.player.seek_to(target);
        }
        if effective != 0 {
            // A plain signed number instead of a rewind/fast-forward glyph:
            // emoji render double-width (or as colour emoji) in some
            // terminals and knock the footer out of alignment.
            self.status_message = format!(
                "{}{}s",
                if effective > 0 { "+" } else { "-" },
                effective.abs()
            );
        }
    }

    /// Index of the queue track to advance to, honouring shuffle order and
    /// repeat mode. `None` means the queue has run out (repeat off).
    fn next_queue_index(&self) -> Option<usize> {
        if self.queue.is_empty() {
            return None;
        }
        if self.shuffle {
            let pos = self.shuffle_order.iter().position(|&i| i == self.queue_index)?;
            if pos + 1 < self.shuffle_order.len() {
                return Some(self.shuffle_order[pos + 1]);
            }
            return if self.repeat == RepeatMode::All {
                Some(self.shuffle_order[0])
            } else {
                None
            };
        }
        if self.queue_index + 1 < self.queue.len() {
            Some(self.queue_index + 1)
        } else if self.repeat == RepeatMode::All {
            Some(0)
        } else {
            None
        }
    }

    /// Index of the previous queue track; `previous` always wraps around.
    fn previous_queue_index(&self) -> Option<usize> {
        if self.queue.is_empty() {
            return None;
        }
        if self.shuffle {
            let pos = self.shuffle_order.iter().position(|&i| i == self.queue_index)?;
            return Some(if pos == 0 {
                *self.shuffle_order.last()?
            } else {
                self.shuffle_order[pos - 1]
            });
        }
        Some(if self.queue_index == 0 {
            self.queue.len() - 1
        } else {
            self.queue_index - 1
        })
    }

    /// Advance to the next queue track. Repeat "one" only affects the
    /// automatic end-of-track advance (see `advance_on_end`); an explicit
    /// skip always moves on, and running out stops playback.
    pub fn play_next(&mut self) {
        match self.next_queue_index() {
            Some(i) => self.jump_to(i),
            None => {
                self.stop_playback();
                self.status_message = "End of queue".to_string();
            }
        }
    }

    /// Step back through the queue (wraps to the end).
    pub fn play_previous(&mut self) {
        if let Some(i) = self.previous_queue_index() {
            self.jump_to(i);
        }
    }

    /// Switch playback to `queue[i]` and start it.
    fn jump_to(&mut self, i: usize) {
        let Some(t) = self.queue.get(i).cloned() else {
            return;
        };
        self.queue_index = i;
        if self.shuffle {
            self.rebuild_shuffle_order(None);
        }
        self.play_track(t);
    }

    /// A track ended on its own: honour repeat one, otherwise advance.
    fn advance_on_end(&mut self) {
        let next = if self.repeat == RepeatMode::One {
            Some(self.queue_index)
        } else {
            self.next_queue_index()
        };
        match next {
            Some(i) if i < self.queue.len() => self.jump_to(i),
            _ => {
                self.position_sampled_at = None;
                self.playback_state = PlaybackState::Stopped;
                self.status_message = "End of queue".to_string();
            }
        }
    }

    /// Rebuild the shuffled playback order after a queue change: the current
    /// track first, then the rest shuffled. `keep_next` pins one track to
    /// play right after the current one (play-next inserts).
    fn rebuild_shuffle_order(&mut self, keep_next: Option<usize>) {
        self.queue_version = self.queue_version.wrapping_add(1);
        if !self.shuffle {
            self.shuffle_order.clear();
            return;
        }
        use rand::seq::SliceRandom;
        let len = self.queue.len();
        let mut rest: Vec<usize> = (0..len)
            .filter(|&i| i != self.queue_index && keep_next != Some(i))
            .collect();
        rest.shuffle(&mut rand::thread_rng());
        let mut order = Vec::with_capacity(len);
        if self.queue_index < len {
            order.push(self.queue_index);
        }
        if let Some(k) = keep_next.filter(|&k| k < len && k != self.queue_index) {
            order.push(k);
        }
        order.extend(rest);
        self.shuffle_order = order;
    }

    /// `z`: open the queue page, remembering where to return.
    pub fn open_queue(&mut self) {
        if self.current_page == Page::Queue {
            return;
        }
        self.queue_return_page = self.current_page;
        self.queue_return_index = self.selected_index;
        self.current_page = Page::Queue;
        self.selected_index = self.queue_index.min(self.queue.len().saturating_sub(1));
    }

    /// `Esc` on the queue page: back where it was opened from.
    pub fn leave_queue(&mut self) {
        self.current_page = self.queue_return_page;
        self.selected_index = self.queue_return_index;
    }

    /// Queue page `Enter`: play the highlighted track right now.
    pub fn jump_to_selected(&mut self) {
        if self.current_page != Page::Queue {
            return;
        }
        self.jump_to(self.selected_index);
    }

    /// `X` on the queue page: move the highlighted track up to play next.
    /// Elsewhere: queue the highlighted track to play after the current one.
    pub fn queue_add_next(&mut self, track: Track) {
        if self.queue.is_empty() {
            self.queue = vec![track];
            self.queue_index = 0;
            self.rebuild_shuffle_order(None);
            if self.current_track.is_none() {
                self.play_track(self.queue[0].clone());
            }
            return;
        }
        let pos = self.queue_index + 1;
        self.queue.insert(pos, track.clone());
        // Inserting shifts every later index, so rebuild the shuffle order
        // with the new track pinned right after the current one.
        self.rebuild_shuffle_order(Some(pos));
        self.status_message = format!("Playing next: {} - {}", track.title, track.artist);
    }

    /// `X`: on the queue page, promote the highlighted track to play next;
    /// on any other page, queue the highlighted track after the current one.
    pub fn queue_focused_next(&mut self) {
        if self.current_page == Page::Queue {
            let i = self.selected_index;
            if i < self.queue.len() && i != self.queue_index {
                let t = self.queue.remove(i);
                if i < self.queue_index {
                    self.queue_index -= 1;
                }
                let pos = self.queue_index + 1;
                self.queue.insert(pos, t);
                self.selected_index = pos;
                self.rebuild_shuffle_order(Some(pos));
                self.status_message = "Playing next".to_string();
            }
            return;
        }
        if let Some(t) = self.selected_track() {
            self.queue_add_next(t);
        }
    }

    /// `Z`: append the highlighted track to the end of the queue.
    pub fn queue_append(&mut self, track: Track) {
        if self.queue.is_empty() {
            self.queue = vec![track];
            self.queue_index = 0;
            self.rebuild_shuffle_order(None);
            if self.current_track.is_none() {
                self.play_track(self.queue[0].clone());
            }
            return;
        }
        self.queue.push(track.clone());
        self.rebuild_shuffle_order(None);
        self.status_message = format!("Added to queue: {} - {}", track.title, track.artist);
    }

    /// Queue page `d`: remove the highlighted track. Removing the current
    /// track moves playback onto whatever now sits in its place.
    pub fn queue_remove_selected(&mut self) {
        if self.current_page != Page::Queue {
            return;
        }
        let i = self.selected_index;
        if i >= self.queue.len() {
            return;
        }
        let was_current = i == self.queue_index;
        let removed = self.queue.remove(i);
        if self.queue.is_empty() {
            self.queue_version = self.queue_version.wrapping_add(1);
            self.queue_index = 0;
            self.selected_index = 0;
            if was_current {
                self.stop_playback();
                self.current_track = None;
                self.sync_art();
                self.sync_lyrics();
            }
        } else {
            if was_current {
                self.queue_index = i.min(self.queue.len() - 1);
            } else if i < self.queue_index {
                self.queue_index -= 1;
            }
            if self.selected_index >= self.queue.len() {
                self.selected_index = self.queue.len() - 1;
            }
            self.rebuild_shuffle_order(None);
            if was_current {
                self.jump_to(self.queue_index);
            }
        }
        self.status_message = format!("Removed: {}", removed.title);
    }

    /// Queue page `J`/`K`: move the highlighted track down/up, keeping the
    /// current track in place.
    pub fn queue_move(&mut self, down: bool) {
        if self.current_page != Page::Queue {
            return;
        }
        let i = self.selected_index;
        let len = self.queue.len();
        if len < 2 {
            return;
        }
        let j = if down {
            if i + 1 >= len {
                return;
            }
            i + 1
        } else {
            if i == 0 {
                return;
            }
            i - 1
        };
        self.queue.swap(i, j);
        if self.queue_index == i {
            self.queue_index = j;
        } else if self.queue_index == j {
            self.queue_index = i;
        }
        self.selected_index = j;
        self.rebuild_shuffle_order(None);
    }

    /// `Ctrl+R`: cycle repeat off → all → one.
    pub fn cycle_repeat(&mut self) {
        self.repeat = self.repeat.next();
        self.status_message = format!("Repeat: {}", self.repeat.label());
    }

    /// `Ctrl+S`: toggle shuffle.
    pub fn toggle_shuffle(&mut self) {
        self.shuffle = !self.shuffle;
        self.rebuild_shuffle_order(None);
        self.status_message = format!("Shuffle: {}", if self.shuffle { "on" } else { "off" });
    }

    pub fn toggle_play(&mut self) {
        if self.current_track.is_none() {
            return;
        }
        match self.playback_state {
            PlaybackState::Playing => {
                self.playback_position = self.position_secs_f64();
                self.position_sampled_at = None;
                self.player.pause();
                self.playback_state = PlaybackState::Paused;
                self.status_message = "Paused".to_string();
            }
            PlaybackState::Paused => {
                self.player.pause();
                self.playback_state = PlaybackState::Playing;
                self.position_sampled_at = Some(Instant::now());
                self.status_message = "Resumed".to_string();
            }
            PlaybackState::Buffering => {}
            PlaybackState::Stopped => {}
        }
    }

    /// Current playback position with sub-second precision. The value comes
    /// from mpv and is interpolated only until the next tick, so progress
    /// and lyrics stay tied to what is actually audible.
    pub fn position_secs_f64(&self) -> f64 {
        let active = if self.playback_state == PlaybackState::Playing {
            self.position_sampled_at
                .map(|s| s.elapsed().as_secs_f64())
                .unwrap_or(0.0)
        } else {
            0.0
        };
        (self.playback_position + active).clamp(0.0, self.playback_duration_secs as f64)
    }

    /// Whole-second view of [`Self::position_secs_f64`] for labels, MPRIS
    /// and the queue bridge.
    pub fn position_secs(&self) -> u64 {
        self.position_secs_f64().floor() as u64
    }

    /// Stop playback entirely (MPRIS `Stop`): end mpv but keep the last
    /// track's metadata so widgets can still show what was playing.
    pub fn stop_playback(&mut self) {
        self.playback_position = self.position_secs_f64();
        self.position_sampled_at = None;
        self.player.stop();
        self.playback_state = PlaybackState::Stopped;
        self.status_message = "Stopped".to_string();
    }

    /// Apply commands pushed from the MPRIS widget (Quickshell & co.).
    fn apply_mpris_commands(&mut self) {
        use crate::mpris::MprisCmd;
        while let Ok(cmd) = self.mpris_rx.try_recv() {
            match cmd {
                MprisCmd::Play => match self.playback_state {
                    PlaybackState::Paused => self.toggle_play(),
                    PlaybackState::Stopped => {
                        if let Some(t) = self.current_track.clone() {
                            self.play_track(t);
                        }
                    }
                    _ => {}
                },
                MprisCmd::Pause => {
                    if self.playback_state == PlaybackState::Playing {
                        self.toggle_play();
                    }
                }
                MprisCmd::PlayPause => self.toggle_play(),
                MprisCmd::Stop => self.stop_playback(),
                MprisCmd::Next => self.play_next(),
                MprisCmd::Previous => self.play_previous(),
                MprisCmd::Seek(offset_us) => self.seek_relative(offset_us / 1_000_000),
                MprisCmd::SetPosition(pos_us) => {
                    let current = self.position_secs_f64();
                    let target = pos_us as f64 / 1_000_000.0;
                    self.seek_relative((target - current).round() as i64);
                }
            }
        }
    }

    /// Mirror playback to MPRIS; the handle diffs the snapshot and emits
    /// PropertiesChanged / Seeked only for what actually changed.
    fn sync_mpris(&self) {
        use crate::mpris::Snapshot;
        let status = match self.playback_state {
            PlaybackState::Playing | PlaybackState::Buffering => "Playing",
            PlaybackState::Paused => "Paused",
            PlaybackState::Stopped => "Stopped",
        };
        let (track_key, title, artist, album, length_us) = match &self.current_track {
            Some(t) => (
                t.id.as_str(),
                t.title.as_str(),
                t.artist.as_str(),
                t.album.as_deref().unwrap_or(""),
                self.playback_duration_secs as i64 * 1_000_000,
            ),
            None => ("", "", "", "", 0),
        };
        let art_url = self
            .current_track
            .as_ref()
            .map(crate::art::primary_cover_url)
            .unwrap_or_default();
        self.mpris.apply(Snapshot {
            status,
            track_key,
            title,
            artist,
            album,
            length_us,
            art_url: &art_url,
            pos_us: (self.position_secs_f64() * 1_000_000.0) as i64,
        });
    }

    /// Apply commands pushed through the Unix-socket bridge (`ytmtui jump`,
    /// `ytmtui queue clear` — i.e. Quickshell's queue list).
    fn apply_ipc_commands(&mut self) {
        use crate::ipc::IpcCmd;
        while let Ok(cmd) = self.ipc_rx.try_recv() {
            match cmd {
                IpcCmd::Jump(i) => {
                    if i < self.queue.len() {
                        self.jump_to(i);
                    }
                }
                IpcCmd::Clear => {
                    // Clearing drops everything but the track that is
                    // already playing, so the user's ears aren't.
                    if matches!(
                        self.playback_state,
                        PlaybackState::Playing | PlaybackState::Paused | PlaybackState::Buffering
                    ) {
                        if let Some(cur) = self.current_track.clone() {
                            self.queue = vec![cur];
                            self.queue_index = 0;
                        } else {
                            self.queue.clear();
                        }
                    } else {
                        self.queue.clear();
                        self.queue_index = 0;
                    }
                    self.rebuild_shuffle_order(None);
                    self.status_message = "Queue cleared".to_string();
                }
            }
        }
    }

    /// Mirror the queue to Unix-socket clients (the widget's queue list).
    ///
    /// The queue rows are rebuilt only when the queue actually changes.
    /// Position, status and the active index still update every tick, but an
    /// idle player no longer clones every title and thumbnail URL ten times a
    /// second.
    fn sync_ipc(&mut self) {
        let rebuild_tracks = self.queue_version != self.ipc_queue_version;
        let tracks = if rebuild_tracks {
            self.ipc_queue_version = self.queue_version;
            Some(
                self.queue
                    .iter()
                    .map(|t| crate::ipc::QueueRow {
                        title: t.title.clone(),
                        artist: t.artist.clone(),
                        album: t.album.clone().unwrap_or_default(),
                        duration_secs: parse_duration(&t.duration).unwrap_or(0),
                        // The widget draws row thumbnails from this URL; fall
                        // back to YouTube's per-video cover like the art panel.
                        thumbnail_url: match t.thumbnail_url.as_deref() {
                            Some(url) if !url.is_empty() => url.to_string(),
                            _ => format!("https://i.ytimg.com/vi/{}/hqdefault.jpg", t.id),
                        },
                    })
                    .collect(),
            )
        } else {
            None
        };
        let active = matches!(
            self.playback_state,
            PlaybackState::Playing | PlaybackState::Buffering | PlaybackState::Paused
        );
        let status = match self.playback_state {
            PlaybackState::Playing | PlaybackState::Buffering => "Playing",
            PlaybackState::Paused => "Paused",
            PlaybackState::Stopped => "Stopped",
        };
        let track_id = self
            .current_track
            .as_ref()
            .map(|t| t.id.as_str())
            .unwrap_or("");
        let mut guard = self.ipc.write().unwrap();
        if let Some(tracks) = tracks {
            guard.tracks = tracks;
        }
        guard.current_index = if active { self.queue_index as i32 } else { -1 };
        set_string(&mut guard.repeat, self.repeat.label());
        guard.shuffle = self.shuffle;
        set_string(&mut guard.status, status);
        guard.position_secs = self.position_secs();
        set_string(&mut guard.track_id, track_id);
    }
}

/// Replace `dst` only when the value changed, avoiding a fresh allocation on
/// every UI tick for state that is usually stable.
fn set_string(dst: &mut String, value: &str) {
    if dst != value {
        dst.clear();
        dst.push_str(value);
    }
}

/// Format seconds the way track durations are written (`M:SS` / `H:MM:SS`).
fn format_duration(secs: u64) -> String {
    if secs >= 3600 {
        format!(
            "{}:{:02}:{:02}",
            secs / 3600,
            (secs % 3600) / 60,
            secs % 60
        )
    } else {
        format!("{}:{:02}", secs / 60, secs % 60)
    }
}

/// Parse a `"M:SS"` / `"H:MM:SS"` duration string into seconds.
fn parse_duration(duration: &Option<String>) -> Option<u64> {
    let text = duration.as_deref()?.trim();
    if text.is_empty() {
        return None;
    }
    let mut secs: u64 = 0;
    for part in text.split(':') {
        secs = secs * 60 + part.trim().parse::<u64>().ok()?;
    }
    Some(secs)
}

/// Keep only the well-known, useful filters, in a sensible order.
fn curate_filters(filters: Vec<SearchFilter>) -> Vec<SearchFilter> {
    let mut curated = Vec::new();
    for wanted in SEARCH_FILTER_ORDER {
        if let Some(f) = filters.iter().find(|f| f.title == wanted) {
            curated.push(f.clone());
        }
    }
    curated
}

#[cfg(test)]
mod browse_section_tests {
    use super::*;

    #[test]
    fn sections_have_requested_order_and_match_shelves() {
        assert_eq!(BROWSE_SECTIONS, ["TRENDING", "MIX", "COMMUNITY", "MOODS", "GENRES"]);
        assert_eq!(browse_section_for_title("Trending songs for you"), Some(0));
        assert_eq!(browse_section_for_title("Mix for you"), Some(1));
        assert_eq!(browse_section_for_title("From the community"), Some(2));
        assert_eq!(browse_section_for_title("Moods"), Some(3));
        assert_eq!(browse_section_for_title("Genres"), Some(4));
        assert_eq!(browse_section_for_title("Chill"), None);
    }
}
