use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// What mpv says about the current timeline.
#[derive(Debug, Clone, Copy, Default)]
pub struct Timeline {
    /// Playback clock in seconds; `None` until a file is loaded.
    pub position: Option<f64>,
    /// Total duration in seconds, when the stream reports one.
    pub duration: Option<f64>,
}

/// A thin wrapper around an `mpv` process.
///
/// `mpv` is launched with an IPC socket so that playback can be paused and
/// resumed in place (instead of killing and restarting the process, which
/// would lose the playback position).
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
        self.stop();
        // A stale socket left behind by a previous process would make mpv fail
        // to bind; remove it first.
        let _ = std::fs::remove_file(&self.socket_path);

        // Offline rows carry a `local:` prefix naming a file on disk; every
        // other id is a YouTube video. mpv takes either as its final
        // argument, so the rest of the command line is shared — and the
        // queue can mix both across an `o` toggle.
        let target = match crate::offline::local_path(track_id) {
            Some(path) => path.display().to_string(),
            None => format!("https://www.youtube.com/watch?v={}", track_id),
        };
        let child = Command::new("mpv")
            .arg("--no-video")
            .arg("--audio-display=no")
            .arg("--no-terminal")
            // Run libmpv with no user config or scripts, with the best
            // available audio stream and no album-art decode path. Cache is
            // re-enabled explicitly below: it keeps network playback smooth
            // while local files still open directly with no startup delay.
            .arg("--no-config")
            .arg("--load-scripts=no")
            .arg("--cache=auto")
            // Only ever consulted for URLs; a local path simply skips the
            // ytdl hook, so this stays harmless offline.
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
                // If mpv is not installed, fail gracefully or try another backend
                anyhow::bail!("Failed to start mpv (is mpv installed?): {}", e)
            }
        }
    }

    /// Toggle pause/resume on the running mpv process via its IPC socket.
    pub fn pause(&mut self) {
        if let Ok(mut stream) = UnixStream::connect(&self.socket_path) {
            let _ = stream.write_all(b"{\"command\":[\"cycle\",\"pause\"]}\n");
        }
    }

    /// Ask mpv where playback actually is. The UI clock is slaved to this
    /// instead of a wall-clock guess, so a slow stream start or a seek can
    /// never leave the progress bar and lyrics out of step with the audio.
    ///
    /// Every call uses a short-lived IPC connection with tight read/write
    /// timeouts; a missing or wedged mpv returns `None` without stalling a
    /// frame.
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
            // mpv interleaves unsolicited events (start-file, file-loaded,
            // playback-restart) with replies; only the request ids matter.
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

    /// Non-blocking check for the mpv process ending on its own: a clean
    /// exit (status 0) is a normal end-of-file, anything else an error.
    /// Returns `None` while mpv is still alive or when nothing is playing.
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
