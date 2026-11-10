//! Persistent playback session.
//!
//! The queue, cursor, position, shuffle order and repeat mode are written to
//! one small JSON file in the config directory. Saving is atomic and happens
//! from the UI thread (periodically while playback is active, and once on
//! exit), so no extra worker or daemon is needed.
//!
//! Loading is deliberately best-effort: a missing, corrupt, oversized or
//! future-version file starts a fresh session instead of blocking startup.

use std::fs::{self, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::app::RepeatMode;
use crate::models::Track;

/// Bump when the on-disk shape changes incompatibly.
pub const SESSION_VERSION: u32 = 1;
/// A queue larger than this is treated as corrupt rather than loaded.
const MAX_SESSION_BYTES: u64 = 16 * 1024 * 1024;
/// Guard against absurd seek values from a damaged file.
const MAX_POSITION_SECS: f64 = 24.0 * 60.0 * 60.0;

fn default_paused() -> bool {
    true
}

/// Owned session file shape used when loading.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionState {
    pub version: u32,
    #[serde(default)]
    pub queue: Vec<Track>,
    #[serde(default)]
    pub queue_index: usize,
    #[serde(default)]
    pub current: Option<Track>,
    #[serde(default)]
    pub position_secs: f64,
    #[serde(default)]
    pub repeat: RepeatMode,
    #[serde(default)]
    pub shuffle: bool,
    #[serde(default)]
    pub shuffle_order: Vec<usize>,
    /// `true` means restore paused; `false` resumes playback.
    #[serde(default = "default_paused")]
    pub paused: bool,
}

/// Borrowed snapshot used when saving, so writing the queue does not clone it.
#[derive(Serialize)]
pub struct SessionRef<'a> {
    pub version: u32,
    pub queue: &'a [Track],
    pub queue_index: usize,
    pub current: &'a Track,
    pub position_secs: f64,
    pub repeat: RepeatMode,
    pub shuffle: bool,
    pub shuffle_order: &'a [usize],
    pub paused: bool,
}

impl SessionState {
    /// Read and validate a session. Any error means "no session".
    pub fn load(path: &Path) -> Option<Self> {
        let metadata = fs::metadata(path).ok()?;
        if !metadata.is_file() || metadata.len() > MAX_SESSION_BYTES {
            return None;
        }
        let text = fs::read_to_string(path).ok()?;
        let mut state: Self = serde_json::from_str(&text).ok()?;
        state.sanitize().then_some(state)
    }

    /// Repair harmless drift and reject states that cannot be resumed.
    ///
    /// Returns `false` for an empty queue/current pair, empty video ids, or an
    /// unsupported version.
    pub fn sanitize(&mut self) -> bool {
        if self.version != SESSION_VERSION {
            return false;
        }
        if self.queue.is_empty() {
            let Some(current) = self.current.take() else {
                return false;
            };
            if current.id.trim().is_empty() {
                return false;
            }
            self.queue.push(current);
            self.queue_index = 0;
        }
        if self.queue.iter().any(|track| track.id.trim().is_empty()) {
            return false;
        }
        if self.queue_index >= self.queue.len() {
            // Prefer the saved current track when it can still be found; the
            // cursor alone is enough when it cannot.
            self.queue_index = self
                .current
                .as_ref()
                .and_then(|current| self.queue.iter().position(|track| track.id == current.id))
                .unwrap_or(self.queue.len() - 1);
        }
        // The queue is authoritative; keep the duplicate single-track copy in
        // sync for older readers and for the empty-queue fallback above.
        self.current = Some(self.queue[self.queue_index].clone());

        if !self.shuffle {
            self.shuffle_order.clear();
        } else {
            let mut seen = vec![false; self.queue.len()];
            let mut order = Vec::with_capacity(self.queue.len());
            for &index in &self.shuffle_order {
                if index < self.queue.len() && !seen[index] {
                    seen[index] = true;
                    order.push(index);
                }
            }
            // Only a complete permutation can preserve playback; anything
            // else is rebuilt from scratch by the app.
            if order.len() == self.queue.len() {
                self.shuffle_order = order;
            } else {
                self.shuffle_order.clear();
            }
        }

        if !self.position_secs.is_finite() || self.position_secs < 0.0 {
            self.position_secs = 0.0;
        }
        self.position_secs = self.position_secs.min(MAX_POSITION_SECS);
        true
    }

