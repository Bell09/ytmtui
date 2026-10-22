//! Colour theme: a 16-colour palette detected from the desktop's wallpaper
//! theming tool, mapped onto semantic UI roles so every palette slot has a job.
//!
//! pywal, wallust and matugen all produce compatible palettes. We look in
//! their cache/config directories (and at JSON template outputs declared in
//! their configs), pick the newest valid palette, and watch it live. If none
//! is present we fall back to the terminal's own indexed palette (color0..15).

use ratatui::style::Color;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

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
    /// Load the newest palette, returning the file it came from so the live
    /// watcher can follow it.
    pub fn load_with_source() -> Option<(Self, PathBuf)> {
        for (path, _) in palette_files_by_mtime() {
            if let Some(theme) = Self::reload_from(&path) {
                return Some((theme, path));
            }
        }
        None
    }

    /// No palette: use the terminal's indexed colours as-is.
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

    /// The newest palette file that actually parses. Used by the UI poller;
    /// validation is cached by mtime, so a file being rewritten in place is
    /// skipped until the write finishes without re-parsing every tick.
    pub fn source_file() -> Option<PathBuf> {
        palette_files_by_mtime()
            .into_iter()
            .find(|(path, mtime)| is_valid_palette(path, *mtime))
            .map(|(path, _)| path)
    }

    /// The newest existing palette candidate, whether or not it parses right
    /// now. The poller uses this to distinguish "no theming tool at all" from
    /// "the newest file is still being written".
    pub fn latest_file() -> Option<PathBuf> {
        palette_files_by_mtime()
            .into_iter()
            .next()
            .map(|(path, _)| path)
    }

    /// Re-read one specific palette file. `None` when it can't be parsed:
    /// tools rewrite their cache in place, so a read racing the write sees a
    /// partial file — callers keep the current theme instead of flashing to
    /// the fallback, and retry once the write lands.
    pub fn reload_from(path: &Path) -> Option<Self> {
        load_file(path).map(Self::from_palette)
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::fallback()
    }
}

/// Candidate palette files from the common wallpaper tools.
fn palette_files() -> Vec<PathBuf> {
    let mut files = Vec::new();

    for base in xdg_dirs("XDG_CACHE_HOME", ".cache") {
        for tool in ["wal", "wallust", "matugen"] {
            let dir = base.join(tool);
            files.push(dir.join("colors.json"));
            files.push(dir.join("colors"));
        }
    }
    for base in xdg_dirs("XDG_CONFIG_HOME", ".config") {
        files.push(base.join("matugen").join("colors.json"));
        files.push(base.join("wallust").join("colors.json"));
    }
    for base in xdg_dirs("XDG_DATA_HOME", ".local/share") {
        files.push(base.join("matugen").join("colors.json"));
    }

    files.extend(tool_config_outputs());
    dedup(files)
}

/// Existing candidates, newest first. Ties are broken by path so selection is
/// deterministic when two tools write in the same timestamp tick.
fn palette_files_by_mtime() -> Vec<(PathBuf, SystemTime)> {
    let mut found = Vec::new();
    for path in palette_files() {
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let Ok(mtime) = meta.modified() else {
            continue;
        };
        found.push((path, mtime));
    }
    found.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    found
}

/// Output files declared in matugen/wallust configs, filtered to formats a
/// palette can live in. Read once at startup: new output paths need a restart,
/// while palette contents keep reloading live.
fn tool_config_outputs() -> Vec<PathBuf> {
    static CACHE: OnceLock<Vec<PathBuf>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            let mut outputs = Vec::new();
            for base in xdg_dirs("XDG_CONFIG_HOME", ".config") {
                outputs.extend(config_outputs(
                    &base.join("matugen").join("config.toml"),
                    &["output_path"],
                ));
                outputs.extend(config_outputs(
                    &base.join("wallust").join("wallust.toml"),
                    &["target", "dst"],
                ));
            }
            dedup(outputs)
        })
        .clone()
}

/// Pull output paths out of the `[templates]` section (and wallust's old
/// `[[entry]]` sections). This is intentionally a small TOML-line scanner
/// rather than a full parser: template output paths are simple quoted strings.
fn config_outputs(path: &Path, keys: &[&str]) -> Vec<PathBuf> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Some(config_dir) = path.parent() else {
        return Vec::new();
    };

    let mut outputs = Vec::new();
    let mut section = String::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            let header = line.trim_start_matches('[');
            section = header
                .split(']')
                .next()
                .unwrap_or(header)
                .trim()
                .to_ascii_lowercase();
            continue;
        }

        let in_templates =
            section == "templates" || section.starts_with("templates.") || section == "entry";
        if !in_templates {
            continue;
        }

        for key in keys {
            let Some(value) = find_assignment(line, key) else {
                continue;
            };
            let Some(raw_path) = toml_string(value) else {
                continue;
            };
            if let Some(output) = resolve_output_path(&raw_path, config_dir) {
                outputs.push(output);
            }
        }
    }
    outputs
}

