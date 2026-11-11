use std::fs;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::Once;
use std::time::{SystemTime, UNIX_EPOCH};

use directories::ProjectDirs;
use serde_json::{json, Map, Value};
use sha1::{Digest, Sha1};

use crate::browser;

const USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
const ORIGIN: &str = "https://music.youtube.com";
const CLIENT_VERSION: &str = "1.20240101.01.00";
const API_KEY: &str = "AIzaSyAO_FJ2SlqU8Q4STEHLGCilw_Y9_11qcW8";

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

pub fn config_dir() -> PathBuf {
    static MIGRATE: Once = Once::new();
    MIGRATE.call_once(migrate_legacy);
    if let Some(proj_dirs) = ProjectDirs::from("", "", "ytmtui") {
        let dir = proj_dirs.config_dir().to_path_buf();
        let _ = fs::create_dir_all(&dir);
        dir
    } else {
        let dir = PathBuf::from(".ytmtui");
        let _ = fs::create_dir_all(&dir);
        dir
    }
}

/// The app was called `ytm-tui` before; move its config directory once so the
/// rename keeps the user's login and download list.
fn migrate_legacy() {
    let Some(dirs) = ProjectDirs::from("", "", "ytmtui") else {
        return;
    };
    let new = dirs.config_dir();
    let old = new.with_file_name("ytm-tui");
    if new.exists() || !old.is_dir() {
        return;
    }
    if let Some(parent) = new.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::rename(old, new);
}

pub fn auth_path() -> PathBuf {
    config_dir().join("auth.json")
}

// ---------------------------------------------------------------------------
// Header helpers
// ---------------------------------------------------------------------------

fn default_headers() -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("accept".into(), json!("*/*"));
    m.insert("accept-encoding".into(), json!("gzip, deflate"));
    m.insert("accept-language".into(), json!("en-US,en;q=0.9"));
    m.insert("content-type".into(), json!("application/json"));
    m.insert("user-agent".into(), json!(USER_AGENT));
    m.insert("x-goog-authuser".into(), json!("0"));
    m.insert("x-goog-api-format-version".into(), json!("2"));
    m.insert("x-youtube-client-name".into(), json!("67"));
    m.insert("x-youtube-client-version".into(), json!(CLIENT_VERSION));
    m.insert("origin".into(), json!(ORIGIN));
    m.insert("x-origin".into(), json!(ORIGIN));
    m.insert("referer".into(), json!("https://music.youtube.com/"));
    m
}

fn sapisid_from_cookie(cookie: &str) -> Option<String> {
    for key in ["__Secure-3PAPISID", "SAPISID", "__Secure-1PAPISID"] {
        for part in cookie.split(';') {
            if let Some((k, v)) = part.trim().split_once('=') {
                if k.trim() == key && !v.trim().is_empty() {
                    return Some(v.trim().to_string());
                }
            }
        }
    }
    None
}

/// `SAPISIDHASH {ts}_{sha1("{ts} {SAPISID} https://music.youtube.com")}`
fn sapisidhash(sapisid: &str) -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut hasher = Sha1::new();
    hasher.update(format!("{} {} {}", ts, sapisid, ORIGIN).as_bytes());
    let digest = hasher.finalize();
    let hex: String = digest.iter().map(|b| format!("{:02x}", b)).collect();
    format!("SAPISIDHASH {}_{}", ts, hex)
}

fn build_browser_headers(cookie: &str) -> Map<String, Value> {
    let mut h = default_headers();
    h.insert("cookie".into(), json!(cookie));
    if let Some(sapisid) = sapisid_from_cookie(cookie) {
        h.insert("authorization".into(), json!(sapisidhash(&sapisid)));
    }
    h
}

// ---------------------------------------------------------------------------
// Header normalization
// ---------------------------------------------------------------------------

const PSEUDO_HEADERS: [&str; 5] = [":authority", ":method", ":path", ":scheme", ":status"];
const DECODED_OPEN: &str = "Decoded:";
const DECODED_CLOSE: &str = "}";

