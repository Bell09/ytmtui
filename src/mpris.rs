//! MPRIS — the freedesktop `org.mpris.MediaPlayer2` D-Bus service.
//!
//! Desktop media widgets (Quickshell, playerctld, panel bars) discover
//! players through this bus; without it ytmtui is invisible to them.
//! The UI loop mirrors playback into [`MprisState`] on every tick and
//! diffs become `PropertiesChanged` / `Seeked` signals; widget actions
//! arrive as [`MprisCmd`]s on a channel the loop drains.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

use tokio::sync::mpsc::UnboundedSender;
use zbus::zvariant::{Array, ObjectPath, OwnedObjectPath, OwnedValue, Value};
use zbus::Connection;

const OBJ_PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";

/// Actions pushed from the widget into the UI loop.
#[derive(Debug, Clone, Copy)]
pub enum MprisCmd {
    Play,
    Pause,
    PlayPause,
    Stop,
    Next,
    Previous,
    Seek(i64),
    SetPosition(i64),
}

/// The playback picture advertised over D-Bus.
#[derive(Default)]
pub struct MprisState {
    pub status: String,
    pub track_key: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub length_us: i64,
    pub art_url: String,
    pub pos_us: i64,
}

/// One tick's worth of playback truth, fed to [`MprisHandle::apply`].
pub struct Snapshot<'a> {
    pub status: &'a str,
    pub track_key: &'a str,
    pub title: &'a str,
    pub artist: &'a str,
    pub album: &'a str,
    pub length_us: i64,
    pub art_url: &'a str,
    pub pos_us: i64,
}

/// An `OwnedValue` for anything that converts into a D-Bus value
/// (strings, arrays of strings, …). Only exotic types can fail here.
fn ov<T: Into<Value<'static>>>(v: T) -> OwnedValue {
    OwnedValue::try_from(v.into()).expect("basic D-Bus values convert infallibly")
}

/// Reuse a string's allocation when the value is unchanged.
fn set_string(dst: &mut String, value: &str) {
    if dst != value {
        dst.clear();
        dst.push_str(value);
    }
}

/// Everything the D-Bus side shares with the UI loop.
#[derive(Default)]
pub struct MprisHandle {
    state: RwLock<MprisState>,
    conn: OnceLock<Connection>,
}

