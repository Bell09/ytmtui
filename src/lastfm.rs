//! Last.fm authentication and scrobbling.
//!
//! Credentials live in `lastfm.json` beside `auth.json` with owner-only
//! permissions. The module never prints or logs the API secret, password
//! hash or session key — [`Credentials`]' `Debug` output is redacted.
//!
//! Scrobbling policy is pure ([`Scrobbler`]), so timing, deduplication and
//! disabled-state behaviour are testable without the network. The app turns
//! the returned [`Action`]s into fire-and-forget tasks; playback never waits
//! on Last.fm.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::models::Track;

const API_URL: &str = "https://ws.audioscrobbler.com/2.0/";
/// Last.fm ignores tracks shorter than this, so there is nothing to submit.
const MIN_TRACK_SECS: u64 = 30;
/// A scrobble is due after half the track, but never later than four minutes.
const MAX_THRESHOLD_SECS: u64 = 240;
/// A stalled UI tick must not count as listening time.
const MAX_TICK: Duration = Duration::from_secs(5);
/// Two retries cover a flaky connection without keeping tasks alive for long.
const RETRY_DELAYS: [Duration; 2] = [Duration::from_secs(1), Duration::from_secs(3)];

fn default_enabled() -> bool {
    true
}

// ---------------------------------------------------------------------------
// Stored credentials
// ---------------------------------------------------------------------------

/// The credentials needed to sign Last.fm requests. A password hash is
/// deliberately absent: it is exchanged for a session key during setup and
/// never written to disk.
#[derive(Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub api_key: String,
    pub api_secret: String,
    #[serde(default)]
    pub username: String,
    pub session_key: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("api_key", &"<redacted>")
            .field("api_secret", &"<redacted>")
            .field("username", &self.username)
            .field("session_key", &"<redacted>")
            .field("enabled", &self.enabled)
            .finish()
    }
}

impl Credentials {
    fn complete(&self) -> bool {
        !self.api_key.trim().is_empty()
            && !self.api_secret.trim().is_empty()
            && !self.session_key.trim().is_empty()
    }
}

pub fn credentials_path() -> PathBuf {
    crate::auth::config_dir().join("lastfm.json")
}

pub fn load() -> Option<Credentials> {
    let text = fs::read_to_string(credentials_path()).ok()?;
    let creds: Credentials = serde_json::from_str(&text).ok()?;
    creds.complete().then_some(creds)
}

pub fn save(creds: &Credentials) -> Result<(), String> {
    let payload =
        serde_json::to_string_pretty(creds).map_err(|e| format!("encode credentials: {e}"))?;
    let path = credentials_path();
    // These are credentials: create the file owner-only from the start, then
    // re-assert the mode in case an older file exists with looser bits.
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| format!("write {}: {e}", path.display()))?;
        file.write_all(payload.as_bytes())
            .map_err(|e| format!("write {}: {e}", path.display()))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("restrict {}: {e}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        fs::write(&path, payload).map_err(|e| format!("write {}: {e}", path.display()))?;
    }
    Ok(())
}

pub fn clear() -> Result<(), String> {
    match fs::remove_file(credentials_path()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("remove Last.fm credentials: {e}")),
    }
}

pub fn set_enabled(enabled: bool) -> Result<(), String> {
    let mut creds = load().ok_or("Last.fm is not configured")?;
    creds.enabled = enabled;
    save(&creds)
}

/// One-line connection status for `ytmtui status`. Never includes secrets.
pub fn status_line() -> String {
    match load() {
        None => "Last.fm: not configured — run `ytmtui setup lastfm`".to_string(),
        Some(creds) => {
            let name = if creds.username.trim().is_empty() {
                "connected"
            } else {
                creds.username.trim()
            };
            format!(
                "Last.fm: connected as {} · scrobbling {}",
                name,
                if creds.enabled { "on" } else { "off" }
            )
        }
    }
}

// ---------------------------------------------------------------------------
// Input validation and signing
// ---------------------------------------------------------------------------

