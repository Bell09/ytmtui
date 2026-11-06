//! Offline mode: the local music folder, the registry of saved video ids, and
//! the scan that turns a folder into a library.
//!
//! Three pieces answer one question — *what is available without a network?*:
//!
//! * [`music_dir`] — `config.json`'s `music_dir`, else `$XDG_MUSIC_DIR`, else
//!   `~/Music`.
//! * [`Registry`] — `downloads.json`: video id → file, with the metadata known
//!   at download time. That is what lets an online row show a check mark.
//! * [`scan`] — walk the folder and build [`Track`]s for its audio files,
//!   enriching any the registry knows.
//!
//! Local ids carry [`LOCAL_PREFIX`] so playback, art and the like-toggle can
//! tell a disk file from a YouTube video id.

use crate::models::{Playlist, PlaylistKind, Track};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Marks a track id as a file on disk rather than a YouTube video id.
pub const LOCAL_PREFIX: &str = "local:";

/// Audio extensions picked up while scanning.
const AUDIO_EXT: [&str; 9] = [
    "mp3", "flac", "ogg", "opus", "m4a", "aac", "wav", "webm", "mp4",
];

/// Extensions we accept as sidecar cover art.
const IMAGE_EXT: [&str; 4] = ["jpg", "jpeg", "png", "webp"];

/// Whether a track id points at a file on disk.
pub fn is_local(id: &str) -> bool {
    id.starts_with(LOCAL_PREFIX)
}

/// The file a local track id names.
pub fn local_path(id: &str) -> Option<&Path> {
    id.strip_prefix(LOCAL_PREFIX).map(Path::new)
}

/// The track id for a file on disk.
pub fn local_id(path: &Path) -> String {
    format!("{}{}", LOCAL_PREFIX, path.display())
}

/// Expand a leading `~/` so `music_dir` may be written either way.
fn expand(value: &str) -> PathBuf {
    if value == "~" {
        return home_dir();
    }
    if let Some(rest) = value.strip_prefix("~/") {
        return home_dir().join(rest);
    }
    PathBuf::from(value)
}

fn home_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".to_string()))
}

/// Where offline music lives: `music_dir` in `config.json`, else
/// `$XDG_MUSIC_DIR`, else `~/Music`.
///
/// Read on every call so config edits apply without a restart — it is two
/// `stat`s and a tiny JSON parse.
pub fn music_dir() -> PathBuf {
    if let Ok(text) = std::fs::read_to_string(crate::auth::config_dir().join("config.json")) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(dir) = value.get("music_dir").and_then(|d| d.as_str()) {
                if !dir.trim().is_empty() {
                    return expand(dir.trim());
                }
            }
        }
    }
    if let Ok(dir) = std::env::var("XDG_MUSIC_DIR") {
        if !dir.trim().is_empty() {
            return expand(dir.trim());
        }
    }
    home_dir().join("Music")
}

/// One saved track: where the video id now lives on disk, plus the metadata the
/// API already gave us (so offline rows keep real artist, album and duration
/// without re-reading tags).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    /// The YouTube video id it was downloaded from.
    pub id: String,
    /// Absolute path of the audio file.
    pub path: String,
    pub title: String,
    pub artist: String,
    #[serde(default)]
    pub album: Option<String>,
    #[serde(default)]
    pub duration: Option<String>,
    /// Cover file beside the track, if any — `d` embeds the cover in the audio,
    /// so this is only set for a cover the user placed by hand.
    #[serde(default)]
    pub art: Option<String>,
    /// Seconds since the epoch, newest first.
    #[serde(default)]
    pub at: u64,
}

/// The `downloads.json` manifest, indexed by video id and by path.
///
/// Prunes entries whose file was deleted, so a check mark never outlives a file
/// the user removed behind the app's back.
pub struct Registry {
    entries: HashMap<String, Entry>,
    by_path: HashMap<PathBuf, String>,
    path: PathBuf,
}

impl Registry {
    pub fn load() -> Self {
        let path = crate::auth::config_dir().join("downloads.json");
        let mut entries: HashMap<String, Entry> = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();

        // Drop entries whose file is gone; note whether anything changed, so a
        // first run doesn't create an empty manifest just to say it has nothing.
        let before = entries.len();
        entries.retain(|_, e| Path::new(&e.path).is_file());
        let pruned = entries.len() != before;

        let by_path = entries
            .iter()
            .map(|(id, e)| (PathBuf::from(&e.path), id.clone()))
            .collect();
        let registry = Self {
            entries,
            by_path,
            path,
        };
        if pruned {
            registry.save();
        }
        registry
    }

    pub fn save(&self) {
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(text) = serde_json::to_string_pretty(&self.entries) {
            let _ = std::fs::write(&self.path, text);
        }
    }