impl MprisHandle {
    /// Store the live connection once the service has claimed its name.
    pub fn adopt(&self, conn: Connection) {
        let _ = self.conn.set(conn);
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, MprisState> {
        self.state.read().unwrap_or_else(|e| e.into_inner())
    }

    pub fn status(&self) -> String {
        self.read().status.clone()
    }

    pub fn position(&self) -> i64 {
        self.read().pos_us
    }

    /// The `mpris:metadata` map for the current (or last) track.
    pub fn metadata(&self) -> HashMap<String, OwnedValue> {
        let st = self.read();
        let mut m = HashMap::new();
        let path = ObjectPath::try_from(track_path(&st.track_key)).unwrap_or_default();
        m.insert("mpris:trackid".to_string(), OwnedValue::from(path));
        m.insert("mpris:length".to_string(), st.length_us.into());
        if !st.art_url.is_empty() {
            m.insert("mpris:artUrl".to_string(), ov(st.art_url.clone()));
        }
        m.insert("xesam:title".to_string(), ov(st.title.clone()));
        if !st.artist.is_empty() {
            m.insert(
                "xesam:artist".to_string(),
                ov(Array::from(vec![st.artist.clone()])),
            );
        }
        if !st.album.is_empty() {
            m.insert("xesam:album".to_string(), ov(st.album.clone()));
        }
        m
    }

    /// Mirror this tick's state; push signals for whatever changed.
    pub fn apply(&self, s: Snapshot<'_>) {
        let (track_changed, status_changed, seek_pos) = {
            let mut st = self.state.write().unwrap_or_else(|e| e.into_inner());
            let track_changed = st.track_key != s.track_key;
            let status_changed = st.status != s.status;
            // A discontinuity beyond normal ticking means a seek (or a
            // track cut, handled by the id change below).
            let jumped = !track_changed
                && (s.pos_us + 1_500_000 < st.pos_us || s.pos_us > st.pos_us + 2_000_000);
            // These fields are usually stable, so only rewrite the strings
            // when they actually change. The D-Bus mirror is fed at 10Hz;
            // unconditionally reallocating six strings every tick is pure
            // churn while a song is playing.
            set_string(&mut st.status, s.status);
            set_string(&mut st.track_key, s.track_key);
            set_string(&mut st.title, s.title);
            set_string(&mut st.artist, s.artist);
            set_string(&mut st.album, s.album);
            st.length_us = s.length_us;
            set_string(&mut st.art_url, s.art_url);
            st.pos_us = s.pos_us;
            let seek_pos = if track_changed || jumped {
                Some(s.pos_us)
            } else {
                None
            };
            (track_changed, status_changed, seek_pos)
        };

        if status_changed || track_changed {
            let mut changed: Vec<(&'static str, OwnedValue)> =
                vec![("PlaybackStatus", ov(self.status()))];
            if track_changed {
                changed.push(("Metadata", self.metadata().into()));
            }
            self.emit_properties(changed);
        }
        if let Some(pos) = seek_pos {
            self.emit_seeked(pos);
        }
    }

    fn emit_properties(&self, changed: Vec<(&'static str, OwnedValue)>) {
        let Some(conn) = self.conn.get().cloned() else {
            return;
        };
        let changed: HashMap<&'static str, OwnedValue> = changed.into_iter().collect();
        tokio::spawn(async move {
            let _ = conn
                .emit_signal(
                    None::<&str>,
                    OBJ_PATH,
                    "org.freedesktop.DBus.Properties",
                    "PropertiesChanged",
                    &(PLAYER_IFACE, changed, Vec::<String>::new()),
                )
                .await;
        });
    }

    fn emit_seeked(&self, pos_us: i64) {
        let Some(conn) = self.conn.get().cloned() else {
            return;
        };
        tokio::spawn(async move {
            let _ = conn
                .emit_signal(None::<&str>, OBJ_PATH, PLAYER_IFACE, "Seeked", &(pos_us,))
                .await;
        });
    }
}

/// A valid D-Bus object path for a track id (ids may contain `-`, which
/// object paths do not allow).
fn track_path(key: &str) -> String {
    let safe: String = key
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect();
    if safe.is_empty() {
        "/ytmtui/track/none".to_string()
    } else {
        format!("/ytmtui/track/{}", safe)
    }
}

struct Root;

#[zbus::interface(name = "org.mpris.MediaPlayer2")]
impl Root {
    async fn raise(&self) {}
    async fn quit(&self) {}

    #[zbus(property)]
    async fn can_quit(&self) -> bool {
        false
    }
    #[zbus(property)]
    async fn can_raise(&self) -> bool {
        false
    }
    #[zbus(property)]
    async fn can_set_fullscreen(&self) -> bool {
        false
    }
    #[zbus(property)]
    async fn has_track_list(&self) -> bool {
        false
    }
    #[zbus(property)]
    async fn identity(&self) -> String {
        "ytmtui".to_string()
    }
    #[zbus(property)]
    async fn desktop_entry(&self) -> String {
        "ytmtui".to_string()
    }
    #[zbus(property)]
    async fn supported_uri_schemes(&self) -> Vec<String> {
        Vec::new()
    }
    #[zbus(property)]
    async fn supported_mime_types(&self) -> Vec<String> {
        Vec::new()
    }
}

struct Player {
    handle: Arc<MprisHandle>,
    tx: UnboundedSender<MprisCmd>,
}

#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl Player {
    async fn next(&self) {
        let _ = self.tx.send(MprisCmd::Next);
    }
    async fn previous(&self) {
        let _ = self.tx.send(MprisCmd::Previous);
    }
    async fn pause(&self) {
        let _ = self.tx.send(MprisCmd::Pause);
    }
    async fn play_pause(&self) {
        let _ = self.tx.send(MprisCmd::PlayPause);
    }
    async fn stop(&self) {
        let _ = self.tx.send(MprisCmd::Stop);
    }
    async fn play(&self) {
        let _ = self.tx.send(MprisCmd::Play);
    }
    async fn seek(&self, offset: i64) {
        let _ = self.tx.send(MprisCmd::Seek(offset));
    }
    async fn set_position(&self, _track_id: OwnedObjectPath, position: i64) {
        let _ = self.tx.send(MprisCmd::SetPosition(position));
    }

    #[zbus(property)]
    async fn playback_status(&self) -> String {
        self.handle.status()
    }
    #[zbus(property)]
    async fn metadata(&self) -> HashMap<String, OwnedValue> {
        self.handle.metadata()
    }
    #[zbus(property)]
    async fn position(&self) -> i64 {
        self.handle.position()
    }
    #[zbus(property)]
    async fn rate(&self) -> f64 {
        1.0
    }
    #[zbus(property)]
    async fn minimum_rate(&self) -> f64 {
        1.0
    }
    #[zbus(property)]
    async fn maximum_rate(&self) -> f64 {
        1.0
    }
    #[zbus(property)]
    async fn volume(&self) -> f64 {
        1.0
    }
    #[zbus(property)]
    async fn can_control(&self) -> bool {
        true
    }
    #[zbus(property)]
    async fn can_seek(&self) -> bool {
        true
    }
    #[zbus(property)]
    async fn can_pause(&self) -> bool {
        true
    }
    #[zbus(property)]
    async fn can_play(&self) -> bool {
        true
    }
    #[zbus(property)]
    async fn can_go_next(&self) -> bool {
        true
    }
    #[zbus(property)]
    async fn can_go_previous(&self) -> bool {
        true
    }
}

/// Claim `org.mpris.MediaPlayer2.ytmtui` on the session bus and serve
/// both interfaces from one object. Fails only when there is no bus.
pub async fn serve(
    handle: Arc<MprisHandle>,
    tx: UnboundedSender<MprisCmd>,
) -> zbus::Result<Connection> {
    zbus::connection::Builder::session()?
        .name("org.mpris.MediaPlayer2.ytmtui")?
        .serve_at(OBJ_PATH, Root)?
        .serve_at(OBJ_PATH, Player { handle, tx })?
        .build()
        .await
}