/// Validated setup input. A session key wins over a password hash when both
/// are supplied, as Last.fm recommends.
pub struct AuthInput {
    pub api_key: String,
    pub api_secret: String,
    pub username: String,
    pub session_key: Option<String>,
    pub password_hash: Option<String>,
}

pub fn prepare_auth_inputs(
    api_key: &str,
    api_secret: &str,
    username: &str,
    password_hash: Option<&str>,
    session_key: Option<&str>,
) -> Result<AuthInput, String> {
    let api_key = required_token(api_key, "API key")?;
    let api_secret = required_token(api_secret, "shared secret")?;
    let username = username.trim().to_string();

    if let Some(key) = session_key.map(str::trim).filter(|key| !key.is_empty()) {
        if key.len() < 8 || key.len() > 256 || !key.chars().all(|c| !c.is_whitespace() && !c.is_control()) {
            return Err("the session key is not valid".to_string());
        }
        return Ok(AuthInput {
            api_key,
            api_secret,
            username,
            session_key: Some(key.to_string()),
            password_hash: None,
        });
    }

    let hash = password_hash
        .map(str::trim)
        .filter(|hash| !hash.is_empty())
        .ok_or("a session key or MD5 password hash is required")?;
    if hash.len() != 32 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("the MD5 password hash must be 32 hexadecimal characters".to_string());
    }
    if username.is_empty() {
        return Err("a Last.fm username is required".to_string());
    }
    Ok(AuthInput {
        api_key,
        api_secret,
        username,
        session_key: None,
        password_hash: Some(hash.to_ascii_lowercase()),
    })
}

fn required_token(value: &str, what: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("{what} is required"));
    }
    if value.len() > 256 || !value.chars().all(|c| !c.is_whitespace() && !c.is_control()) {
        return Err(format!("{what} is not valid"));
    }
    Ok(value.to_string())
}

