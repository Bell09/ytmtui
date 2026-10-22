use std::io::Write;
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Stdio};

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

        let _ = std::fs::remove_file(&self.socket_path);

        let target = match crate::offline::local_path(track_id) {
            Some(path) => path.display().to_string(),
            None => format!("https://www.youtube.com/watch?v={}", track_id),
        };
        let child = Command::new("mpv")
            .arg("--no-video")
            .arg("--no-terminal")
            .arg("--no-config")
            .arg("--load-scripts=no")
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
                anyhow::bail!("Failed to start mpv (is mpv installed?): {}", e)
            }
        }
    }

    pub fn pause(&mut self) {
        if let Ok(mut stream) = UnixStream::connect(&self.socket_path) {
            let _ = stream.write_all(b"{\"command\":[\"cycle\",\"pause\"]}\n");
        }
    }

    pub fn seek(&mut self, seconds: i64) {
        if let Ok(mut stream) = UnixStream::connect(&self.socket_path) {
            let cmd = format!(r#"{{"command":["seek",{},"relative"]}}
"#, seconds);
            let _ = stream.write_all(cmd.as_bytes());
        }
    }

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
