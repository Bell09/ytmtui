//! Lyrics — YouTube Music's timed model first, then LRCLIB for a synced
//! fallback, and finally plain text. Parsing covers both LRC and YouTube
//! Music's timed formats, and titles are sanitised for the LRCLIB lookup.

use crate::api::YtmApi;
use serde_json::Value;
use std::sync::LazyLock;
use std::time::Duration;

/// Parsed lyrics: timestamp-synced (YTM's timed model or LRC) or plain.
#[derive(Debug, Clone)]
pub enum Lyrics {
    Synced(Vec<(f64, String)>),
    Unsynced(Vec<String>),
}

/// What the YouTube Music endpoints returned, before fallbacks.
#[derive(Debug, Clone)]
pub enum YtLyrics {
    Timed(Vec<(f64, String)>),
    Text(String),
    None,
}

/// The panel's state for the current track.
#[derive(Debug, Clone, Default)]
pub enum LyricsState {
    Loading,
    Ready(Lyrics),
    #[default]
    Missing,
    Failed,
}

/// What a finished fetch sends back over the channel: video id + outcome.
pub type LyricsMsg = (String, Result<Option<Lyrics>, ()>);

/// One fetch for a track: timed lyrics first, LRCLIB next, plain text last.
pub async fn fetch_for_track(
    api: &YtmApi,
    video_id: &str,
    title: &str,
    artist: &str,
    duration_secs: Option<u64>,
) -> Result<Option<Lyrics>, ()> {
    // A local file has no video id to ask YouTube about, so skip that call
    // and let the title/artist sources answer — the only ones that can when
    // there's no network either.
    let yt = if crate::offline::is_local(video_id) {
        YtLyrics::None
    } else {
        api.get_lyrics(video_id).await.map_err(|_| ())?
    };

    // Timed lyrics from YTM: use them immediately.
    if let YtLyrics::Timed(lines) = &yt {
        return Ok(Some(Lyrics::Synced(lines.clone())));
    }

    // LRCLIB fallback for synced lyrics.
    if !title.is_empty() && !artist.is_empty() {
        if let Some(lrc) = lrclib_synced(title, artist, duration_secs).await {
            let synced = parse_lrc(&lrc);
            if !synced.is_empty() {
                return Ok(Some(Lyrics::Synced(synced)));
            }
        }
    }

    // The plain text might still be LRC; otherwise show it line by line.
    match yt {
        YtLyrics::Text(text) => {
            let synced = parse_lrc(&text);
            if !synced.is_empty() {
                return Ok(Some(Lyrics::Synced(synced)));
            }
            let lines: Vec<String> = text.lines().map(str::to_string).collect();
            if lines.is_empty() {
                Ok(None)
            } else {
                Ok(Some(Lyrics::Unsynced(lines)))
            }
        }
        _ => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// LRCLIB.net — the synced-lyrics fallback.
// ---------------------------------------------------------------------------

async fn lrclib_synced(
    title: &str,
    artist: &str,
    duration_secs: Option<u64>,
) -> Option<String> {
    let clean = sanitize_title_for_lyric_lookup(title, artist);
    let client = reqwest::Client::builder()
        .user_agent("ytmtui/1.0")
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(5))
        .build()
        .ok()?;
    let mut req = client
        .get("https://lrclib.net/api/get")
        .query(&[("track_name", clean.as_str()), ("artist_name", artist)]);
    if let Some(d) = duration_secs {
        req = req.query(&[("duration", d.to_string())]);
    }
    let resp = req.send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let value: Value = resp.json().await.ok()?;
    value
        .get("syncedLyrics")?
        .as_str()
        .map(str::to_string)
}

// ---------------------------------------------------------------------------
// Title sanitising — strip the noise YouTube titles carry.
// ---------------------------------------------------------------------------

static NOISE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?xi)
        \s*                                  # leading whitespace
        [\(\[]\s*                            # opening bracket
        (?:
            official\s*(?:music\s*)?(?:video|audio|lyric|lyrics)
          | lyrics?\s*video
          | official
          | audio
          | video
          | hd
          | 4k
          | (?:feat\.?|ft\.?|featuring)\b(?:[^()\]]|\([^)]*\))*
          | remaster(?:ed)?(?:\s+\d{4})?
          | (?:[^)\]]+\s+)?remix(?:\s+[^)\]]*)?
          | deluxe(?:\s+edition)?
          | live(?:\s+[^)\]]*)?
          | acoustic(?:\s+[^)\]]*)?
        )
        \s*[\)\]]                            # closing bracket
        \s*                                  # trailing whitespace
        ",
    )
    .expect("valid lyric noise regex")
});

