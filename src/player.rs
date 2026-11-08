use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Current timeline reported by mpv.
#[derive(Debug, Clone, Copy, Default)]
pub struct Timeline {
    /// Playback position in seconds; `None` until a file loads.
    pub position: Option<f64>,
    /// Total duration in seconds, when known.
    pub duration: Option<f64>,
}

/// How to load a track into mpv.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadMode {
    /// Clear the playlist and play this track immediately (used for the
    /// first track, after a user skip/stop, or on recovery).
    Replace,
    /// Append to mpv's playlist for gapless playback — mpv plays the current
    /// track to completion, then moves to the queued track automatically.
    Append,
}

/// Thin wrapper around a persistent `mpv` process.
///
/// mpv is started once and kept alive across the session. Tracks are loaded
/// through IPC: `loadfile … replace` for immediate playback, `loadfile …
/// append-play` to queue the next track gaplessly. `--gapless-audio=yes`
/// lets mpv buffer the next stream while the current one finishes.
pub struct Player {
    process: Option<Child>,
    socket_path: String,
}

impl Player {
    pub fn new() -> Self {
        let socket_path = format!("/tmp/ytmtui-mpv-{}.sock", std::process::id());
        Self {
            process: None,
            socket_path,
        }
    }

    /// Path of this player's mpv IPC socket. The background timeline sampler
    /// only needs this string, so it never has to borrow the `Player`.
    pub fn socket_path(&self) -> &str {
        &self.socket_path
    }

    /// Whether the wrapped mpv process is still alive.
    fn is_running(&mut self) -> bool {
        match self.process.as_mut() {
            Some(p) => match p.try_wait() {
                Ok(None) => true, // still running
                Ok(Some(_)) => {
                    // Just exited; reap it so poll_exited doesn't double-handle.
                    self.process = None;
                    false
                }
                Err(_) => false,
            },
            None => false,
        }
    }

    pub fn play(&mut self, track_id: &str) -> anyhow::Result<()> {
        self.play_with(track_id, None)
    }

    /// Start `track_id` immediately, replacing whatever is in mpv's
    /// playlist. Uses a prefetched stream URL when available.
    ///
    /// If mpv is already running, loads via IPC `loadfile … replace`. If
    /// mpv is not running (or died), spawns a fresh process.
    pub fn play_with(&mut self, track_id: &str, stream_url: Option<&str>) -> anyhow::Result<()> {
        self.load(track_id, stream_url, LoadMode::Replace)
    }

    /// Queue `track_id` after the currently playing track in mpv's playlist,
    /// for gapless playback. mpv will auto-advance to it when the current
    /// track finishes. Returns an error if mpv is not running.
    pub fn queue_next(&mut self, track_id: &str, stream_url: Option<&str>) -> anyhow::Result<()> {
        if !self.is_running() {
            anyhow::bail!("mpv is not running");
        }
        self.load(track_id, stream_url, LoadMode::Append)
    }

    /// Clear mpv's playlist. Used before a user skip/stop so stale queued
    /// tracks don't play after the transition.
    pub fn playlist_clear(&self) -> anyhow::Result<()> {
        self.send_command(&["playlist-clear"])
    }

    /// Resolve the playable target (URL or local path) for a track.
    fn resolve_target(&self, track_id: &str, stream_url: Option<&str>) -> String {
        match stream_url {
            Some(url) => url.to_string(),
            None => match crate::offline::local_path(track_id) {
                Some(path) => path.display().to_string(),
                None => format!("https://www.youtube.com/watch?v={}", track_id),
            },
        }
    }

    /// Load `track_id` into mpv with the given mode.
    ///
    /// If mpv is not running, spawns a fresh process (always Replace mode
    /// for the first track). If running, sends the appropriate IPC command.
    fn load(&mut self, track_id: &str, stream_url: Option<&str>, mode: LoadMode) -> anyhow::Result<()> {
        let target = self.resolve_target(track_id, stream_url);

        match mode {
            LoadMode::Replace => {
                if self.is_running() {
                    self.playlist_clear()?;
                    self.send_command(&["loadfile", &target, "replace"])
                } else {
                    self.spawn_mpv(&target)
                }
            }
            LoadMode::Append => {
                if !self.is_running() {
                    anyhow::bail!("mpv is not running");
                }
                self.send_command(&["loadfile", &target, "append-play"])
            }
        }
    }

    /// Send a raw IPC command array to the running mpv process.
    fn send_command(&self, args: &[&str]) -> anyhow::Result<()> {
        let body = json!({ "command": args });
        let cmd = serde_json::to_string(&body)? + "\n";
        let mut stream = UnixStream::connect(&self.socket_path)?;
        stream.set_write_timeout(Some(Duration::from_millis(100)))?;
        stream.write_all(cmd.as_bytes())?;
        Ok(())
    }

