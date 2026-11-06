use serde_json::Value;
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

/// Thin wrapper around an `mpv` process.
///
/// mpv runs with an IPC socket so pause/resume works in place. Killing and
/// restarting would lose the playback position.
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

    pub fn play(&mut self, track_id: &str) -> anyhow::Result<()> {
        self.play_with(track_id, None)
    }

    /// Start `track_id`, using a prefetched stream URL when available. That
    /// skips the yt-dlp round-trip, so the next song starts almost instantly.
    pub fn play_with(&mut self, track_id: &str, stream_url: Option<&str>) -> anyhow::Result<()> {
        self.stop();
        // Remove a socket left by a previous process; otherwise mpv can't bind.
        let _ = std::fs::remove_file(&self.socket_path);

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
        let child = Command::new("mpv")
            .arg("--no-video")
            .arg("--audio-display=no")
            .arg("--no-terminal")
            // No user config or scripts, best audio stream only. Cache is
            // enabled below for smooth network playback; local files still
            // open instantly.
            .arg("--no-config")
            .arg("--load-scripts=no")
            .arg("--cache=auto")
            // Only used for URLs; local paths skip the ytdl hook.
            .arg("--ytdl-format=bestaudio/best")
            .arg(format!("--input-ipc-server={}", self.socket_path))
            .arg(&target)
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

    /// Ask mpv where playback really is. The UI clock follows this instead of
    /// guessing, so slow starts and seeks keep progress and lyrics in sync.
    ///
    /// Short-lived IPC connection with tight timeouts; a missing or wedged
    /// mpv returns `None` without stalling a frame.
    pub fn timeline(&self) -> Option<Timeline> {
        let mut stream = UnixStream::connect(&self.socket_path).ok()?;
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
