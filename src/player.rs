use std::io::Write;
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Stdio};

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
            .arg("--no-terminal")
            // Run libmpv with no user config, no scripts, and the "high"
            // preset bestaudio/best stream. --no-config also drops the short
            // 2s cache and the video+audio ytdl-format from mpv.conf;
            // volume stays at the default 100 (unity).
            .arg("--no-config")
            .arg("--load-scripts=no")
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

    /// Seek the running mpv process by a relative number of seconds (may be
    /// negative) via its IPC socket.
    pub fn seek(&mut self, seconds: i64) {
        if let Ok(mut stream) = UnixStream::connect(&self.socket_path) {
            let cmd = format!("{{\"command\":[\"seek\",{},\"relative\"]}}\n", seconds);
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
