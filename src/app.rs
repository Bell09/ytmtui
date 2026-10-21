use crate::models::{Page, Playlist, PlaylistKind, SearchHit, Track};
use crate::api::{SearchFilter, YtmApi};
use crate::player::Player;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

type PreviewResult = (String, Result<Vec<Track>, String>);

/// The heart key's outcome: video id, the rating that was requested,
/// which press asked for it (so an older reply can't clobber a newer
/// press), and whether the API call succeeded.
type LikeResult = (String, bool, u64, Result<(), String>);

/// A queued `h` press: (video id, want like, generation).
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

/// The "Liked Music" auto-playlist's id — the list the `h` heart keeps
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

    /// Lazily-fetched preview of the highlighted library entry, plus a cache so
    /// moving back to a previously seen entry is instant.
    pub preview_tracks: Vec<Track>,
    pub preview_for: Option<String>,
    preview_cache: HashMap<String, Vec<Track>>,
    preview_tx: tokio::sync::mpsc::UnboundedSender<PreviewResult>,
    preview_rx: tokio::sync::mpsc::UnboundedReceiver<PreviewResult>,

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
    /// Where `Esc` returns to after viewing the queue page.
    pub queue_return_page: Page,
    pub queue_return_index: usize,
    pub playback_state: PlaybackState,
    pub api: YtmApi,
    pub player: Player,
    pub should_quit: bool,
    pub status_message: String,
    pub playback_start_time: Option<Instant>,
    pub elapsed_secs: u64,
    pub playback_duration_secs: u64,
    pub buffering_timer: Option<Instant>,
    /// Colour theme (pywal-derived, reloaded live when the wallpaper
    /// changes the palette file).
    pub theme: crate::theme::Theme,
    /// The pywal file the theme came from and the mtime we last read it
    /// at — the whole live-reload state. A `stat()` per tick (~1µs) is
    /// enough to notice a wallpaper change; the palette is only re-parsed
    /// when the mtime actually moves.
    theme_src: Option<PathBuf>,
    theme_mtime: Option<SystemTime>,
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
    /// Queued `h` heart toggles. One worker applies them strictly in
    /// press order — concurrent rate calls race server-side, and a rapid
    /// `h h` could otherwise land the unlike after the like and drop the
    /// song from the account entirely.
    like_cmd_tx: tokio::sync::mpsc::UnboundedSender<LikeRequest>,
    like_rx: tokio::sync::mpsc::UnboundedReceiver<LikeResult>,
    /// Bumped on every `h` press — only the latest press's reply may
    /// change the state back.
    like_generation: u64,
    /// When to re-sync the visible liked-songs list with the server
    /// (VLLM's listing converges ~5-6s after a rating change), and the
    /// press generation that scheduled it.
    like_refresh_at: Option<(Instant, u64)>,
    like_refresh_tx: tokio::sync::mpsc::UnboundedSender<LikeRefreshResult>,
    like_refresh_rx: tokio::sync::mpsc::UnboundedReceiver<LikeRefreshResult>,
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
        // The pywal palette, plus the file it came from and that file's
        // mtime — together these let `poll_theme` notice a wallpaper
        // change and repaint without re-reading the palette every tick.
        let theme = crate::theme::Theme::load();
        let theme_src = crate::theme::Theme::source_file();
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
        tokio::spawn(async move {
            if let Some(name) = crate::auth::Auth::current_account().await {
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
            preview_tracks: Vec::new(),
            preview_for: None,
            preview_cache: HashMap::new(),
            preview_tx,
            preview_rx,
            selected_index: 0,
            current_track: None,
            queue: Vec::new(),
            queue_index: 0,
            repeat: RepeatMode::Off,
            shuffle: false,
            shuffle_order: Vec::new(),
            queue_return_page: Page::Library,
            queue_return_index: 0,
            playback_state: PlaybackState::Stopped,
            api,
            player: Player::new(),
            should_quit: false,
            status_message: auth_status.to_string(),
            playback_start_time: None,
            elapsed_secs: 0,
            playback_duration_secs: 210,
            buffering_timer: None,
            theme,
            theme_src,
            theme_mtime,
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
        // VLLM's listing converges ~5-6s after a rating change; when that
        // deadline lands, refresh whatever liked-songs view is on screen.
        // A superseded press (a newer `h` bumped the generation) is skipped
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
        // Transition out of buffering once the stream has had time to start.
        if self.playback_state == PlaybackState::Buffering {
            if let Some(timer) = self.buffering_timer {
                if timer.elapsed() >= Duration::from_millis(1500) {
                    self.playback_state = PlaybackState::Playing;
                    self.playback_start_time = Some(Instant::now());
                    if let Some(ref t) = self.current_track {
                        self.status_message = format!("Playing: {} - {}", t.title, t.artist);
                    }
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
                    self.playback_start_time = None;
                    self.playback_state = PlaybackState::Stopped;
                    self.status_message = "Playback error: stream failed".to_string();
                }
            }
        }
        // Mirror the resulting state to desktop media widgets.
        self.sync_mpris();
        self.sync_ipc();
    }

    /// Repaint from pywal when the wallpaper changes the palette file.
    ///
    /// One `stat()` per tick (the loop runs at 10Hz) is enough to notice a
    /// change; the palette is only re-parsed when the mtime actually moves.
    /// pywal rewrites the file in place, so a read racing the write sees a
    /// half-written palette — that parse fails, we keep the colours we
    /// have, and the next tick retries against the finished file.
    fn poll_theme(&mut self) {
        let path = match crate::theme::Theme::source_file() {
            Some(path) => path,
            // No palette right now. Only drop back to the terminal's own
            // colours if we never had a pywal one to begin with.
            None => {
                if self.theme_src.is_some() {
                    self.theme_src = None;
                    self.theme_mtime = None;
                    self.theme = crate::theme::Theme::fallback();
                }
                return;
            }
        };
        // If we can't even stat it, leave the watched state alone and try
        // again next tick rather than latching on a missing mtime.
        let Ok(meta) = std::fs::metadata(&path) else {
            return;
        };
        let mtime = meta.modified().ok();
        if self.theme_src.as_ref() == Some(&path) && self.theme_mtime == mtime {
            return;
        }
        // Only latch the mtime once we've actually got a theme. A failed
        // parse means pywal is mid-write, and the finished write may share
        // its mtime — latching here would make it look already-seen and the
        // repaint would never happen. A permanently broken file just costs
        // one small parse per tick.
        if let Some(theme) = crate::theme::Theme::reload_from(&path)
            .or_else(crate::theme::Theme::reload_pywal)
        {
            self.theme = theme;
            self.theme_src = Some(path);
            self.theme_mtime = mtime;
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
            Page::Playlist => self.playlist_tracks.len(),
            Page::Queue => self.queue.len(),
        }
    }

    pub fn next(&mut self) {
        let len = self.list_len();
        if len > 0 {
            self.selected_index = (self.selected_index + 1) % len;
        }
        self.sync_preview();
    }

    pub fn previous(&mut self) {
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
            Page::Library => None,
        }
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

    pub async fn load_library(&mut self) {
        self.status_message = "Loading your library...".to_string();
        match self.api.library_playlists().await {
            Ok(mut playlists) => {
                // Group the list by kind: playlists, then albums (artists
                // and channels are already dropped by the API).
                playlists.sort_by_key(|p| match p.kind {
                    PlaylistKind::Playlist => 0,
                    PlaylistKind::Album => 1,
                    PlaylistKind::Artist => 2,
                    PlaylistKind::Channel => 3,
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
    pub async fn toggle_offline(&mut self) {
        if self.offline {
            self.offline = false;
            self.preview_cache.clear();
            self.current_page = Page::Library;
            self.load_library().await;
            return;
        }
        self.offline = true;
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
        self.dl_progress.insert(id.clone(), 0);
        self.status_message = format!("Downloading '{}'…", track.title);
        tokio::spawn(async move {
            let pid = id.clone();
            let result = crate::offline::download(&url, &dir, &track, move |pct| {
                let _ = ptx.send((pid.clone(), pct));
            })
            .await
            .map_err(|e| e.to_string());
            let _ = tx.send((id, result));
        });
    }

    pub async fn activate_selected(&mut self) {
        match self.current_page {
            Page::Library => self.open_selected_playlist().await,
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
            // the way playing a collection does.
            self.queue = self.current_track_list();
            self.queue_index = self.selected_index.min(self.queue.len().saturating_sub(1));
            self.rebuild_shuffle_order(None);
            self.play_track(t);
        }
    }

    /// Start playback of `t`. Resets the elapsed clock and adopts the track's
    /// own duration for the progress bar.
    fn play_track(&mut self, t: Track) {
        self.status_message = format!("Buffering stream for: {} - {}", t.title, t.artist);
        if let Err(e) = self.player.play(&t.id) {
            self.status_message = format!("Playback error: {}", e);
            self.playback_state = PlaybackState::Stopped;
            return;
        }
        self.elapsed_secs = 0;
        self.playback_duration_secs = parse_duration(&t.duration).unwrap_or(210);
        self.current_track = Some(t);
        self.playback_state = PlaybackState::Buffering;
        self.buffering_timer = Some(Instant::now());
        self.sync_art();
        self.sync_lyrics();
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

    /// `h`: like the currently-playing song — cycle between liked and no
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

    /// Live-update the liked-songs list on screen for an `h` press:
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
        let active = if self.playback_state == PlaybackState::Playing {
            self.playback_start_time
                .map(|s| s.elapsed().as_secs())
                .unwrap_or(0)
        } else {
            0
        };
        let current = (self.elapsed_secs + active) as i64;
        let total = self.playback_duration_secs as i64;
        let target = (current + delta).clamp(0, total.max(0));
        let effective = target - current;
        self.elapsed_secs = target as u64;
        if self.playback_state == PlaybackState::Playing {
            self.playback_start_time = Some(Instant::now());
        }
        if effective != 0 {
            self.player.seek(effective);
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
                self.playback_start_time = None;
                self.playback_state = PlaybackState::Stopped;
                self.status_message = "End of queue".to_string();
            }
        }
    }

    /// Rebuild the shuffled playback order after a queue change: the current
    /// track first, then the rest shuffled. `keep_next` pins one track to
    /// play right after the current one (play-next inserts).
    fn rebuild_shuffle_order(&mut self, keep_next: Option<usize>) {
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
                if let Some(start) = self.playback_start_time.take() {
                    self.elapsed_secs += start.elapsed().as_secs();
                }
                self.player.pause();
                self.playback_state = PlaybackState::Paused;
                self.status_message = "Paused playback".to_string();
            }
            PlaybackState::Paused => {
                self.player.pause();
                self.playback_state = PlaybackState::Playing;
                self.playback_start_time = Some(Instant::now());
                self.status_message = "Resumed playback".to_string();
            }
            PlaybackState::Buffering => {}
            PlaybackState::Stopped => {}
        }
    }

    /// Current playback position in whole seconds (mirrors the progress
    /// bar's clock: it only advances while actually playing).
    pub fn position_secs(&self) -> u64 {
        let active = if self.playback_state == PlaybackState::Playing {
            self.playback_start_time
                .map(|s| s.elapsed().as_secs())
                .unwrap_or(0)
        } else {
            0
        };
        self.elapsed_secs + active
    }

    /// Stop playback entirely (MPRIS `Stop`): end mpv but keep the last
    /// track's metadata so widgets can still show what was playing.
    pub fn stop_playback(&mut self) {
        self.player.stop();
        self.playback_start_time = None;
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
                    let current = self.position_secs() as i64 * 1_000_000;
                    self.seek_relative((pos_us - current) / 1_000_000);
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
                parse_duration(&t.duration).unwrap_or(0) as i64 * 1_000_000,
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
            pos_us: self.position_secs() as i64 * 1_000_000,
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
    fn sync_ipc(&self) {
        let tracks: Vec<crate::ipc::QueueRow> = self
            .queue
            .iter()
            .map(|t| crate::ipc::QueueRow {
                title: t.title.clone(),
                artist: t.artist.clone(),
                album: t.album.clone().unwrap_or_default(),
                duration_secs: parse_duration(&t.duration).unwrap_or(0),
                // The widget draws row thumbnails from this URL; fall back
                // to YouTube's per-video cover like the art panel does.
                thumbnail_url: match t.thumbnail_url.as_deref() {
                    Some(url) if !url.is_empty() => url.to_string(),
                    _ => format!("https://i.ytimg.com/vi/{}/hqdefault.jpg", t.id),
                },
            })
            .collect();
        let active = matches!(
            self.playback_state,
            PlaybackState::Playing | PlaybackState::Buffering | PlaybackState::Paused
        );
        let mut guard = self.ipc.write().unwrap();
        guard.tracks = tracks;
        guard.current_index = if active { self.queue_index as i32 } else { -1 };
        guard.repeat = self.repeat.label().to_string();
        guard.shuffle = self.shuffle;
        guard.status = match self.playback_state {
            PlaybackState::Playing | PlaybackState::Buffering => "Playing",
            PlaybackState::Paused => "Paused",
            PlaybackState::Stopped => "Stopped",
        }
        .to_string();
        guard.position_secs = self.position_secs();
        guard.track_id = self
            .current_track
            .as_ref()
            .map(|t| t.id.clone())
            .unwrap_or_default();
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
