use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::OnceLock;
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

/// A resolved audio stream: the signed URL plus the HTTP headers yt-dlp says
/// the server expects. Headers are normally only needed by non-YouTube
/// sources, but replaying them costs a few hundred bytes and prevents 403s.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stream {
    pub url: String,
    pub headers: Vec<(String, String)>,
}

impl Stream {
    /// `Name: value` strings for mpv's `http-header-fields` property.
    fn header_fields(&self) -> Vec<String> {
        self.headers
            .iter()
            .map(|(name, value)| format!("{name}: {value}"))
            .collect()
    }

    /// The subset safe to place on mpv's command line, where commas separate
    /// fields. Values such as `Accept` contain commas, so they are omitted
    /// here and applied through IPC (an array) before later loads.
    fn cli_header_fields(&self) -> Vec<String> {
        self.headers
            .iter()
            .filter(|(_, value)| !value.contains(','))
            .map(|(name, value)| format!("{name}: {value}"))
            .collect()
    }
}

/// Why a stream could not be resolved. The display text is safe to show in
/// the TUI: URLs (which may be signed) and credentials are redacted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtractError {
    /// yt-dlp is not installed or not on `PATH`.
    MissingTool,
    /// yt-dlp did not finish within the extraction deadline.
    Timeout { secs: u64 },
    /// yt-dlp ran but failed. `transient` marks failures worth retrying.
    Failed { message: String, transient: bool },
}

impl ExtractError {
    fn retryable(&self) -> bool {
        match self {
            Self::Timeout { .. } => true,
            Self::Failed { transient, .. } => *transient,
            Self::MissingTool => false,
        }
    }
}

impl std::fmt::Display for ExtractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingTool => write!(
                f,
                "yt-dlp is required for streaming but was not found on PATH. \
                 Install it (for example: sudo pacman -S yt-dlp) and restart ytmtui."
            ),
            Self::Timeout { secs } => write!(
                f,
                "yt-dlp did not return a stream within {secs}s. Check the connection, \
                 update yt-dlp, and try again."
            ),
            Self::Failed { message, .. } => f.write_str(message),
        }
    }
}

impl std::error::Error for ExtractError {}