fn md5_hex(input: &str) -> String {
    use md5::{Digest, Md5};
    let digest = Md5::digest(input.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// `authToken` for `auth.getMobileSession`: MD5 of the username followed by
/// the MD5 password hash the user supplied.
pub fn auth_token(username: &str, password_hash: &str) -> String {
    md5_hex(&format!(
        "{username}{}",
        password_hash.trim().to_ascii_lowercase()
    ))
}

/// Last.fm's request signature: parameters sorted by name, concatenated as
/// `namevalue`, then the shared secret, hashed with MD5. `format`, `callback`
/// and `api_sig` itself are excluded.
fn signature(params: &BTreeMap<String, String>, api_secret: &str) -> String {
    let mut input = String::new();
    for (key, value) in params {
        if matches!(key.as_str(), "api_sig" | "format" | "callback") {
            continue;
        }
        input.push_str(key);
        input.push_str(value);
    }
    input.push_str(api_secret);
    md5_hex(&input)
}

/// Transient Last.fm errors worth another attempt: operation failed, service
/// offline, temporary unavailability and rate limiting.
fn retryable_code(code: u64) -> bool {
    matches!(code, 8 | 11 | 16 | 29)
}

// ---------------------------------------------------------------------------
// API client
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    api_key: String,
    api_secret: String,
    session_key: String,
    retry_delays: Vec<Duration>,
}

impl Client {
    fn build_http() -> reqwest::Client {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap_or_default()
    }

    fn new(creds: &Credentials) -> Option<Self> {
        if !creds.complete() {
            return None;
        }
        Some(Self {
            http: Self::build_http(),
            base: API_URL.to_string(),
            api_key: creds.api_key.clone(),
            api_secret: creds.api_secret.clone(),
            session_key: creds.session_key.clone(),
            retry_delays: RETRY_DELAYS.to_vec(),
        })
    }

    /// Build a client for the scrobbling path. `None` when Last.fm has not
    /// been configured or scrobbling is disabled, so playback costs nothing.
    pub fn from_config() -> Option<Self> {
        let creds = load()?;
        if !creds.enabled {
            return None;
        }
        Self::new(&creds)
    }

    fn for_auth(base: &str, api_key: &str, api_secret: &str, session_key: &str) -> Self {
        Self {
            http: Self::build_http(),
            base: base.to_string(),
            api_key: api_key.to_string(),
            api_secret: api_secret.to_string(),
            session_key: session_key.to_string(),
            retry_delays: RETRY_DELAYS.to_vec(),
        }
    }

    async fn call(
        &self,
        method: &str,
        params: BTreeMap<String, String>,
        signed: bool,
        authenticated: bool,
    ) -> Result<Value, String> {
        let mut attempt = 0usize;
        loop {
            match self
                .call_once(method, &params, signed, authenticated)
                .await
            {
                Ok(value) => {
                    if let Some((code, message)) = api_error(&value) {
                        if !retryable_code(code) || attempt >= self.retry_delays.len() {
                            return Err(format!("Last.fm error {code}: {message}"));
                        }
                    } else {
                        return Ok(value);
                    }
                }
                Err(error) => {
                    if attempt >= self.retry_delays.len() {
                        return Err(error);
                    }
                }
            }
            let delay = self.retry_delays[attempt];
            attempt += 1;
            tokio::time::sleep(delay).await;
        }
    }

    async fn call_once(
        &self,
        method: &str,
        params: &BTreeMap<String, String>,
        signed: bool,
        authenticated: bool,
    ) -> Result<Value, String> {
        let mut form = params.clone();
        form.insert("method".to_string(), method.to_string());
        form.insert("api_key".to_string(), self.api_key.clone());
        if authenticated {
            form.insert("sk".to_string(), self.session_key.clone());
        }
        if signed {
            let sig = signature(&form, &self.api_secret);
            form.insert("api_sig".to_string(), sig);
        }
        // `format` is excluded from the signature and therefore added last.
        form.insert("format".to_string(), "json".to_string());

        let resp = self
            .http
            .post(&self.base)
            .form(&form)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        resp.json::<Value>()
            .await
            .map_err(|e| format!("invalid Last.fm response: {e}"))
    }

    /// Tell Last.fm what is playing now. Failures are silent to the caller's
    /// playback path; the client retries transient errors a couple of times.
    pub async fn update_now_playing(
        &self,
        track: &Track,
        duration_secs: Option<u64>,
    ) -> Result<(), String> {
        self.call("track.updateNowPlaying", track_params(track, duration_secs), true, true)
            .await
            .map(|_| ())
    }

    /// Submit one scrobble. The caller marks the track as submitted before
    /// spawning this, so retries can never produce a duplicate.
    pub async fn scrobble(
        &self,
        track: &Track,
        timestamp: u64,
        duration_secs: Option<u64>,
    ) -> Result<(), String> {
        let mut params = track_params(track, duration_secs);
        params.insert("timestamp".to_string(), timestamp.to_string());
        self.call("track.scrobble", params, true, true)
            .await
            .map(|_| ())
    }
}

fn track_params(track: &Track, duration_secs: Option<u64>) -> BTreeMap<String, String> {
    let mut params = BTreeMap::new();
    params.insert("artist".to_string(), track.artist.trim().to_string());
    params.insert("track".to_string(), track.title.trim().to_string());
    if let Some(album) = track.album.as_deref().map(str::trim).filter(|album| !album.is_empty()) {
        params.insert("album".to_string(), album.to_string());
    }
    if let Some(secs) = duration_secs.filter(|secs| *secs > 0) {
        params.insert("duration".to_string(), secs.to_string());
    }
    params
}

fn api_error(value: &Value) -> Option<(u64, String)> {
    let code = value.get("error")?.as_u64()?;
    let message = value
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("request failed")
        .to_string();
    Some((code, message))
}

// ---------------------------------------------------------------------------
// Authentication
// ---------------------------------------------------------------------------

pub struct Authenticated {
    pub username: String,
    pub session_key: String,
}

/// Validate the supplied credentials against Last.fm and return a session.
/// A supplied session key is verified with `user.getInfo`; otherwise the
/// password hash is exchanged for one via `auth.getMobileSession`.
pub async fn authenticate(input: AuthInput) -> Result<Authenticated, String> {
    authenticate_at(API_URL, input).await
}

async fn authenticate_at(base: &str, input: AuthInput) -> Result<Authenticated, String> {
    if let Some(key) = input.session_key {
        let client = Client::for_auth(base, &input.api_key, &input.api_secret, &key);
        let mut params = BTreeMap::new();
        if !input.username.trim().is_empty() {
            params.insert("user".to_string(), input.username.trim().to_string());
        }
        let value = client.call("user.getInfo", params, true, true).await?;
        let name = value
            .pointer("/user/name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .ok_or("Last.fm did not return a user for this session key")?;
        return Ok(Authenticated {
            username: name.to_string(),
            session_key: key,
        });
    }

    let hash = input
        .password_hash
        .ok_or("a session key or MD5 password hash is required")?;
    let client = Client::for_auth(base, &input.api_key, &input.api_secret, "");
    let mut params = BTreeMap::new();
    params.insert("username".to_string(), input.username.clone());
    params.insert(
        "authToken".to_string(),
        auth_token(&input.username, &hash),
    );
    let value = client
        .call("auth.getMobileSession", params, true, false)
        .await?;
    let key = value
        .pointer("/session/key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .ok_or("Last.fm did not return a session key")?;
    let name = value
        .pointer("/session/name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(input.username.trim());
    Ok(Authenticated {
        username: name.to_string(),
        session_key: key.to_string(),
    })
}

// ---------------------------------------------------------------------------
// Scrobbling policy
// ---------------------------------------------------------------------------

/// A submission the app should run on its background runtime.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    NowPlaying {
        track: Track,
        duration_secs: Option<u64>,
    },
    Scrobble {
        track: Track,
        duration_secs: Option<u64>,
        timestamp: u64,
    },
}

struct Current {
    track: Track,
    duration_secs: Option<u64>,
    listened: Duration,
    now_playing_sent: bool,
    scrobbled: bool,
    /// Unix time when the track actually began playing; used as the
    /// scrobble timestamp, which Last.fm defines as the play start.
    started_at: u64,
}

/// Tracks listening time and decides when Last.fm should hear from us.
#[derive(Default)]
pub struct Scrobbler {
    configured: bool,
    enabled: bool,
    username: Option<String>,
    current: Option<Current>,
    last_tick: Option<Instant>,
}

impl Scrobbler {
    pub fn new(configured: bool, enabled: bool, username: Option<String>) -> Self {
        Self {
            configured,
            enabled,
            username,
            current: None,
            last_tick: None,
        }
    }

    /// Load the persisted connection state. Reading a tiny JSON file at
    /// startup keeps `App::new` synchronous and allocation-free after this.
    pub fn from_config() -> Self {
        match load() {
            Some(creds) => Self::new(true, creds.enabled, Some(creds.username)),
            None => Self::default(),
        }
    }

    pub fn configured(&self) -> bool {
        self.configured
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// The name shown in the header, sanitised for terminal output.
    pub fn badge(&self) -> Option<String> {
        let name = self.username.as_deref()?.trim();
        if name.is_empty() {
            return None;
        }
        let cleaned: String = name
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        Some(cleaned.split_whitespace().collect::<Vec<_>>().join(" "))
    }

    /// Start a fresh playback instance for `track`. Retries of the same track
    /// deliberately do not call this, so listening time is preserved.
    pub fn track_started(&mut self, track: &Track, duration_secs: Option<u64>) {
        self.last_tick = None;
        if !self.configured
            || !self.enabled
            || track.artist.trim().is_empty()
            || track.title.trim().is_empty()
        {
            self.current = None;
            return;
        }
        self.current = Some(Current {
            track: track.clone(),
            duration_secs,
            listened: Duration::ZERO,
            now_playing_sent: false,
            scrobbled: false,
            started_at: unix_now(),
        });
    }

    /// Called once per UI tick. Returns at most one submission: Now Playing on
    /// the first audible tick, then a scrobble when the listening threshold is
    /// crossed. The flag flips before the request runs, so a slow or failing
    /// network can never submit twice.
    pub fn tick(
        &mut self,
        now: Instant,
        playing: bool,
        duration_secs: Option<u64>,
    ) -> Option<Action> {
        if !self.configured || !self.enabled {
            return None;
        }
        let current = self.current.as_mut()?;
        if duration_secs.is_some() {
            current.duration_secs = duration_secs;
        }
        if !playing {
            self.last_tick = None;
            return None;
        }
        if let Some(previous) = self.last_tick {
            current.listened += now.saturating_duration_since(previous).min(MAX_TICK);
        }
        self.last_tick = Some(now);

        if !current.now_playing_sent {
            current.now_playing_sent = true;
            // The first audible tick is the closest point to the real start
            // of playback, so it anchors the scrobble timestamp.
            current.started_at = unix_now();
            return Some(Action::NowPlaying {
                track: current.track.clone(),
                duration_secs: current.duration_secs,
            });
        }
        if current.scrobbled {
            return None;
        }
        let threshold = scrobble_threshold(current.duration_secs)?;
        if current.listened < threshold {
            return None;
        }
        current.scrobbled = true;
        Some(Action::Scrobble {
            track: current.track.clone(),
            duration_secs: current.duration_secs,
            timestamp: current.started_at,
        })
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled != enabled {
            self.enabled = enabled;
            // Restart the window so re-enabling mid-track does not count
            // listening time from before the pause in scrobbling.
            self.current = None;
            self.last_tick = None;
        }
    }
}

/// Last.fm's rule: tracks must be at least 30 seconds long and are scrobbled
/// after half their duration or four minutes, whichever comes first. With no
/// known duration, use the four-minute cap.
pub fn scrobble_threshold(duration_secs: Option<u64>) -> Option<Duration> {
    match duration_secs {
        Some(secs) if secs < MIN_TRACK_SECS => None,
        Some(secs) => Some(Duration::from_secs((secs / 2).min(MAX_THRESHOLD_SECS))),
        None => Some(Duration::from_secs(MAX_THRESHOLD_SECS)),
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: &str, artist: &str, title: &str) -> Track {
        Track {
            id: id.to_string(),
            title: title.to_string(),
            artist: artist.to_string(),
            album: Some("Album".to_string()),
            duration: Some("3:00".to_string()),
            thumbnail_url: None,
            like: None,
            downloaded: false,
            set_video_id: None,
        }
    }

    fn enabled_scrobbler() -> Scrobbler {
        Scrobbler::new(true, true, Some("tester".to_string()))
    }

    #[test]
    fn signature_sorts_parameters_and_excludes_unsigned_fields() {
        let mut params = BTreeMap::new();
        params.insert("api_key".to_string(), "abc123".to_string());
        params.insert("artist".to_string(), "Artist".to_string());
        params.insert("method".to_string(), "track.scrobble".to_string());
        params.insert("sk".to_string(), "sessionkey".to_string());
        params.insert("timestamp".to_string(), "1700000000".to_string());
        params.insert("track".to_string(), "Title".to_string());
        assert_eq!(
            signature(&params, "s3cret"),
            "8c5aa272d407ee6d76fb59d5d144f3c8"
        );

        params.insert("format".to_string(), "json".to_string());
        params.insert("callback".to_string(), "bogus".to_string());
        params.insert("api_sig".to_string(), "bogus".to_string());
        assert_eq!(
            signature(&params, "s3cret"),
            "8c5aa272d407ee6d76fb59d5d144f3c8"
        );
    }

    #[test]
    fn auth_token_matches_the_mobile_session_flow() {
        // MD5("password") = 5f4dcc3b5aa765d61d8327deb882cf99
        assert_eq!(
            auth_token("user", "5F4DCC3B5AA765D61D8327DEB882CF99"),
            "f8030584958fba84e562bbaa4bcf204e"
        );
    }

    #[test]
    fn a_session_key_is_preferred_over_a_password_hash() {
        let input = prepare_auth_inputs(
            "key",
            "secret",
            "tester",
            Some("5f4dcc3b5aa765d61d8327deb882cf99"),
            Some("0123456789abcdef0123456789abcdef"),
        )
        .expect("valid input");
        assert_eq!(
            input.session_key.as_deref(),
            Some("0123456789abcdef0123456789abcdef")
        );
        assert!(input.password_hash.is_none());
    }

    #[test]
    fn invalid_credentials_are_rejected_before_any_request() {
        assert!(prepare_auth_inputs("", "secret", "tester", None, None).is_err());
        assert!(prepare_auth_inputs("key", "", "tester", None, None).is_err());
        assert!(prepare_auth_inputs("key", "secret", "tester", None, None).is_err());
        assert!(
            prepare_auth_inputs("key", "secret", "tester", Some("not-a-hash"), None).is_err()
        );
        assert!(prepare_auth_inputs(
            "key",
            "secret",
            "",
            Some("5f4dcc3b5aa765d61d8327deb882cf99"),
            None
        )
        .is_err());
        assert!(prepare_auth_inputs("key", "secret", "tester", None, Some("has space")).is_err());
    }

    #[test]
    fn credentials_debug_output_is_redacted() {
        let creds = Credentials {
            api_key: "visible-key".to_string(),
            api_secret: "super-secret".to_string(),
            username: "tester".to_string(),
            session_key: "session-secret".to_string(),
            enabled: true,
        };
        let debug = format!("{creds:?}");
        assert!(!debug.contains("visible-key"));
        assert!(!debug.contains("super-secret"));
        assert!(!debug.contains("session-secret"));
        assert!(debug.contains("tester"));
    }

    #[test]
    fn credentials_default_to_enabled_when_the_field_is_missing() {
        let creds: Credentials = serde_json::from_str(
            r#"{"api_key":"k","api_secret":"s","username":"u","session_key":"x"}"#,
        )
        .expect("parse");
        assert!(creds.enabled);
    }

    #[test]
    fn thresholds_follow_lastfm_rules() {
        assert_eq!(scrobble_threshold(Some(29)), None);
        assert_eq!(scrobble_threshold(Some(30)), Some(Duration::from_secs(15)));
        assert_eq!(scrobble_threshold(Some(180)), Some(Duration::from_secs(90)));
        assert_eq!(scrobble_threshold(Some(600)), Some(Duration::from_secs(240)));
        assert_eq!(scrobble_threshold(None), Some(Duration::from_secs(240)));
    }

    #[test]
    fn now_playing_is_sent_once_when_playback_starts() {
        let mut scrobbler = enabled_scrobbler();
        let track = track("1", "Artist", "Title");
        scrobbler.track_started(&track, Some(180));
        let start = Instant::now();

        assert!(matches!(
            scrobbler.tick(start, true, Some(180)),
            Some(Action::NowPlaying { .. })
        ));
        assert!(scrobbler
            .tick(start + Duration::from_secs(1), true, Some(180))
            .is_none());
    }

    #[test]
    fn a_track_scrobbles_once_after_half_its_duration() {
        let mut scrobbler = enabled_scrobbler();
        let track = track("1", "Artist", "Title");
        scrobbler.track_started(&track, Some(100));
        let start = Instant::now();

        let mut scrobbles = 0;
        for second in 0..=100 {
            if let Some(Action::Scrobble { timestamp, .. }) =
                scrobbler.tick(start + Duration::from_secs(second), true, Some(100))
            {
                assert!(timestamp > 0);
                scrobbles += 1;
            }
        }
        assert_eq!(scrobbles, 1);
        // Further ticks cannot produce a duplicate.
        assert!(scrobbler
            .tick(start + Duration::from_secs(101), true, Some(100))
            .is_none());
    }

    #[test]
    fn paused_time_does_not_count_toward_the_threshold() {
        let mut scrobbler = enabled_scrobbler();
        let track = track("1", "Artist", "Title");
        scrobbler.track_started(&track, Some(100));
        let start = Instant::now();

        // 10 seconds of listening, then a pause.
        for second in 0..=10 {
            scrobbler.tick(start + Duration::from_secs(second), true, Some(100));
        }
        let paused_at = start + Duration::from_secs(11);
        assert!(scrobbler.tick(paused_at, false, Some(100)).is_none());
        // The clock jumps while paused: that time must not be credited.
        assert!(scrobbler
            .tick(paused_at + Duration::from_secs(300), false, Some(100))
            .is_none());

        let resumed = paused_at + Duration::from_secs(301);
        assert!(scrobbler.tick(resumed, true, Some(100)).is_none());
        // 39 more seconds of real playback reaches the 50-second threshold.
        let mut scrobbled = false;
        for second in 1..=40 {
            if let Some(Action::Scrobble { .. }) =
                scrobbler.tick(resumed + Duration::from_secs(second), true, Some(100))
            {
                scrobbled = true;
            }
        }
        assert!(scrobbled);
    }

    #[test]
    fn tracks_shorter_than_thirty_seconds_are_never_scrobbled() {
        let mut scrobbler = enabled_scrobbler();
        let track = track("1", "Artist", "Title");
        scrobbler.track_started(&track, Some(29));
        let start = Instant::now();
        let mut actions = Vec::new();
        for second in 0..=120 {
            if let Some(action) = scrobbler.tick(start + Duration::from_secs(second), true, Some(29)) {
                actions.push(action);
            }
        }
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], Action::NowPlaying { .. }));
    }

    #[test]
    fn unknown_duration_uses_the_four_minute_cap() {
        let mut scrobbler = enabled_scrobbler();
        let track = track("1", "Artist", "Title");
        scrobbler.track_started(&track, None);
        let start = Instant::now();
        let mut scrobbled_at = None;
        for second in 0..=241 {
            if matches!(
                scrobbler.tick(start + Duration::from_secs(second), true, None),
                Some(Action::Scrobble { .. })
            ) {
                scrobbled_at = Some(second);
            }
        }
        assert_eq!(scrobbled_at, Some(240));
    }

    #[test]
    fn disabling_scrobbling_stops_all_submissions() {
        let mut scrobbler = enabled_scrobbler();
        let track = track("1", "Artist", "Title");
        scrobbler.track_started(&track, Some(100));
        let start = Instant::now();
        scrobbler.set_enabled(false);
        for second in 0..=100 {
            assert!(scrobbler
                .tick(start + Duration::from_secs(second), true, Some(100))
                .is_none());
        }
        scrobbler.track_started(&track, Some(100));
        assert!(scrobbler.tick(start, true, Some(100)).is_none());
    }

    #[test]
    fn unconfigured_or_incomplete_tracks_are_not_submitted() {
        let mut scrobbler = Scrobbler::default();
        let song = track("1", "Artist", "Title");
        scrobbler.track_started(&song, Some(100));
        assert!(scrobbler.tick(Instant::now(), true, Some(100)).is_none());

        let mut scrobbler = enabled_scrobbler();
        let missing_artist = track("1", "", "Title");
        scrobbler.track_started(&missing_artist, Some(100));
        assert!(scrobbler.tick(Instant::now(), true, Some(100)).is_none());
    }

    #[test]
    fn the_header_badge_is_sanitised_and_absent_without_a_user() {
        let scrobbler = Scrobbler::new(true, true, Some("  a\u{0007}b  ".to_string()));
        assert_eq!(scrobbler.badge().as_deref(), Some("a b"));
        assert!(Scrobbler::new(true, true, None).badge().is_none());
    }

    // -------------------------------------------------------------------
    // API behaviour against a local HTTP server
    // -------------------------------------------------------------------

    async fn serve(responses: Vec<&'static str>) -> (String, tokio::task::JoinHandle<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let handle = tokio::spawn(async move {
            let mut captured = String::new();
            for body in responses {
                let (mut socket, _) = listener.accept().await.expect("accept");
                let mut buf = vec![0u8; 8192];
                let read = socket.read(&mut buf).await.expect("read");
                captured.push_str(&String::from_utf8_lossy(&buf[..read]));
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                socket.write_all(response.as_bytes()).await.expect("write");
                let _ = socket.shutdown().await;
            }
            captured
        });
        (format!("http://{addr}/"), handle)
    }

    #[tokio::test]
    async fn a_session_key_is_validated_and_its_username_read() {
        let (base, server) = serve(vec![r#"{"user":{"name":"tester"}}"#]).await;
        let input = prepare_auth_inputs(
            "key",
            "secret",
            "old-name",
            None,
            Some("0123456789abcdef0123456789abcdef"),
        )
        .expect("input");
        let auth = authenticate_at(&base, input).await.expect("authenticate");
        assert_eq!(auth.username, "tester");
        assert_eq!(auth.session_key, "0123456789abcdef0123456789abcdef");
        let request = server.await.expect("server");
        assert!(request.contains("method=user.getInfo"));
        assert!(request.contains("api_sig="));
        // The shared secret is only ever part of the signature hash.
        assert!(!request.contains("secret"));
    }

    #[tokio::test]
    async fn an_invalid_session_key_reports_the_api_error_without_secrets() {
        let (base, server) =
            serve(vec![r#"{"error":9,"message":"Invalid session key"}"#]).await;
        let input = prepare_auth_inputs(
            "key",
            "secret",
            "tester",
            None,
            Some("0123456789abcdef0123456789abcdef"),
        )
        .expect("input");
        let error = authenticate_at(&base, input)
            .await
            .err()
            .expect("rejected");
        assert!(error.contains("Invalid session key"));
        assert!(!error.contains("0123456789abcdef0123456789abcdef"));
        let _ = server.await;
    }

    #[tokio::test]
    async fn a_password_hash_is_exchanged_for_a_session_key() {
        let (base, server) = serve(vec![
            r#"{"session":{"name":"tester","key":"fedcba9876543210fedcba9876543210"}}"#,
        ])
        .await;
        let input = prepare_auth_inputs(
            "key",
            "secret",
            "tester",
            Some("5f4dcc3b5aa765d61d8327deb882cf99"),
            None,
        )
        .expect("input");
        let auth = authenticate_at(&base, input).await.expect("authenticate");
        assert_eq!(auth.username, "tester");
        assert_eq!(auth.session_key, "fedcba9876543210fedcba9876543210");
        let request = server.await.expect("server");
        assert!(request.contains("method=auth.getMobileSession"));
        assert!(request.contains("authToken=ea0f2ca19ae0981c83200e98bdaebf0d"));
        assert!(!request.contains("5f4dcc3b5aa765d61d8327deb882cf99"));
    }

    #[tokio::test]
    async fn transient_failures_are_retried() {
        let (base, server) = serve(vec![
            r#"{"error":11,"message":"Service Offline"}"#,
            r#"{"error":11,"message":"Service Offline"}"#,
            r#"{"user":{"name":"tester"}}"#,
        ])
        .await;
        let mut client = Client::for_auth(&base, "key", "secret", "sk");
        client.retry_delays = vec![Duration::ZERO, Duration::ZERO];
        let value = client
            .call("user.getInfo", BTreeMap::new(), true, true)
            .await
            .expect("eventually succeeds");
        assert_eq!(value["user"]["name"], "tester");
        let _ = tokio::time::timeout(Duration::from_secs(5), server).await;
    }

    #[tokio::test]
    async fn exhausted_retries_return_an_error_instead_of_hanging() {
        let (base, server) = serve(vec![
            r#"{"error":16,"message":"Temporarily unavailable"}"#,
            r#"{"error":16,"message":"Temporarily unavailable"}"#,
            r#"{"error":16,"message":"Temporarily unavailable"}"#,
        ])
        .await;
        let mut client = Client::for_auth(&base, "key", "secret", "sk");
        client.retry_delays = vec![Duration::ZERO, Duration::ZERO];
        let error = client
            .call("user.getInfo", BTreeMap::new(), true, true)
            .await
            .expect_err("gives up");
        assert!(error.contains("error 16"));
        let _ = tokio::time::timeout(Duration::from_secs(5), server).await;
    }
}
