use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

/// Current timeline reported by mpv.
#[derive(Debug, Clone, Copy, Default)]
pub struct Timeline {
    /// Playback position in seconds; `None` until a file loads.
    pub position: Option<f64>,
    /// Total duration in seconds, when known.
    pub duration: Option<f64>,
    pub playlist_position: Option<u64>,
    pub entry_id: Option<i64>,
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
/// How long the resume seek waits for mpv to open the restored file. A
/// network stream has to be extracted and connected first, so this is
/// generous; the seek is skipped rather than delayed past the track.
const RESUME_WAIT: Duration = Duration::from_secs(30);

/// Thin wrapper around a persistent `mpv` process.
///
/// mpv is started once and kept alive across the session. Tracks are loaded
/// through IPC: `loadfile … replace` for immediate playback, `loadfile …
/// append-play` to queue the next track gaplessly. `--gapless-audio=yes`
/// lets mpv buffer the next stream while the current one finishes.
pub struct Player {
    process: Option<Child>,
    socket_path: String,
    /// The mpv executable. Kept as a field so the test suite can point the
    /// player at a fake mpv.
    program: PathBuf,
    /// Extra mpv arguments. Empty in production; the test suite uses it to
    /// run mpv without an audio device.
    extra_args: Vec<String>,
    commands: SyncSender<IpcRequest>,
    errors: Receiver<(u64, String)>,
    error_tx: SyncSender<(u64, String)>,
    generation: Arc<AtomicU64>,
    resume: Arc<ResumeState>,
}

/// The low bits track active/paused; upper bits invalidate a pending seek.
/// A compare-and-swap closes the race between restore completion and input.
struct ResumeState {
    mode: AtomicU64,
    position: AtomicU64,
}

impl ResumeState {
    fn new(position: f64, paused: bool) -> Self {
        Self {
            mode: AtomicU64::new(1 | ((paused as u64) << 1)),
            position: AtomicU64::new(position.to_bits()),
        }
    }
    fn active(&self) -> bool {
        self.mode.load(Ordering::Acquire) & 1 != 0
    }
    fn cancel(&self) {
        self.mode.fetch_and(!1, Ordering::AcqRel);
    }
}

struct IpcRequest {
    socket: String,
    body: String,
    generation: u64,
}

/// A socket path that no other player in this process uses. Two players (the
/// app and a test, for instance) must not fight over the same mpv socket.
fn unique_socket_path() -> String {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("/tmp/ytmtui-mpv-{}-{sequence}.sock", std::process::id())
}

impl Player {
    pub fn new() -> Self {
        Self::with_program(PathBuf::from("mpv"))
    }

    fn with_program(program: PathBuf) -> Self {
        // One sleeping worker, with bounded queues. IPC acknowledgements never
        // wait on the frame thread, and rapid input cannot grow memory forever.
        let (commands, requests) = mpsc::sync_channel::<IpcRequest>(16);
        let (error_tx, errors) = mpsc::sync_channel(16);
        let generation = Arc::new(AtomicU64::new(0));
        let current = generation.clone();
        let worker_errors = error_tx.clone();
        std::thread::spawn(move || {
            while let Ok(request) = requests.recv() {
                if request.generation != current.load(Ordering::Acquire) {
                    continue;
                }
                let result = UnixStream::connect(&request.socket)
                    .map_err(anyhow::Error::from)
                    .and_then(|stream| {
                        if request.generation != current.load(Ordering::Acquire) {
                            return Ok(());
                        }
                        exchange_commands(stream, &request.body)
                    });
                if let Err(error) = result {
                    let _ = worker_errors.try_send((request.generation, error.to_string()));
                }
            }
        });
        Self {
            commands,
            errors,
            error_tx,
            generation,
            resume: Arc::new(ResumeState {
                mode: AtomicU64::new(0),
                position: AtomicU64::new(0),
            }),
            process: None,
            socket_path: unique_socket_path(),
            program,
            extra_args: Vec::new(),
        }
    }

    /// Path of this player's mpv IPC socket. The background timeline sampler
    /// only needs this string, so it never has to borrow the `Player`.
    pub fn socket_path(&self) -> &str {
        &self.socket_path
    }

    pub fn generation(&self) -> Arc<AtomicU64> {
        self.generation.clone()
    }
    pub fn current_generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
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
    /// at startup to restore the previous session.
    ///
    /// The position is applied with a seek once mpv has opened the file, so
    /// it belongs to that track alone: tracks loaded later start at 0:00.
    /// mpv opens the file paused, so the restored track is never heard from
    /// the beginning before the seek lands.
    pub fn play_at(
        &mut self,
        track_id: &str,
        stream: Option<&Stream>,
        start_secs: f64,
        paused: bool,
    ) -> anyhow::Result<()> {
        self.stop();
        let target = self.resolve_target(track_id, stream);
        self.spawn_mpv(&target, stream, true)?;
        // Opening a network stream can take seconds, so the seek runs on a
        // background thread: the first frame is not held up by it.
        let socket = self.socket_path.clone();
        let position = resume_position(start_secs);
        self.resume = Arc::new(ResumeState::new(position, paused));
        let resume = self.resume.clone();
        let errors = self.error_tx.clone();
        let generation = self.current_generation();
        std::thread::spawn(move || {
            if let Err(error) = apply_resume_guarded(&socket, &resume)
                && resume.active()
            {
                let _ = errors.try_send((generation, error.to_string()));
            }
            resume.cancel();
        });
        Ok(())
    }