    pub fn contains(&self, id: &str) -> bool {
        self.entries.contains_key(id)
    }

    pub fn entry_for_path(&self, path: &Path) -> Option<&Entry> {
        self.by_path.get(path).and_then(|id| self.entries.get(id))
    }

    pub fn insert(&mut self, entry: Entry) {
        self.by_path.insert(PathBuf::from(&entry.path), entry.id.clone());
        self.entries.insert(entry.id.clone(), entry);
        self.save();
    }
}

/// Build a track for a file on disk.
///
/// The registry wins when it knows the file: a `d` download stored the artist,
/// album and duration the API reported, which we'd otherwise have to probe from
/// tags. Loose files fall back to the path layout (`Artist/Album/01 -
/// Title.ext`), the usual shape of hand-copied libraries.
fn track_for(path: &Path, registry: &Registry, root: &Path) -> Track {
    if let Some(e) = registry.entry_for_path(path) {
        return Track {
            id: local_id(path),
            title: e.title.clone(),
            artist: e.artist.clone(),
            album: e.album.clone(),
            duration: e.duration.clone(),
            // Cover file beside the track if present; otherwise the art is
            // embedded in the audio and read at render time.
            thumbnail_url: e
                .art
                .clone()
                .filter(|p| Path::new(p).is_file())
                .or_else(|| sidecar_art(path)),
            downloaded: true,
            like: None,
            set_video_id: None,
        };
    }
    let (title, artist, album) = derive_metadata(path, root);
    Track {
        id: local_id(path),
        title,
        artist,
        album,
        // Unknown without reading tags, and probing every file at scan time
        // would stall startup. Downloaded files never land here.
        duration: None,
        thumbnail_url: sidecar_art(path),
        downloaded: true,
        like: None,
        set_video_id: None,
    }
}

/// Leading track-number pattern, compiled once per scan.
fn track_number_re() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"^\s*\d{1,3}\s*[-._)]\s*").expect("static pattern"))
}

/// Split a path into title/artist/album from `Music/Artist/Album/01 -
/// Title.ext`; missing levels collapse.
///
/// `root` is the music folder every scanned path came from, so stripping it
/// needs no per-file `canonicalize`: paths were built by joining onto it and
/// can't have drifted.
fn derive_metadata(path: &Path, root: &Path) -> (String, String, Option<String>) {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Untitled".to_string());
    // Drop a leading track number: "01 - Title", "01. Title", "01_Title".
    let title = track_number_re()
        .replace(&stem, "")
        .into_owned();

    // Levels below the root: `Artist/Album` for a two-deep file.
    let dirs: Vec<String> = path
        .parent()
        .and_then(|p| p.strip_prefix(root).ok())
        .map(|rel| {
            rel.components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();

    match dirs.len() {
        0 => (title, "Unknown artist".to_string(), None),
        1 => (title, dirs[0].clone(), None),
        // Deepest level is the album, the one above it the artist.
        _ => {
            let album = dirs.last().cloned();
            let artist = dirs[dirs.len() - 2].clone();
            (title, artist, album)
        }
    }
}

/// Cover sitting next to an audio file, if any.
fn sidecar_art(path: &Path) -> Option<String> {
    let dir = path.parent()?;
    let stem = path.file_stem()?;
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let lower = name.to_lowercase();
        let ext = lower.rsplit('.').next().unwrap_or("");
        if !IMAGE_EXT.contains(&ext) {
            continue;
        }
        let file_stem = Path::new(&name)
            .file_stem()
            .map(|s| s.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        // Same stem as the audio, or the conventional folder cover names.
        if file_stem == stem.to_string_lossy().to_lowercase()
            || matches!(file_stem.as_str(), "cover" | "folder" | "album" | "front")
        {
            return Some(entry.path().display().to_string());
        }
    }
    None
}

/// Recursively collect audio files under `dir`.
fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| !p.file_name().map(|n| n.to_string_lossy().starts_with('.')).unwrap_or(false))
        .collect();
    // Sorted so folders and files render in a stable order.
    paths.sort();
    for path in paths {
        if path.is_dir() {
            collect(&path, out);
        } else if path
            .extension()
            .map(|e| AUDIO_EXT.contains(&e.to_string_lossy().to_lowercase().as_str()))
            .unwrap_or(false)
        {
            out.push(path);
        }
    }
}

/// Every audio file under the music folder, registry metadata where known.
pub fn scan(music_dir: &Path, registry: &Registry) -> Vec<Track> {
    let mut files = Vec::new();
    collect(music_dir, &mut files);
    files
        .into_iter()
        .map(|p| track_for(&p, registry, music_dir))
        .collect()
}