fn strip_decoded_blocks(lines: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim() != DECODED_OPEN {
            out.push(lines[i].clone());
            i += 1;
            continue;
        }
        match (i + 1..lines.len()).find(|&j| lines[j].trim() == DECODED_CLOSE) {
            Some(j) => i = j + 1,
            None => {
                out.push(lines[i].clone());
                i += 1;
            }
        }
    }
    out
}

/// Normalize raw headers into `Name: Value` lines. Handles the three Chrome
/// DevTools copy formats plus standard lines.
pub fn normalize_raw_headers(raw: &str) -> String {
    // Terminals in bracketed-paste mode wrap pasted text in these markers;
    // they aren't data and would confuse the ESC-separated DevTools format
    // below.
    let raw = raw.replace("\x1b[200~", "").replace("\x1b[201~", "");
    let raw = raw.as_str();

    if raw.contains("^[E") || raw.contains('\x1b') {
        let sep = if raw.contains("^[E") { "^[E" } else { "\x1b" };
        let parts: Vec<&str> = raw.split(sep).collect();
        let mut lines = Vec::new();
        let mut i = 0;
        while i + 1 < parts.len() {
            let name = parts[i].trim();
            let value = parts[i + 1].trim();
            i += 2;
            if name.is_empty() || PSEUDO_HEADERS.contains(&name) {
                continue;
            }
            lines.push(format!("{}: {}", name, value));
        }
        return lines.join("\n");
    }

    let raw_lines: Vec<String> = strip_decoded_blocks(
        raw.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.to_string())
            .collect(),
    );
    let colon_lines = raw_lines.iter().filter(|l| l.contains(": ")).count();
    let is_alternating = raw_lines.len() > 2 && (colon_lines as f64) < raw_lines.len() as f64 * 0.2;

    if is_alternating {
        let mut lines = Vec::new();
        let mut i = 0;
        while i + 1 < raw_lines.len() {
            let name = raw_lines[i].trim();
            let value = raw_lines[i + 1].trim();
            i += 2;
            if PSEUDO_HEADERS.contains(&name) {
                continue;
            }
            lines.push(format!("{}: {}", name, value));
        }
        return lines.join("\n");
    }

    let mut result = Vec::new();
    for line in &raw_lines {
        let stripped = line.trim();
        if stripped.starts_with(':') {
            continue;
        }
        result.push(stripped.to_string());
    }
    result.join("\n")
}

fn parse_headers(normalized: &str) -> Map<String, Value> {
    let mut map = Map::new();
    for line in normalized.lines() {
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim().to_lowercase();
            if !name.is_empty() {
                map.insert(name, json!(value.trim()));
            }
        }
    }
    map
}

// ---------------------------------------------------------------------------
// Auth file
// ---------------------------------------------------------------------------

pub struct Auth;

impl Auth {
    pub fn is_authenticated() -> bool {
        Self::load_headers()
            .and_then(|h| h.get("cookie").and_then(|c| c.as_str()).map(|s| !s.is_empty()))
            .unwrap_or(false)
    }

    pub fn load_headers() -> Option<Map<String, Value>> {
        let data = fs::read_to_string(auth_path()).ok()?;
        serde_json::from_str::<Value>(&data).ok()?.as_object().cloned()
    }

    pub async fn validate() -> anyhow::Result<bool> {
        let headers = match Self::load_headers() {
            Some(h) if h.get("cookie").is_some() => h,
            _ => return Ok(false),
        };
        Ok(account_name(&headers).await.ok().flatten().is_some())
    }

    /// The signed-in account's display name, or `None` when not signed in.
    pub async fn current_account() -> Option<String> {
        let headers = Auth::load_headers()?;
        let cookie = headers.get("cookie").and_then(|v| v.as_str())?;
        if cookie.is_empty() {
            return None;
        }
        account_name(&headers).await.ok().flatten()
    }
}

