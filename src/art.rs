//! Album art: fetch the current track's cover and hand it to the renderer as
//! square RGBA. Kitty shows a true inline image (graphics protocol); elsewhere
//! it is drawn with quadrant blocks at two pixels per cell in both axes.

use crate::models::Track;
use image::RgbaImage;

/// How album art can be rendered in this terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Graphics {
    /// Kitty's inline image protocol: a real, high-resolution picture.
    Kitty,
    /// Unicode quadrant blocks: 2×2 pixels per cell.
    Blocks,
}

/// Detect terminal capability from the environment. Protocol queries need a
/// terminal reply, which the key-reading event loop would swallow.
pub fn graphics_mode() -> Graphics {
    let kitty = std::env::var_os("KITTY_WINDOW_ID").is_some()
        || std::env::var("TERM").is_ok_and(|t| t.contains("kitty"));
    if kitty {
        Graphics::Kitty
    } else {
        Graphics::Blocks
    }
}

/// Whether the terminal supports kitty's text sizing protocol (OSC 66).
/// Kitty added it in 0.40; older versions swallow the scaled text and leave
/// the pane blank.
pub fn text_scaling() -> bool {
    static SUPPORT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *SUPPORT.get_or_init(|| {
        if graphics_mode() != Graphics::Kitty {
            return false;
        }
        let Ok(out) = std::process::Command::new("kitty").arg("--version").output() else {
            return false;
        };
        let text = String::from_utf8_lossy(&out.stdout);
        let Some(ver) = text.split_whitespace().nth(1) else {
            return false;
        };
        let mut parts = ver.split('.');
        let major = parts.next().and_then(|p| p.parse::<u32>().ok()).unwrap_or(0);
        let minor = parts.next().and_then(|p| p.parse::<u32>().ok()).unwrap_or(0);
        (major, minor) >= (0, 40)
    })
}

/// Terminal cell size in pixels, read straight from the kernel via TIOCGWINSZ.
/// An ioctl, not an escape query, so no reply can reach the key loop. Needed
/// to keep square covers square across fonts — classic cells are 2:1, but
/// proportional fonts and HiDPI skew that. Falls back to 2:1 when the terminal
/// reports no pixel size.
pub fn cell_size() -> (f32, f32) {
    if let Ok(ws) = crossterm::terminal::window_size() {
        if ws.width > 0 && ws.height > 0 && ws.columns > 0 && ws.rows > 0 {
            return (
                f32::from(ws.width) / f32::from(ws.columns),
                f32::from(ws.height) / f32::from(ws.rows),
            );
        }
    }
    (1.0, 2.0)
}

/// Best single cover URL for a track: the video's largest still, also what
/// MPRIS advertises as `mpris:artUrl`.
pub fn primary_cover_url(t: &Track) -> String {
    // Local art is the cover file beside the track, spelled as a `file://`
    // URI for MPRIS.
    if crate::offline::is_local(&t.id) {
        return t
            .thumbnail_url
            .as_deref()
            .filter(|p| !p.is_empty())
            .map(|path| format!("file://{}", urlencoding::encode(path)))
            .unwrap_or_default();
    }
    if !t.id.is_empty() {
        return format!("https://i.ytimg.com/vi/{}/maxresdefault.jpg", t.id);
    }
    t.thumbnail_url.clone().unwrap_or_default()
}

/// Candidate cover URLs, largest first: maxres, smaller stills, then the API
/// thumbnail. The renderer centre-crops to a square, cutting letterbox bars
/// off music-video frames (album art stays centred).
pub fn cover_urls(t: &Track) -> Vec<String> {
    // Local rows have no video id and no network. The cover is embedded in
    // the audio (or placed beside it), so try that file first.
    if crate::offline::is_local(&t.id) {
        let mut urls: Vec<String> = t
            .thumbnail_url
            .iter()
            .filter(|u| !u.is_empty())
            .cloned()
            .collect();
        if let Some(path) = crate::offline::local_path(&t.id) {
            urls.push(path.display().to_string());
        }
        return urls;
    }
    let mut urls = Vec::new();
    if !t.id.is_empty() {
        urls.push(format!("https://i.ytimg.com/vi/{}/maxresdefault.jpg", t.id));
        urls.push(format!("https://i.ytimg.com/vi/{}/hq720.jpg", t.id));
        urls.push(format!("https://i.ytimg.com/vi/{}/hqdefault.jpg", t.id));
    }
    if let Some(u) = t.thumbnail_url.as_deref() {
        if !u.is_empty() {
            urls.push(u.to_string());
        }
    }
    urls
}

/// Decode a path: an image file if possible, otherwise the cover embedded in
/// the audio tag.
fn cover_from_disk(path: &str) -> Option<RgbaImage> {
    if let Ok(rgba) = image::open(path) {
        return Some(rgba.to_rgba8());
    }
    embedded_cover(path)
}

/// Pull the cover from an audio file's tag. Downloads embed art in the mp3
/// (`--embed-thumbnail`), so the music folder stays audio-only while covers
/// still render offline.
fn embedded_cover(path: &str) -> Option<RgbaImage> {
    use lofty::file::TaggedFileExt;
    let tagged = lofty::read_from_path(path).ok()?;
    let picture = tagged.tags().iter().flat_map(|tag| tag.pictures()).next()?;
    image::load_from_memory(picture.data()).ok().map(|img| img.to_rgba8())
}