/// Find `key = ...` (or `entry.key = ...`) and return everything after `=`.
fn find_assignment<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let mut from = 0;
    while let Some(offset) = line[from..].find(key) {
        let index = from + offset;
        let boundary = match line[..index].chars().next_back() {
            None => true,
            Some(ch) => ch.is_whitespace() || matches!(ch, '{' | ',' | '.'),
        };
        if boundary {
            let rest = line[index + key.len()..].trim_start();
            if let Some(value) = rest.strip_prefix('=') {
                return Some(value.trim_start());
            }
        }
        from = index + key.len();
    }
    None
}

/// Extract a quoted TOML string, ignoring an inline comment after it.
fn toml_string(value: &str) -> Option<String> {
    let value = value.trim_start();
    let quote = value.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }

    let rest = &value[1..];
    let mut escaped = false;
    for (index, ch) in rest.char_indices() {
        if quote == '"' && ch == '\\' && !escaped {
            escaped = true;
            continue;
        }
        if ch == quote && !escaped {
            return Some(rest[..index].to_string());
        }
        escaped = false;
    }
    None
}

/// Expand `~` and resolve relative output paths against the config's folder.
/// Only JSON files (and extensionless `colors` files) can be palettes.
fn resolve_output_path(raw: &str, config_dir: &Path) -> Option<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }

    let home = || std::env::var_os("HOME").map(PathBuf::from);
    let path = if raw == "~" {
        home()?
    } else if let Some(rest) = raw.strip_prefix("~/") {
        home()?.join(rest)
    } else if let Some(rest) = raw.strip_prefix("$HOME/") {
        home()?.join(rest)
    } else if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        config_dir.join(raw)
    };

    let is_json = path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("json"));
    let is_colors = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "colors");
    (is_json || is_colors).then_some(path)
}

fn xdg_dirs(variable: &str, fallback: &str) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(value) = std::env::var(variable) {
        if !value.is_empty() {
            dirs.push(PathBuf::from(value));
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let default = PathBuf::from(home).join(fallback);
        if !dirs.contains(&default) {
            dirs.push(default);
        }
    }
    dirs
}

fn dedup(mut paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut unique = Vec::with_capacity(paths.len());
    for path in paths.drain(..) {
        if !unique.contains(&path) {
            unique.push(path);
        }
    }
    unique
}

/// Cached parse success, keyed by file mtime. Failed parses are deliberately
/// not cached: a writer can finish within the same timestamp tick, and we want
/// the next poll to retry rather than ignore the finished palette forever.
fn is_valid_palette(path: &Path, mtime: SystemTime) -> bool {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, SystemTime>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut cache = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    if cache.get(path) == Some(&mtime) {
        return true;
    }

    let valid = load_file(path).is_some();
    if valid {
        cache.insert(path.to_path_buf(), mtime);
    } else {
        cache.remove(path);
    }
    valid
}

/// Parse one palette file, trying the JSON forms first.
fn load_file(path: &Path) -> Option<[Color; 16]> {
    load_json(path).or_else(|| load_plain(path))
}

/// Parse a pywal/wallust/Material-role/base16 JSON palette.
fn load_json(path: &Path) -> Option<[Color; 16]> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;

    ansi_palette(&value)
        .or_else(|| base16_palette(&value))
        .or_else(|| matugen_palette(&value))
}

/// Parse the canonical pywal/wallust form: `colors.color0`..`color15`, with
/// optional `special.background`/`special.foreground` fallbacks.
fn ansi_palette(value: &Value) -> Option<[Color; 16]> {
    let colors = value.get("colors").unwrap_or(value);
    let special = value.get("special").and_then(Value::as_object);

    let mut palette = [Color::Indexed(0); 16];
    let mut found = false;

    if let Some(object) = colors.as_object() {
        for (i, slot) in palette.iter_mut().enumerate() {
            let key = format!("color{i}");
            let from_colors = object.get(&key).and_then(json_color);
            let from_special = match i {
                0 | 7 => special
                    .and_then(|s| s.get(if i == 0 { "background" } else { "foreground" }))
                    .and_then(json_color),
                _ => None,
            };
            if let Some(color) = from_colors.or(from_special) {
                *slot = color;
                found = true;
            }
        }
    } else if let Some(array) = colors.as_array() {
        for (i, item) in array.iter().take(16).enumerate() {
            if let Some(color) = json_color(item) {
                palette[i] = color;
                found = true;
            }
        }
    }

    found.then_some(palette)
}