fn save_headers(headers: &Map<String, Value>) -> anyhow::Result<()> {
    let payload = serde_json::to_string_pretty(&Value::Object(headers.clone()))?;
    let path = auth_path();
    fs::write(&path, payload)?;
    // Session cookies are credentials: keep them owner-only (0600), not at the
    // umask default, which is usually world-readable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Account probing
// ---------------------------------------------------------------------------

/// `Ok(Some(name))` = valid account, `Ok(None)` = server reachable but no
/// usable account (e.g. expired session), `Err` = server unreachable.
async fn account_name(headers: &Map<String, Value>) -> Result<Option<String>, ()> {
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|_| ())?;
    account_name_with(&client, headers).await
}

/// Same probe on a caller-supplied client. The TUI passes its API client so
/// the lookup shares the connection pool instead of opening another TLS
/// connection at startup.
pub(crate) async fn account_name_with(
    client: &reqwest::Client,
    headers: &Map<String, Value>,
) -> Result<Option<String>, ()> {
    let url = format!(
        "https://music.youtube.com/youtubei/v1/account/account_menu?key={}&prettyPrint=false",
        API_KEY
    );
    let body = json!({
        "context": {
            "client": {
                "clientName": "WEB_REMIX",
                "clientVersion": CLIENT_VERSION,
                "hl": "en"
            },
            "user": { "lockedSafetyMode": false },
            "request": { "useSsl": true }
        }
    });

    let mut req = client.post(url).json(&body);
    for (k, v) in headers {
        if let Some(s) = v.as_str() {
            req = req.header(k.as_str(), s);
        }
    }

    let resp = req.send().await.map_err(|_| ())?;
    // A reachable server is not a network failure: an unauthenticated request
    // returns a non-JSON (401/consent) body, which just means "no account".
    let text = resp.text().await.map_err(|_| ())?;
    match serde_json::from_str::<Value>(&text) {
        Ok(value) => Ok(find_account_name(&value)),
        Err(_) => Ok(None),
    }
}

fn find_account_name(value: &Value) -> Option<String> {
    if let Value::Object(map) = value {
        if let Some(name) = map.get("accountName") {
            if let Some(s) = name.as_str() {
                return Some(s.to_string());
            }
            if let Some(s) = name
                .get("runs")
                .and_then(|r| r.get(0))
                .and_then(|r| r.get("text"))
                .and_then(|t| t.as_str())
            {
                return Some(s.to_string());
            }
        }
        for v in map.values() {
            if let Some(s) = find_account_name(v) {
                return Some(s);
            }
        }
    } else if let Value::Array(arr) = value {
        for v in arr {
            if let Some(s) = find_account_name(v) {
                return Some(s);
            }
        }
    }
    None
}

enum ProbeOutcome {
    Chosen(u8, Option<String>),
    NetworkDown,
    NoAccount,
}

/// Probe `x-goog-authuser` slots 0..4 and return the account to sign in with.
async fn select_account(base: &Map<String, Value>) -> ProbeOutcome {
    let mut valid: Vec<(u8, String)> = Vec::new();
    let mut network_down = false;

    for slot in 0u8..5 {
        let mut h = base.clone();
        h.insert("x-goog-authuser".into(), json!(slot.to_string()));
        match account_name(&h).await {
            Ok(Some(name)) => valid.push((slot, name)),
            Ok(None) => {}
            Err(()) => network_down = true,
        }
    }

    match valid.len() {
        0 if network_down => ProbeOutcome::NetworkDown,
        0 => ProbeOutcome::NoAccount,
        1 => ProbeOutcome::Chosen(valid[0].0, Some(valid[0].1.clone())),
        _ => {
            println!();
            println!("  Multiple Google accounts found. Select your YouTube Music account.");
            println!("  If you have YouTube Music Premium, pick that account.");
            println!();
            for (i, (slot, name)) in valid.iter().enumerate() {
                println!("  [{}] {} (browser slot {})", i + 1, name, slot);
            }
            println!();
            let chosen = loop {
                let ans = prompt_line(&format!("  Enter number [1-{}]: ", valid.len()))
                    .unwrap_or_default();
                if let Ok(n) = ans.trim().parse::<usize>() {
                    if n >= 1 && n <= valid.len() {
                        break valid[n - 1].clone();
                    }
                }
                println!("  Please enter a number between 1 and {}.", valid.len());
            };
            ProbeOutcome::Chosen(chosen.0, Some(chosen.1))
        }
    }
}

