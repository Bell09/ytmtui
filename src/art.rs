//! Album art: fetch the current track's cover and hand it to the renderer as
//! square RGBA. Terminals speaking the Kitty graphics protocol (Kitty,
//! WezTerm, Ghostty, Konsole) show a true inline image; elsewhere it is
//! drawn with quadrant blocks at two pixels per cell in both axes.

use crate::models::{Playlist, SearchHit, Track};
use image::RgbaImage;
use std::borrow::Cow;

/// How album art can be rendered in this terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Graphics {
    /// Kitty's inline image protocol: a real, high-resolution picture.
    /// Supported by Kitty, WezTerm, Ghostty and Konsole.
    Kitty,
    /// Unicode quadrant blocks: 2×2 pixels per cell.
    Blocks,
}

/// Detect terminal capability from the environment. Protocol queries need a
/// terminal reply, which the key-reading event loop would swallow.
pub fn graphics_mode() -> Graphics {
    let term = std::env::var("TERM").unwrap_or_default();
    let term_program = std::env::var("TERM_PROGRAM").unwrap_or_default();

    // Kitty graphics protocol is spoken by Kitty, WezTerm, Ghostty and
    // Konsole. Environment variables each terminal sets:
    let kitty = std::env::var_os("KITTY_WINDOW_ID").is_some() || term.contains("kitty");
    let wezterm = std::env::var_os("WEZTERM_PANE").is_some()
        || std::env::var_os("WEZTERM_EXECUTABLE").is_some()
        || term_program.contains("WezTerm");
    let ghostty = std::env::var_os("GHOSTTY").is_some() || term_program.contains("Ghostty");
    let konsole = std::env::var_os("KONSOLE_VERSION").is_some() || term.contains("konsole");

    if kitty || wezterm || ghostty || konsole {
        Graphics::Kitty
    } else {
        Graphics::Blocks
    }
}

