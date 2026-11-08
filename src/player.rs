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

/// Thin wrapper around a persistent `mpv` process.
///
/// mpv is started once and kept alive across tracks. Tracks are swapped with
/// the IPC `loadfile` command so playback is gapless — there's no kill /
/// restart between songs. `--gapless-audio=yes` lets mpv buffer the next
/// stream while the current one finishes.
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

    /// Start `track_id`, using a prefetched stream URL when available. That
    /// skips the yt-dlp round-trip, so the next song starts almost instantly.
    ///
    /// If mpv is already running, the new track is loaded through the IPC
    /// socket with `loadfile … replace`, so playback is gapless. If mpv is
    /// not running (or died), a fresh process is spawned.
    pub fn play_with(&mut self, track_id: &str, stream_url: Option<&str>) -> anyhow::Result<()> {
        // Offline ids carry a `local:` prefix and map to a file; everything
        // else is a YouTube video. mpv accepts either, so the queue can mix
        // both across an `o` toggle.
        let target = match stream_url {
            Some(url) => url.to_string(),
            None => match crate::offline::local_path(track_id) {
                Some(path) => path.display().to_string(),
                None => format!("https://www.youtube.com/watch?v={}", track_id),
            },
        };

        if self.is_running() {
            // Gapless: keep the mpv process alive and load the new track
            // into it via IPC. If loadfile fails (mpv may be stuck in an
            // error state), kill mpv and spawn fresh below.
            match self.loadfile(&target) {
                Ok(()) => return Ok(()),
                Err(_) => {
                    self.stop();
                    let _ = std::fs::remove_file(&self.socket_path);
                }
            }
        }

        self.spawn_mpv(&target)
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

    /// Load `target` into the running mpv process via IPC, replacing the
    /// current track. This is what makes playback gapless.
    fn loadfile(&self, target: &str) -> anyhow::Result<()> {
        let body = json!({
            "command": ["loadfile", target, "replace"]
        });
        let cmd = serde_json::to_string(&body)? + "\n";
        let mut stream = UnixStream::connect(&self.socket_path)?;
        stream.set_write_timeout(Some(Duration::from_millis(100)))?;
        stream.write_all(cmd.as_bytes())?;
        Ok(())
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