/// Extraction is bounded so a wedged yt-dlp cannot hold a worker forever.
const EXTRACT_TIMEOUT: Duration = Duration::from_secs(30);
/// One retry covers a transient network/player-response failure.
const EXTRACT_ATTEMPTS: usize = 2;
/// Output caps keep a chatty extractor from growing memory without bound.
const STDOUT_CAP: usize = 64 * 1024;
const STDERR_CAP: usize = 8 * 1024;
const MAX_HEADERS: usize = 16;
const MAX_HEADER_NAME: usize = 64;
const MAX_HEADER_VALUE: usize = 1024;
/// Keep diagnostics short enough for the footer.
const DIAGNOSTIC_CHARS: usize = 320;

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
    /// playlist. Uses a prefetched stream when available.
    ///
    /// If mpv is already running, loads via IPC `loadfile … replace`. If
    /// mpv is not running (or died), spawns a fresh process.
    pub fn play_with(
        &mut self,
        track_id: &str,
        stream: Option<&Stream>,
    ) -> anyhow::Result<()> {
        self.load(track_id, stream, LoadMode::Replace)
    }

    /// Start `track_id` at an absolute position, optionally paused. Used once
    /// at startup to restore the previous session. mpv is restarted so
    /// `--start`/`--pause` apply deterministically to the loaded file.
    pub fn play_at(
        &mut self,
        track_id: &str,
        stream: Option<&Stream>,
        start_secs: f64,
        paused: bool,
    ) -> anyhow::Result<()> {
        if self.is_running() {
            self.stop();
        }
        let target = self.resolve_target(track_id, stream);
        self.spawn_mpv(&target, stream, Some(start_secs), paused)
    }

    /// Queue `track_id` after the currently playing track in mpv's playlist,
    /// for gapless playback. mpv will auto-advance to it when the current
    /// track finishes. Returns an error if mpv is not running.
    pub fn queue_next(
        &mut self,
        track_id: &str,
        stream: Option<&Stream>,
    ) -> anyhow::Result<()> {
        if !self.is_running() {
            anyhow::bail!("mpv is not running");
        }
        self.load(track_id, stream, LoadMode::Append)
    }

    /// Clear mpv's playlist. Used before a user skip/stop so stale queued
    /// tracks don't play after the transition.
    pub fn playlist_clear(&self) -> anyhow::Result<()> {
        self.send_command(&["playlist-clear"])
    }

    /// Resolve the playable target (URL or local path) for a track.
    fn resolve_target(&self, track_id: &str, stream: Option<&Stream>) -> String {
        match stream {
            Some(stream) => stream.url.clone(),
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
    fn load(
        &mut self,
        track_id: &str,
        stream: Option<&Stream>,
        mode: LoadMode,
    ) -> anyhow::Result<()> {
        let target = self.resolve_target(track_id, stream);

        match mode {
            LoadMode::Replace => {
                if self.is_running() {
                    self.playlist_clear()?;
                    self.load_target(&target, "replace", stream)
                } else {
                    self.spawn_mpv(&target, stream, None, false)
                }
            }
            LoadMode::Append => {
                if !self.is_running() {
                    anyhow::bail!("mpv is not running");
                }
                self.load_target(&target, "append-play", stream)
            }
        }
    }

    /// Send headers (when present) and the load command in one IPC write, so
    /// mpv applies them in order rather than racing two connections.
    fn load_target(
        &self,
        target: &str,
        mode: &str,
        stream: Option<&Stream>,
    ) -> anyhow::Result<()> {
        let mut body = String::new();
        if let Some(stream) = stream {
            let fields = stream.header_fields();
            if !fields.is_empty() {
                body.push_str(&serde_json::to_string(&json!({
                    "command": ["set_property", "http-header-fields", fields]
                }))?);
                body.push('\n');
            }
        }
        body.push_str(&serde_json::to_string(&json!({
            "command": ["loadfile", target, mode]
        }))?);
        body.push('\n');
        self.send_raw(&body)
    }

    /// Send a raw IPC command array to the running mpv process.
    fn send_command(&self, args: &[&str]) -> anyhow::Result<()> {
        let body = serde_json::to_string(&json!({ "command": args }))? + "\n";
        self.send_raw(&body)
    }

    fn send_raw(&self, body: &str) -> anyhow::Result<()> {
        let mut stream = UnixStream::connect(&self.socket_path)?;
        stream.set_write_timeout(Some(Duration::from_millis(100)))?;
        stream.write_all(body.as_bytes())?;
        Ok(())
    }

    /// Spawn a fresh mpv process for `target`. Used for the first track and
    /// as a fallback when the persistent mpv dies or wedges. `start` seeks
    /// before playback; `paused` keeps the file paused until the user resumes.
    fn spawn_mpv(
        &mut self,
        target: &str,
        stream: Option<&Stream>,
        start: Option<f64>,
        paused: bool,
    ) -> anyhow::Result<()> {
        // Remove a socket left by a previous process; otherwise mpv can't bind.
        let _ = std::fs::remove_file(&self.socket_path);

        let mut command = Command::new("mpv");
        command
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
            // mpv's own ytdl fallback uses these documented yt-dlp options.
            // Without them its internal yt-dlp may retry ten times and wait
            // indefinitely on a dead socket before the watchdog can help.
            .arg(
                "--ytdl-raw-options=no-playlist=,no-progress=,\
                 socket-timeout=10,retries=2,extractor-retries=1",
            )
            .arg(format!("--input-ipc-server={}", self.socket_path));
        if let Some(stream) = stream {
            let fields = stream.cli_header_fields();
            if !fields.is_empty() {
                command.arg(format!("--http-header-fields={}", fields.join(",")));
            }
        }
        if let Some(seconds) = start {
            command.arg(start_arg(seconds));
        }
        if paused {
            command.arg("--pause=yes");
        }
        let child = command
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

/// mpv `--start=` argument for a resume position. Bad values collapse to 0
/// instead of reaching the command line.
fn start_arg(seconds: f64) -> String {
    let seconds = if seconds.is_finite() {
        seconds.max(0.0)
    } else {
        0.0
    };
    format!("--start={seconds:.3}")
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

/// Resolve a video's direct audio stream for later playback. Local files are
/// already playable and return their path unchanged.
pub fn resolve_stream(track_id: &str) -> Result<Stream, ExtractError> {
    resolve_stream_with(Path::new("yt-dlp"), track_id, EXTRACT_TIMEOUT)
}

fn resolve_stream_with(
    program: &Path,
    track_id: &str,
    timeout: Duration,
) -> Result<Stream, ExtractError> {
    if let Some(path) = crate::offline::local_path(track_id) {
        return Ok(Stream {
            url: path.display().to_string(),
            headers: Vec::new(),
        });
    }
    if track_id.is_empty()
        || track_id.len() > 64
        || !track_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        return Err(ExtractError::Failed {
            message: "Invalid video id; no stream was requested.".to_string(),
            transient: false,
        });
    }

    let url = format!("https://www.youtube.com/watch?v={track_id}");
    let mut last: Option<ExtractError> = None;
    for attempt in 0..EXTRACT_ATTEMPTS {
        match extract_once(program, &url, timeout) {
            Ok(stream) => return Ok(stream),
            Err(error) if error.retryable() && attempt + 1 < EXTRACT_ATTEMPTS => {
                last = Some(error);
            }
            Err(error) => return Err(error),
        }
    }
    Err(last.unwrap_or(ExtractError::Failed {
        message: "yt-dlp did not return a stream.".to_string(),
        transient: false,
    }))
}

/// Run yt-dlp once and parse the URL and optional headers it prints.
///
/// Uses documented options only: the default YouTube clients, cookies/cache
/// and PO Token configuration remain under yt-dlp's own control (including
/// the user's `yt-dlp` config). No player client is forced here.
fn extract_once(
    program: &Path,
    url: &str,
    timeout: Duration,
) -> Result<Stream, ExtractError> {
    let mut command = Command::new(program);
    command
        .args([
            "--no-playlist",
            "--no-progress",
            "--socket-timeout",
            "10",
            "--retries",
            "2",
            "--extractor-retries",
            "1",
            "-f",
            "bestaudio/best",
            "--print",
            "%(url)s",
            "--print",
            "%(http_headers)j",
            url,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command.spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            ExtractError::MissingTool
        } else {
            ExtractError::Failed {
                message: format!("Could not start yt-dlp: {error}"),
                transient: false,
            }
        }
    })?;

    let stdout = child
        .stdout
        .take()
        .map(|pipe| drain(pipe, STDOUT_CAP))
        .ok_or_else(|| ExtractError::Failed {
            message: "Could not read yt-dlp output.".to_string(),
            transient: false,
        })?;
    let stderr = child.stderr.take().map(|pipe| drain(pipe, STDERR_CAP));

    let status = match wait_with_timeout(&mut child, timeout) {
        Ok(status) => status,
        Err(WaitError::Timeout) => {
            // wait_with_timeout already killed and reaped the child.
            let _ = stdout.join();
            if let Some(stderr) = stderr {
                let _ = stderr.join();
            }
            return Err(ExtractError::Timeout {
                secs: timeout.as_secs(),
            });
        }
        Err(WaitError::Other(error)) => {
            let _ = stdout.join();
            if let Some(stderr) = stderr {
                let _ = stderr.join();
            }
            return Err(ExtractError::Failed {
                message: format!("Could not wait for yt-dlp: {error}"),
                transient: false,
            });
        }
    };

    let stdout = stdout.join().unwrap_or_default();
    let stderr = stderr.and_then(|handle| handle.join().ok()).unwrap_or_default();

    if !status.success() {
        return Err(failure(stderr.trim(), status));
    }
    parse_output(&stdout)
}

enum WaitError {
    Timeout,
    Other(std::io::Error),
}

/// Poll for exit instead of blocking forever. On timeout the child is killed
/// and reaped so neither the process nor its pipes are left behind.
fn wait_with_timeout(child: &mut Child, timeout: Duration) -> Result<ExitStatus, WaitError> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(WaitError::Timeout);
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(WaitError::Other(error));
            }
        }
    }
}