    /// Position to hand to mpv, clamped to the track and restarted when the
    /// saved point is at (or effectively at) the end.
    pub fn resume_position(&self, duration_secs: Option<u64>) -> f64 {
        let position = if self.position_secs.is_finite() {
            self.position_secs.max(0.0)
        } else {
            0.0
        }
        .min(MAX_POSITION_SECS);
        match duration_secs.filter(|seconds| *seconds > 0) {
            Some(seconds) => {
                let duration = seconds as f64;
                if position >= duration - 0.5 {
                    0.0
                } else {
                    position.min(duration - 0.1)
                }
            }
            None => position,
        }
    }

    /// Owned counterpart of [`SessionRef::save`], used by the module tests.
    #[cfg(test)]
    pub fn save(&self, path: &Path) -> io::Result<()> {
        write_session(path, self)
    }
}

impl SessionRef<'_> {
    pub fn save(&self, path: &Path) -> io::Result<()> {
        write_session(path, self)
    }
}

/// Where the last playback session lives between runs.
pub fn session_path() -> PathBuf {
    crate::auth::config_dir().join("session.json")
}

/// Drop a stale session when there is nothing worth resuming.
pub fn clear(path: &Path) {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => {}
    }
}

/// Write to a sibling temp file, fsync it, then rename into place. A crash
/// leaves either the previous complete file or the new complete file.
fn write_session<T: Serialize + ?Sized>(path: &Path, value: &T) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;

    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "session path has no file"))?;
    let tmp = parent.join(format!(".{}.tmp", name.to_string_lossy()));
    let _ = fs::remove_file(&tmp);

    let result = (|| {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer(&mut writer, value).map_err(io::Error::other)?;
        writer.flush()?;
        let file = writer.into_inner().map_err(|error| error.into_error())?;
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod session_tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn track(id: &str) -> Track {
        Track {
            id: id.to_string(),
            title: format!("Title {id}"),
            artist: "Artist".to_string(),
            album: Some("Album".to_string()),
            duration: Some("3:00".to_string()),
            thumbnail_url: Some(format!("https://example.test/{id}.jpg")),
            like: None,
            downloaded: false,
            set_video_id: None,
        }
    }

    fn temp_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "ytmtui-session-test-{}-{}",
            std::process::id(),
            nanos
        ));
        fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    fn cleanup(path: &Path) {
        if let Some(parent) = path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }

    fn sample() -> SessionState {
        SessionState {
            version: SESSION_VERSION,
            queue: vec![track("a"), track("b"), track("c")],
            queue_index: 1,
            current: Some(track("b")),
            position_secs: 42.5,
            repeat: RepeatMode::All,
            shuffle: true,
            shuffle_order: vec![1, 2, 0],
            paused: true,
        }
    }

    #[test]
    fn round_trip_preserves_every_field() {
        let path = temp_path("session.json");
        let tmp = path.with_file_name(".session.json.tmp");
        // A crash may leave a stale temp file behind; the next save must
        // replace it rather than fail.
        fs::write(&tmp, "stale").unwrap();
        let state = sample();
        state.save(&path).unwrap();

        let loaded = SessionState::load(&path).unwrap();
        assert_eq!(loaded, state);
        assert!(!tmp.exists());
        cleanup(&path);
    }

    #[test]
    fn clear_removes_the_session_and_tolerates_missing_files() {
        let path = temp_path("session.json");
        sample().save(&path).unwrap();
        assert!(path.exists());

        clear(&path);
        assert!(!path.exists());
        clear(&path);
        cleanup(&path);
    }

    #[test]
    fn borrowed_snapshot_matches_owned_state() {
        let state = sample();
        let current = state.current.as_ref().unwrap();
        let borrowed = SessionRef {
            version: state.version,
            queue: &state.queue,
            queue_index: state.queue_index,
            current,
            position_secs: state.position_secs,
            repeat: state.repeat,
            shuffle: state.shuffle,
            shuffle_order: &state.shuffle_order,
            paused: state.paused,
        };
        let json = serde_json::to_value(&borrowed).unwrap();
        let decoded: SessionState = serde_json::from_value(json).unwrap();
        assert_eq!(decoded, state);
    }

    #[test]
    fn save_replaces_the_previous_session_atomically() {
        let path = temp_path("session.json");
        let mut first = sample();
        first.position_secs = 10.0;
        first.save(&path).unwrap();

        let mut second = sample();
        second.position_secs = 99.0;
        second.queue_index = 2;
        second.current = Some(track("c"));
        second.save(&path).unwrap();

        let loaded = SessionState::load(&path).unwrap();
        assert_eq!(loaded.position_secs, 99.0);
        assert_eq!(loaded.queue_index, 2);
        assert_eq!(loaded.current.unwrap().id, "c");
        cleanup(&path);
    }

    #[test]
    fn save_creates_parent_directories_and_private_file() {
        let path = temp_path("nested/deeper/session.json");
        sample().save(&path).unwrap();
        assert!(path.exists());
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        cleanup(&path);
    }

    #[test]
    fn missing_corrupt_and_future_sessions_are_ignored() {
        let path = temp_path("session.json");
        assert!(SessionState::load(&path).is_none());

        fs::write(&path, "not json").unwrap();
        assert!(SessionState::load(&path).is_none());

        let mut future = sample();
        future.version = SESSION_VERSION + 1;
        fs::write(&path, serde_json::to_string(&future).unwrap()).unwrap();
        assert!(SessionState::load(&path).is_none());
        cleanup(&path);
    }

    #[test]
    fn oversized_sessions_are_ignored_before_reading() {
        let path = temp_path("session.json");
        let file = fs::File::create(&path).unwrap();
        file.set_len(MAX_SESSION_BYTES + 1).unwrap();
        assert!(SessionState::load(&path).is_none());
        cleanup(&path);
    }

    #[test]
    fn empty_queue_is_seeded_from_the_current_track() {
        let mut state = sample();
        state.queue.clear();
        state.queue_index = 99;
        state.shuffle_order.clear();
        state.shuffle = false;
        assert!(state.sanitize());
        assert_eq!(state.queue.len(), 1);
        assert_eq!(state.queue_index, 0);
        assert_eq!(state.current.unwrap().id, "b");
    }

    #[test]
    fn empty_queue_without_current_is_rejected() {
        let mut state = sample();
        state.queue.clear();
        state.current = None;
        assert!(!state.sanitize());
    }

    #[test]
    fn empty_or_blank_track_ids_are_rejected() {
        let mut state = sample();
        state.queue[1].id.clear();
        assert!(!state.sanitize());
    }

    #[test]
    fn out_of_range_index_uses_the_saved_current_track() {
        let mut state = sample();
        state.queue_index = 99;
        // Current is "b", which sits at index 1.
        assert!(state.sanitize());
        assert_eq!(state.queue_index, 1);

        let mut state = sample();
        state.queue_index = 99;
        state.current = Some(track("missing"));
        assert!(state.sanitize());
        assert_eq!(state.queue_index, 2);
        assert_eq!(state.current.unwrap().id, "c");
    }

    #[test]
    fn invalid_shuffle_orders_are_dropped() {
        let mut state = sample();
        state.shuffle_order = vec![0, 0, 9];
        assert!(state.sanitize());
        assert!(state.shuffle_order.is_empty());

        let mut state = sample();
        state.shuffle = false;
        state.shuffle_order = vec![0, 1, 2];
        assert!(state.sanitize());
        assert!(state.shuffle_order.is_empty());

        let mut state = sample();
        state.shuffle_order = vec![2, 0, 1];
        assert!(state.sanitize());
        assert_eq!(state.shuffle_order, vec![2, 0, 1]);
    }

    #[test]
    fn resume_position_is_clamped_and_restarts_at_the_end() {
        let mut state = sample();
        state.position_secs = 42.5;
        assert_eq!(state.resume_position(Some(180)), 42.5);

        state.position_secs = 179.8;
        assert_eq!(state.resume_position(Some(180)), 0.0);

        state.position_secs = -5.0;
        assert_eq!(state.resume_position(Some(180)), 0.0);

        state.position_secs = f64::NAN;
        assert_eq!(state.resume_position(Some(180)), 0.0);

        state.position_secs = 500.0;
        assert_eq!(state.resume_position(None), 500.0);

        state.position_secs = MAX_POSITION_SECS + 10.0;
        assert_eq!(state.resume_position(None), MAX_POSITION_SECS);
    }

    #[test]
    fn repeat_mode_uses_stable_lowercase_names() {
        assert_eq!(serde_json::to_string(&RepeatMode::Off).unwrap(), "\"off\"");
        assert_eq!(serde_json::to_string(&RepeatMode::All).unwrap(), "\"all\"");
        assert_eq!(serde_json::to_string(&RepeatMode::One).unwrap(), "\"one\"");
        assert_eq!(
            serde_json::from_str::<RepeatMode>("\"all\"").unwrap(),
            RepeatMode::All
        );
    }

    #[test]
    fn minimal_session_defaults_to_paused_and_off() {
        let json = serde_json::json!({
            "version": SESSION_VERSION,
            "queue": [{
                "id": "a",
                "title": "Title",
                "artist": "Artist"
            }]
        });
        let mut state: SessionState = serde_json::from_value(json).unwrap();
        assert!(state.sanitize());
        assert_eq!(state.queue_index, 0);
        assert_eq!(state.repeat, RepeatMode::Off);
        assert!(!state.shuffle);
        assert!(state.paused);
        assert_eq!(state.position_secs, 0.0);
    }
}