static SQUEEZE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\s{2,}").expect("valid squeeze regex"));

/// Strip common noise from a track title for better LRCLIB matching —
/// same behaviour as `sanitize_title_for_lyric_lookup`.
pub fn sanitize_title_for_lyric_lookup(title: &str, artist: &str) -> String {
    if title.is_empty() {
        return title.to_string();
    }
    let cleaned = NOISE.replace_all(title, " ");
    let mut cleaned = SQUEEZE.replace_all(&cleaned, " ").into_owned();
    if !artist.is_empty() {
        let prefix = format!("{} - ", artist.to_lowercase());
        let lower = cleaned.to_lowercase();
        if lower.starts_with(&prefix) {
            let n = prefix.chars().count();
            let head: String = cleaned.chars().take(n).collect();
            if head.to_lowercase() == prefix {
                cleaned = cleaned.chars().skip(n).collect();
            }
        }
    }
    let cleaned = cleaned.trim().to_string();
    if cleaned.is_empty() {
        title.to_string()
    } else {
        cleaned
    }
}

// ---------------------------------------------------------------------------
// LRC parsing — mirrors `_parse_synced_lyrics` in lyrics_sidebar.py.
// ---------------------------------------------------------------------------

/// Parse LRC-format synced lyrics into (seconds, text) pairs, sorted.
pub fn parse_lrc(raw: &str) -> Vec<(f64, String)> {
    let mut lines = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        let Some((ts, rest)) = split_timestamp(line) else {
            continue;
        };
        lines.push((ts, strip_timestamps(rest).trim().to_string()));
    }
    lines.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    lines
}

/// `[(h:)mm:ss(.fff)]` at the line's start → seconds + remainder.
/// Minute/second digit counts are `1-2` and `2`.
fn split_timestamp(line: &str) -> Option<(f64, &str)> {
    let rest = line.strip_prefix('[')?;
    let close = rest.find(']')?;
    let ts = parse_stamp(&rest[..close])?;
    Some((ts, &rest[close + 1..]))
}

fn parse_stamp(stamp: &str) -> Option<f64> {
    let (mins, secs_part) = stamp.split_once(':')?;
    if !(1..=2).contains(&mins.len()) || !mins.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (secs, frac) = match secs_part.split_once('.') {
        Some((s, f)) => (s, f),
        None => (secs_part, ""),
    };
    if secs.len() != 2 || !secs.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut total = mins.parse::<f64>().ok()? * 60.0 + secs.parse::<f64>().ok()?;
    if !frac.is_empty() {
        if frac.len() > 3 || !frac.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let n: f64 = frac.parse().ok()?;
        total += n / 10f64.powi(frac.len() as i32);
    }
    Some(total)
}

/// Drop every timestamp tag from the text.
fn strip_timestamps(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        let Some(open) = rest.find('[') else {
            out.push_str(rest);
            return out;
        };
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find(']') {
            Some(close) => {
                if parse_stamp(&after[..close]).is_some() {
                    rest = &after[close + 1..];
                } else {
                    out.push('[');
                    rest = after;
                }
            }
            None => {
                out.push('[');
                out.push_str(after);
                return out;
            }
        }
    }
}