// ---------------------------------------------------------------------------
// Interactive setup wizard
// ---------------------------------------------------------------------------

fn prompt_line(prompt: &str) -> Option<String> {
    print!("{}", prompt);
    let _ = io::stdout().flush();
    let mut line = String::new();
    match io::stdin().lock().read_line(&mut line) {
        Ok(0) => None,
        Ok(_) => Some(line.trim_end_matches(['\r', '\n']).to_string()),
        Err(_) => None,
    }
}

/// `ytmtui setup`: pick which service to configure. `--manual` and
/// `--browser` keep their old meaning and go straight to YouTube Music, so
/// existing scripts and instructions behave exactly as before.
pub async fn run_setup(manual: bool, browser_arg: Option<String>) -> anyhow::Result<()> {
    if manual || browser_arg.is_some() {
        return run_youtube_setup(manual, browser_arg).await;
    }

    println!();
    println!("============================================================");
    println!("  YTMTUI Setup");
    println!("============================================================");
    println!();
    println!("  What would you like to configure?");
    println!("    [1] YouTube Music");
    println!("    [2] Last.fm");
    println!("    [3] Both");
    println!();

    let choice = loop {
        let Some(answer) = prompt_line("  Choice [1]: ") else {
            println!("  Setup cancelled.");
            return Ok(());
        };
        let answer = answer.trim();
        if answer.is_empty() {
            break 1;
        }
        match answer.parse::<u8>() {
            Ok(choice @ 1..=3) => break choice,
            _ => println!("  Please enter 1, 2, or 3."),
        }
    };

    // Configure both on request, and keep going if one fails so the other
    // service is still usable.
    let mut error: Option<anyhow::Error> = None;
    if choice == 1 || choice == 3 {
        if let Err(e) = run_youtube_setup(false, None).await {
            error = Some(e);
        }
    }
    if choice == 2 || choice == 3 {
        if let Err(e) = run_lastfm_setup().await {
            if error.is_none() {
                error = Some(e);
            }
        }
    }
    match error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

async fn run_youtube_setup(manual: bool, browser_arg: Option<String>) -> anyhow::Result<()> {
    println!();
    println!("============================================================");
    println!("  YouTube Music Authentication");
    println!("============================================================");
    println!();

    if Auth::is_authenticated() {
        println!("  Existing authentication found.");
        let ans = prompt_line("  Do you want to re-authenticate? [y/N]: ").unwrap_or_default();
        if !matches!(ans.trim().to_lowercase().as_str(), "y" | "yes") {
            println!("  Setup cancelled.");
            return Ok(());
        }
        println!();
    }

    let saved = if manual {
        setup_manual()?
    } else {
        match extract_cookies(browser_arg) {
            Some((name, cookies)) => {
                println!("  Found YouTube cookies in {}.", name);
                println!("  Extracting automatically...");
                println!();
                if save_from_cookies(&cookies).await {
                    println!("  Cookies extracted from {} and saved.", name);
                    true
                } else {
                    println!("  Auto-extraction failed. Falling back to manual setup.");
                    println!();
                    setup_manual()?
                }
            }
            None => setup_manual()?,
        }
    };

    if !saved {
        anyhow::bail!("Authentication setup failed.");
    }

    println!();
    println!("Validating credentials...");
    match Auth::validate().await {
        Ok(true) => println!("Authentication is valid. You're all set!"),
        Ok(false) => println!(
            "Warning: Could not validate authentication. Try launching `ytmtui` anyway —\n\
             cookies were saved and may still work.\n\
             If it doesn't work, run `ytmtui setup --manual` to paste headers directly."
        ),
        Err(_) => println!(
            "Warning: Could not reach YouTube Music servers to validate.\n\
             Your credentials were saved but could not be verified."
        ),
    }

    Ok(())
}

/// `ytmtui setup lastfm`: authenticate a new connection or manage an existing
/// one. Secrets are always read without echo and are never printed back.
pub async fn run_lastfm_setup() -> anyhow::Result<()> {
    println!();
    println!("============================================================");
    println!("  Last.fm Scrobbling");
    println!("============================================================");
    println!();

    let existing = crate::lastfm::load();
    if let Some(creds) = &existing {
        println!("  Connected as {}.", creds.username);
        println!(
            "  Scrobbling is {}.",
            if creds.enabled { "on" } else { "off" }
        );
        println!();
        println!("  [1] Update credentials");
        println!(
            "  [2] {} scrobbling",
            if creds.enabled { "Disable" } else { "Enable" }
        );
        println!("  [3] Disconnect Last.fm");
        println!("  [Enter] Keep these settings");
        println!();
        let Some(answer) = prompt_line("  Choice: ") else {
            println!("  Setup cancelled.");
            return Ok(());
        };
        match answer.trim() {
            "" => {
                println!("  Last.fm settings unchanged.");
                return Ok(());
            }
            "1" => {}
            "2" => {
                let enabled = !creds.enabled;
                crate::lastfm::set_enabled(enabled).map_err(anyhow::Error::msg)?;
                println!(
                    "  Scrobbling {}.",
                    if enabled { "enabled" } else { "disabled" }
                );
                return Ok(());
            }
            "3" => {
                let answer =
                    prompt_line("  Disconnect Last.fm and delete stored credentials? [y/N]: ")
                        .unwrap_or_default();
                if matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
                    crate::lastfm::clear().map_err(anyhow::Error::msg)?;
                    println!("  Last.fm disconnected.");
                } else {
                    println!("  Nothing changed.");
                }
                return Ok(());
            }
            _ => {
                println!("  Unknown choice; nothing changed.");
                return Ok(());
            }
        }
    } else {
        println!("  Scrobble your YouTube Music listening to Last.fm.");
        println!("  Create an API account at https://www.last.fm/api/account/create");
        println!("  to get an API key and shared secret.");
        println!();
    }

    let Some((api_key, api_secret, username, session_key, password_hash)) =
        prompt_lastfm_credentials(existing.as_ref())
    else {
        println!("  Setup cancelled.");
        return Ok(());
    };

    let input = crate::lastfm::prepare_auth_inputs(
        &api_key,
        &api_secret,
        &username,
        password_hash.as_deref(),
        session_key.as_deref(),
    )
    .map_err(|e| anyhow::anyhow!("Invalid Last.fm settings: {e}"))?;

    println!();
    println!("  Authenticating with Last.fm...");
    let auth = crate::lastfm::authenticate(input)
        .await
        .map_err(|e| anyhow::anyhow!("Last.fm authentication failed: {e}"))?;

    let creds = crate::lastfm::Credentials {
        api_key,
        api_secret,
        username: auth.username,
        session_key: auth.session_key,
        enabled: true,
    };
    crate::lastfm::save(&creds).map_err(anyhow::Error::msg)?;

    println!();
    println!(
        "  Connected as {}. Scrobbling enabled.",
        creds.username
    );
    Ok(())
}