/// Centre-crop to a square, trimming letterbox bars (album art is centred).
fn center_square(rgba: RgbaImage) -> Option<RgbaImage> {
    let side = rgba.width().min(rgba.height());
    if side == 0 {
        return None;
    }
    let x = (rgba.width() - side) / 2;
    let y = (rgba.height() - side) / 2;
    Some(image::imageops::crop_imm(&rgba, x, y, side, side).to_image())
}

/// Fetch and decode the first working cover, centre-cropped. Candidates are
/// fetched concurrently (largest first); the biggest that answered wins, so a
/// slow or rate-limited CDN URL can't hold the art back. `None` when all fail
/// and the caller drops the panel.
pub async fn fetch_cover(urls: Vec<String>) -> Option<RgbaImage> {
    let mut remote = Vec::new();
    for url in urls {
        if url.starts_with("http://") || url.starts_with("https://") {
            remote.push(url);
        } else if !url.is_empty() {
            // Anything else is a disk path: open it locally, never the
            // network — that's the point offline. Downloads keep the cover
            // in the audio tag.
            if let Some(rgba) = cover_from_disk(&url) {
                if let Some(square) = center_square(rgba) {
                    return Some(square);
                }
            }
        }
    }
    if remote.is_empty() {
        return None;
    }
    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0")
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .ok()?;
    let tasks: Vec<_> = remote
        .into_iter()
        .map(|url| {
            let client = client.clone();
            tokio::spawn(async move {
                let resp = client.get(&url).send().await.ok()?;
                if !resp.status().is_success() {
                    return None;
                }
                // Fetch bytes only; decoding is memory-heavy and happens one
                // candidate at a time below.
                resp.bytes().await.ok()
            })
        })
        .collect();
    // Decode strictly one at a time, largest first. A maxres frame is ~3.5 MB
    // of RGBA; keeping every answered candidate decoded (the old behaviour)
    // wasted memory since the first success wins anyway.
    for task in tasks {
        let Some(bytes) = task.await.ok().flatten() else {
            continue;
        };
        let Some(rgba) = image::load_from_memory(&bytes).ok().map(|i| i.to_rgba8()) else {
            continue;
        };
        if let Some(square) = center_square(rgba) {
            return Some(square);
        }
    }
    None
}

/// Resize the cover to an exact pixel canvas (Lanczos3), never cropping: the
/// art block letterboxes whatever doesn't fit.
pub fn fit_square(src: &RgbaImage, px_w: u32, px_h: u32) -> RgbaImage {
    image::imageops::resize(
        src,
        px_w.max(1),
        px_h.max(1),
        image::imageops::FilterType::Lanczos3,
    )
}

/// Encode an image as PNG bytes (kitty's preferred payload).
pub fn png_bytes(img: &RgbaImage) -> Option<Vec<u8>> {
    use image::ImageEncoder;
    let mut buf = Vec::new();
    // Encode from the borrowed pixel buffer; building a `DynamicImage` used
    // to clone the whole canvas.
    image::codecs::png::PngEncoder::new(&mut buf)
        .write_image(
            img.as_raw(),
            img.width(),
            img.height(),
            image::ExtendedColorType::Rgba8,
        )
        .ok()?;
    Some(buf)
}

/// Base64 for kitty payloads; avoids an extra dependency.
fn base64(data: &[u8]) -> String {
    const TABLE: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = ((chunk[0] as u32) << 16)
            | ((chunk.get(1).copied().unwrap_or(0) as u32) << 8)
            | chunk.get(2).copied().unwrap_or(0) as u32;
        out.push(TABLE[(n >> 18 & 63) as usize] as char);
        out.push(TABLE[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6 & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Escape bytes that draw `png` inline over a `cols`×`rows` cell area at
/// (`x`, `y`), chunked per the kitty spec. Kitty scales the `px_w`×`px_h`
/// pixels to the cell block, so one high-res payload works at any size.
pub fn kitty_transfer(x: u16, y: u16, cols: u16, rows: u16, px_w: u32, px_h: u32, png: &[u8]) -> Vec<u8> {
    let payload = base64(png);
    let mut out = Vec::with_capacity(payload.len() + 128);
    // Move the cursor to the art's top-left cell (1-based CUP).
    out.extend_from_slice(format!("\x1b[{};{}H", y + 1, x + 1).as_bytes());
    let mut chunks = payload.as_bytes().chunks(4096).peekable();
    let mut first = true;
    while let Some(chunk) = chunks.next() {
        let last = chunks.peek().is_none();
        let more = if last { 0 } else { 1 };
        if first {
            out.extend_from_slice(
                format!(
                    "\x1b_Ga=T,f=100,i=7,s={},v={},c={},r={},m={};",
                    px_w, px_h, cols, rows, more
                )
                .as_bytes(),
            );
            first = false;
        } else {
            out.extend_from_slice(format!("\x1b_Gm={};", more).as_bytes());
        }
        out.extend_from_slice(chunk);
        out.extend_from_slice(b"\x1b\\");
    }
    out
}

/// Delete all visible inline images (terminal cleanup on exit).
pub fn kitty_cleanup() -> Vec<u8> {
    b"\x1b_Ga=d,d=A\x1b\\".to_vec()
}