/// Read a pipe while keeping only its last `cap` bytes. Bounded even when a
/// broken extractor spews output.
fn drain(mut reader: impl Read + Send + 'static, cap: usize) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        let mut kept: Vec<u8> = Vec::with_capacity(cap.min(8192));
        loop {
            match reader.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    kept.extend_from_slice(&chunk[..read]);
                    if kept.len() > cap * 2 {
                        let excess = kept.len() - cap;
                        kept.drain(..excess);
                    }
                }
            }
        }
        if kept.len() > cap {
            let excess = kept.len() - cap;
            kept.drain(..excess);
        }
        String::from_utf8_lossy(&kept).into_owned()
    })
}

/// Turn the extractor's stdout into a stream. Old yt-dlp versions may not
/// print the headers line; that is not an error.
fn parse_output(stdout: &str) -> Result<Stream, ExtractError> {
    let mut lines = stdout.lines().map(str::trim).filter(|line| !line.is_empty());
    let url = lines
        .find(|line| line.starts_with("http"))
        .filter(|line| !line.chars().any(|c| c.is_control() || c.is_whitespace()))
        .ok_or_else(|| ExtractError::Failed {
            message: "yt-dlp returned no usable stream URL.".to_string(),
            transient: false,
        })?;
    let headers = lines
        .next()
        .and_then(|line| serde_json::from_str::<Value>(line).ok())
        .map(|value| parse_headers(&value))
        .unwrap_or_default();
    Ok(Stream {
        url: url.to_string(),
        headers,
    })
}