type LastfmSetupInput = (String, String, String, Option<String>, Option<String>);

/// Prompt for the Last.fm connection details. `None` means the user
/// cancelled. Existing secret values are kept when the answer is empty and
/// are never rendered on screen.
fn prompt_lastfm_credentials(
    existing: Option<&crate::lastfm::Credentials>,
) -> Option<LastfmSetupInput> {
    let api_key = prompt_keep("  API Key", existing.map(|c| c.api_key.as_str()))?;
    let api_secret = prompt_keep_secret(
        "  Shared Secret (hidden)",
        existing.map(|c| c.api_secret.as_str()),
    )?;
    let username = prompt_keep("  Last.fm Username", existing.map(|c| c.username.as_str()))?;

    println!();
    println!("  Authentication method:");
    println!("    [1] Session key (recommended)");
    println!("    [2] MD5 password hash");
    println!();
    let use_session = !matches!(prompt_line("  Choice [1]: ")?.trim(), "2");
    let (session_key, password_hash) = if use_session {
        let key = prompt_keep_secret(
            "  Session Key (hidden)",
            existing.map(|c| c.session_key.as_str()),
        )?;
        (Some(key), None)
    } else {
        // A stored session key cannot replace a hash; ask for a fresh one and
        // let authentication exchange it for a session key.
        let hash = prompt_secret("  MD5 Password Hash (hidden): ")?;
        (None, Some(hash))
    };
    Some((api_key, api_secret, username, session_key, password_hash))
}