/// Whether the terminal supports kitty's text sizing protocol (OSC 66).
/// Kitty (0.40+), Ghostty and WezTerm support it; Konsole does not.
pub fn text_scaling() -> bool {
    static SUPPORT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *SUPPORT.get_or_init(|| {
        if graphics_mode() != Graphics::Kitty {
            return false;
        }
        let term = std::env::var("TERM").unwrap_or_default();
        let term_program = std::env::var("TERM_PROGRAM").unwrap_or_default();

        // Ghostty supports OSC 66 from its early releases.
        if std::env::var_os("GHOSTTY").is_some() || term_program.contains("Ghostty") {
            return true;
        }
        // WezTerm supports OSC 66.
        if std::env::var_os("WEZTERM_PANE").is_some()
            || std::env::var_os("WEZTERM_EXECUTABLE").is_some()
            || term_program.contains("WezTerm")
        {
            return true;
        }
        // Kitty: check the CLI version (0.40+).
        if std::env::var_os("KITTY_WINDOW_ID").is_some() || term.contains("kitty") {
            let Ok(out) = std::process::Command::new("kitty")
                .arg("--version")
                .output()
            else {
                return false;
            };
            let text = String::from_utf8_lossy(&out.stdout);
            let Some(ver) = text.split_whitespace().nth(1) else {
                return false;
            };
            let mut parts = ver.split('.');
            let major = parts
                .next()
                .and_then(|p| p.parse::<u32>().ok())
                .unwrap_or(0);
            let minor = parts
                .next()
                .and_then(|p| p.parse::<u32>().ok())
                .unwrap_or(0);
            return (major, minor) >= (0, 40);
        }
        // Konsole speaks the Kitty graphics protocol but not OSC 66.
        false
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

/// Small thumbnail URL for a list row. YouTube video ids get the compact
/// `mqdefault` still, while local files use their sidecar path (or the file
/// itself, whose embedded tag is read at fetch time). API-provided artwork is
/// kept when it is not already a full-size `i.ytimg.com` still, so Google's
/// image server can be asked for a 128-pixel copy instead.
pub fn track_thumbnail_url(t: &Track) -> Option<Cow<'_, str>> {
    if crate::offline::is_local(&t.id) {
        if let Some(path) = t.thumbnail_url.as_deref().filter(|p| !p.is_empty()) {
            return Some(Cow::Borrowed(path));
        }
        return crate::offline::local_path(&t.id)
            .map(|path| Cow::Owned(path.display().to_string()));
    }
    if !t.id.is_empty() {
        if let Some(url) = t.thumbnail_url.as_deref().filter(|u| !u.is_empty()) {
            if !url.contains("i.ytimg.com") {
                return Some(Cow::Borrowed(url));
            }
        }
        return Some(Cow::Owned(format!(
            "https://i.ytimg.com/vi/{}/mqdefault.jpg",
            t.id
        )));
    }
    t.thumbnail_url
        .as_deref()
        .filter(|u| !u.is_empty())
        .map(Cow::Borrowed)
}

/// Thumbnail URL for either kind of search/browse row.
pub fn hit_thumbnail_url(hit: &SearchHit) -> Option<Cow<'_, str>> {
    match hit {
        SearchHit::Track(track) => track_thumbnail_url(track),
        SearchHit::Collection(collection) => collection_thumbnail_url(collection),
    }
}

/// Collections only carry whatever the API returned; their browse ids are not
/// video ids, so there is no URL to derive when it is missing.
pub fn collection_thumbnail_url(collection: &Playlist) -> Option<Cow<'_, str>> {
    collection
        .thumbnail_url
        .as_deref()
        .filter(|u| !u.is_empty())
        .map(Cow::Borrowed)
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
    image::load_from_memory(picture.data())
        .ok()
        .map(|img| img.to_rgba8())
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

/// Largest download, and largest decoded side, accepted for a discovery card.
/// Cards are drawn at 32x16 pixels, so anything bigger is wasted memory.
const DISCOVERY_MAX_BYTES: usize = 256 * 1024;
const DISCOVERY_MAX_SIDE: u32 = 1280;
const DISCOVERY_MAX_ALLOC: u64 = 8 * 1024 * 1024;
/// Side length asked of Google's image server for a card cover.
const DISCOVERY_COVER_SIDE: u32 = 128;

/// Google-hosted covers end in a size option such as `=w544-h544-l90-rj`.
/// Ask the server for a card-sized copy rather than downloading and decoding
/// the large one only to shrink it to 32x16. Any other URL (notably signed
/// `i.ytimg.com` links, which cannot be edited) is returned untouched.
fn card_cover_url(url: &str) -> std::borrow::Cow<'_, str> {
    use std::borrow::Cow;
    let host = url
        .split_once("://")
        .and_then(|(_, rest)| rest.split('/').next())
        .unwrap_or("");
    if !(host.ends_with(".googleusercontent.com") || host.ends_with(".ggpht.com")) {
        return Cow::Borrowed(url);
    }
    let Some((head, options)) = url.rsplit_once('=') else {
        return Cow::Borrowed(url);
    };
    if options.is_empty()
        || !options
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Cow::Borrowed(url);
    }
    let mut changed = false;
    let mut out = String::with_capacity(url.len());
    out.push_str(head);
    out.push('=');
    for (index, token) in options.split('-').enumerate() {
        if index > 0 {
            out.push('-');
        }
        let (prefix, digits) = token.split_at(token.len().min(1));
        match (prefix, digits.parse::<u32>()) {
            ("w" | "h" | "s", Ok(side)) if side > DISCOVERY_COVER_SIDE => {
                changed = true;
                out.push_str(prefix);
                out.push_str(&DISCOVERY_COVER_SIDE.to_string());
            }
            _ => out.push_str(token),
        }
    }
    if changed {
        Cow::Owned(out)
    } else {
        Cow::Borrowed(url)
    }
}

/// A small, bounded thumbnail for discovery cards. The response, decoder and
/// final cache entry have limits so opening a shelf does not retain large art.
pub async fn fetch_discovery_thumbnail(client: &reqwest::Client, url: &str) -> Option<RgbaImage> {
    let url = card_cover_url(url);
    let mut response = client
        .get(url.as_ref())
        .header(reqwest::header::USER_AGENT, "Mozilla/5.0")
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?;
    let length = response.content_length();
    if length.is_some_and(|len| len > DISCOVERY_MAX_BYTES as u64) {
        return None;
    }
    // Size the buffer once from Content-Length so it never doubles.
    let mut bytes = Vec::with_capacity(length.map_or(16 * 1024, |len| len as usize));
    while let Some(chunk) = response.chunk().await.ok()? {
        if bytes.len().saturating_add(chunk.len()) > DISCOVERY_MAX_BYTES {
            return None;
        }
        bytes.extend_from_slice(&chunk);
    }
    tokio::task::spawn_blocking(move || decode_discovery_thumbnail(bytes))
        .await
        .ok()
        .flatten()
}

