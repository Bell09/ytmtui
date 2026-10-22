//! Lyrics — YouTube Music's timed model first, then LRCLIB for a synced
//! fallback, and finally plain text. Parsing covers both LRC and YouTube
//! Music's timed formats, and titles are sanitised for the LRCLIB lookup.

use crate::api::YtmApi;
use serde_json::Value;
use std::path::{Path, PathBuf};
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
    // A downloaded track carries its own `.lrc` sidecar, which keeps synced
    // lyrics working offline and avoids a pointless network round trip.
    let local = crate::offline::local_path(video_id);
    if let Some(path) = local {
        if let Some(lyrics) = sidecar_lyrics(path) {
            return Ok(Some(lyrics));
        }
    }

    // A local file has no video id to ask YouTube about, so skip that call
    // and let the title/artist sources answer — the only ones that can when
    // there's no network either.
    let yt = if local.is_some() {
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

// ---------------------------------------------------------------------------
// LRC sidecars — synced lyrics saved beside a downloaded track.
// ---------------------------------------------------------------------------

/// The `.lrc` file that belongs next to an audio file.
fn sidecar_path(audio: &Path) -> PathBuf {
    audio.with_extension("lrc")
}

/// Read synced lyrics saved beside a local audio file.
///
/// Only timestamped LRC counts as synced; a plain-text sidecar is ignored
/// so callers can fall back to their normal sources.
pub fn sidecar_lyrics(audio: &Path) -> Option<Lyrics> {
    let text = std::fs::read_to_string(sidecar_path(audio)).ok()?;
    let lines = parse_lrc(&text);
    if lines.is_empty() {
        None
    } else {
        Some(Lyrics::Synced(lines))
    }
}

/// Serialise synced lyrics as LRC, or `None` for plain text (which has no
/// timestamps to write).
pub fn to_lrc(lyrics: &Lyrics) -> Option<String> {
    let Lyrics::Synced(lines) = lyrics else {
        return None;
    };
    if lines.is_empty() {
        return None;
    }
    // YTM's model is already ordered, but a hand-built list may not be;
    // LRC readers expect timestamps to climb.
    let mut sorted = lines.clone();
    sorted.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut out = String::new();
    for (ts, text) in sorted {
        out.push('[');
        out.push_str(&lrc_timestamp(ts));
        out.push(']');
        // One lyric per line: an embedded newline would swallow the next
        // timestamp and corrupt the file.
        out.push_str(text.trim().replace(['\n', '\r'], " ").trim());
        out.push('\n');
    }
    Some(out)
}

/// `12.345` seconds → `00:12.35` (LRC permits minutes > 59).
fn lrc_timestamp(seconds: f64) -> String {
    let seconds = if seconds.is_finite() { seconds.max(0.0) } else { 0.0 };
    let centis = (seconds * 100.0).round() as u64;
    let minutes = centis / 6000;
    let secs = (centis / 100) % 60;
    let hundredths = centis % 100;
    format!("{:02}:{:02}.{:02}", minutes, secs, hundredths)
}

/// Write synced lyrics next to `audio` as `<stem>.lrc`.
///
/// Returns `Ok(None)` when there is nothing synced to write, and
/// `Ok(Some(path))` for a saved (or already present) sidecar. An existing
/// non-empty `.lrc` is never clobbered — it may be the user's own file.
pub fn write_sidecar(audio: &Path, lyrics: &Lyrics) -> std::io::Result<Option<PathBuf>> {
    let Some(text) = to_lrc(lyrics) else {
        return Ok(None);
    };
    let path = sidecar_path(audio);
    if path.metadata().map(|m| m.len() > 0).unwrap_or(false) {
        return Ok(Some(path));
    }
    std::fs::write(&path, text)?;
    Ok(Some(path))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn sample() -> Lyrics {
        Lyrics::Synced(vec![
            (0.0, "Intro".to_string()),
            (65.5, "Second verse".to_string()),
            (12.345, "First verse".to_string()),
        ])
    }

    fn scratch_dir(name: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "ytmtui-lyrics-{}-{}-{}",
            name,
            std::process::id(),
            stamp
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn to_lrc_sorts_and_formats_timestamps() {
        let text = to_lrc(&sample()).expect("synced lyrics");
        assert_eq!(
            text,
            "[00:00.00]Intro\n[00:12.35]First verse\n[01:05.50]Second verse\n"
        );
        let parsed = parse_lrc(&text);
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].1, "Intro");
        assert!((parsed[1].0 - 12.35).abs() < 0.001);
        assert_eq!(parsed[2].1, "Second verse");
    }

    #[test]
    fn to_lrc_ignores_unsynced_lyrics() {
        let lyrics = Lyrics::Unsynced(vec!["line one".to_string()]);
        assert!(to_lrc(&lyrics).is_none());
    }

    #[test]
    fn sidecar_round_trips_and_preserves_user_file() {
        let dir = scratch_dir("roundtrip");
        let audio = dir.join("Song.mp3");
        std::fs::write(&audio, b"audio").unwrap();

        let path = write_sidecar(&audio, &sample()).unwrap().expect("saved");
        assert_eq!(path, dir.join("Song.lrc"));
        let read = sidecar_lyrics(&audio).expect("sidecar parsed");
        let Lyrics::Synced(lines) = read else {
            panic!("expected synced lyrics");
        };
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[1].1, "First verse");

        // A sidecar the user edited is not replaced by a later download.
        std::fs::write(&path, "[00:01.00]My own words\n").unwrap();
        assert_eq!(write_sidecar(&audio, &sample()).unwrap(), Some(path.clone()));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[00:01.00]My own words\n"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn plain_text_sidecar_is_not_synced() {
        let dir = scratch_dir("plain");
        let audio = dir.join("Song.mp3");
        std::fs::write(&audio, b"audio").unwrap();
        std::fs::write(audio.with_extension("lrc"), "just words\n").unwrap();
        assert!(sidecar_lyrics(&audio).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn local_track_reads_sidecar_without_network() {
        let dir = scratch_dir("local");
        let audio = dir.join("Song.mp3");
        std::fs::write(&audio, b"audio").unwrap();
        write_sidecar(&audio, &sample()).unwrap().expect("saved");

        let api = YtmApi::new();
        let id = crate::offline::local_id(&audio);
        let lyrics = fetch_for_track(&api, &id, "Song", "Artist", None)
            .await
            .unwrap()
            .expect("sidecar lyrics");
        let Lyrics::Synced(lines) = lyrics else {
            panic!("expected synced lyrics");
        };
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].1, "Intro");

        let _ = std::fs::remove_dir_all(dir);
    }
}