/// Read a value, keeping `current` when the answer is empty.
fn prompt_keep(prompt: &str, current: Option<&str>) -> Option<String> {
    loop {
        let label = if current.is_some() {
            format!("{prompt} [keep current]: ")
        } else {
            format!("{prompt}: ")
        };
        let value = prompt_line(&label)?;
        let value = value.trim();
        if !value.is_empty() {
            return Some(value.to_string());
        }
        if let Some(current) = current {
            return Some(current.to_string());
        }
        println!("  This value is required.");
    }
}

/// [`prompt_keep`] for values that must not be echoed.
fn prompt_keep_secret(prompt: &str, current: Option<&str>) -> Option<String> {
    loop {
        let label = if current.is_some() {
            format!("{prompt} [keep current]: ")
        } else {
            format!("{prompt}: ")
        };
        let value = prompt_secret(&label)?;
        let value = value.trim();
        if !value.is_empty() {
            return Some(value.to_string());
        }
        if let Some(current) = current {
            return Some(current.to_string());
        }
        println!("  This value is required.");
    }
}

/// Read a secret without echoing it. Raw mode delivers the keystrokes
/// directly; when it is unavailable (piped input) fall back to a normal line
/// read — the value is still never printed.
fn prompt_secret(prompt: &str) -> Option<String> {
    use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
    use crossterm::terminal;

    print!("{}", prompt);
    let _ = io::stdout().flush();
    if terminal::enable_raw_mode().is_err() {
        return prompt_line("");
    }

    let mut value = String::new();
    loop {
        match event::read() {
            Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Enter => break,
                KeyCode::Backspace => {
                    value.pop();
                }
                KeyCode::Esc => {
                    let _ = terminal::disable_raw_mode();
                    println!();
                    return None;
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    let _ = terminal::disable_raw_mode();
                    println!();
                    return None;
                }
                KeyCode::Char(c) => value.push(c),
                _ => {}
            },
            Ok(Event::Paste(text)) => value.extend(text.chars().filter(|c| !c.is_control())),
            Ok(_) => {}
            Err(_) => {
                let _ = terminal::disable_raw_mode();
                println!();
                return None;
            }
        }
    }
    let _ = terminal::disable_raw_mode();
    println!();
    Some(value)
}

fn extract_cookies(browser_arg: Option<String>) -> Option<(String, Vec<(String, String)>)> {
    if let Some(name) = browser_arg {
        println!("  Trying browser: {}", name);
        println!();
        match browser::extract_from_browser(&name) {
            Some(cookies) => Some((name, cookies)),
            None => {
                println!("  Could not extract from {}. Falling back to manual setup.", name);
                println!();
                None
            }
        }
    } else {
        browser::detect_browser()
    }
}

