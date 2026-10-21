//! Browser cookie extraction (Firefox family first, then Chromium family).
//!
//! Reads each browser's SQLite cookie store, decrypting Chromium cookies with
//! the platform key where possible. Falls back gracefully: any browser whose
//! store can't be read is skipped so the caller can try the next one.

use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;

use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockDecryptMut, KeyIvInit};
use aes::Aes128;
use cbc::cipher::block_padding::Pkcs7;
use cbc::Decryptor;
use pbkdf2::pbkdf2_hmac;
use rusqlite::types::Value as SqlValue;
use rusqlite::Connection;
use sha1::Sha1;

type Aes128CbcDec = Decryptor<Aes128>;

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Firefox,
    Chromium,
}

struct BrowserSpec {
    name: &'static str,
    kind: Kind,
    roots: &'static [&'static str],
}

/// Detection/attempt order, most common browser first.
const BROWSERS: &[BrowserSpec] = &[
    BrowserSpec {
        name: "helium",
        kind: Kind::Chromium,
        roots: &[".config/net.imput.helium"],
    },
    BrowserSpec {
        name: "chrome",
        kind: Kind::Chromium,
        roots: &[
            ".config/google-chrome",
            ".var/app/com.google.Chrome/config/google-chrome",
        ],
    },
    BrowserSpec {
        name: "chromium",
        kind: Kind::Chromium,
        roots: &[
            ".config/chromium",
            ".var/app/org.chromium.Chromium/config/chromium",
        ],
    },
    BrowserSpec {
        name: "brave",
        kind: Kind::Chromium,
        roots: &[
            ".config/BraveSoftware/Brave-Browser",
            ".var/app/com.brave.Browser/config/BraveSoftware/Brave-Browser",
        ],
    },
    BrowserSpec {
        name: "firefox",
        kind: Kind::Firefox,
        roots: &[
            ".mozilla/firefox",
            ".var/app/org.mozilla.firefox/.mozilla/firefox",
            "snap/firefox/common/.mozilla/firefox",
        ],
    },
    BrowserSpec {
        name: "zen",
        kind: Kind::Firefox,
        roots: &[
            ".zen",
            ".var/app/app.zen_browser.zen/.zen",
            ".config/zen",
        ],
    },
    BrowserSpec {
        name: "edge",
        kind: Kind::Chromium,
        roots: &[
            ".config/microsoft-edge",
            ".var/app/com.microsoft.Edge/config/microsoft-edge",
        ],
    },
    BrowserSpec {
        name: "vivaldi",
        kind: Kind::Chromium,
        roots: &[
            ".config/vivaldi",
            ".var/app/com.vivaldi.Vivaldi/config/vivaldi",
        ],
    },
    BrowserSpec {
        name: "opera",
        kind: Kind::Chromium,
        roots: &[
            ".config/opera",
            ".var/app/com.opera.Opera/config/opera",
        ],
    },
];

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn root_paths(spec: &BrowserSpec) -> Vec<PathBuf> {
    let Some(home) = home() else {
        return Vec::new();
    };
    spec.roots.iter().map(|r| home.join(r)).collect()
}

/// Cookie DB candidate files for a browser root.
fn db_candidates(root: &Path, kind: Kind) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if !root.exists() {
        return out;
    }
    match kind {
        Kind::Firefox => {
            if let Ok(entries) = fs::read_dir(root) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.is_dir() {
                        let db = p.join("cookies.sqlite");
                        if db.exists() {
                            out.push(db);
                        }
                    }
                }
            }
            let db = root.join("cookies.sqlite");
            if db.exists() {
                out.push(db);
            }
        }
        Kind::Chromium => {
            if let Ok(entries) = fs::read_dir(root) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.is_dir() {
                        for rel in ["Cookies", "Network/Cookies"] {
                            let db = p.join(rel);
                            if db.exists() {
                                out.push(db);
                            }
                        }
                    }
                }
            }
            for rel in ["Cookies", "Network/Cookies"] {
                let db = root.join(rel);
                if db.exists() {
                    out.push(db);
                }
            }
        }
    }
    out
}

/// Open a private copy of *src* (plus WAL/journal sidecars) so a running
/// browser's locks never block us and recent WAL data is included.
fn open_db_copy(src: &Path) -> Option<Connection> {
    let mut hasher = DefaultHasher::new();
    src.hash(&mut hasher);
    let tag = hasher.finish();
    let dir = std::env::temp_dir().join(format!("ytmtui-cookies-{tag:x}"));
    fs::create_dir_all(&dir).ok()?;
    let file_name = src.file_name()?.to_string_lossy().to_string();
    let dst = dir.join(&file_name);
    fs::copy(src, &dst).ok()?;
    for suffix in ["-wal", "-shm", "-journal"] {
        let side = PathBuf::from(format!("{}{}", src.display(), suffix));
        if side.exists() {
            let _ = fs::copy(&side, dir.join(format!("{file_name}{suffix}")));
        }
    }
    Connection::open(&dst).ok()
}

fn is_youtube_host(host: &str) -> bool {
    host.trim_start_matches('.').ends_with("youtube.com")
}