    /// Queue `track_id` after the currently playing track in mpv's playlist,
    /// for gapless playback. mpv will auto-advance to it when the current
    /// track finishes. Returns an error if mpv is not running.
    pub fn queue_next(&mut self, track_id: &str, stream: Option<&Stream>) -> anyhow::Result<()> {
        if self.resume.active() {
            anyhow::bail!("session restore is still loading");
        }
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
                // A pending resume owns the old process. Terminating it also
                // closes its IPC connection before any replacement can load.
                if self.resume.active() {
                    self.stop();
                }
                if self.is_running() {
                    self.generation.fetch_add(1, Ordering::AcqRel);
                    self.playlist_clear()?;
                    self.load_target(&target, "replace", stream)
                } else {
                    self.spawn_mpv(&target, stream, false)
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
        // No start position here: a track loaded later must begin at 0:00,
        // even when the session was restored from a saved position.
        body.push_str(&serde_json::to_string(&json!({
            "command": ["loadfile", target, mode]
        }))?);
        body.push('\n');
        if mode == "replace" {
            body.push_str("{\"command\":[\"set_property\",\"pause\",false]}\n");
        }
        self.send_raw(&body)
    }

    /// Send a raw IPC command array to the running mpv process.
    fn send_command(&self, args: &[&str]) -> anyhow::Result<()> {
        let body = serde_json::to_string(&json!({ "command": args }))? + "\n";
        self.send_raw(&body)
    }

    fn send_raw(&self, body: &str) -> anyhow::Result<()> {
        let generation = self.current_generation();
        if self
            .commands
            .try_send(IpcRequest {
                socket: self.socket_path.clone(),
                body: body.to_string(),
                generation,
            })
            .is_err()
        {
            let message = "mpv command queue is busy".to_string();
            let _ = self.error_tx.try_send((generation, message.clone()));
            anyhow::bail!(message);
        }
        Ok(())
    }

    pub fn poll_error(&self) -> Option<String> {
        while let Ok((generation, error)) = self.errors.try_recv() {
            if generation == self.generation.load(Ordering::Acquire) {
                return Some(error);
            }
        }
        None
    }

    /// Spawn a fresh mpv process for `target`. Used for the first track and
    /// as a fallback when the persistent mpv dies or wedges.
    ///
    /// `paused` opens the file paused. A restored session needs that: the
    /// resume position is applied with a seek once the file is open, and a
    /// file that starts playing right away would be heard from the
    /// beginning. No start position is ever passed on the command line —
    /// mpv would apply it to every later track as well.
    fn spawn_mpv(
        &mut self,
        target: &str,
        stream: Option<&Stream>,
        paused: bool,
    ) -> anyhow::Result<()> {
        self.generation.fetch_add(1, Ordering::AcqRel);
        // Remove a socket left by a previous process; otherwise mpv can't bind.
        let _ = std::fs::remove_file(&self.socket_path);

        let mut command = Command::new(&self.program);
        command.args(&self.extra_args);
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
        let mut mode = self.resume.mode.load(Ordering::Acquire);
        while mode & 1 != 0 {
            match self.resume.mode.compare_exchange(
                mode,
                mode ^ 2,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(current) => mode = current,
            }
        }
        let _ = self.send_command(&["cycle", "pause"]);
    }

    /// Seek to an absolute position via mpv's IPC socket.
    pub fn seek_to(&mut self, seconds: f64) {
        if self.resume.active() {
            self.resume
                .position
                .store(resume_position(seconds).to_bits(), Ordering::Release);
            if self.resume.mode.fetch_add(4, Ordering::AcqRel) & 1 != 0 {
                return;
            }
        }
        let _ = self.send_command(&["seek", &format!("{:.3}", seconds.max(0.0)), "absolute"]);
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
        self.resume.cancel();
        self.generation.fetch_add(1, Ordering::AcqRel);
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

/// The resume position of a restored track. Bad values collapse to 0
/// instead of reaching mpv.
fn resume_position(seconds: f64) -> f64 {
    if seconds.is_finite() {
        seconds.max(0.0)
    } else {
        0.0
    }
}

/// Send a raw IPC command body to mpv's socket.
#[cfg(test)]
fn send_to_socket(socket_path: &str, body: &str) -> anyhow::Result<()> {
    let stream = UnixStream::connect(socket_path)?;
    exchange_commands(stream, body)
}

/// Read every acknowledgement, ignoring unrelated events. Each request and
/// reply is bounded; a broken peer cannot wedge the worker or allocate forever.
fn exchange_commands(stream: UnixStream, body: &str) -> anyhow::Result<()> {
    exchange_replies(stream, body).map(|_| ())
}

fn exchange_replies(mut stream: UnixStream, body: &str) -> anyhow::Result<Vec<Value>> {
    static REQUEST_SEQUENCE: AtomicU64 = AtomicU64::new(1);
    let count = body.lines().count();
    let first_id = REQUEST_SEQUENCE.fetch_add(count as u64 + 1, Ordering::Relaxed);
    let mut replies = vec![Value::Null; count];
    stream.set_write_timeout(Some(Duration::from_millis(100)))?;
    let mut pending = std::collections::HashSet::new();
    let mut requests = String::new();
    for (index, line) in body.lines().enumerate() {
        let mut value: Value = serde_json::from_str(line)?;
        let id = first_id + index as u64;
        value["request_id"] = json!(id);
        pending.insert(id);
        requests.push_str(&serde_json::to_string(&value)?);
        requests.push('\n');
    }
    stream.write_all(requests.as_bytes())?;
    let deadline = Instant::now() + Duration::from_millis(500);
    let mut reader = BufReader::new(stream);
    // Keep bytes across per-read timeouts, including a UTF-8 character
    // split between reads. Decode only after the complete line arrives.
    let mut line = Vec::new();
    while !pending.is_empty() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        anyhow::ensure!(
            !remaining.is_zero(),
            "mpv command acknowledgement timed out"
        );
        reader
            .get_ref()
            .set_read_timeout(Some(remaining.min(Duration::from_millis(100))))?;
        anyhow::ensure!(line.len() < 16 * 1024, "mpv reply exceeds size limit");
        let available = (16 * 1024 - line.len()) as u64;
        let count = match reader.by_ref().take(available).read_until(b'\n', &mut line) {
            Ok(count) => count,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) =>
            {
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        anyhow::ensure!(
            count > 0 && line.ends_with(b"\n"),
            "mpv sent an incomplete reply"
        );
        let reply: Value = serde_json::from_slice(&line)?;
        line.clear();
        let Some(id) = reply["request_id"].as_u64() else {
            continue;
        };
        if !pending.remove(&id) {
            continue;
        }
        replies[(id - first_id) as usize] = reply["data"].clone();
        anyhow::ensure!(
            reply["error"].as_str() == Some("success"),
            "mpv rejected command: {}",
            reply["error"].as_str().unwrap_or("missing status")
        );
    }
    Ok(replies)
}

/// Ask mpv for a list of properties and collect the answers. The answers
/// keep the order of `names`; `None` marks a property mpv did not answer
/// in time.
fn read_properties(socket_path: &str, names: &[&str], timeout: Duration) -> Vec<Option<Value>> {
    let mut answers: Vec<Option<Value>> = vec![None; names.len()];
    let Ok(mut stream) = UnixStream::connect(socket_path) else {
        return answers;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(25)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(100)));

    let mut body = String::new();
    for (index, name) in names.iter().enumerate() {
        let request = json!({ "command": ["get_property", name], "request_id": index + 1 });
        if let Ok(text) = serde_json::to_string(&request) {
            body.push_str(&text);
            body.push('\n');
        }
    }
    if stream.write_all(body.as_bytes()).is_err() {
        return answers;
    }

    let mut reader = BufReader::new(stream);
    let deadline = Instant::now() + timeout;
    let mut seen = 0usize;
    let mut line = String::new();
    while seen < names.len() {
        if Instant::now() >= deadline {
            break;
        }
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                let parsed = serde_json::from_str::<Value>(&line).ok();
                line.clear();
                let Some(value) = parsed else { continue };
                // mpv interleaves unsolicited events with replies; match only
                // our own request ids.
                let Some(id) = value.get("request_id").and_then(Value::as_u64) else {
                    continue;
                };
                let Some(index) = id.checked_sub(1).map(|i| i as usize) else {
                    continue;
                };
                if index >= names.len() || answers[index].is_some() {
                    continue;
                }
                answers[index] = value.get("data").cloned();
                seen += 1;
            }
            Err(_) => {
                // mpv is busy: keep waiting until the deadline.
                continue;
            }
        }
    }
    answers
}

/// Read one property from mpv's IPC socket. `None` when the answer does not
/// arrive in time.
#[cfg(test)]
fn read_property(socket_path: &str, name: &str, timeout: Duration) -> Option<Value> {
    read_properties(socket_path, &[name], timeout)
        .into_iter()
        .next()
        .flatten()
}

/// Restore through one connection to the original process. Once connected,
/// never reconnect: a later player may reuse the socket pathname.
fn apply_resume_guarded(socket_path: &str, resume: &ResumeState) -> anyhow::Result<()> {
    let deadline = Instant::now() + RESUME_WAIT;
    let mut stream = loop {
        anyhow::ensure!(resume.active(), "resume cancelled");
        match UnixStream::connect(socket_path) {
            Ok(stream) => break stream,
            Err(error) if Instant::now() >= deadline => return Err(error.into()),
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    };
    // Keep the original connection alive, including while the file opens.
    loop {
        anyhow::ensure!(resume.active(), "resume cancelled");
        anyhow::ensure!(Instant::now() < deadline, "session restore timed out");
        let mode = resume.mode.load(Ordering::Acquire);
        let position = f64::from_bits(resume.position.load(Ordering::Acquire));
        let paused = mode & 2 != 0;
        // A zero position still waits for IPC readiness and acknowledges unpause.
        let command = if position > 0.0 {
            json!({"command": ["seek", format!("{position:.3}"), "absolute"]})
        } else {
            json!({"command": ["set_property", "pause", paused]})
        };
        let body = serde_json::to_string(&command)? + "\n";
        if exchange_commands(stream.try_clone()?, &body).is_ok() {
            if position > 0.0 {
                // Seek acceptance can precede completion. Keep the original
                // process paused until its timeline confirms that seek.
                let values = exchange_replies(stream.try_clone()?,
                    "{\"command\":[\"get_property\",\"time-pos\"]}\n{\"command\":[\"get_property\",\"duration\"]}\n").unwrap_or_default();
                let current = values.first().and_then(Value::as_f64);
                let duration = values.get(1).and_then(Value::as_f64);
                if !current.is_some_and(|value| value >= position - 1.0)
                    && !duration.is_some_and(|value| value > 0.0 && value <= position)
                {
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }
                anyhow::ensure!(resume.active(), "resume cancelled");
                let body =
                    serde_json::to_string(&json!({"command": ["set_property", "pause", paused]}))?
                        + "\n";
                exchange_commands(stream.try_clone()?, &body)?;
            }
            if resume
                .mode
                .compare_exchange(mode, mode & !1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Ok(());
            }
            continue;
        }
        // A process exit makes the connected socket unusable; never send a
        // pending seek to a new process at the same filesystem path.
        if stream.write_all(b"\n").is_err() {
            anyhow::bail!("mpv disconnected during restore");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
fn apply_resume(socket_path: &str, position: f64, paused: bool) -> anyhow::Result<()> {
    apply_resume_guarded(socket_path, &ResumeState::new(position, paused))
}

/// Query an mpv IPC socket for `time-pos` and `duration`.
///
/// This does blocking socket I/O with tight timeouts, which is why the TUI
/// runs it on a background task rather than the frame thread.
pub fn timeline_path(socket_path: &str) -> Option<Timeline> {
    let answers = read_properties(
        socket_path,
        &["time-pos", "duration", "playlist-pos", "playlist"],
        Duration::from_millis(40),
    );
    let position = answers.first().and_then(|value| value.as_ref()).and_then(Value::as_f64);
    let duration = answers.get(1).and_then(|value| value.as_ref()).and_then(Value::as_f64);
    (position.is_some() || duration.is_some()).then_some(Timeline {
        position,
        duration,
        playlist_position: answers
            .get(2)
            .and_then(|v| v.as_ref())
            .and_then(Value::as_u64),
        entry_id: answers
            .get(3)
            .and_then(|v| v.as_ref())
            .and_then(Value::as_array)
            .and_then(|entries| entries.iter().find(|entry| entry["current"] == true))
            .and_then(|entry| entry["id"].as_i64()),
    })
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
pub(crate) mod player_tests {
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
    fn the_resume_position_is_clamped_to_a_usable_value() {
        assert_eq!(resume_position(42.5), 42.5);
        assert_eq!(resume_position(0.0), 0.0);
        assert_eq!(resume_position(-5.0), 0.0);
        assert_eq!(resume_position(f64::NAN), 0.0);
        assert_eq!(resume_position(f64::INFINITY), 0.0);
    }

    // -----------------------------------------------------------------
    // Resume position (fake mpv)
    // -----------------------------------------------------------------

    /// A player for the real mpv, without an audio device.
    fn silent_player() -> Player {
        let mut player = Player::new();
        player.extra_args = vec!["--ao=null".to_string()];
        player
    }

    /// A silent WAV file of the given length. A real file gives mpv a known
    /// duration and a reliable seek, which a synthetic source does not.
    fn temp_wav(tag: &str, seconds: u32) -> (PathBuf, PathBuf) {
        let (dir, _) = temp_script(tag, "");
        let path = dir.join("tone.wav");
        const RATE: u32 = 8_000;
        let samples = RATE * seconds;
        let mut bytes: Vec<u8> = Vec::with_capacity(44 + samples as usize);
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + samples).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
        bytes.extend_from_slice(&1u16.to_le_bytes()); // mono
        bytes.extend_from_slice(&RATE.to_le_bytes());
        bytes.extend_from_slice(&RATE.to_le_bytes()); // bytes per second
        bytes.extend_from_slice(&1u16.to_le_bytes()); // block align
        bytes.extend_from_slice(&8u16.to_le_bytes()); // bits per sample
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&samples.to_le_bytes());
        bytes.extend(std::iter::repeat(128u8).take(samples as usize));
        fs::write(&path, &bytes).expect("write wav");
        (dir, path)
    }

    /// A fake mpv that records its arguments and then idles.
    fn fake_mpv(tag: &str) -> (PathBuf, PathBuf) {
        let body = "#!/bin/sh\n\
                    dir=\"$(dirname \"$0\")\"\n\
                    printf '%s\\n' \"$@\" > \"$dir/argv\"\n\
                    exec sleep 30\n";
        let (dir, script) = temp_script(tag, body);
        (dir, script)
    }

    /// The arguments the fake mpv was started with. The script writes them
    /// asynchronously, so give it a moment.
    fn recorded_argv(dir: &Path) -> Vec<String> {
        let path = dir.join("argv");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let text = fs::read_to_string(&path).unwrap_or_default();
            if !text.is_empty() || Instant::now() >= deadline {
                return text.lines().map(str::to_string).collect();
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// A fake mpv IPC server. It records every command it receives and
    /// answers property requests, so the client side can be exercised
    /// without a real mpv.
    fn fake_ipc_server(tag: &str) -> (PathBuf, std::sync::mpsc::Receiver<String>) {
        use std::sync::{Arc, Mutex};
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
        let path = dir.join("mpv.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");
        let (sender, receiver) = std::sync::mpsc::channel();
        // The position the fake mpv reports; a seek moves it.
        let position = Arc::new(Mutex::new(0.0f64));
        let server_position = Arc::clone(&position);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let Ok(clone) = stream.try_clone() else { continue };
                let mut reader = BufReader::new(clone);
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                    let command = line.trim().to_string();
                    if command.is_empty() {
                        continue;
                    }
                    let _ = sender.send(command.clone());
                    // Answer like mpv does: every command gets a reply.
                    if let Ok(value) = serde_json::from_str::<Value>(&command) {
                        let args = value.get("command").and_then(|c| c.as_array());
                        let name = args
                            .and_then(|a| a.first())
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        if name == "seek" {
                            if let Some(target) = args
                                .and_then(|a| a.get(1))
                                .and_then(Value::as_str)
                                .and_then(|text| text.parse::<f64>().ok())
                            {
                                if let Ok(mut current) = server_position.lock() {
                                    *current = target;
                                }
                            }
                        }
                        let property = args.and_then(|a| a.get(1)).and_then(Value::as_str);
                        let data = match (name, property) {
                            ("get_property", Some("time-pos")) => {
                                json!(server_position.lock().map(|guard| *guard).unwrap_or(0.0))
                            }
                            ("get_property", Some("duration")) => json!(600.0),
                            ("get_property", _) => json!("fake-path"),
                            _ => Value::Null,
                        };
                        let id = value.get("request_id").cloned().unwrap_or(json!(1));
                        let reply = json!({ "data": data, "error": "success", "request_id": id });
                        if let Ok(body) = serde_json::to_string(&reply) {
                            let _ = stream.write_all((body + "\n").as_bytes());
                        }
                    }
                }
            }
        });
        (path, receiver)
    }

    pub(crate) fn recording_player() -> (Player, std::sync::mpsc::Receiver<String>) {
        let (socket, commands) = fake_ipc_server("recording-player");
        let mut player = Player::new();
        player.socket_path = socket.to_string_lossy().into_owned();
        player.process = Some(Command::new("sleep").arg("30").spawn().unwrap());
        (player, commands)
    }

    /// The restored track must not be started through `--start`: mpv would
    /// apply that position to every later track as well.
    #[test]
    fn zero_resume_waits_for_a_delayed_socket() {
        if which("mpv").is_none() {
            return;
        }
        let (dir, script) = temp_script("delayed-mpv", "#!/bin/sh\nsleep 0.3\nexec mpv \"$@\"\n");
        let (audio_dir, source) = temp_wav("delayed-audio", 60);
        let mut player = Player::with_program(script);
        player.extra_args.push("--ao=null".into());
        let local = crate::offline::local_id(&source);
        player.play_at(&local, None, 0.0, false).unwrap();
        assert!(wait_for_position(player.socket_path(), |position| position > 0.1));
        player.stop();
        let _ = fs::remove_dir_all(dir);
        let _ = fs::remove_dir_all(audio_dir);
    }

    #[test]
    fn switching_tracks_cancels_a_pending_resume() {
        if which("mpv").is_none() {
            return;
        }
        let (dir, script) = temp_script("slow-resume", "#!/bin/sh\nsleep 0.3\nexec mpv \"$@\"\n");
        let (audio_dir, source) = temp_wav("cancel-resume", 60);
        let mut player = Player::with_program(script);
        player.extra_args.push("--ao=null".into());
        let local = crate::offline::local_id(&source);
        player.play_at(&local, None, 42.0, false).unwrap();
        player.play(&local).unwrap();
        assert!(wait_for_position(player.socket_path(), |position| position
            > 0.1
            && position < 5.0));
        std::thread::sleep(Duration::from_millis(600));
        assert!(read_position(player.socket_path()).unwrap() < 5.0);
        player.stop();
        let _ = fs::remove_dir_all(dir);
        let _ = fs::remove_dir_all(audio_dir);
    }

    #[test]
    fn input_during_restore_controls_the_final_position_and_pause_state() {
        if which("mpv").is_none() {
            return;
        }
        let (dir, script) = temp_script("resume-input", "#!/bin/sh\nsleep 0.3\nexec mpv \"$@\"\n");
        let (audio_dir, source) = temp_wav("resume-input-audio", 60);
        let mut player = Player::with_program(script);
        player.extra_args.push("--ao=null".into());
        player
            .play_at(&crate::offline::local_id(&source), None, 42.0, true)
            .unwrap();
        player.seek_to(12.0);
        player.pause();
        assert!(wait_for_position(player.socket_path(), |position| position
            > 12.1
            && position < 20.0));
        assert_eq!(
            read_property(player.socket_path(), "pause", Duration::from_millis(500)),
            Some(json!(false))
        );
        player.stop();
        let _ = fs::remove_dir_all(dir);
        let _ = fs::remove_dir_all(audio_dir);
    }

    #[test]
    fn ipc_reply_arriving_after_a_read_timeout_is_accepted() {
        assert_eq!(delayed_ipc_reply(false), json!("café"));
    }

    #[test]
    fn ipc_reply_fragment_survives_a_read_timeout() {
        assert_eq!(delayed_ipc_reply(true), json!("café"));
    }

    fn delayed_ipc_reply(fragmented: bool) -> Value {
        let (client, mut server) = UnixStream::pair().unwrap();
        let peer = std::thread::spawn(move || {
            let mut line = String::new();
            BufReader::new(server.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            let reply = format!("{}\n", json!({
                "request_id": request["request_id"],
                "error": "success", "data": "café"
            }));
            let bytes = reply.as_bytes();
            // Split inside a UTF-8 character to verify byte preservation
            // as well as the acknowledgement deadline.
            let split = if fragmented {
                bytes.iter().position(|&byte| byte == 0xc3).unwrap() + 1
            } else {
                0
            };
            server.write_all(&bytes[..split]).unwrap();
            std::thread::sleep(Duration::from_millis(220));
            server.write_all(&bytes[split..]).unwrap();
        });
        let replies = exchange_replies(client, "{\"command\":[\"get_property\",\"title\"]}\n")
            .expect("reply within the 500 ms deadline");
        peer.join().unwrap();
        replies[0].clone()
    }

    #[test]
    fn ipc_reply_timeout_respects_the_overall_deadline() {
        let (client, server) = UnixStream::pair().unwrap();
        let peer = std::thread::spawn(move || {
            let mut reader = BufReader::new(server);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            // Hold the socket open until the client reaches its deadline.
            reader.get_ref().set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            reader.read_to_end(&mut Vec::new()).unwrap();
        });
        let started = Instant::now();
        let error = exchange_replies(client, "{\"command\":[\"cycle\",\"pause\"]}\n")
            .unwrap_err();
        let elapsed = started.elapsed();
        peer.join().unwrap();
        assert!(error.to_string().contains("acknowledgement timed out"), "{error}");
        assert!(elapsed >= Duration::from_millis(450), "{elapsed:?}");
        assert!(elapsed < Duration::from_millis(1500), "{elapsed:?}");
    }

    #[test]
    fn rejected_ipc_commands_are_reported() {
        let (path, _commands) = fake_ipc_server("error-location");
        // Replace the normal server's path with a rejecting peer.
        let rejected_path = path.with_file_name("rejected.sock");
        let listener = std::os::unix::net::UnixListener::bind(&rejected_path).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            stream.write_all(b"{\"event\":\"idle\"}\n").unwrap();
            writeln!(
                stream,
                "{}",
                json!({"request_id": request["request_id"], "error": "invalid parameter"})
            )
            .unwrap();
        });
        let error = send_to_socket(
            rejected_path.to_str().unwrap(),
            "{\"command\":[\"seek\",\"bad\"]}\n",
        )
        .unwrap_err();
        assert!(error.to_string().contains("invalid parameter"));
        server.join().unwrap();
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn an_unresponsive_ipc_peer_does_not_block_the_caller() {
        let path = unique_socket_path();
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            std::thread::sleep(Duration::from_millis(500));
        });
        let mut player = Player::new();
        player.socket_path = path.clone();
        let started = Instant::now();
        player.send_command(&["cycle", "pause"]).unwrap();
        assert!(started.elapsed() < Duration::from_millis(100));
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if player.poll_error().is_some() {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        server.join().unwrap();
        let _ = fs::remove_file(path);
    }

    #[test]
    fn a_restored_track_is_never_started_with_a_start_option() {
        let (dir, script) = fake_mpv("resume-start");
        let mut player = Player::with_program(script);
        player
            .spawn_mpv("https://example.invalid/audio", None, true)
            .expect("spawn");
        let argv = recorded_argv(&dir);
        assert!(
            !argv.iter().any(|arg| arg.starts_with("--start")),
            "mpv must not receive a start position: {argv:?}"
        );
        // The file opens paused, so the seek can land before any audio.
        assert!(
            argv.iter().any(|arg| arg == "--pause=yes"),
            "the restored track should open paused: {argv:?}"
        );
        player.stop();
        let _ = fs::remove_dir_all(dir);
    }

    /// A track loaded later replaces the playlist; it must start at 0:00 and
    /// must not inherit a paused state from the command line.
    #[test]
    fn a_replaced_track_carries_no_start_option() {
        let (dir, script) = fake_mpv("replace-start");
        let mut player = Player::with_program(script);
        player
            .spawn_mpv("https://example.invalid/audio", None, false)
            .expect("spawn");
        let argv = recorded_argv(&dir);
        assert!(
            !argv.iter().any(|arg| arg.starts_with("--start")),
            "mpv must not receive a start position: {argv:?}"
        );
        assert!(
            !argv.iter().any(|arg| arg == "--pause=yes"),
            "a fresh track must not start paused: {argv:?}"
        );
        player.stop();
        let _ = fs::remove_dir_all(dir);
    }

    /// The load commands themselves carry no position: mpv starts the file
    /// at 0:00 unless a seek says otherwise.
    #[test]
    fn the_load_command_carries_no_start_position() {
        let (path, commands) = fake_ipc_server("load");
        let mut player = Player::new();
        player.socket_path = path.to_string_lossy().into_owned();

        player
            .load_target("https://example.invalid/audio", "replace", None)
            .expect("load");
        let replace = commands.recv_timeout(Duration::from_secs(2)).expect("command");
        assert!(replace.contains("loadfile"));
        assert!(replace.contains("replace"));
        assert!(!replace.contains("start"));
        let unpause = commands
            .recv_timeout(Duration::from_secs(2))
            .expect("unpause");
        assert!(unpause.contains("pause"));

        // The gapless append used by the queue is just as position-free.
        player
            .load_target("https://example.invalid/next", "append-play", None)
            .expect("append");
        let append = commands.recv_timeout(Duration::from_secs(2)).expect("command");
        assert!(append.contains("append-play"));
        assert!(!append.contains("start"));
    }

    /// The resume position is applied with a seek on the open file, and the
    /// paused state follows the restored session.
    #[test]
    fn the_resume_position_is_applied_with_a_seek() {
        let (path, commands) = fake_ipc_server("seek");
        let socket = path.to_string_lossy().into_owned();

        apply_resume(&socket, 42.0, false).expect("resume");
        let mut seen = Vec::new();
        while let Ok(command) = commands.recv_timeout(Duration::from_millis(300)) {
            seen.push(command);
        }
        assert!(
            seen.iter().any(|c| c.contains("seek") && c.contains("42.000")),
            "expected a seek to 42s, got {seen:?}"
        );
        assert!(
            seen.iter().any(|c| c.contains("pause") && c.contains("false")),
            "expected the paused state to be cleared, got {seen:?}"
        );
    }

    /// A paused session stays paused at the restored position.
    #[test]
    fn a_paused_restore_stays_paused() {
        let (path, commands) = fake_ipc_server("paused");
        let socket = path.to_string_lossy().into_owned();

        apply_resume(&socket, 12.0, true).expect("resume");
        let mut seen = Vec::new();
        while let Ok(command) = commands.recv_timeout(Duration::from_millis(300)) {
            seen.push(command);
        }
        assert!(seen.iter().any(|c| c.contains("seek") && c.contains("12.000")));
        assert!(
            seen.iter().any(|c| c.contains("pause") && c.contains("true")),
            "expected the file to stay paused, got {seen:?}"
        );
    }

    /// A position of zero needs no seek: the file simply starts at 0:00.
    #[test]
    fn a_zero_resume_position_sends_no_seek() {
        let (path, commands) = fake_ipc_server("zero");
        let socket = path.to_string_lossy().into_owned();

        apply_resume(&socket, 0.0, false).expect("resume");
        let mut seen = Vec::new();
        while let Ok(command) = commands.recv_timeout(Duration::from_millis(300)) {
            seen.push(command);
        }
        assert!(!seen.iter().any(|c| c.contains("seek")));
        assert!(seen.iter().any(|c| c.contains("pause")));
    }

    // -----------------------------------------------------------------
    // End-to-end check against a real mpv
    // -----------------------------------------------------------------

    /// A real mpv: the restored track plays from its saved position and the
    /// tracks loaded afterwards start at 0:00.
    #[test]
    fn a_restored_position_does_not_leak_into_later_tracks() {
        if which("mpv").is_none() {
            // mpv is a runtime dependency; without it there is nothing to
            // check here.
            return;
        }
        // Long enough that the track cannot end while the test runs — mpv
        // exits when its playlist runs out.
        let (dir, source) = temp_wav("restore-wav", 120);
        let (_, next) = temp_wav("restore-next-wav", 120);

        let mut player = silent_player();
        player.spawn_mpv(&source.to_string_lossy(), None, true).expect("spawn mpv");
        let socket = player.socket_path().to_string();

        // Restore: the track opens paused and is seeked to 20 seconds.
        apply_resume(&socket, 20.0, false).expect("resume");
        let restored = wait_for_position(&socket, |position| position > 15.0);
        assert!(
            restored,
            "the restored track should play from about 20s"
        );

        // The next track replaces it and must start at the beginning.
        player.load_target(&next.to_string_lossy(), "replace", None).expect("load next");
        assert!(
            wait_for_path(&socket, &next),
            "the next track should start (path {:?}, pos {:?})",
            current_path(&socket),
            read_position(&socket)
        );
        // The position of the replaced track must not carry over.
        let started_at_zero =
            wait_for_position(&socket, |position| position > 0.05 && position < 10.0);
        assert!(
            started_at_zero,
            "the next track should start at 0:00"
        );

        player.stop();
        let _ = fs::remove_dir_all(dir);
    }

    /// The track mpv queues up plays from the beginning when the current
    /// one ends — not from the position the session was restored to.
    #[test]
    fn a_queued_track_starts_at_the_beginning() {
        if which("mpv").is_none() {
            return;
        }
        // A short current track: it ends while the test waits, so mpv moves
        // on to the queued one.
        let (dir, current) = temp_wav("queued-wav", 8);
        let (_, queued) = temp_wav("queued-next-wav", 60);

        let mut player = silent_player();
        player.spawn_mpv(&current.to_string_lossy(), None, true).expect("spawn mpv");
        let socket = player.socket_path().to_string();
        // Restore to 1.5s, as a session resume would.
        apply_resume(&socket, 1.5, false).expect("resume");
        assert!(
            wait_for_path(&socket, &current),
            "the first track should open"
        );
        // The resume position has to be in place, otherwise the first track
        // would not end in time and the test would be measuring the wrong
        // thing.
        assert!(
            wait_for_position(&socket, |position| position > 1.0),
            "the restored track should play from about 1.5s"
        );
        player
            .load_target(&queued.to_string_lossy(), "append-play", None)
            .expect("queue next");

        // When the first track ends, mpv moves to the queued one.
        assert!(
            wait_for_path(&socket, &queued),
            "the queued track should start (path {:?}, pos {:?})",
            current_path(&socket),
            read_position(&socket)
        );
        let timeline = timeline_path(&socket).expect("queued timeline");
        assert_eq!(timeline.playlist_position, Some(1));
        assert!(timeline.entry_id.is_some());
        let position = read_position(&socket).unwrap_or(-1.0);
        assert!(
            position < 5.0,
            "the queued track should start at 0:00, got {position}"
        );

        player.stop();
        let _ = fs::remove_dir_all(dir);
    }

    /// mpv's current position.
    fn read_position(socket_path: &str) -> Option<f64> {
        read_property(socket_path, "time-pos", Duration::from_millis(500))
            .and_then(|value| value.as_f64())
    }

    /// Wait until mpv plays the given source.
    fn wait_for_path(socket_path: &str, target: &Path) -> bool {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            let path = read_property(socket_path, "path", Duration::from_millis(500))
                .and_then(|value| value.as_str().map(str::to_string));
            if path.as_deref() == Some(target.to_string_lossy().as_ref()) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    /// The source mpv is playing right now.
    fn current_path(socket_path: &str) -> Option<String> {
        read_property(socket_path, "path", Duration::from_millis(500))
            .and_then(|value| value.as_str().map(str::to_string))
    }

    /// Wait until the position satisfies `condition`.
    fn wait_for_position(
        socket_path: &str,
        condition: impl Fn(f64) -> bool,
    ) -> bool {
        // Generous: the whole suite runs several real-mpv tests in parallel
        // and a loaded machine can delay mpv's first reply.
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if let Some(position) = read_position(socket_path) {
                if condition(position) {
                    return true;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
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