async fn save_from_cookies(cookies: &[(String, String)]) -> bool {
    let cookie_str = browser::cookie_header(cookies);
    let mut headers = build_browser_headers(&cookie_str);

    match select_account(&headers).await {
        ProbeOutcome::Chosen(slot, name) => {
            headers.insert("x-goog-authuser".into(), json!(slot.to_string()));
            if let Some(name) = name {
                println!("  Authenticated as: {}", name);
            }
        }
        ProbeOutcome::NetworkDown => {
            println!("  Warning: could not reach YouTube Music to identify the account;");
            println!("  saving the extracted cookies for browser slot 0.");
        }
        ProbeOutcome::NoAccount => {
            println!("  No valid YouTube Music account found in the extracted cookies.");
            return false;
        }
    }

    save_headers(&headers).is_ok()
}

fn setup_manual() -> anyhow::Result<bool> {
    println!("  Manual setup — copy the request headers from music.youtube.com.");
    println!();
    println!("  1. Open music.youtube.com, press F12 > Network, refresh.");
    println!("  2. Click any /browse request, right-click Request Headers > Copy.");
    println!("  3. Paste below, then press Enter on a blank line.");
    println!();
    println!("  Only have the cookie? Paste just the cookie value.");
    println!();
    println!("  Headers:");
    println!();

    let stdin = io::stdin();
    let mut lines: Vec<String> = Vec::new();
    loop {
        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                let trimmed = line.trim_end_matches(['\r', '\n']);
                if trimmed.trim().is_empty() {
                    if lines.is_empty() {
                        continue;
                    }
                    break;
                }
                lines.push(trimmed.to_string());
                // A single line with cookie credentials is a complete paste
                // (bare cookie value or lone cookie: line).
                if lines.len() == 1 && looks_like_cookie_line(trimmed) {
                    break;
                }
            }
            Err(_) => break,
        }
    }

    if lines.is_empty() {
        println!();
        println!("  No headers provided.");
        return Ok(false);
    }

    println!();
    println!("  Done reading {} line(s).", lines.len());

    let raw = lines.join("\n");
    let normalized = normalize_raw_headers(&raw);
    let has_cookie_header = normalized.lines().any(|l| {
        l.split(':')
            .next()
            .map(|n| n.trim().eq_ignore_ascii_case("cookie"))
            .unwrap_or(false)
    });

    let mut headers = default_headers();

    if has_cookie_header {
        for (k, v) in parse_headers(&normalized) {
            headers.insert(k, v);
        }
    } else if raw.contains("SAPISID=") {
        // User pasted only the Cookie value (no header name).
        let cookie = raw.split_whitespace().collect::<Vec<_>>().join("");
        headers.insert("cookie".into(), json!(cookie));
    } else {
        for (k, v) in parse_headers(&normalized) {
            headers.insert(k, v);
        }
    }

    let has_cookie = headers
        .get("cookie")
        .and_then(|v| v.as_str())
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    if !has_cookie {
        println!();
        println!("  Error: no 'cookie' found. Make sure you copied from a");
        println!("  music.youtube.com request, then run `ytmtui setup` again.");
        return Ok(false);
    }

    // Derive authorization from SAPISID when the paste didn't include one.
    if !headers.contains_key("authorization") {
        if let Some(cookie) = headers.get("cookie").and_then(|v| v.as_str()) {
            if let Some(sapisid) = sapisid_from_cookie(cookie) {
                headers.insert("authorization".into(), json!(sapisidhash(&sapisid)));
            }
        }
    }
    if !headers.contains_key("x-goog-authuser") {
        headers.insert("x-goog-authuser".into(), json!("0"));
    }

    if sapisid_from_cookie(
        headers
            .get("cookie")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
    )
    .is_none()
    {
        println!();
        println!("  Warning: no SAPISID found in the cookie. Authentication is");
        println!("  unlikely to work — make sure you are signed in to YouTube Music.");
    }

    save_headers(&headers)?;

    println!();
    println!("  Browser authentication saved to: {}", auth_path().display());
    Ok(true)
}

fn looks_like_cookie_line(line: &str) -> bool {
    line.contains("SAPISID=") && line.contains(';')
}