/// Keep only well-formed, bounded headers. The extractor is an external
/// process, so nothing it prints is trusted blindly.
fn parse_headers(value: &Value) -> Vec<(String, String)> {
    let Some(map) = value.as_object() else {
        return Vec::new();
    };
    map.iter()
        .take(MAX_HEADERS)
        .filter_map(|(name, value)| {
            let name = name.trim();
            if name.is_empty()
                || name.len() > MAX_HEADER_NAME
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
            {
                return None;
            }
            let value = value.as_str()?.trim();
            if value.is_empty()
                || value.len() > MAX_HEADER_VALUE
                || value.chars().any(char::is_control)
            {
                return None;
            }
            Some((name.to_string(), value.to_string()))
        })
        .collect()
}

/// Build a short, safe error message from yt-dlp's stderr. Signed URLs and
/// credential-bearing lines never reach the UI.
fn failure(stderr: &str, status: ExitStatus) -> ExtractError {
    let lower = stderr.to_ascii_lowercase();
    let transient = is_transient(&lower);
    let detail = stderr
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty() && !is_secret_line(line))
        .map(redact)
        .filter(|line| !line.is_empty())
        .unwrap_or_else(|| format!("yt-dlp exited with {status}"));
    let hint = guidance(&lower);
    ExtractError::Failed {
        message: if hint.is_empty() {
            detail
        } else {
            format!("{detail} {hint}")
        },
        transient,
    }
}

fn is_transient(lower: &str) -> bool {
    const PERMANENT: [&str; 8] = [
        "video unavailable",
        "private video",
        "members-only",
        "unsupported url",
        "does not exist",
        "removed by the uploader",
        "requested format is not available",
        "sign in to confirm",
    ];
    if PERMANENT.iter().any(|marker| lower.contains(marker)) {
        return false;
    }
    const TRANSIENT: [&str; 8] = [
        "timed out",
        "temporarily",
        "connection reset",
        "connection refused",
        "http error 5",
        "http error 429",
        "unable to download",
        "network is unreachable",
    ];
    TRANSIENT.iter().any(|marker| lower.contains(marker))
}

fn guidance(lower: &str) -> &'static str {
    if lower.contains("sign in to confirm") || lower.contains("not a bot") {
        "YouTube asked for a sign-in check; run `ytmtui setup`, update yt-dlp, and try again."
    } else if lower.contains("javascript runtime") || lower.contains("js runtime") {
        "yt-dlp needs a JavaScript runtime for this video; install Deno or Node (ytmtui does not bundle one)."
    } else if lower.contains("po token") || lower.contains("po_token") {
        "YouTube requested a PO Token; update yt-dlp or configure a provider in yt-dlp's own config."
    } else if lower.contains("requested format is not available") {
        "No audio stream matched bestaudio/best; update yt-dlp and try again."
    } else {
        "Update yt-dlp if this persists."
    }
}

fn is_secret_line(line: &str) -> bool {
    let lower = line.trim_start().to_ascii_lowercase();
    lower.starts_with("cookie:")
        || lower.starts_with("authorization:")
        || lower.starts_with("proxy-authorization:")
}