fn clean(s: &str) -> bool {
    !s.contains(['\t', '\n', '\r'])
}

fn sql_value_to_string(v: SqlValue) -> Option<String> {
    match v {
        SqlValue::Text(s) => Some(s),
        SqlValue::Blob(b) => String::from_utf8(b).ok(),
        _ => None,
    }
}

/// Chrome on Linux encrypts cookie values with AES-128-CBC keyed by
/// PBKDF2-HMAC-SHA1(password, "saltysalt", 1, 16).
fn chrome_key() -> [u8; 16] {
    let password = chrome_password();
    let mut key = [0u8; 16];
    pbkdf2_hmac::<Sha1>(password.as_bytes(), b"saltysalt", 1, &mut key);
    key
}

/// Retrieve the "Chrome Safe Storage" secret from the login keyring if
/// available; otherwise Chrome's hard-coded Linux fallback.
fn chrome_password() -> String {
    for args in [
        ["lookup", "application", "chrome"].as_slice(),
        ["lookup", "service", "Chrome Safe Storage"].as_slice(),
        ["lookup", "application", "chromium"].as_slice(),
    ] {
        if let Ok(out) = Command::new("secret-tool").args(args).output() {
            if out.status.success() {
                let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !s.is_empty() {
                    return s;
                }
            }
        }
    }
    "peanuts".to_string()
}

fn decrypt_chrome_value(enc: &[u8], key: &[u8; 16]) -> Option<String> {
    if enc.len() <= 3 {
        return None;
    }
    let version = std::str::from_utf8(&enc[..3]).ok()?;
    if version != "v10" && version != "v11" {
        return None;
    }
    let iv = [0x20u8; 16];
    let mut buf = enc[3..].to_vec();
    let cipher = Aes128CbcDec::new(
        GenericArray::from_slice(key),
        GenericArray::from_slice(&iv),
    );
    let plain = cipher.decrypt_padded_mut::<Pkcs7>(&mut buf).ok()?;
    let text = String::from_utf8(plain.to_vec()).ok()?;
    Some(text.trim_end_matches('\0').to_string())
}

fn read_firefox(db: &Path) -> Option<Vec<(String, String)>> {
    let conn = open_db_copy(db)?;
    let mut stmt = conn
        .prepare("SELECT name, value, host FROM moz_cookies")
        .ok()?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, SqlValue>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .ok()?;

    let mut cookies = Vec::new();
    for row in rows.flatten() {
        let (name, value, host) = row;
        if !is_youtube_host(&host) {
            continue;
        }
        if let Some(value) = sql_value_to_string(value) {
            if clean(&name) && clean(&value) {
                cookies.push((name, value));
            }
        }
    }
    Some(cookies)
}

fn read_chromium(db: &Path) -> Option<Vec<(String, String)>> {
    let conn = open_db_copy(db)?;
    let key = chrome_key();
    let mut stmt = conn
        .prepare("SELECT name, value, encrypted_value, host_key FROM cookies")
        .ok()?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, SqlValue>(1)?,
                row.get::<_, SqlValue>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .ok()?;

    let mut cookies = Vec::new();
    for row in rows.flatten() {
        let (name, value, encrypted, host) = row;
        if !is_youtube_host(&host) {
            continue;
        }
        let resolved = sql_value_to_string(value)
            .filter(|s| !s.is_empty())
            .or_else(|| match encrypted {
                SqlValue::Blob(b) => decrypt_chrome_value(&b, &key),
                _ => None,
            });
        if let Some(v) = resolved {
            if clean(&name) && clean(&v) {
                cookies.push((name, v));
            }
        }
    }
    Some(cookies)
}

fn looks_authenticated(cookies: &[(String, String)]) -> bool {
    cookies.iter().any(|(n, _)| {
        n == "SAPISID" || n == "__Secure-3PAPISID" || n == "__Secure-1PAPISID"
    })
}

fn try_browser(spec: &BrowserSpec) -> Option<Vec<(String, String)>> {
    for root in root_paths(spec) {
        for db in db_candidates(&root, spec.kind) {
            let cookies = match spec.kind {
                Kind::Firefox => read_firefox(&db),
                Kind::Chromium => read_chromium(&db),
            };
            if let Some(cookies) = cookies {
                if looks_authenticated(&cookies) {
                    return Some(cookies);
                }
            }
        }
    }
    None
}

/// Extract YouTube cookies from a specific named browser.
pub fn extract_from_browser(name: &str) -> Option<Vec<(String, String)>> {
    let spec = BROWSERS.iter().find(|b| b.name == name)?;
    try_browser(spec)
}

/// Auto-detect a browser that holds YouTube credentials.
pub fn detect_browser() -> Option<(String, Vec<(String, String)>)> {
    for spec in BROWSERS {
        if let Some(cookies) = try_browser(spec) {
            return Some((spec.name.to_string(), cookies));
        }
    }
    None
}

pub fn cookie_header(cookies: &[(String, String)]) -> String {
    cookies
        .iter()
        .map(|(n, v)| format!("{n}={v}"))
        .collect::<Vec<_>>()
        .join("; ")
}