    /// Spawn a fresh mpv process for `target`. Used for the first track and
    /// as a fallback when the persistent mpv dies or wedges.
    fn spawn_mpv(&mut self, target: &str) -> anyhow::Result<()> {
        // Remove a socket left by a previous process; otherwise mpv can't bind.
        let _ = std::fs::remove_file(&self.socket_path);

        let child = Command::new("mpv")
            .arg("--no-video")
            .arg("--audio-display=no")
            .arg("--no-terminal")
            // No user config or scripts, best audio stream only. Cache is
            // enabled below for smooth network playback; local files still
            // open instantly. `--gapless-audio=yes` lets mpv buffer the next
            // stream while the current one finishes.
            .arg("--no-config")
            .arg("--load-scripts=no")
            .arg("--cache=auto")
            .arg("--gapless-audio=yes")
            // Only used for URLs; local paths skip the ytdl hook.
            .arg("--ytdl-format=bestaudio/best")
            .arg(format!("--input-ipc-server={}", self.socket_path))
            .arg(target)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();

        match child {
            Ok(p) => {
                self.process = Some(p);
                Ok(())
            }
            Err(e) => {
                // mpv missing: fail with a clear message.
                anyhow::bail!("Failed to start mpv (is mpv installed?): {}", e)
            }
        }
    }

    /// Toggle pause/resume via mpv's IPC socket.
    pub fn pause(&mut self) {
        if let Ok(mut stream) = UnixStream::connect(&self.socket_path) {
            let _ = stream.write_all(b"{\"command\":[\"cycle\",\"pause\"]}\n");
        }
    }

    /// Seek to an absolute position via mpv's IPC socket.
    pub fn seek_to(&mut self, seconds: f64) {
        if let Ok(mut stream) = UnixStream::connect(&self.socket_path) {
            let cmd = format!(
                "{{\"command\":[\"seek\",{:.3},\"absolute\"]}}\n",
                seconds.max(0.0)
            );
            let _ = stream.write_all(cmd.as_bytes());
        }
    }

    /// Non-blocking check for mpv exiting on its own: status 0 is a normal
    /// end-of-file, anything else an error. `None` while alive or idle.
    pub fn poll_exited(&mut self) -> Option<std::process::ExitStatus> {
        let status = match self.process.as_mut() {
            Some(p) => match p.try_wait() {
                Ok(Some(status)) => status,
                _ => return None,
            },
            None => return None,
        };
        self.process = None;
        let _ = std::fs::remove_file(&self.socket_path);
        Some(status)
    }

    pub fn stop(&mut self) {
        if let Some(mut p) = self.process.take() {
            let _ = p.kill();
            let _ = p.wait();
        }
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Query an mpv IPC socket for `time-pos` and `duration`.
///
/// This does blocking socket I/O with tight timeouts, which is why the TUI
/// runs it on a background task rather than the frame thread.
pub fn timeline_path(socket_path: &str) -> Option<Timeline> {
    let mut stream = UnixStream::connect(socket_path).ok()?;
    let _ = stream.set_read_timeout(Some(Duration::from_millis(25)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(25)));
    let commands = concat!(
        "{\"command\":[\"get_property\",\"time-pos\"],\"request_id\":1}\n",
        "{\"command\":[\"get_property\",\"duration\"],\"request_id\":2}\n",
    );
    stream.write_all(commands.as_bytes()).ok()?;

    let mut reader = BufReader::new(stream);
    let deadline = Instant::now() + Duration::from_millis(40);
    let mut timeline = Timeline::default();
    let mut seen = 0u8;
    while seen != 3 && Instant::now() < deadline {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        // mpv interleaves unsolicited events with replies; match only
        // request ids.
        match value.get("request_id").and_then(Value::as_u64) {
            Some(1) => {
                timeline.position = value.get("data").and_then(Value::as_f64);
                seen |= 1;
            }
            Some(2) => {
                timeline.duration = value.get("data").and_then(Value::as_f64);
                seen |= 2;
            }
            _ => {}
        }
    }
    (seen != 0).then_some(timeline)
}

/// Resolve a video's direct audio URL for later playback. Local files are
/// already playable and return their path unchanged.
pub fn resolve_stream(track_id: &str) -> anyhow::Result<String> {
    if let Some(path) = crate::offline::local_path(track_id) {
        return Ok(path.display().to_string());
    }
    let output = Command::new("yt-dlp")
        .arg("--no-warnings")
        .arg("-f")
        .arg("bestaudio/best")
        .arg("-g")
        .arg(format!("https://www.youtube.com/watch?v={}", track_id))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()?;
    if !output.status.success() {
        anyhow::bail!("yt-dlp exited with {}", output.status);
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("http"))
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("yt-dlp returned no stream URL"))
}

#[cfg(test)]
mod player_tests {
    use super::*;

    /// `resolve_stream` maps a local id (`local:` prefix) to its file path
    /// without invoking yt-dlp or the network.
    #[test]
    fn resolve_stream_returns_local_path_for_offline_id() {
        let result = resolve_stream("local:/tmp/song.mp3");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "/tmp/song.mp3");
    }

    /// `resolve_stream` with a non-local id calls yt-dlp (network). Not run
    /// here — just verifies the function signature exists and returns an
    /// error when yt-dlp fails.
    #[test]
    fn resolve_stream_returns_error_for_unknown_video() {
        let result = resolve_stream("nonexistent_video_id_xyz");
        assert!(result.is_err());
    }

    /// `LoadMode` variants are distinct.
    #[test]
    fn load_mode_variants_are_distinct() {
        assert_ne!(LoadMode::Replace, LoadMode::Append);
    }

    /// `LoadMode` derives `Copy` so it can be passed by value.
    #[test]
    fn load_mode_is_copy() {
        let a = LoadMode::Replace;
        let b = a; // Copy, not move
        assert_eq!(a, b);
        assert_eq!(b, LoadMode::Replace);
    }
}