/// Local music folders as sidebar entries, with "All tracks" first so an
/// unorganised folder is still browsable.
pub fn folders(tracks: &[Track], music_dir: &Path) -> Vec<Playlist> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut names: Vec<String> = Vec::new();

    for track in tracks {
        let Some(path) = local_path(&track.id) else {
            continue;
        };
        let Some(rel) = path.parent().and_then(|p| p.strip_prefix(music_dir).ok()) else {
            continue;
        };
        let name = rel
            .components()
            .next()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .unwrap_or_default();
        if !name.is_empty() && seen.insert(name.clone()) {
            names.push(name);
        }
    }
    names.sort();

    let mut out = vec![Playlist {
        id: format!("{}*", LOCAL_PREFIX),
        title: "All tracks".to_string(),
        subtitle: format!("{} files", tracks.len()),
        track_count: Some(tracks.len()),
        kind: PlaylistKind::Playlist,
    }];
    for name in names {
        let count = tracks
            .iter()
            .filter(|t| {
                local_path(&t.id)
                    .and_then(|p| p.parent())
                    .and_then(|p| p.strip_prefix(music_dir).ok())
                    .and_then(|r| r.components().next())
                    .map(|c| c.as_os_str() == std::ffi::OsStr::new(&name))
                    .unwrap_or(false)
            })
            .count();
        out.push(Playlist {
            id: format!("{}{}", LOCAL_PREFIX, name),
            title: name,
            subtitle: format!("{} files", count),
            track_count: Some(count),
            kind: PlaylistKind::Playlist,
        });
    }
    out
}

/// The tracks a local sidebar entry holds. `local:*` is "All tracks";
/// anything else matches its top-level folder.
pub fn tracks_for(tracks: &[Track], playlist_id: &str, music_dir: &Path) -> Vec<Track> {
    let Some(suffix) = playlist_id.strip_prefix(LOCAL_PREFIX) else {
        return Vec::new();
    };
    if suffix == "*" {
        return tracks.to_vec();
    }
    tracks
        .iter()
        .filter(|t| {
            local_path(&t.id)
                .and_then(|p| p.parent())
                .and_then(|p| p.strip_prefix(music_dir).ok())
                .and_then(|r| r.components().next())
                .map(|c| c.as_os_str() == std::ffi::OsStr::new(suffix))
                .unwrap_or(false)
        })
        .cloned()
        .collect()
}

/// Make a string safe as a file or directory name (`Artist/Album/Title`)
/// without losing what made it recognisable.
fn sanitise(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            c if c.is_control() => '-',
            c => c,
        })
        .collect();
    // Trim both ends: a trailing "." is illegal on some filesystems, and a
    // name of only dots would collapse into the directory itself.
    let trimmed = cleaned.trim().trim_matches('.').trim();
    let mut out: String = trimmed.chars().take(180).collect();
    if out.is_empty() {
        out = "Unknown".to_string();
    }
    out
}

/// Run yt-dlp once for `template`, converting to mp3.
///
/// `embed` adds `--embed-thumbnail`, putting the cover *inside* the file so
/// other players show it too. There is deliberately no `--write-thumbnail`: a
/// download must leave exactly one file behind, the audio. yt-dlp reaches for
/// ffmpeg when the Python tag library isn't installed, so embedding still
/// works; if it fails after the audio is on disk, the caller retries without it
/// rather than lose the song.
fn ytdlp(
    url: &str,
    template: &str,
    embed: bool,
    progress: &dyn Fn(u8),
) -> anyhow::Result<()> {
    let mut cmd = std::process::Command::new("yt-dlp");
    cmd.args(["-x", "--audio-format", "mp3", "--audio-quality", "0"])
        .args(["--no-playlist", "--no-warnings"])
        // Machine-readable progress, one line per update, so the row can show
        // a live percentage instead of an indefinite "Downloading…". The prefix
        // before the colon names the template; only the text after is printed,
        // hence the `PCT` marker.
        .args([
            "--newline",
            "--progress-template",
            "download:PCT %(progress._percent_str)s",
        ])
        .arg("-o")
        .arg(template)
        .arg(url);
    if embed {
        cmd.arg("--embed-thumbnail");
    }
    let mut child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    // Drain stderr on its own thread: a chatty ffmpeg failure must not fill the
    // pipe and stall the child while we read progress.
    let stderr = child.stderr.take().map(|mut pipe| {
        std::thread::spawn(move || {
            use std::io::Read;
            let mut text = String::new();
            let _ = pipe.read_to_string(&mut text);
            text
        })
    });

    let mut last = u8::MAX;
    if let Some(out) = child.stdout.take() {
        use std::io::BufRead;
        for line in std::io::BufReader::new(out).lines().map_while(|l| l.ok()) {
            let Some(rest) = line.strip_prefix("PCT") else {
                continue;
            };
            let Ok(pct) = rest.trim().trim_end_matches('%').trim().parse::<f32>() else {
                continue;
            };
            let pct = pct.round().clamp(0.0, 100.0) as u8;
            // yt-dlp repeats the same percentage every tick; the UI needs one
            // update per whole percent.
            if pct != last {
                last = pct;
                progress(pct);
            }
        }
    }

    let status = child.wait()?;
    let stderr = stderr.and_then(|h| h.join().ok()).unwrap_or_default();
    if status.success() {
        return Ok(());
    }
    let text = stderr
        .lines()
        .rfind(|l| !l.trim().is_empty())
        .unwrap_or("yt-dlp failed")
        .trim();
    anyhow::bail!("{}", text)
}