/// Parse matugen's base16 JSON (base00..base0F) and map it to ANSI roles.
fn base16_palette(value: &Value) -> Option<[Color; 16]> {
    let root = value.get("base16").unwrap_or(value);
    let slots = base16_slots(root.as_object()?)
        .or_else(|| value.get("colors")?.as_object().and_then(base16_slots))?;

    // base16 -> ANSI: 00 bg, 05 fg, 03 dim, 07 bright, 08..0F accents.
    const MAP: [usize; 16] = [0, 8, 11, 10, 13, 14, 12, 5, 3, 8, 11, 10, 13, 14, 12, 7];
    let mut palette = [Color::Indexed(0); 16];
    for (ansi, base) in MAP.iter().enumerate() {
        palette[ansi] = slots[*base].unwrap_or(Color::Indexed(ansi as u8));
    }
    Some(palette)
}

fn base16_slots(object: &Map<String, Value>) -> Option<[Option<Color>; 16]> {
    if !object.keys().any(|key| key.eq_ignore_ascii_case("base00")) {
        return None;
    }

    let mut slots = [None; 16];
    for (i, slot) in slots.iter_mut().enumerate() {
        let key = format!("base{i:02X}");
        *slot = object
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(&key))
            .and_then(|(_, value)| json_color(value));
    }

    // A partial or unrelated object should not masquerade as a palette.
    if slots.iter().filter(|slot| slot.is_some()).count() < 8 {
        return None;
    }
    Some(slots)
}

/// Parse matugen's Material role JSON, flat or nested under `colors` /
/// `schemes.dark` / `schemes.light`.
fn matugen_palette(value: &Value) -> Option<[Color; 16]> {
    role_candidates(value)
        .into_iter()
        .find_map(roles_to_palette)
}

fn role_candidates(value: &Value) -> Vec<&Map<String, Value>> {
    fn walk<'a>(value: &'a Value, depth: usize, out: &mut Vec<&'a Map<String, Value>>) {
        let Some(object) = value.as_object() else {
            return;
        };
        out.push(object);
        if depth == 0 {
            return;
        }
        for key in ["colors", "schemes", "palettes", "dark", "light", "default"] {
            if let Some(child) = object.get(key) {
                walk(child, depth - 1, out);
            }
        }
    }

    let mut out = Vec::new();
    walk(value, 3, &mut out);
    out
}

fn roles_to_palette(object: &Map<String, Value>) -> Option<[Color; 16]> {
    let background = role(object, &["background", "surface", "surface_dim"])?;
    let foreground = role(object, &["on_background", "on_surface", "foreground"])?;
    let primary = role(object, &["primary"])?;

    let secondary = role(object, &["secondary"]).unwrap_or(primary);
    let tertiary = role(object, &["tertiary"]).unwrap_or(secondary);
    let error = role(object, &["error"]).unwrap_or(primary);
    let outline =
        role(object, &["outline", "outline_variant", "surface_variant"]).unwrap_or(foreground);
    let bright = role(object, &["on_surface", "on_background"]).unwrap_or(foreground);

    let mut palette = [Color::Indexed(0); 16];
    palette[0] = background;
    palette[1] = error;
    palette[2] = tertiary;
    palette[3] = secondary;
    palette[4] = primary;
    palette[5] = tertiary;
    palette[6] = secondary;
    palette[7] = foreground;
    palette[8] = outline;
    palette[9] = error;
    palette[10] = primary;
    palette[11] = secondary;
    palette[12] = primary;
    palette[13] = tertiary;
    palette[14] = secondary;
    palette[15] = bright;
    Some(palette)
}

fn role(object: &Map<String, Value>, keys: &[&str]) -> Option<Color> {
    keys.iter()
        .find_map(|key| object.get(*key).and_then(json_color))
}