fn redact(text: &str) -> String {
    static URLS: OnceLock<regex::Regex> = OnceLock::new();
    let urls = URLS.get_or_init(|| {
        regex::Regex::new(r#"https?://[^\s'"<>]+"#).expect("url regex")
    });
    let text = urls.replace_all(text.trim(), "<redacted-url>");
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() > DIAGNOSTIC_CHARS {
        let mut out: String = text.chars().take(DIAGNOSTIC_CHARS).collect();
        out.push('…');
        out
    } else {
        text
    }
}

/// One-line dependency summary for `ytmtui status`. This only inspects
/// `PATH` — probing `--version` can block for seconds on loaded systems and
/// would make a status command feel broken.
pub fn tool_status_lines() -> Vec<String> {
    vec![
        tool_line("yt-dlp", "required for streaming"),
        tool_line("mpv", "required for playback"),
        js_runtime_status(),
    ]
}

fn tool_line(program: &str, purpose: &str) -> String {
    match which(program) {
        Some(path) => format!("{program}: {}", path.display()),
        None => format!("{program}: not found — {purpose} (install it)"),
    }
}

fn which(name: &str) -> Option<std::path::PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths).find_map(|dir| {
        let candidate = dir.join(name);
        let meta = std::fs::metadata(&candidate).ok()?;
        if !meta.is_file() {
            return None;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if meta.permissions().mode() & 0o111 == 0 {
                return None;
            }
        }
        Some(candidate)
    })
}

fn js_runtime_status() -> String {
    const RUNTIMES: [&str; 4] = ["deno", "node", "bun", "qjs"];
    match RUNTIMES.iter().find(|name| which(name).is_some()) {
        Some(name) => format!("JavaScript runtime: {name} (found on PATH for yt-dlp)"),
        None => "JavaScript runtime: none found — some YouTube videos need Deno or Node"
            .to_string(),
    }
}

