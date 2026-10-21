//! Colour theme: the full 16-colour pywal palette, mapped onto semantic UI
//! roles so every palette slot has a job.
//!
//! pywal writes a palette to `$XDG_CACHE_HOME/wal/colors.json` (usually
//! `~/.cache/wal/colors.json`). We read it at startup so the UI matches the
//! rest of the desktop, and keep watching it — a wallpaper change rewrites
//! the file, and the palette repaints live without a restart. If it isn't
//! present we fall back to the terminal's own indexed palette (color0..color15).

use ratatui::style::Color;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    // -- Text hierarchy -------------------------------------------------
    /// Primary text (color7).
    pub fg: Color,
    /// Emphasis text — titles that should pop (color15).
    pub bright: Color,
    /// Secondary text: meta, counts, artists, hairlines, status (color8).
    pub dim: Color,

    // -- Accents --------------------------------------------------------
    /// Primary accent: `▌` cursors, focused search, progress fill (color4).
    pub accent: Color,
    /// Bright accent: the active search filter (color12).
    pub accent_bright: Color,
    /// Kind glyphs and footer key hints (color6).
    pub accent2: Color,
    /// Artists, status numbers and the search `▸` marker (color5).
    pub mark: Color,
    /// The app name in the header (color13).
    pub brand: Color,

    // -- Playback states ------------------------------------------------
    /// Playing indicator (color10).
    pub ok: Color,
    /// The row of the track that is currently playing (color2).
    pub ok_soft: Color,
    /// Buffering indicator and "Loading…" text (color3).
    pub warn: Color,
    /// Paused indicator (color11).
    pub warn_bright: Color,
    /// Stopped indicator (color1).
    pub stop: Color,
    /// Error status messages (color9).
    pub err: Color,

    // -- Selection ------------------------------------------------------
    /// Background of the highlighted row (color4).
    pub selection_bg: Color,
    /// Foreground of the highlighted row (color0).
    pub selection_fg: Color,
}

impl Theme {
    /// Load the pywal palette, falling back to the terminal's own colours.
    pub fn load() -> Self {
        Self::from_pywal().unwrap_or_else(Self::fallback)
    }

    /// No pywal palette: use the terminal's indexed colours as-is.
    pub fn fallback() -> Self {
        Self::from_palette(std::array::from_fn(|i| Color::Indexed(i as u8)))
    }

    /// Map a 16-colour palette onto the semantic roles above. colour0 is
    /// the selection foreground; colour14 currently has no role (row
    /// durations are deliberately left plain).
    fn from_palette(p: [Color; 16]) -> Self {
        Self {
            fg: p[7],
            bright: p[15],
            dim: p[8],
            accent: p[4],
            accent_bright: p[12],
            accent2: p[6],
            mark: p[5],
            brand: p[13],
            ok: p[10],
            ok_soft: p[2],
            warn: p[3],
            warn_bright: p[11],
            stop: p[1],
            err: p[9],
            selection_bg: p[4],
            selection_fg: p[0],
        }
    }

    /// The pywal file `Theme::load` resolved to — the one a live reload
    /// should watch. `None` while no palette exists (it may appear later,
    /// e.g. the app started before the first `wal` run).
    pub fn source_file() -> Option<PathBuf> {
        for dir in wal_dirs() {
            for name in ["colors.json", "colors"] {
                let path = dir.join(name);
                if path.exists() {
                    return Some(path);
                }
            }
        }
        None
    }

    /// Re-read one specific pywal file. `None` when it can't be parsed:
    /// pywal rewrites the file in place, so a read racing the write sees a
    /// partial file — callers keep the current theme instead of flashing to
    /// the fallback, and retry once the write lands.
    pub fn reload_from(path: &std::path::Path) -> Option<Self> {
        load_file(path).map(Self::from_palette)
    }

    /// Re-resolve the palette exactly the way `load` did at startup. Used
    /// when the watched file turns out to be unreadable, so a live reload
    /// can never land on a different candidate than startup chose.
    pub fn reload_pywal() -> Option<Self> {
        Self::from_pywal()
    }

    fn from_pywal() -> Option<Self> {
        for dir in wal_dirs() {
            if let Some(palette) = load_dir(&dir) {
                return Some(Self::from_palette(palette));
            }
        }
        None
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::fallback()
    }
}

/// Candidate pywal cache directories, most specific first.
fn wal_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(cache) = std::env::var("XDG_CACHE_HOME") {
        if !cache.is_empty() {
            dirs.push(PathBuf::from(cache).join("wal"));
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let default = PathBuf::from(home).join(".cache/wal");
        if !dirs.contains(&default) {
            dirs.push(default);
        }
    }
    dirs
}

fn load_dir(dir: &Path) -> Option<[Color; 16]> {
    load_file(&dir.join("colors.json")).or_else(|| load_file(&dir.join("colors")))
}

/// Parse one pywal palette file, trying the JSON form first.
fn load_file(path: &Path) -> Option<[Color; 16]> {
    load_json(path).or_else(|| load_plain(path))
}

/// Parse `colors.json` (the canonical pywal output). Missing slots fall back
/// to the terminal's indexed colour of the same number.
fn load_json(path: &Path) -> Option<[Color; 16]> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;

    let colors = value.get("colors")?;
    let special = value.get("special");

    let mut palette = [Color::Indexed(0); 16];
    for (i, slot) in palette.iter_mut().enumerate() {
        let key = format!("color{}", i);
        let from_colors = colors
            .get(&key)
            .and_then(|v| v.as_str())
            .and_then(parse_hex);
        // color0/color7 also honour pywal's `special` background/foreground.
        let from_special = match i {
            0 | 7 => special
                .and_then(|s| s.get(if i == 0 { "background" } else { "foreground" }))
                .and_then(|v| v.as_str())
                .and_then(parse_hex),
            _ => None,
        };
        *slot = from_colors
            .or(from_special)
            .unwrap_or(Color::Indexed(i as u8));
    }
    Some(palette)
}

/// Parse the plain `colors` file (16 hex lines, `color0`..`color15`).
fn load_plain(path: &Path) -> Option<[Color; 16]> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut palette = [Color::Indexed(0); 16];
    let mut found = false;
    for (i, hex) in text.lines().filter_map(parse_hex).take(16).enumerate() {
        palette[i] = hex;
        found = true;
    }
    if !found {
        return None;
    }
    // Any short file keeps the indexed defaults for the remaining slots.
    Some(palette)
}

fn parse_hex(s: &str) -> Option<Color> {
    let s = s.trim().trim_start_matches('#');
    if s.len() < 6 {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some(Color::Rgb(r, g, b))
}