/// Read a colour from a JSON string or from matugen's nested
/// `{ "default": { "hex": "#..." } }` / `{ "dark": { "hex": ... } }` forms.
fn json_color(value: &Value) -> Option<Color> {
    match value {
        Value::String(text) => parse_hex(text),
        Value::Object(object) => {
            for key in ["hex", "default", "dark", "light", "value", "color"] {
                if let Some(child) = object.get(key) {
                    if let Some(color) = json_color(child) {
                        return Some(color);
                    }
                }
            }
            None
        }
        _ => None,
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn color(hex: &str) -> Color {
        parse_hex(hex).expect("valid test colour")
    }

    #[test]
    fn reads_pywal_and_wallust_json() {
        let value = json!({
            "special": {
                "background": "#101010",
                "foreground": "#e0e0e0"
            },
            "colors": {
                "color0": "#000000",
                "color4": "#0000ff",
                "color7": "#ffffff"
            }
        });

        let palette = ansi_palette(&value).expect("pywal palette");
        assert_eq!(palette[0], color("#000000"));
        assert_eq!(palette[4], color("#0000ff"));
        assert_eq!(palette[7], color("#ffffff"));
    }

    #[test]
    fn special_colours_fill_missing_pywal_slots() {
        let value = json!({
            "special": {
                "background": "#101010",
                "foreground": "#e0e0e0"
            },
            "colors": {
                "color4": "#0000ff"
            }
        });

        let palette = ansi_palette(&value).expect("pywal palette");
        assert_eq!(palette[0], color("#101010"));
        assert_eq!(palette[7], color("#e0e0e0"));
        assert_eq!(palette[4], color("#0000ff"));
    }

    #[test]
    fn reads_matugen_flat_roles() {
        let value = json!({
            "background": "#101014",
            "on_background": "#e5e1e6",
            "primary": "#cbc4cb",
            "secondary": "#cac5c8",
            "tertiary": "#d1c3c6",
            "error": "#ffb4ab",
            "outline": "#948f94"
        });

        let palette = matugen_palette(&value).expect("matugen palette");
        assert_eq!(palette[0], color("#101014"));
        assert_eq!(palette[1], color("#ffb4ab"));
        assert_eq!(palette[4], color("#cbc4cb"));
        assert_eq!(palette[7], color("#e5e1e6"));
    }

    #[test]
    fn reads_matugen_nested_roles() {
        let value = json!({
            "colors": {
                "background": { "default": { "hex": "#101010" } },
                "on_background": { "default": { "hex": "#eeeeee" } },
                "primary": { "default": { "hex": "#80c0ff" } }
            }
        });

        let palette = matugen_palette(&value).expect("matugen palette");
        assert_eq!(palette[0], color("#101010"));
        assert_eq!(palette[4], color("#80c0ff"));
        assert_eq!(palette[7], color("#eeeeee"));
    }

    #[test]
    fn reads_base16_json() {
        let value = json!({
            "base16": {
                "base00": "#101010",
                "base03": "#505050",
                "base05": "#e0e0e0",
                "base07": "#ffffff",
                "base08": "#ff0000",
                "base0A": "#ffff00",
                "base0B": "#00ff00",
                "base0C": "#00ffff",
                "base0D": "#0000ff",
                "base0E": "#ff00ff",
                "base0F": "#aa5500"
            }
        });

        let palette = base16_palette(&value).expect("base16 palette");
        assert_eq!(palette[0], color("#101010"));
        assert_eq!(palette[1], color("#ff0000"));
        assert_eq!(palette[2], color("#00ff00"));
        assert_eq!(palette[3], color("#ffff00"));
        assert_eq!(palette[4], color("#0000ff"));
        assert_eq!(palette[7], color("#e0e0e0"));
        assert_eq!(palette[8], color("#505050"));
        assert_eq!(palette[15], color("#ffffff"));
    }

    #[test]
    fn rejects_unrelated_json() {
        let value = json!({
            "colors": {
                "background": "#101010"
            }
        });

        assert!(ansi_palette(&value).is_none());
        assert!(base16_palette(&value).is_none());
        assert!(matugen_palette(&value).is_none());
    }

    #[test]
    fn reads_template_output_paths() {
        let dir = std::env::temp_dir().join(format!(
            "ytmtui-theme-test-{}-{}",
            std::process::id(),
            line!()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let config = dir.join("config.toml");
        std::fs::write(
            &config,
            r#"
[config]
version_check = false

[templates.colors]
input_path = 'colors.json.template'
output_path = 'colors.json'

[templates.stylesheet]
output_path = 'colors.css'

[hooks]
target = 'ignored.json'
"#,
        )
        .expect("test config");

        let outputs = config_outputs(&config, &["output_path"]);
        assert_eq!(outputs, vec![dir.join("colors.json")]);

        std::fs::remove_dir_all(&dir).ok();
    }
}