/// Download one song into the music folder, cover embedded in the audio.
///
/// Layout is `Artist/Album/Title.mp3` (`Artist/Title.mp3` when the album is
/// unknown), which [`scan`] can read back with no metadata of its own and which
/// matches hand-copied libraries. The returned [`Entry`] ties the video id to
/// the new file, so the row can show a check mark.
///
/// `progress` is called from the blocking download thread with yt-dlp's
/// percentage (0–100), so the row can show how far along it is.
pub async fn download<F>(
    url: &str,
    music_dir: &Path,
    track: &Track,
    progress: F,
) -> anyhow::Result<Entry>
where
    F: Fn(u8) + Send + Sync + 'static,
{
    let artist = sanitise(&track.artist);
    let stem = sanitise(&track.title);
    // A known album nests under it; an unknown one doesn't invent a folder —
    // `Artist/Title.mp3` is how a single sits on disk.
    let out_dir = match track.album.as_deref().filter(|a| !a.trim().is_empty()) {
        Some(album) => music_dir.join(&artist).join(sanitise(album)),
        None => music_dir.join(&artist),
    };
    std::fs::create_dir_all(&out_dir)?;
    let template = out_dir.join(format!("{}.%(ext)s", stem));
    let audio = out_dir.join(format!("{}.mp3", stem));

    // An interrupted download leaves its file behind but never reaches the
    // registry. Adopting it beats fetching the bytes again, and lets `d` after a
    // crash repair the check mark instead of erroring.
    if audio.is_file() {
        return Ok(entry_for(audio, track));
    }

    // yt-dlp and ffmpeg are blocking child processes; keep them off the async
    // runtime's threads so frames keep drawing while it downloads.
    let progress = std::sync::Arc::new(progress);
    let attempt = {
        let url = url.to_string();
        let template = template.display().to_string();
        let progress = progress.clone();
        tokio::task::spawn_blocking(move || {
            ytdlp(&url, &template, true, progress.as_ref())
        })
        .await
        .map_err(|e| anyhow::anyhow!("{}", e))?
    };
    if let Err(first) = attempt {
        let url = url.to_string();
        let template = template.display().to_string();
        let progress = progress.clone();
        let retry = tokio::task::spawn_blocking(move || {
            ytdlp(&url, &template, false, progress.as_ref())
        })
        .await
        .map_err(|e| anyhow::anyhow!("{}", e))?;
        if let Err(second) = retry {
            // Report the original reason: it names the real obstacle.
            anyhow::bail!("{} (thumbnail embed failed too: {})", first, second);
        }
    }

    // `-x --audio-format mp3` plus the template fixes the name exactly.
    if !audio.is_file() {
        // A title collision can leave yt-dlp nothing to rename; take the newest
        // mp3 it wrote rather than fail a download that actually succeeded.
        let newest = std::fs::read_dir(&out_dir)?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().map(|x| x.eq_ignore_ascii_case("mp3")).unwrap_or(false))
            .max_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
        match newest {
            Some(path) if path.is_file() => return Ok(entry_for(path, track)),
            _ => anyhow::bail!("download finished but no audio file appeared in {}", out_dir.display()),
        }
    }
    Ok(entry_for(audio, track))
}

/// Build the registry entry for a freshly downloaded file.
///
/// No image is recorded — the cover was embedded in the audio — but a
/// `cover.jpg` the user placed beside the track is still worth remembering,
/// since it renders without opening the file.
fn entry_for(path: PathBuf, track: &Track) -> Entry {
    Entry {
        id: track.id.clone(),
        path: path.display().to_string(),
        title: track.title.clone(),
        artist: track.artist.clone(),
        album: track.album.clone(),
        duration: track.duration.clone(),
        art: sidecar_art(&path),
        at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    }
}