/// Fetch one list-row thumbnail. HTTP(S) URLs use the small Google rewrite;
/// anything else is a local path, including audio files whose embedded cover
/// is extracted by `cover_from_disk`.
pub async fn fetch_thumbnail(client: &reqwest::Client, url: &str) -> Option<RgbaImage> {
    if url.starts_with("http://") || url.starts_with("https://") {
        return fetch_discovery_thumbnail(client, url).await;
    }
    let path = match url.strip_prefix("file://") {
        Some(rest) => urlencoding::decode(rest)
            .map(|decoded| decoded.into_owned())
            .unwrap_or_else(|_| rest.to_string()),
        None => url.to_string(),
    };
    tokio::task::spawn_blocking(move || decode_local_thumbnail(&path))
        .await
        .ok()
        .flatten()
}

fn decode_local_thumbnail(path: &str) -> Option<RgbaImage> {
    let source = cover_from_disk(path)?;
    let small = image::DynamicImage::ImageRgba8(source).thumbnail(64, 64).into_rgba8();
    let square = center_square(small)?;
    Some(fit_square(&square, 32, 16))
}

fn decode_discovery_thumbnail(bytes: Vec<u8>) -> Option<RgbaImage> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(DISCOVERY_MAX_SIDE);
    limits.max_image_height = Some(DISCOVERY_MAX_SIDE);
    limits.max_alloc = Some(DISCOVERY_MAX_ALLOC);
    reader.limits(limits);
    let decoded = reader.decode().ok()?;
    // Downscale before converting or cropping, so those copies stay tiny.
    let small = decoded.thumbnail(64, 64).into_rgba8();
    drop(decoded);
    let square = center_square(small)?;
    Some(fit_square(&square, 32, 16))
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
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
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
pub fn kitty_transfer(
    x: u16,
    y: u16,
    cols: u16,
    rows: u16,
    px_w: u32,
    px_h: u32,
    png: &[u8],
) -> Vec<u8> {
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

#[cfg(test)]
mod discovery_memory_tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn thumbnail_keeps_the_center_and_has_fixed_small_dimensions() {
        let mut source = RgbaImage::from_pixel(1200, 600, image::Rgba([0, 0, 255, 255]));
        for y in 0..600 {
            for x in 300..900 {
                source.put_pixel(x, y, image::Rgba([255, 0, 0, 255]));
            }
        }
        let bytes = png_bytes(&source).unwrap();
        drop(source);
        let thumbnail = decode_discovery_thumbnail(bytes).unwrap();
        assert_eq!(thumbnail.dimensions(), (32, 16));
        for (x, y) in [(2, 8), (16, 8), (29, 8)] {
            let pixel = thumbnail.get_pixel(x, y);
            assert!(pixel[0] > 200 && pixel[2] < 50, "{pixel:?}");
        }
    }

    #[test]
    fn card_cover_url_requests_a_small_google_cover_and_leaves_other_urls_alone() {
        let big = "https://lh3.googleusercontent.com/abc_DEF-12=w544-h544-l90-rj";
        assert_eq!(
            card_cover_url(big),
            "https://lh3.googleusercontent.com/abc_DEF-12=w128-h128-l90-rj"
        );
        let square = "https://yt3.ggpht.com/xyz=s400-c-k-c0x00ffffff-no-rj";
        assert_eq!(
            card_cover_url(square),
            "https://yt3.ggpht.com/xyz=s128-c-k-c0x00ffffff-no-rj"
        );
        // Already small, signed, unknown host, or no size option: unchanged.
        for url in [
            "https://lh3.googleusercontent.com/abc=w60-h60-l90-rj",
            "https://i.ytimg.com/vi/id/sddefault.jpg?sqp=-oaymwEj&rs=AOn4",
            "https://example.invalid/cover=w544-h544",
            "https://lh3.googleusercontent.com/no-size-option",
            "https://lh3.googleusercontent.com/abc=w544-h544?token=1",
        ] {
            assert!(
                matches!(card_cover_url(url), std::borrow::Cow::Borrowed(_)),
                "{url}"
            );
        }
    }

    #[test]
    fn thumbnail_rejects_images_above_the_existing_dimension_limit() {
        let source = RgbaImage::from_pixel(DISCOVERY_MAX_SIDE + 1, 1, image::Rgba([0, 0, 0, 255]));
        assert!(decode_discovery_thumbnail(png_bytes(&source).unwrap()).is_none());
    }

    #[tokio::test]
    async fn thumbnail_fetch_uses_shared_client_and_rejects_oversized_response() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 2048];
            let _ = stream.read(&mut request).unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1048577\r\n\r\n")
                .unwrap();
        });
        let client = reqwest::Client::new();
        assert!(
            fetch_discovery_thumbnail(&client, &format!("http://{address}/cover"))
                .await
                .is_none()
        );
        server.join().unwrap();
    }
}
