//! Album art: download the current track's cover and hand it to the
//! renderer as a square RGBA image. In a kitty terminal the cover is
//! shown as a true inline image (graphics protocol); elsewhere it is
//! drawn with quadrant blocks at two pixels per cell in both axes.

use crate::models::Track;
use image::RgbaImage;

/// How the album art can be shown in this terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Graphics {
    /// Kitty's inline image protocol: a real, high-resolution picture.
    Kitty,
    /// Unicode quadrant blocks: 2×2 pixels per cell.
    Blocks,
}

/// Detect the terminal's capability from the environment only — the
/// protocol alternatives all require *querying* the terminal, and its
/// reply would be swallowed by the key-reading event loop.
pub fn graphics_mode() -> Graphics {
    let kitty = std::env::var_os("KITTY_WINDOW_ID").is_some()
        || std::env::var("TERM").is_ok_and(|t| t.contains("kitty"));
    if kitty {
        Graphics::Kitty
    } else {
        Graphics::Blocks
    }
}

/// The terminal's cell size in pixels, read straight from the kernel
/// (TIOCGWINSZ — an ioctl, not an escape-code query, so no reply can
/// ever reach the key loop). Album-art geometry uses it so a square
/// cover lands square on screen whatever the font's proportions: cells
/// are twice as tall as wide on classic terminals, but proportional
/// fonts (or HiDPI setups) skew that. Falls back to 2:1 when the
/// terminal reports no pixel dimensions.
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

/// The best single cover URL for a track — the video's largest still,
/// also what MPRIS advertises as `mpris:artUrl`.
pub fn primary_cover_url(t: &Track) -> String {
    // A local track's art is the cover file beside it, which MPRIS wants
    // spelled as a `file://` URI.
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

/// Candidate cover URLs for a track, largest first: the max-res still,
/// then the smaller ones, and finally the API thumbnail. The renderer
/// centre-crops to a square, which strips letterbox bars off music-video
/// frames (album art is centred, so the crop lands on the cover).
pub fn cover_urls(t: &Track) -> Vec<String> {
    // A local row has no video id to build a thumbnail URL from and no
    // network to fetch one with. Its cover lives inside the audio —
    // `d` embeds it — so the file itself is the candidate; a cover the
    // user placed beside the track is tried first.
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

/// Decode whatever a path on disk holds: an image file if it is one,
/// otherwise the cover embedded in the audio file's tag.
fn cover_from_disk(path: &str) -> Option<RgbaImage> {
    if let Ok(rgba) = image::open(path) {
        return Some(rgba.to_rgba8());
    }
    embedded_cover(path)
}

/// Pull the cover straight out of an audio file's tag. A download keeps
/// its art inside the mp3 (`--embed-thumbnail`), so the music folder
/// holds only audio and the cover still has to be renderable offline.
fn embedded_cover(path: &str) -> Option<RgbaImage> {
    use lofty::file::TaggedFileExt;
    let tagged = lofty::read_from_path(path).ok()?;
    let picture = tagged.tags().iter().flat_map(|tag| tag.pictures()).next()?;
    image::load_from_memory(picture.data()).ok().map(|img| img.to_rgba8())
}

/// Centre-crop to a square: strips the letterbox bars off music-video
/// frames (album art is centred, so the crop lands on the cover).
fn center_square(rgba: RgbaImage) -> Option<RgbaImage> {
    let side = rgba.width().min(rgba.height());
    if side == 0 {
        return None;
    }
    let x = (rgba.width() - side) / 2;
    let y = (rgba.height() - side) / 2;
    Some(image::imageops::crop_imm(&rgba, x, y, side, side).to_image())
}

/// Fetch and decode the first working cover, centre-cropped to a square.
/// All candidates are fetched at once (ordered largest → smallest) and
/// the biggest one that answered wins — a slow or rate-limited CDN on
/// one URL can't hold the art back. Returns `None` when every candidate
/// fails — the caller drops the panel.
pub async fn fetch_cover(urls: Vec<String>) -> Option<RgbaImage> {
    let mut remote = Vec::new();
    for url in urls {
        if url.starts_with("http://") || url.starts_with("https://") {
            remote.push(url);
        } else if !url.is_empty() {
            // Anything else is a path on disk: open it directly and never
            // go near the network, which is the whole point offline. A
            // downloaded track keeps its cover inside the audio file, so
            // when the path isn't an image itself, read the tag.
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
                let bytes = resp.bytes().await.ok()?;
                Some(image::load_from_memory(&bytes).ok()?.to_rgba8())
            })
        })
        .collect();
    for task in tasks {
        let Some(rgba) = task.await.ok().flatten() else {
            continue;
        };
        if let Some(square) = center_square(rgba) {
            return Some(square);
        }
    }
    None
}

/// Resize the whole cover to an exact pixel canvas (Lanczos3) — never
/// cropped: the art block letterboxes whatever doesn't fit.
pub fn fit_square(src: &RgbaImage, px_w: u32, px_h: u32) -> RgbaImage {
    image::imageops::resize(
        src,
        px_w.max(1),
        px_h.max(1),
        image::imageops::FilterType::Lanczos3,
    )
}

/// Encode an image as PNG bytes (the kitty protocol's preferred payload).
pub fn png_bytes(img: &RgbaImage) -> Option<Vec<u8>> {
    let mut buf = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(img.clone())
        .write_to(&mut buf, image::ImageFormat::Png)
        .ok()?;
    Some(buf.into_inner())
}

/// Base64 without extra dependencies (needed for kitty payloads).
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

/// The escape bytes that show `png` as an inline image covering the
/// `cols`×`rows` cell area whose top-left cell is (`x`, `y`). Chunked per
/// the kitty spec; kitty scales the `px_w`×`px_h` pixels onto the cell
/// block, so one high-res payload works at any terminal size.
pub fn kitty_transfer(x: u16, y: u16, cols: u16, rows: u16, px_w: u32, px_h: u32, png: &[u8]) -> Vec<u8> {
    let payload = base64(png);
    let mut out = Vec::with_capacity(payload.len() + 128);
    // Move the cursor to the art area's top-left cell (1-based CUP).
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

/// Delete every visible inline image (terminal cleanup on exit).
pub fn kitty_cleanup() -> Vec<u8> {
    b"\x1b_Ga=d,d=A\x1b\\".to_vec()
}
