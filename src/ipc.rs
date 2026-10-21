//! A tiny Unix-socket bridge so external tools can query and control the
//! running TUI — most importantly Quickshell's media widget, which polls
//! `ytmtui --json queue` / `ytmtui --json now` and jumps with
//! `ytmtui jump <i>`.
//!
//! Newline-delimited JSON: one `{"command": …, "args": …}` request in, one
//! response line out, so the same widget works unchanged.

use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc::UnboundedSender;

/// One queue row as the widget wants it: flat, with numeric seconds.
pub struct QueueRow {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_secs: u64,
    pub thumbnail_url: String,
}

/// The queue facts mirrored to external clients, diffed and rewritten by
/// the app on every tick.
#[derive(Default)]
pub struct IpcState {
    pub tracks: Vec<QueueRow>,
    pub current_index: i32,
    pub repeat: String,
    pub shuffle: bool,
    pub status: String,
    pub position_secs: u64,
    pub track_id: String,
}

pub type IpcHandle = Arc<RwLock<IpcState>>;

/// Commands external clients can push into the app.
#[derive(Debug)]
pub enum IpcCmd {
    Jump(usize),
    Clear,
}

fn socket_path() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("ytmtui.sock")
}

/// Serve the bridge until the process exits. Refuses to fight a live
/// instance: if the socket already answers, another ytmtui owns it.
pub async fn serve(state: IpcHandle, tx: UnboundedSender<IpcCmd>) {
    let path = socket_path();
    if UnixStream::connect(&path).await.is_ok() {
        return; // another instance is already bridging
    }
    let _ = std::fs::remove_file(&path); // stale socket from a dead run
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(_) => return,
    };
    // Same privacy stance: only the user may query the player.
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));

    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let state = state.clone();
                let tx = tx.clone();
                tokio::spawn(async move {
                    let _ = handle(stream, &state, &tx).await;
                });
            }
            Err(_) => continue,
        }
    }
}

async fn handle(
    stream: UnixStream,
    state: &IpcHandle,
    tx: &UnboundedSender<IpcCmd>,
) -> std::io::Result<()> {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let mut line = String::new();
    // One line in; a line out; close.
    reader.read_line(&mut line).await?;
    let response = route(line.trim(), state, tx);
    write.write_all(response.as_bytes()).await?;
    write.write_all(b"\n").await?;
    write.shutdown().await?;
    Ok(())
}

fn route(line: &str, state: &IpcHandle, tx: &UnboundedSender<IpcCmd>) -> String {
    let Ok(req) = serde_json::from_str::<serde_json::Value>(line) else {
        return err("malformed request");
    };
    let command = req.get("command").and_then(|c| c.as_str()).unwrap_or("");
    let args = req.get("args");

    let guard = state.read().unwrap();
    match command {
        // The widget's queue poll: { tracks, current_index, repeat, shuffle }.
        "queue" => {
            let tracks: Vec<serde_json::Value> = guard
                .tracks
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "title": t.title,
                        "artist": t.artist,
                        "album": t.album,
                        "duration": t.duration_secs,
                        "thumbnail_url": t.thumbnail_url,
                    })
                })
                .collect();
            ok(serde_json::json!({
                "tracks": tracks,
                "current_index": guard.current_index,
                "length": tracks.len(),
                "repeat": guard.repeat,
                "shuffle": guard.shuffle,
            }))
        }
        // The widget's now-playing poll (drives the like chip only).
        "now" => ok(serde_json::json!({
            "track": {
                "title": "",
                "artist": "",
                "likeStatus": "INDIFFERENT",
            },
            "position": guard.position_secs,
            "status": guard.status,
            "video_id": guard.track_id,
        })),
        "jump" => {
            let index = args
                .and_then(|a| a.get("index"))
                .and_then(|i| i.as_u64())
                .map(|i| i as usize);
            match index {
                Some(i) if i < guard.tracks.len() => {
                    drop(guard);
                    let _ = tx.send(IpcCmd::Jump(i));
                    ok(serde_json::Value::Null)
                }
                Some(_) => err("index out of range"),
                None => err("missing index"),
            }
        }
        "queue_clear" => {
            drop(guard);
            let _ = tx.send(IpcCmd::Clear);
            ok(serde_json::Value::Null)
        }
        _ => err(&format!("unknown command: {command}")),
    }
}

fn ok(data: serde_json::Value) -> String {
    serde_json::json!({"ok": true, "data": data}).to_string()
}

fn err(message: &str) -> String {
    serde_json::json!({"ok": false, "error": message}).to_string()
}

// ---------------------------------------------------------------------------
// Client side — used by the `ytmtui queue` / `ytmtui jump` CLI.
// ---------------------------------------------------------------------------

/// Send a command to the running TUI. Blocking on purpose: the CLI runs
/// outside the async runtime.
pub fn request(command: &str, args: Option<serde_json::Value>) -> anyhow::Result<serde_json::Value> {
    use std::io::{BufRead, Write};
    let stream = std::os::unix::net::UnixStream::connect(socket_path())?;
    let payload = serde_json::json!({"command": command, "args": args});
    let mut stream = stream;
    stream.write_all(payload.to_string().as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    let mut reader = std::io::BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    Ok(serde_json::from_str(line.trim())?)
}