#[cfg(test)]
mod player_tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    /// `resolve_stream` maps a local id (`local:` prefix) to its file path
    /// without invoking yt-dlp or the network.
    #[test]
    fn resolve_stream_returns_local_path_for_offline_id() {
        let result = resolve_stream("local:/tmp/song.mp3");
        assert!(result.is_ok());
        assert_eq!(result.unwrap().url, "/tmp/song.mp3");
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

    #[test]
    fn start_arg_formats_and_clamps_the_resume_position() {
        assert_eq!(start_arg(42.5), "--start=42.500");
        assert_eq!(start_arg(0.0), "--start=0.000");
        assert_eq!(start_arg(-5.0), "--start=0.000");
        assert_eq!(start_arg(f64::NAN), "--start=0.000");
        assert_eq!(start_arg(f64::INFINITY), "--start=0.000");
    }

    // -----------------------------------------------------------------
    // Extractor parsing and failure handling (fake yt-dlp scripts)
    // -----------------------------------------------------------------

    fn temp_script(tag: &str, body: &str) -> (PathBuf, PathBuf) {
        let unique = format!(
            "ytmtui-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let dir = std::env::temp_dir().join(unique);
        fs::create_dir_all(&dir).expect("temp dir");
        let script = dir.join("yt-dlp");
        fs::write(&script, body).expect("write script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).expect("chmod");
        }
        (dir, script)
    }

    fn parse_with_script(tag: &str, body: &str) -> (Result<Stream, ExtractError>, PathBuf) {
        let (dir, script) = temp_script(tag, body);
        let result = resolve_stream_with(&script, "dQw4w9WgXcQ", Duration::from_secs(5));
        (result, dir)
    }

    #[test]
    fn parse_output_reads_url_and_headers() {
        let stdout = concat!(
            "https://example.invalid/audio?expire=1&sig=secret\n",
            "{\"User-Agent\":\"Test UA\",\"Accept\":\"text/html\",\"Bad\\nName\":\"x\"}\n"
        );
        let stream = parse_output(stdout).expect("parsed");
        assert_eq!(stream.url, "https://example.invalid/audio?expire=1&sig=secret");
        assert_eq!(
            stream.headers,
            vec![
                ("Accept".to_string(), "text/html".to_string()),
                ("User-Agent".to_string(), "Test UA".to_string()),
            ]
        );
    }

    #[test]
    fn parse_output_accepts_a_missing_headers_line() {
        let stream = parse_output("https://example.invalid/audio\n").expect("parsed");
        assert!(stream.headers.is_empty());
    }

    #[test]
    fn headers_are_bounded_and_sanitised() {
        let mut map = serde_json::Map::new();
        map.insert("User-Agent".to_string(), Value::String("ok".to_string()));
        map.insert("Bad Name".to_string(), Value::String("x".to_string()));
        map.insert("Cookie".to_string(), Value::String("a\nb".to_string()));
        map.insert("Empty".to_string(), Value::String("   ".to_string()));
        for i in 0..40 {
            map.insert(format!("X-{i}"), Value::String("v".to_string()));
        }
        let headers = parse_headers(&Value::Object(map));
        assert!(headers.len() <= MAX_HEADERS);
        assert!(headers.contains(&("User-Agent".to_string(), "ok".to_string())));
        assert!(!headers.iter().any(|(name, _)| name == "Bad Name"));
        assert!(!headers.iter().any(|(name, _)| name == "Cookie"));
        assert!(!headers.iter().any(|(_, value)| value.contains('\n')));
    }

    #[test]
    fn cli_headers_skip_values_with_commas() {
        let stream = Stream {
            url: "u".to_string(),
            headers: vec![
                ("User-Agent".to_string(), "no commas".to_string()),
                ("Accept".to_string(), "text/html,application/xml".to_string()),
            ],
        };
        assert_eq!(stream.cli_header_fields(), vec!["User-Agent: no commas"]);
        assert_eq!(stream.header_fields().len(), 2);
    }

    #[test]
    fn a_missing_extractor_is_reported_with_install_guidance() {
        let result = resolve_stream_with(
            Path::new("/nonexistent/yt-dlp-for-ytmtui"),
            "dQw4w9WgXcQ",
            Duration::from_secs(2),
        );
        let error = result.expect_err("missing tool");
        assert!(matches!(error, ExtractError::MissingTool));
        let text = error.to_string();
        assert!(text.contains("yt-dlp"));
        assert!(text.contains("Install"));
    }

    #[test]
    fn extractor_failure_redacts_signed_urls() {
        let body = "#!/bin/sh\n\
                    echo 'ERROR: unable to download https://example.invalid/v?expire=1&sig=SECRET' >&2\n\
                    exit 1\n";
        let (result, dir) = parse_with_script("redact", body);
        let error = result.expect_err("failure");
        let text = error.to_string();
        assert!(text.contains("<redacted-url>"));
        assert!(!text.contains("SECRET"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn permanent_failures_are_not_retried() {
        let body = "#!/bin/sh\n\
                    echo 'ERROR: [youtube] x: Video unavailable' >&2\n\
                    echo called >> \"$(dirname \"$0\")/calls\"\n\
                    exit 1\n";
        let (result, dir) = parse_with_script("permanent", body);
        let error = result.expect_err("permanent");
        assert!(matches!(
            error,
            ExtractError::Failed {
                transient: false,
                ..
            }
        ));
        let calls = fs::read_to_string(dir.join("calls")).unwrap_or_default();
        assert_eq!(calls.lines().count(), 1);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn transient_failures_are_retried_once_then_succeed() {
        let body = "#!/bin/sh\n\
                    dir=\"$(dirname \"$0\")\"\n\
                    if [ ! -f \"$dir/calls\" ]; then echo first >> \"$dir/calls\"; \
                    echo 'ERROR: unable to download webpage: timed out' >&2; exit 1; fi\n\
                    echo called >> \"$dir/calls\"\n\
                    echo 'https://example.invalid/audio'\n\
                    echo '{\"User-Agent\":\"Test UA\"}'\n";
        let (result, dir) = parse_with_script("transient", body);
        let stream = result.expect("second attempt succeeds");
        assert_eq!(stream.url, "https://example.invalid/audio");
        assert_eq!(
            stream.headers,
            vec![("User-Agent".to_string(), "Test UA".to_string())]
        );
        assert_eq!(
            fs::read_to_string(dir.join("calls")).unwrap_or_default().lines().count(),
            2
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_hung_extractor_is_killed_and_reported() {
        let body = "#!/bin/sh\nexec sleep 10\n";
        let (dir, script) = temp_script("timeout", body);
        let started = Instant::now();
        let result = resolve_stream_with(&script, "dQw4w9WgXcQ", Duration::from_millis(200));
        let error = result.expect_err("times out");
        assert!(matches!(error, ExtractError::Timeout { .. }));
        assert!(started.elapsed() < Duration::from_secs(5));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn an_invalid_video_id_is_rejected_without_spawning_a_process() {
        let result = resolve_stream("../../etc/passwd");
        assert!(matches!(
            result,
            Err(ExtractError::Failed {
                transient: false,
                ..
            })
        ));
    }

    #[test]
    fn error_guidance_covers_documented_yt_dlp_failures() {
        assert!(guidance("sign in to confirm you're not a bot").contains("ytmtui setup"));
        assert!(guidance("no supported javascript runtime").contains("Deno"));
        assert!(guidance("po token required").contains("PO Token"));
        assert!(guidance("requested format is not available").contains("bestaudio/best"));
        assert!(guidance("something else").contains("Update yt-dlp"));
    }
}
