//! Chrome-free rendering: no box borders. Structure comes from hairlines,
//! whitespace and right-aligned metadata; a one-cell gutter keeps content off
//! the terminal edges; the pywal palette colours active vs. secondary things.

use crate::app::{App, InputMode, PlaybackState};
use crate::keymap;
use crate::models::{BrowseRow, Page, Playlist, PlaylistKind, SearchHit, Track};
use crate::theme::Theme;
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};
use std::time::{Duration, Instant};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Decorative rule under the search input.
const TOP_RULE: &str = "°𝄞·──────°,─────────｡“─────────────˚｡────────────˳◜───────────｡`──────────°,──────────,'─────────`,────────.`───────',──────⸝'─────`.────.ᐣ───ᐟ.──.ᐟ─~";

/// Colour for one glyph of the top rule: dashes use the secondary accent, the
/// punctuation ticks pull other palette colours so the rule looks stitched,
/// not like one flat hairline.
fn rule_char_style(c: char, t: &Theme) -> Style {
    match c {
        ',' => Style::default().fg(t.accent_bright),
        '\'' => Style::default().fg(t.mark),
        '`' => Style::default().fg(t.ok),
        '.' => Style::default().fg(t.warn),
        _ => Style::default().fg(t.accent2),
    }
}

/// The top rule as spans, with same-coloured runs merged so a full-width rule
/// costs only a few spans.
fn top_rule_line(width: usize, t: &Theme) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut current: Option<(Style, String)> = None;
    for c in TOP_RULE.chars().cycle().take(width) {
        let style = rule_char_style(c, t);
        match current.as_mut() {
            Some((style_now, text)) if *style_now == style => text.push(c),
            _ => {
                if let Some((style_prev, text)) = current.take() {
                    spans.push(Span::styled(text, style_prev));
                }
                current = Some((style, c.to_string()));
            }
        }
    }
    if let Some((style, text)) = current {
        spans.push(Span::styled(text, style));
    }
    Line::from(spans)
}

/// Width of the left playlist sidebar (narrower on small terminals).
fn sidebar_width(total: u16) -> u16 {
    if total >= 100 {
        27
    } else if total >= 64 {
        20
    } else {
        16
    }
}

/// A list-row thumbnail: two quadrant cells (four by two pixels) plus one
/// column of separation. On a terminal cell that is twice as tall as it is
/// wide, this renders as a square.
const ROW_THUMB_CELLS: u32 = 2;
const ROW_THUMB_TOTAL: usize = ROW_THUMB_CELLS as usize + 1;

/// Resolve a row's cached thumbnail and queue a fetch when it is missing.
/// `waiting` is true only while no result (success or failure) is cached.
fn row_thumb<'a>(
    app: &'a App,
    url: Option<&str>,
    wanted: &mut Vec<String>,
    show: bool,
) -> (Option<&'a image::RgbaImage>, bool) {
    if !show {
        return (None, false);
    }
    let Some(url) = url else {
        return (None, false);
    };
    match app.thumb_art.get(url) {
        Some(Some(img)) => (Some(img), false),
        Some(None) => (None, false),
        None => {
            if !app.thumbnail_requested(url) {
                wanted.push(url.to_owned());
            }
            (None, true)
        }
    }
}

/// The two-cell prefix shared by track, collection and sidebar rows.
fn thumb_prefix(
    theme: &Theme,
    thumb: Option<&image::RgbaImage>,
    waiting: bool,
) -> Vec<Span<'static>> {
    let mut spans = Vec::with_capacity(ROW_THUMB_TOTAL);
    if let Some(img) = thumb {
        for cell in 0..ROW_THUMB_CELLS {
            spans.push(thumb_cell(img, cell));
        }
    } else if waiting {
        spans.push(Span::styled("▧", Style::default().fg(theme.dim)));
        spans.push(Span::raw(" "));
    } else {
        spans.push(Span::raw(" "));
        spans.push(Span::raw(" "));
    }
    spans.push(Span::raw(" "));
    spans
}

/// One cell sampled from the source thumbnail. The source is 32x16 (a 2:1
/// canvas); two cells by one row covers 4x2 pixels, so each destination pixel
/// averages the corresponding 8x8 block.
fn thumb_cell(img: &image::RgbaImage, cell: u32) -> Span<'static> {
    let block_w = (img.width() / (ROW_THUMB_CELLS * 2)).max(1);
    let block_h = (img.height() / 2).max(1);
    let sample = |dx: u32, dy: u32| {
        average_block(img, dx * block_w, dy * block_h, block_w, block_h)
    };
    let x = cell * 2;
    let p = [sample(x, 0), sample(x + 1, 0), sample(x, 1), sample(x + 1, 1)];
    quadrant_span(&p)
}

fn average_block(
    img: &image::RgbaImage,
    x0: u32,
    y0: u32,
    width: u32,
    height: u32,
) -> image::Rgba<u8> {
    let mut sums = [0u64; 4];
    let mut count = 0u64;
    for y in y0..(y0 + height).min(img.height()) {
        for x in x0..(x0 + width).min(img.width()) {
            let p = img.get_pixel(x, y);
            for (sum, channel) in sums.iter_mut().zip(p.0) {
                *sum += u64::from(channel);
            }
            count += 1;
        }
    }
    if count == 0 {
        return image::Rgba([0, 0, 0, 255]);
    }
    image::Rgba([
        (sums[0] / count) as u8,
        (sums[1] / count) as u8,
        (sums[2] / count) as u8,
        (sums[3] / count) as u8,
    ])
}

pub fn draw(f: &mut Frame, app: &mut App) {
    // Queues are flushed around this draw; start a fresh batch.
    app.lyrics_output.clear();
    app.lyrics_clear.clear();

    // Vertical stack: title, main (search line over list/content, lyrics
    // beside them at full height), playback (track + connector bar), footer.
    // No top/bottom padding; one-cell gutter left and right.
    let area = f.area().inner(Margin {
        horizontal: 1,
        vertical: 0,
    });
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title line
            Constraint::Min(12),   // main: search line + body
            Constraint::Length(1), // playback: track + connector bar
            Constraint::Length(1), // footer: status + key guide
        ])
        .split(area);

    if app.now_playing {
        draw_now_playing(f, app, rows[0], rows[1]);
        draw_playback_bar(f, app, rows[2]);
        draw_footer_bar(f, app, rows[3]);
        if app.picker_open {
            draw_playlist_picker(f, app, rows[1]);
        }
        if app.browse_picker_open {
            draw_browse_picker(f, app, rows[1]);
        }
        if app.input_mode == InputMode::CreatePlaylist {
            draw_create_playlist_dialog(f, app, rows[1]);
        }
        if app.help_open {
            draw_help_overlay(f, app, rows[1]);
        }
        return;
    }

    let sidebar_w = sidebar_width(rows[1].width);
    // With the lyrics pane open it takes the right edge: playlists | content |
    // lyrics, borderless. It only shows when there's room for a readable pane
    // plus the content's minimum width.
    let lyrics_w = if app.lyrics_open {
        rows[1].width.saturating_sub(sidebar_w + 1 + 10).min(40)
    } else {
        0
    };
    let show_lyrics = lyrics_w >= 16;
    let mut col_constraints = vec![
        Constraint::Length(sidebar_w),
        Constraint::Length(1), // gap between sidebar and content
        Constraint::Min(10),
    ];
    if show_lyrics {
        col_constraints.push(Constraint::Length(lyrics_w));
    }
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(col_constraints)
        .split(rows[1]);

    // Left of the lyrics, the search line and rule take the top two rows;
    // list and content sit below. The pane spans the main area's full height,
    // so its first line is level with the search input, not the header.
    let left = Rect {
        width: (cols[2].x + cols[2].width).saturating_sub(rows[1].x),
        ..rows[1]
    };
    let left_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // search line + underline rule
            Constraint::Min(10),
        ])
        .split(left);
    draw_header(f, app, rows[0], left_rows[0]);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(sidebar_w),
            Constraint::Length(1), // gap between sidebar and content
            Constraint::Min(10),
        ])
        .split(left_rows[1]);

    // Sidebar: list on top, cover art below. The art sits just above the
    // playback line while a track is loaded; otherwise the list takes the
    // space.
    let mut list_area = body[0];
    let mut art_area: Option<Rect> = None;
    let current_id = app.current_track.as_ref().map(|t| t.id.as_str());
    if app
        .art_track
        .as_deref()
        .is_some_and(|id| Some(id) == current_id)
    {
        // Floating look: two cells of empty margin around the block, one cell
        // of inset inside it, and a small cap so it reads as a card and never
        // touches the list above or the playback line below.
        const PAD: u16 = 2;
        const MAX_W: u16 = 22;
        let (cw, ch) = (body[0].width, body[0].height);
        let art_w = cw.saturating_sub(PAD * 2).min(MAX_W) & !1; // even → clean aspect
        // Square core: one empty column of side margin inside the block.
        // Height in rows follows real cell proportions (cols·cell_w =
        // rows·cell_h), so the cover is square on screen. Cells twice as tall
        // as wide give rows = cols/2; taller cells get fewer rows, which stops
        // kitty from letterboxing dead bands around the image.
        let core = art_w.saturating_sub(2);
        let (cell_w, cell_h) = crate::art::cell_size();
        let art_rows = ((f32::from(core) * cell_w / cell_h).round() as u16)
            .max(1)
            .min(ch / 2);
        if art_rows >= 4 && ch >= art_rows + PAD * 2 + 6 {
            let parts = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(4),                     // list
                    Constraint::Length(1),                  // gap
                    Constraint::Length(art_rows + PAD * 2), // padded block
                ])
                .split(body[0]);
            list_area = parts[0];
            art_area = Some(Rect {
                x: body[0].x + (cw - art_w) / 2,
                y: parts[2].y + PAD,
                width: art_w,
                height: art_rows,
            });
        }
    }

    draw_sidebar(f, app, list_area);
    if let Some(rect) = art_area {
        draw_album_art(f, app, rect);
    }
    draw_content(f, app, body[2]);
    if show_lyrics {
        draw_lyrics(f, app, cols[3], false);
    }

    draw_playback_bar(f, app, rows[2]);
    draw_footer_bar(f, app, rows[3]);
    if app.picker_open {
        draw_playlist_picker(f, app, rows[1]);
    }
    if app.browse_picker_open {
        // Same area as the add picker, so both cards centre on the whole main
        // region, not just the content column.
        draw_browse_picker(f, app, rows[1]);
    }
    if app.input_mode == InputMode::CreatePlaylist {
        draw_create_playlist_dialog(f, app, rows[1]);
    }
    if app.help_open {
        draw_help_overlay(f, app, rows[1]);
    }
}

/// The Last.fm badge on the header. It appears whenever Last.fm is
/// configured, because a configured account always scrobbles — there is no
/// dimmed "off" state. Secrets never reach this string.
fn lastfm_badge(scrobbler: &crate::lastfm::Scrobbler, t: &Theme) -> Option<Span<'static>> {
    let user = scrobbler.badge()?;
    Some(Span::styled(
        format!("  · last.fm:{user}"),
        Style::default().fg(t.accent),
    ))
}

/// The account slot on the header: `Offline` when the local library is in
/// use, otherwise the display name loaded in the background, or `Guest`
/// when no valid account could be identified. Missing, blank and malformed
/// names degrade to `Guest` rather than leaving a gap.
fn header_account(offline: bool, account_name: Option<&str>) -> String {
    if offline {
        return "Offline".to_string();
    }
    let Some(name) = account_name else {
        return "Guest".to_string();
    };
    // The name arrives from the network; keep it on one line and free of
    // control characters so it cannot disturb the header.
    let name: String = name
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        "Guest".to_string()
    } else {
        name
    }
}

/// Header: title line (app name + account) and search input, as separate
/// areas. The title spans the full width; the search line only the columns
/// left of the lyrics pane, so the pane's first line is level with the input.
fn draw_header(f: &mut Frame, app: &App, title: Rect, search: Rect) {
    let t = &app.theme;
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // search input line
            Constraint::Length(1), // underline rule
        ])
        .split(search);

    // Title line: brand, then the signed-in account name. Offline mode uses
    // the same slot for its status because the online account name would be
    // stale there; an unknown name falls back to `Guest`.
    let account = header_account(app.offline, app.account_name.as_deref());
    let account_style = if app.offline {
        Style::default().fg(t.accent).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(t.brand).add_modifier(Modifier::BOLD)
    };
    let title_spans = {
        let mut spans = vec![
            Span::styled(
                " YTMTUI ⟢ ",
                Style::default().fg(t.brand).add_modifier(Modifier::BOLD),
            ),
            Span::styled(account, account_style),
        ];
        // Last.fm connection status: the username when configured. Scrobbling
        // is always on then, so the badge uses the accent colour. Secrets
        // never reach this string.
        if let Some(span) = lastfm_badge(&app.lastfm, t) {
            spans.push(span);
        }
        spans
    };
    f.render_widget(Paragraph::new(Line::from(title_spans)), title);

    // Search input: a text line on an underline rule, no side or top borders.
    // The rule spans the header width, capped at one copy of the motif so
    // ultra-wide terminals keep its intended shape.
    let focused = app.input_mode == InputMode::Search;
    let input_w = usize::from(rows[0].width).min(TOP_RULE.chars().count());

    let mut spans = vec![
        Span::styled(
            "Search ",
            Style::default()
                .fg(if focused { t.accent } else { t.dim })
                .add_modifier(if focused {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        ),
        Span::styled(
            "▸ ",
            Style::default().fg(t.mark).add_modifier(Modifier::BOLD),
        ),
    ];
    // Label + arrow reserve nine columns; leave a margin before the rule's
    // end so text never sits flush against it.
    let text_budget = input_w.saturating_sub(10);
    let cursor_w = usize::from(focused);
    if app.search_query.is_empty() && !focused {
        spans.push(Span::styled(
            truncate("press / to search", text_budget),
            Style::default().fg(t.dim),
        ));
    } else {
        spans.push(Span::styled(
            truncate(&app.search_query, text_budget.saturating_sub(cursor_w)),
            Style::default().fg(t.fg),
        ));
        if focused {
            spans.push(Span::styled("▌", Style::default().fg(t.accent)));
        }
    }
    f.render_widget(Paragraph::new(Line::from(spans)), rows[0]);

    f.render_widget(Paragraph::new(top_rule_line(input_w, t)), rows[1]);
}

/// Maximized "now playing": title and search rows on top, a large square
/// cover on the left, lyrics on the right.
fn draw_now_playing(f: &mut Frame, app: &mut App, title: Rect, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // search input + rule
            Constraint::Min(4),
        ])
        .split(area);
    draw_header(f, app, title, rows[0]);

    let body = rows[1];
    if body.width < 24 || body.height < 4 {
        return;
    }
    // Give the cover the width a square needs at this height, but keep a
    // readable lyrics column on narrow terminals.
    let min_lyrics = 24.min(body.width / 2);
    let (cell_w, cell_h) = crate::art::cell_size();
    let square_w = ((f32::from(body.height) * cell_h / cell_w).round() as u16).saturating_add(2);
    let art_w = square_w
        .max(body.width / 2)
        .min(body.width.saturating_sub(min_lyrics + 1));
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(art_w),
            Constraint::Length(1), // gap between cover and lyrics
            Constraint::Min(10),
        ])
        .split(body);

    // The lyrics block shares the cover's height: computing one square
    // rect keeps both columns lined up instead of lyrics filling the
    // whole body while the art floats in the middle.
    let art_rect = large_art_rect(cols[0]);
    draw_album_art(f, app, art_rect);
    let lyrics_rect = Rect {
        x: cols[2].x,
        y: art_rect.y,
        width: cols[2].width,
        height: art_rect.height,
    };
    draw_lyrics(f, app, lyrics_rect, true);
}

/// Centre the biggest square cover that fits inside `area`. Shared with the
/// lyrics column so both come out the same height.
fn large_art_rect(area: Rect) -> Rect {
    let (cell_w, cell_h) = crate::art::cell_size();
    let max_w = area.width.saturating_sub(4);
    let max_h = area.height.saturating_sub(4);
    let from_height = (f32::from(max_h) * cell_h / cell_w).floor() as u16;
    let core = max_w.min(from_height.max(1)) & !1;
    if core < 4 {
        return area;
    }
    let rows = ((f32::from(core) * cell_w / cell_h).round() as u16)
        .max(1)
        .min(area.height);
    // `draw_album_art` keeps one spare column on each side of the square.
    Rect {
        x: area.x + area.width.saturating_sub(core + 2) / 2,
        y: area.y + area.height.saturating_sub(rows) / 2,
        width: core + 2,
        height: rows,
    }
}

/// Always-visible Library playlist list: kind glyph, name, right-aligned track
/// count. The selected row gets a `▌` cursor and selection colours.
fn draw_sidebar(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(area);
    let highlight = Style::default()
        .fg(t.selection_fg)
        .bg(t.selection_bg)
        .add_modifier(Modifier::BOLD);
    let nav = ["Home", "Explore", "Library"]
        .iter()
        .enumerate()
        .map(|(index, title)| {
            let active = match index {
                0 => {
                    app.discovery_page == Some(crate::app::DiscoveryPage::Home)
                        && app.current_page == Page::Browse
                }
                1 => {
                    app.discovery_page == Some(crate::app::DiscoveryPage::Explore)
                        && app.current_page == Page::Browse
                }
                _ => app.current_page == Page::Library,
            };
            let style = if active {
                Style::default()
                    .fg(t.accent_bright)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(t.fg)
            };
            ListItem::new(Line::from(Span::styled(format!(" {title}"), style)))
        })
        .collect::<Vec<_>>();
    let mut nav_state = ListState::default();
    if app.sidebar_focused {
        nav_state.select(Some(app.sidebar_index));
    }
    f.render_stateful_widget(
        List::new(nav)
            .highlight_style(highlight)
            .highlight_symbol("▌"),
        sections[0],
        &mut nav_state,
    );

    let show_cursor =
        app.current_page == Page::Library && !app.sidebar_focused && !app.playlists.is_empty();
    let text_w = (area.width as usize).saturating_sub(3);
    let mut items = vec![ListItem::new(Line::from(vec![
        Span::styled(
            " LIBRARY",
            Style::default().fg(t.bright).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" · {}", app.playlists.len()),
            Style::default().fg(t.dim),
        ),
    ]))];
    if app.playlists.is_empty() {
        let msg = if app.library_loaded {
            " No playlists found."
        } else {
            " Loading library…"
        };
        items.push(ListItem::new(Line::from(Span::styled(
            msg,
            Style::default().fg(t.dim),
        ))));
    }
    for (i, playlist) in app.playlists.iter().enumerate() {
        items.push(sidebar_item(
            t,
            playlist,
            text_w,
            show_cursor && i == app.selected_index,
        ));
    }
    let mut state = ListState::default();
    if show_cursor {
        state.select(Some(app.selected_index + 1));
    }
    f.render_stateful_widget(
        List::new(items)
            .highlight_style(highlight)
            .highlight_symbol("▌"),
        sections[1],
        &mut state,
    );
}

fn sidebar_item(
    theme: &crate::theme::Theme,
    playlist: &Playlist,
    text_w: usize,
    is_selected: bool,
) -> ListItem<'static> {
    // Selected spans leave the foreground unset so they inherit the list's
    // highlight colours.
    let name_style = if is_selected {
        Style::default()
    } else {
        Style::default().fg(theme.fg)
    };
    let meta_style = if is_selected {
        Style::default()
    } else {
        Style::default().fg(theme.dim)
    };
    let glyph_style = if is_selected {
        Style::default()
    } else {
        Style::default()
            .fg(theme.accent2)
            .add_modifier(Modifier::BOLD)
    };

    let prefix = format!(" {} ", kind_glyph(playlist.kind));
    let prefix_w = display_width(&prefix);
    let meta = match playlist.track_count {
        Some(n) => n.to_string(),
        None => String::new(),
    };
    let meta_w = display_width(&meta);

    let show_meta = meta_w > 0 && text_w >= prefix_w + meta_w + 6;
    let name_budget = if show_meta {
        text_w.saturating_sub(prefix_w + meta_w + 2)
    } else {
        text_w.saturating_sub(prefix_w)
    };
    let name = truncate(&playlist.title, name_budget);

    let mut spans = vec![
        Span::styled(prefix, glyph_style),
        Span::styled(name.clone(), name_style),
    ];
    if show_meta {
        let pad = text_w.saturating_sub(prefix_w + display_width(&name) + meta_w);
        spans.push(Span::styled(" ".repeat(pad), meta_style));
        spans.push(Span::styled(meta, meta_style));
    }
    ListItem::new(Line::from(spans))
}

/// Cover art in the sidebar, flush above the playback line. Terminals
/// speaking the Kitty graphics protocol (Kitty, WezTerm, Ghostty, Konsole)
/// show a true inline image; other terminals get quadrant blocks at two
/// pixels per cell on both axes. The photo keeps its own colours, not the
/// palette. While fetching, a dim cover glyph holds the space.
fn draw_album_art(f: &mut Frame, app: &mut App, area: Rect) {
    let (w, rows) = (area.width, area.height);

    if crate::art::graphics_mode() == crate::art::Graphics::Kitty {
        // True inline image: transmit once per (track, cell size, terminal
        // size). Kitty graphics protocol keeps it placed between frames but
        // drops all placements on resize, so a fullscreen toggle must
        // retransmit. The escape bytes are flushed after each frame (see
        // main's event loop).
        if let Some(id) = app.art_track.clone() {
            if let Some(src) = app.art_source.as_ref() {
                let frame = f.area();
                let key = (id, w, rows, frame.width, frame.height);
                if app.art_kitty_sent.as_ref() != Some(&key) {
                    // The square cover fills the block's square core (cells
                    // are ~2:1), letterboxed by empty columns either side.
                    let core = w.saturating_sub(2);
                    let px = crate::art::fit_square(src, 720, 720);
                    if let Some(png) = crate::art::png_bytes(&px) {
                        // Drop the old placement first so a track or size
                        // change never leaves a ghost cover.
                        app.art_output = crate::art::kitty_cleanup();
                        app.art_output.extend(crate::art::kitty_transfer(
                            area.x + (w - core) / 2,
                            area.y,
                            core,
                            rows,
                            720,
                            720,
                            &png,
                        ));
                        app.art_kitty_sent = Some(key);
                    }
                }
            } else {
                draw_art_placeholder(f, app, area);
            }
        }
        return;
    }

    // Quadrant blocks: two pixels per cell on both axes. The square cover
    // fills the block's square core, letterboxed by untouched background
    // cells either side.
    let core = w.saturating_sub(2);
    if app.art_fitted.as_ref().map(|(key, _)| *key) != Some((core, rows)) {
        app.art_fitted = None;
    }
    if app.art_fitted.is_none() {
        app.art_fitted = app.art_source.as_ref().map(|src| {
            (
                (core, rows),
                crate::art::fit_square(src, u32::from(core) * 2, u32::from(rows) * 2),
            )
        });
    }

    let Some((_, img)) = &app.art_fitted else {
        draw_art_placeholder(f, app, area);
        return;
    };

    let mut lines = Vec::with_capacity(rows as usize);
    for y in 0..rows {
        let mut spans = Vec::with_capacity(core as usize);
        for x in 0..core {
            let (px, py) = (u32::from(x) * 2, u32::from(y) * 2);
            let p = [
                *img.get_pixel(px, py),
                *img.get_pixel(px + 1, py),
                *img.get_pixel(px, py + 1),
                *img.get_pixel(px + 1, py + 1),
            ];
            spans.push(quadrant_span(&p));
        }
        lines.push(Line::from(spans));
    }
    let content = Rect {
        x: area.x + (w - core) / 2,
        ..area
    };
    f.render_widget(Paragraph::new(lines), content);
}

fn draw_art_placeholder(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let mid = Rect {
        y: area.y + area.height / 2,
        height: 1,
        ..area
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled("♪", Style::default().fg(t.dim))))
            .alignment(Alignment::Center),
        mid,
    );
}

/// One cell from four pixels: majority colour becomes the background, the
/// minority is painted with a quadrant/half-block glyph in the foreground.
/// Three-of-a-kind flips the roles so a lone pixel still shows exactly.
fn quadrant_span(p: &[image::Rgba<u8>; 4]) -> Span<'static> {
    let color = |c: &image::Rgba<u8>| Color::Rgb(c[0], c[1], c[2]);

    let mut counts = [0usize; 4];
    for i in 0..4 {
        for j in 0..4 {
            if p[j] == p[i] {
                counts[i] += 1;
            }
        }
    }
    let bg = (0..4).max_by_key(|&i| counts[i]).unwrap_or(0);
    let minority: Vec<usize> = (0..4).filter(|&i| p[i] != p[bg]).collect();

    if minority.is_empty() {
        return Span::styled(" ", Style::default().bg(color(&p[bg])));
    }
    if minority.len() == 3 {
        // One background pixel among three: paint it as the foreground.
        let only = (0..4).find(|&i| p[i] == p[bg]).unwrap_or(0);
        let glyph = ["▘", "▝", "▖", "▗"][only];
        return Span::styled(
            glyph,
            Style::default()
                .fg(color(&p[only]))
                .bg(color(&p[minority[0]])),
        );
    }
    let glyph = match minority.len() {
        1 => ["▘", "▝", "▖", "▗"][minority[0]],
        2 => match (minority[0], minority[1]) {
            (0, 1) => "▀",
            (2, 3) => "▄",
            (0, 2) => "▌",
            (1, 3) => "▐",
            (0, 3) => "▚",
            _ => "▞",
        },
        _ => " ",
    };
    Span::styled(
        glyph,
        Style::default()
            .fg(color(&p[minority[0]]))
            .bg(color(&p[bg])),
    )
}

/// Playback on one line: indicator + track, with the progress bar running
/// from the artist's name into the time readout — the bar *is* the connector.
/// It is always present (dim and empty when idle) so the line keeps its
/// shape. The title gives up characters before the bar gets too short.
fn draw_playback_bar(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let width = area.width as usize;

    // Indicator and colour follow playback state: buffering = yellow,
    // playing = bright green, paused = bright yellow, stopped = red.
    let (indicator, indicator_style) = match app.playback_state {
        PlaybackState::Buffering => (
            "⏳",
            Style::default().fg(t.warn).add_modifier(Modifier::BOLD),
        ),
        PlaybackState::Playing => ("▶", Style::default().fg(t.ok).add_modifier(Modifier::BOLD)),
        PlaybackState::Paused => (
            "⏸",
            Style::default()
                .fg(t.warn_bright)
                .add_modifier(Modifier::BOLD),
        ),
        PlaybackState::Stopped => (
            "⏹",
            Style::default().fg(t.stop).add_modifier(Modifier::BOLD),
        ),
    };

    let (title, artist, time_label, ratio) = if let Some(ref cur) = app.current_track {
        let total_elapsed = app.position_secs_f64();
        let total = app.playback_duration_secs;
        // The clock only advances while playing, so the ratio can be computed
        // directly, including while paused.
        let ratio = if total > 0 {
            (total_elapsed / total as f64).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let elapsed = total_elapsed.floor() as u64;
        let time = match app.playback_state {
            PlaybackState::Buffering => "buffering…".to_string(),
            _ => format!(
                "{:02}:{:02} / {}",
                elapsed / 60,
                elapsed % 60,
                cur.duration.as_deref().unwrap_or("--:--")
            ),
        };
        (cur.title.clone(), cur.artist.clone(), time, ratio)
    } else {
        (
            "Nothing playing".to_string(),
            String::new(),
            "--:-- / --:--".to_string(),
            0.0,
        )
    };

    let right = if time_label.is_empty() {
        String::new()
    } else {
        format!(" {} ", time_label)
    };
    let buffering = app.playback_state == PlaybackState::Buffering && app.current_track.is_some();
    let right_style = if buffering {
        Style::default().fg(t.warn)
    } else {
        Style::default().fg(t.dim)
    };
    let time_w = display_width(&right);

    // Track text gets what's left after the time and a minimum connector
    // length are reserved; the bar never shrinks below that.
    let bar_min = if width >= 80 {
        28
    } else if width >= 50 {
        16
    } else {
        8
    };
    let text_budget = width.saturating_sub(time_w + bar_min + 1);

    let ind_w = display_width(indicator) + 2;
    let mut spans = vec![Span::styled(format!(" {} ", indicator), indicator_style)];
    let mut used = ind_w;

    if let Some(cur) = &app.current_track {
        // Like indicator: accent when liked, muted otherwise (`h` toggles);
        // it takes two columns of the text budget.
        let heart = "♥ ";
        let heart_style = if cur.like == Some(true) {
            Style::default().fg(t.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(t.dim)
        };
        spans.push(Span::styled(heart, heart_style));
        used += display_width(heart);

        // Title in primary text, artist trailing in dim.
        let artist = truncate(&artist, 30);
        let tail_w = if artist.is_empty() {
            0
        } else {
            display_width(&artist) + 3
        };
        let title_budget = text_budget
            .saturating_sub(tail_w + display_width(heart))
            .max(1);
        let title = truncate(&title, title_budget);
        used += display_width(&title);
        spans.push(Span::styled(title, Style::default().fg(t.bright)));
        if !artist.is_empty() {
            spans.push(Span::styled(" ⟢ ", Style::default().fg(t.dim)));
            spans.push(Span::styled(artist, Style::default().fg(t.mark)));
            used += tail_w;
        }
    } else {
        used += display_width(&title);
        spans.push(Span::styled(title, Style::default().fg(t.dim)));
    }

    // The connector: accent fill, a bright playhead, a dim track ahead.
    let gap = usize::from(width > used + time_w);
    let bar_w = width.saturating_sub(used + gap + time_w);
    if bar_w >= 3 {
        if gap == 1 {
            spans.push(Span::raw(" "));
        }
        if app.current_track.is_some() {
            let head = ((ratio * (bar_w - 1) as f64).round() as usize).min(bar_w - 1);
            spans.push(Span::styled(
                "━".repeat(head),
                Style::default().fg(t.accent),
            ));
            spans.push(Span::styled(
                "●",
                Style::default()
                    .fg(t.accent_bright)
                    .add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(
                "─".repeat(bar_w - 1 - head),
                Style::default().fg(t.dim),
            ));
        } else {
            // Idle: an empty dim track, no playhead.
            spans.push(Span::styled("─".repeat(bar_w), Style::default().fg(t.dim)));
        }
    }
    spans.push(Span::styled(right, right_style));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Lyrics pane, borderless at the right: a one-line status while
/// loading/missing, otherwise lines with the current one highlighted and
/// scrolled to centre. It hugs its content, never stretched to full height.
fn draw_lyrics(f: &mut Frame, app: &mut App, area: Rect, full_height: bool) {
    let t = &app.theme;

    if area.height == 0 || area.width == 0 {
        return;
    }

    // No header: the pane opens straight with status or lines.
    let body = area;

    // One-line centred status, dim.
    let status: Option<&str> = if app.current_track.is_none() {
        Some("No track playing.")
    } else {
        match &app.lyrics_state {
            crate::lyrics::LyricsState::Loading => Some("Loading lyrics..."),
            crate::lyrics::LyricsState::Missing => Some("No lyrics available."),
            crate::lyrics::LyricsState::Failed => Some("Failed to load lyrics."),
            crate::lyrics::LyricsState::Ready(_) => None,
        }
    };
    if let Some(text) = status {
        // The section is one row — nothing of the pane is spent around it.
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(text, Style::default().fg(t.dim))))
                .alignment(Alignment::Center),
            Rect {
                y: body.y,
                height: 1,
                ..body
            },
        );
        return;
    }

    let crate::lyrics::LyricsState::Ready(lyrics) = &app.lyrics_state else {
        return;
    };

    // Two cells of padding on each side so wrapped rows share one left
    // edge and text never touches the pane's right edge.
    let inner = Rect {
        x: body.x + 2,
        width: body.width.saturating_sub(4),
        ..body
    };
    if inner.width == 0 {
        return;
    }

    let (rows, current) = lyric_rows(app.position_secs_f64(), lyrics);

    // In the maximized view a kitty terminal draws the lyrics at twice the
    // cell size. The ratatui cells underneath stay blank; the escapes go out
    // after the frame, like the album art.
    if full_height && crate::art::text_scaling() {
        draw_scaled_lyrics(f, app, inner, &rows, current);
        return;
    }

    // Long lines wrap; the row maths below counts wrapped rows so the
    // autoscroll can centre the current line.
    let wrap_w = inner.width as usize;
    let text_rows = |text: &str| -> usize { display_width(text).div_ceil(wrap_w).max(1) };

    let mut spans = Vec::with_capacity(rows.len());
    let mut rows_total = 0usize;
    let mut centre_row = 0usize;
    for (i, row) in rows.iter().enumerate() {
        match row {
            LyricRow::Text(text) => {
                if current == Some(i) {
                    centre_row = rows_total;
                }
                rows_total += text_rows(text);
                // Current line in the accent, past lines dim, upcoming in
                // `fg` — three flat colours, no fade.
                let dist = current.map_or(0, |c| i as i32 - c as i32);
                let mut style = Style::default().fg(lyric_colour(t, dist));
                // The maximized view bolds the whole block: terminal fonts
                // have one size, so weight carries the emphasis.
                if full_height || current == Some(i) {
                    style = style.add_modifier(Modifier::BOLD);
                }
                spans.push(Line::from(Span::styled(text.as_str(), style)));
            }
            LyricRow::Gap => {
                rows_total += 1;
                spans.push(Line::from(""));
            }
        }
    }

    // The section hugs its content: the block is only as tall as the
    // wrapped lines (capped by the pane), so short lyrics and every
    // state leave the rest of the column free instead of padding out
    // to full height.
    let inner = Rect {
        height: if full_height {
            inner.height
        } else {
            rows_total.min(inner.height as usize) as u16
        },
        ..inner
    };
    let offset = lyrics_row_offset(
        &mut app.lyrics_scroll,
        centre_row,
        rows_total,
        inner.height as usize,
    );
    app.lyrics_offset = offset;
    let mut para = Paragraph::new(spans)
        .wrap(Wrap { trim: false })
        .scroll((offset, 0));
    if full_height {
        para = para.alignment(Alignment::Left);
    }
    f.render_widget(para, inner);
}

/// One logical lyric line, or the gap a stanza break leaves behind.
enum LyricRow {
    Text(String),
    Gap,
}

/// Flatten synced or plain lyrics into rows and remember which row is current.
fn lyric_rows(pos: f64, lyrics: &crate::lyrics::Lyrics) -> (Vec<LyricRow>, Option<usize>) {
    match lyrics {
        crate::lyrics::Lyrics::Synced(lines) => {
            // Bisect the timestamps using mpv's sub-second clock so a line
            // flips exactly when its timestamp is reached, not up to a
            // second late from integer truncation.
            let idx = lines.partition_point(|(ts, _)| *ts <= pos) as i64 - 1;
            // A blank row only where the song pauses — verses stay
            // together, sections get air — instead of doubling every line.
            // Instrumental `♪` markers always close a section.
            let gap_after: Vec<bool> = (0..lines.len().saturating_sub(1))
                .map(|i| {
                    let (ts, text) = &lines[i];
                    lines[i + 1].0 - ts >= 6.0 || text.trim() == "♪" || text.trim().is_empty()
                })
                .collect();
            let mut rows = Vec::with_capacity(lines.len() * 2);
            let mut current = None;
            for (i, (_, text)) in lines.iter().enumerate() {
                if i as i64 == idx {
                    current = Some(rows.len());
                }
                rows.push(LyricRow::Text(text.clone()));
                if gap_after.get(i).copied().unwrap_or(false) {
                    rows.push(LyricRow::Gap);
                }
            }
            (rows, current)
        }
        crate::lyrics::Lyrics::Unsynced(lines) => {
            // Plain lyrics carry their own stanza breaks. Only when the
            // source is one dense block do we separate every line.
            let dense = !lines.iter().any(|l| l.trim().is_empty());
            let mut rows = Vec::with_capacity(lines.len() * 2);
            for (i, text) in lines.iter().enumerate() {
                rows.push(LyricRow::Text(text.clone()));
                if i + 1 < lines.len() && dense {
                    rows.push(LyricRow::Gap);
                }
            }
            (rows, None)
        }
    }
}

/// Terminal rows one scaled lyric line occupies: two for the 2× glyphs
/// plus one blank row of air between lines. Stanza gaps get an extra
/// unit so verses read as separate blocks.
const SCALED_LINE_H: usize = 3;

/// Current line in the accent, past lines dim, upcoming lines in `fg`.
fn lyric_colour(t: &crate::theme::Theme, dist: i32) -> Color {
    if dist == 0 {
        t.accent
    } else if dist < 0 {
        t.dim
    } else {
        t.fg
    }
}

/// Draw the maximized lyrics through kitty's text sizing protocol (OSC 66).
/// Each cell becomes a 2×2 block, so a line takes two terminal rows and half
/// the horizontal cells; the wrapping and scroll maths use those units.
fn draw_scaled_lyrics(
    f: &mut Frame,
    app: &mut App,
    inner: Rect,
    rows: &[LyricRow],
    current: Option<usize>,
) {
    if inner.height < 2 || inner.width < 4 {
        return;
    }
    // Keep ratatui's buffer blank so only the scaled text occupies the pane.
    f.render_widget(Clear, inner);

    let max_cells = (inner.width as usize / 2).max(1);
    let mut scaled: Vec<(String, i32)> = Vec::with_capacity(rows.len());
    let mut centre_row = 0usize;
    for (i, row) in rows.iter().enumerate() {
        match row {
            LyricRow::Text(text) => {
                if current == Some(i) {
                    centre_row = scaled.len();
                }
                let dist = current.map_or(0, |c| i as i32 - c as i32);
                for chunk in wrap_cells(text, max_cells) {
                    scaled.push((chunk, dist));
                }
            }
            // A stanza break is one empty line plus the line spacing.
            LyricRow::Gap => scaled.push((String::new(), -100)),
        }
    }

    let viewport = (inner.height as usize / SCALED_LINE_H).max(1);
    let offset =
        lyrics_row_offset(&mut app.lyrics_scroll, centre_row, scaled.len(), viewport) as usize;
    app.lyrics_offset = offset as u16;

    let t = &app.theme;
    let mut out = Vec::new();
    for (i, (text, dist)) in scaled.iter().enumerate().skip(offset).take(viewport) {
        if text.is_empty() {
            continue;
        }
        let width = display_width(text) * 2;
        if width > inner.width as usize {
            continue;
        }
        let y = inner.y + (i - offset) as u16 * SCALED_LINE_H as u16;
        if y + 2 > inner.y + inner.height {
            break;
        }
        let x = inner.x;
        out.extend_from_slice(format!("\x1b[{};{}H", y + 1, x + 1).as_bytes());
        let color = lyric_colour(t, *dist);
        out.extend_from_slice(b"\x1b[1m");
        out.extend_from_slice(sgr_fg(color).as_bytes());
        out.extend_from_slice(b"\x1b]66;s=2;");
        push_safe(&mut out, text);
        out.push(0x07);
    }
    out.extend_from_slice(b"\x1b[0m");

    // Leave the block alone unless the text, colours, or geometry changed;
    // clearing and redrawing it every frame makes the lyrics flicker.
    let area = (f.area().width, f.area().height);
    let sig = overlay_sig(&out, f.area(), inner);
    let rect = (inner.x, inner.y, inner.width, inner.height);
    if app.lyrics_rect == Some(rect) && app.lyrics_sig == Some(sig) {
        return;
    }
    // A resize already cleared the screen; don't erase over the new layout.
    if app.lyrics_area == Some(area) {
        if let Some((x, y, width, height)) = app.lyrics_rect {
            push_clear(
                &mut app.lyrics_clear,
                Rect {
                    x,
                    y,
                    width,
                    height,
                },
            );
        }
    }
    app.lyrics_output = out;
    app.lyrics_sig = Some(sig);
    app.lyrics_rect = Some(rect);
    app.lyrics_area = Some(area);
}

/// Erase the scaled block before ratatui draws the next frame, once the
/// maximized lyrics are no longer shown.
pub fn prepare_lyrics(app: &mut App, area: (u16, u16)) {
    if app.lyrics_rect.is_none() || scaled_view(app) {
        return;
    }
    if app.lyrics_area == Some(area) {
        if let Some((x, y, width, height)) = app.lyrics_rect {
            push_clear(
                &mut app.lyrics_clear,
                Rect {
                    x,
                    y,
                    width,
                    height,
                },
            );
        }
    }
    app.lyrics_rect = None;
    app.lyrics_sig = None;
    app.lyrics_area = None;
}

/// Is the maximized lyrics overlay active this frame?
fn scaled_view(app: &App) -> bool {
    app.now_playing
        && matches!(&app.lyrics_state, crate::lyrics::LyricsState::Ready(_))
        && crate::art::text_scaling()
}

/// Hash the scaled output and its geometry so an identical block can be
/// left on screen instead of being redrawn.
fn overlay_sig(out: &[u8], area: Rect, inner: Rect) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    out.hash(&mut h);
    (
        area.width,
        area.height,
        inner.x,
        inner.y,
        inner.width,
        inner.height,
    )
        .hash(&mut h);
    h.finish()
}

/// Wrap `text` into chunks that fit `max_cells` unscaled cells. Words are
/// kept whole where possible; anything longer is split by display width.
fn wrap_cells(text: &str, max_cells: usize) -> Vec<String> {
    let max_cells = max_cells.max(1);
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut out = Vec::new();
    let mut line = String::new();
    let mut line_w = 0usize;
    for word in text.split_whitespace() {
        let word_w = display_width(word);
        if word_w > max_cells {
            if !line.is_empty() {
                out.push(std::mem::take(&mut line));
                line_w = 0;
            }
            let mut chunk = String::new();
            let mut chunk_w = 0usize;
            for ch in word.chars() {
                let ch_w = UnicodeWidthChar::width(ch).unwrap_or(0);
                if chunk_w > 0 && chunk_w + ch_w > max_cells {
                    out.push(std::mem::take(&mut chunk));
                    chunk_w = 0;
                }
                chunk.push(ch);
                chunk_w += ch_w;
            }
            if !chunk.is_empty() {
                line = chunk;
                line_w = chunk_w;
            }
            continue;
        }
        let sep = usize::from(line_w > 0);
        if line_w > 0 && line_w + sep + word_w > max_cells {
            out.push(std::mem::take(&mut line));
            line_w = 0;
        }
        if line_w > 0 {
            line.push(' ');
            line_w += 1;
        }
        line.push_str(word);
        line_w += word_w;
    }
    if !line.is_empty() || out.is_empty() {
        out.push(line);
    }
    out
}

/// Erase a rectangle with ECH; this also removes any multicell glyph whose
/// top row intersects the area.
fn push_clear(out: &mut Vec<u8>, rect: Rect) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    for y in rect.y..rect.y + rect.height {
        out.extend_from_slice(
            format!("\x1b[{};{}H\x1b[{}X", y + 1, rect.x + 1, rect.width).as_bytes(),
        );
    }
}

/// Copy text into an OSC 66 payload, replacing control bytes that would end
/// the escape early.
fn push_safe(out: &mut Vec<u8>, text: &str) {
    for ch in text.chars() {
        if ch.is_control() {
            out.push(b' ');
        } else {
            let mut buf = [0u8; 4];
            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
        }
    }
}

/// A foreground SGR sequence for a ratatui colour.
fn sgr_fg(color: Color) -> String {
    match color {
        Color::Reset => "\x1b[39m".to_string(),
        Color::Black => "\x1b[30m".to_string(),
        Color::Red => "\x1b[31m".to_string(),
        Color::Green => "\x1b[32m".to_string(),
        Color::Yellow => "\x1b[33m".to_string(),
        Color::Blue => "\x1b[34m".to_string(),
        Color::Magenta => "\x1b[35m".to_string(),
        Color::Cyan => "\x1b[36m".to_string(),
        Color::Gray => "\x1b[37m".to_string(),
        Color::DarkGray => "\x1b[90m".to_string(),
        Color::LightRed => "\x1b[91m".to_string(),
        Color::LightGreen => "\x1b[92m".to_string(),
        Color::LightYellow => "\x1b[93m".to_string(),
        Color::LightBlue => "\x1b[94m".to_string(),
        Color::LightMagenta => "\x1b[95m".to_string(),
        Color::LightCyan => "\x1b[96m".to_string(),
        Color::White => "\x1b[97m".to_string(),
        Color::Rgb(r, g, b) => format!("\x1b[38;2;{};{};{}m", r, g, b),
        Color::Indexed(i) => format!("\x1b[38;5;{}m", i),
    }
}

/// The row offset to render: a fresh manual scroll (autoscroll pauses
/// for 3s after you scroll) clamped to the content, otherwise
/// centred on the current line.
fn lyrics_row_offset(
    scroll: &mut Option<(u16, Instant)>,
    centre_row: usize,
    rows_total: usize,
    viewport: usize,
) -> u16 {
    let max_off = rows_total.saturating_sub(viewport) as u16;
    let centred = || {
        centre_row
            .saturating_sub(viewport / 2)
            .min(max_off as usize) as u16
    };
    match *scroll {
        Some((off, at)) if at.elapsed() < Duration::from_secs(3) => off.min(max_off),
        Some(_) => {
            *scroll = None;
            centred()
        }
        None => centred(),
    }
}

/// Footer: status message left (red on errors), a compact keybinding
/// cheat-sheet right with the keys picked out in the glyph accent.
fn draw_playlist_picker(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    // A card centred on the main area: narrow enough to feel like a popup,
    // tall enough for a dozen rows plus its song line. Playlists that
    // don't fit keep scrolling under the List's own viewport.
    let width = 44.min((area.width.saturating_sub(4)) as usize) as u16;
    let height = ((app.playlists.len() as u16).min(12) + 4).min(area.height.saturating_sub(2));
    if width < 20 || height < 5 {
        return;
    }
    let rect = Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    );
    // Erase the page drawn beneath, then paint the card on top.
    f.render_widget(Clear, rect);
    f.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(t.dim))
            .title(Span::styled(
                " add to playlist ",
                Style::default().fg(t.accent2).add_modifier(Modifier::BOLD),
            )),
        rect,
    );

    let inner = rect.inner(Margin {
        vertical: 1,
        horizontal: 2,
    });
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(inner);
    // The song the picker holds: accent and cut to the card's width.
    let song = app
        .selected_track()
        .map(|track| track.title)
        .unwrap_or_default();
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            truncate(&song, rows[0].width as usize),
            Style::default().fg(t.accent2),
        ))),
        rows[0],
    );

    let text_w = rows[1].width.saturating_sub(1) as usize;
    let items: Vec<ListItem> = app
        .playlists
        .iter()
        .enumerate()
        .map(|(i, p)| sidebar_item(t, p, text_w, i == app.picker_index))
        .collect();
    let list = List::new(items).highlight_style(
        Style::default()
            .fg(t.selection_fg)
            .bg(t.selection_bg)
            .add_modifier(Modifier::BOLD),
    );
    let mut state = ListState::default();
    state.select(Some(app.picker_index));
    f.render_stateful_widget(list, rows[1], &mut state);
}

/// Centred modal for naming a new playlist (opened with `c`). It mirrors the
/// add-to-playlist card so the two playlist actions feel like one family.
fn draw_create_playlist_dialog(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let width = 52.min(area.width.saturating_sub(4) as usize) as u16;
    let height = 5.min(area.height.saturating_sub(2));
    if width < 24 || height < 5 {
        return;
    }
    let rect = Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    );
    // Erase the page drawn beneath, then paint the card on top.
    f.render_widget(Clear, rect);
    f.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(t.dim))
            .title(Span::styled(
                " create playlist ",
                Style::default().fg(t.accent2).add_modifier(Modifier::BOLD),
            )),
        rect,
    );

    let inner = rect.inner(Margin {
        vertical: 1,
        horizontal: 2,
    });
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // input line
            Constraint::Length(1), // gap
            Constraint::Min(0),    // hint
        ])
        .split(inner);

    // Input line: prompt glyph, the typed name (or a dim placeholder) and a
    // block cursor that stays present while the dialog owns the keys.
    let text_w = rows[0].width.saturating_sub(3) as usize;
    let mut spans = vec![Span::styled(
        "▸ ",
        Style::default().fg(t.mark).add_modifier(Modifier::BOLD),
    )];
    if app.create_playlist_query.is_empty() {
        spans.push(Span::styled(
            truncate("playlist name", text_w),
            Style::default().fg(t.dim),
        ));
    } else {
        spans.push(Span::styled(
            truncate(&app.create_playlist_query, text_w),
            Style::default().fg(t.fg),
        ));
    }
    spans.push(Span::styled("▌", Style::default().fg(t.accent)));
    f.render_widget(Paragraph::new(Line::from(spans)), rows[0]);

    // "enter create" takes the pywal accent (the same colour the footer
    // uses for keys) so the primary action stands out; "esc cancel" stays
    // plain white.
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "enter create",
                Style::default().fg(t.accent2).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" · ", Style::default().fg(t.dim)),
            Span::styled("esc cancel", Style::default().fg(Color::White)),
        ])),
        rows[2],
    );
}

/// Modal picker for choosing a browse shelf (opened with `b`).
fn draw_browse_picker(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let sections = &app.browse_picker_sections;
    // Same card geometry as the add-to-playlist picker: 44 columns wide,
    // a caption line, and up to a dozen rows.
    let width = 44.min((area.width.saturating_sub(4)) as usize) as u16;
    let height = ((sections.len() as u16).min(12) + 4).min(area.height.saturating_sub(2));
    if width < 20 || height < 5 || sections.is_empty() {
        return;
    }
    let rect = Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    );
    f.render_widget(Clear, rect);
    f.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(t.dim))
            .title(Span::styled(
                " browse shelves ",
                Style::default().fg(t.accent2).add_modifier(Modifier::BOLD),
            )),
        rect,
    );

    let inner = rect.inner(Margin {
        vertical: 1,
        horizontal: 2,
    });
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(inner);
    // The hotkey prompt, in the same place and accent the add picker uses
    // for the song it holds.
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "enter to open",
            Style::default().fg(t.accent2),
        ))),
        rows[0],
    );

    let text_w = rows[1].width.saturating_sub(1) as usize;
    let items: Vec<ListItem> = sections
        .iter()
        .enumerate()
        .map(|(i, &section)| {
            let label = crate::app::App::BROWSE_PICKER_LABELS
                .get(section)
                .copied()
                .unwrap_or("Shelf");
            let style = if i == app.browse_picker_index {
                Style::default()
                    .fg(t.selection_fg)
                    .bg(t.selection_bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(t.fg)
            };
            ListItem::new(Line::from(Span::styled(truncate(label, text_w), style)))
        })
        .collect();
    let list = List::new(items);
    let mut state = ListState::default();
    state.select(Some(app.browse_picker_index));
    f.render_stateful_widget(list, rows[1], &mut state);
}

/// Decorative connectors for the cheatsheet rows, in the stitched style of the
/// search rule. All are six cells wide so descriptions line up.
const HELP_CONNECTORS: [&str; 6] = ["━━━'.━", "━,'━━━", "━━━',━", "━.'━━━", "━━.'━━", "━'━━━."];

/// Scrollable keyboard cheatsheet (`?`). Rows come from the same binding table
/// that dispatches keys, so this screen cannot drift from the application.
fn draw_help_overlay(f: &mut Frame, app: &mut App, area: Rect) {
    let t = app.theme;
    let width = 82.min(area.width.saturating_sub(4) as usize) as u16;
    let height = area.height.saturating_sub(2);
    if width < 36 || height < 7 {
        return;
    }
    let rect = Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    );
    // Erase the page drawn beneath. This is a floating panel without a boxed
    // border: a title line over the scrolling content, then the footer.
    f.render_widget(Clear, rect);

    let inner = rect.inner(Margin {
        vertical: 1,
        horizontal: 2,
    });
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title
            Constraint::Min(1),    // shortcuts
            Constraint::Length(1), // footer
        ])
        .split(inner);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "keybinds",
            Style::default().fg(t.accent2).add_modifier(Modifier::BOLD),
        ))),
        rows[0],
    );
    let content = rows[1];
    let footer = rows[2];

    let sections = keymap::help_sections();
    let key_width = sections
        .iter()
        .flat_map(|section| section.lines.iter())
        .map(|line| display_width(&line.keys))
        .max()
        .unwrap_or(1);
    // Key column, two spaces, connector, two spaces; descriptions wrap into
    // the column after it. Wrapping here (rather than with Paragraph) keeps the
    // scroll offset, page size and percentage exact on narrow terminals.
    let description_x = key_width.saturating_add(10);
    let description_w = (content.width as usize)
        .saturating_sub(description_x)
        .max(8);

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut connector = 0usize;
    for (index, section) in sections.iter().enumerate() {
        if index > 0 {
            lines.push(Line::default());
        }
        for entry in &section.lines {
            let pad = key_width.saturating_sub(display_width(&entry.keys));
            let wrapped = wrap_words(entry.description, description_w);
            let (first, rest) = wrapped.split_first().expect("wrap_words is never empty");
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{}{}", " ".repeat(pad), entry.keys),
                    Style::default().fg(t.accent2).add_modifier(Modifier::BOLD),
                ),
                Span::styled("  ", Style::default()),
                Span::styled(
                    HELP_CONNECTORS[connector % HELP_CONNECTORS.len()],
                    Style::default().fg(t.dim),
                ),
                Span::styled("  ", Style::default()),
                Span::styled(first.clone(), Style::default().fg(t.fg)),
            ]));
            for continuation in rest {
                lines.push(Line::from(vec![
                    Span::raw(" ".repeat(description_x)),
                    Span::styled(continuation.clone(), Style::default().fg(t.fg)),
                ]));
            }
            connector += 1;
        }
    }

    let max_scroll = lines.len().saturating_sub(content.height as usize) as u16;
    app.help_max_scroll = max_scroll;
    app.help_viewport = content.height;
    let scroll = app.help_scroll.min(max_scroll);
    app.help_scroll = scroll;
    f.render_widget(Paragraph::new(lines).scroll((scroll, 0)), content);

    let percent = if max_scroll == 0 {
        100
    } else {
        (usize::from(scroll) * 100 / usize::from(max_scroll)).min(100)
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("{percent}%"),
            Style::default().fg(t.dim),
        ))),
        footer,
    );
    let mut hints = Vec::new();
    if max_scroll > 0 {
        hints.push(Span::styled(
            "↑↓",
            Style::default().fg(t.accent2).add_modifier(Modifier::BOLD),
        ));
        hints.push(Span::styled(" scroll ", Style::default().fg(t.dim)));
        hints.push(Span::styled("· ", Style::default().fg(t.dim)));
    }
    hints.push(Span::styled(
        "esc",
        Style::default().fg(t.accent2).add_modifier(Modifier::BOLD),
    ));
    hints.push(Span::styled(" / ", Style::default().fg(t.dim)));
    hints.push(Span::styled(
        "q",
        Style::default().fg(t.accent2).add_modifier(Modifier::BOLD),
    ));
    hints.push(Span::styled(" close", Style::default().fg(t.dim)));
    f.render_widget(
        Paragraph::new(Line::from(hints)).alignment(Alignment::Right),
        footer,
    );
}

fn draw_footer_bar(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;

    // `esc back` is shown on every page, including Library where it's a
    // no-op (there's nowhere to go back to) — a guide row that never
    // changes beats one that's pedantically accurate. Tab still only
    // cycles filters on Search.
    //
    // The mode toggle earns its slot on Library, where it's the way in, and
    // on a collection page while offline, where it's the way back.
    //
    // `d download` is permanent: the guide row under the progress bar should
    // teach the key the moment a song is highlighted and keep it in the same
    // place on every page, rather than appearing and vanishing with the
    // selection. Queue is the one exception — there `d` deletes, so it keeps
    // its own `d del` and doesn't advertise a download.
    //
    // `a add to playlist` rides beside it: every page that highlights a
    // song can hand it to a library playlist, so it sits in the permanent
    // guide with the other always-live keys.
    let mode: (&str, &str) = if app.offline {
        ("o", "online")
    } else {
        ("o", "offline")
    };
    let guide: Vec<(&str, &str)> = vec![
        ("d", "download"),
        ("a", "add"),
        ("c", "create"),
        ("space", "pause"),
        ("←→", "seek"),
        ("n p", "skip"),
        ("L", "like"),
        ("l", "lyrics"),
        ("m", "max art"),
        ("?", "help"),
        ("q", "quit"),
    ];
    let hints: Vec<(&str, &str)> = match app.current_page {
        Page::Queue => vec![
            ("enter", "play"),
            ("←→", "seek"),
            ("d", "del"),
            ("a", "add"),
            ("r", "remove"),
            ("J K", "move"),
            ("esc", "back"),
            ("L", "like"),
            ("l", "lyrics"),
            ("m", "max art"),
            ("b", "browse"),
            ("q", "quit"),
        ],
        Page::Search => {
            let mut hints = vec![
                ("/", "search"),
                ("tab", "filter"),
                ("b", "browse"),
                ("esc", "back"),
            ];
            hints.extend(guide);
            hints
        }
        Page::Library => {
            let mut hints = vec![
                ("tab", "sidebar"),
                ("esc", "back"),
                ("/", "search"),
                ("b", "browse"),
                mode,
            ];
            hints.extend(guide);
            hints
        }
        Page::Browse => {
            let mut hints = if app.discovery_page.is_some() && app.discovery_is_root() {
                vec![
                    ("tab", "sidebar"),
                    ("[ ]", "item"),
                    ("P", "play all"),
                    ("enter", "play/open"),
                    ("esc", "back"),
                ]
            } else {
                vec![("tab", "sidebar"), ("enter", "play/open"), ("esc", "back")]
            };
            hints.extend(guide);
            hints
        }
        // An open playlist can lose its highlighted song with `r`.
        Page::Playlist => {
            let mut hints = vec![("esc", "back"), ("r", "remove"), ("b", "browse")];
            if app.offline {
                hints.push(mode);
            }
            hints.extend(guide);
            hints
        }
    };
    // Compact spacing is used only when the full guide needs the room; wide
    // terminals keep the roomier original layout. The guide grew again with
    // `a` and `r`, so the roomier cutoff moved up with it.
    let compact = area.width < 128;
    let build_hints = |compact: bool, ultra: bool| {
        let separator = if compact { " " } else { "  " };
        let mut spans = Vec::new();
        let mut width = 0usize;
        if !compact {
            spans.push(Span::styled(" ", Style::default()));
            width += 1;
        }
        for (i, (key, action)) in hints.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(separator, Style::default()));
                width += display_width(separator);
            }
            let key_text = if compact && *key == "space" {
                "␣".to_string()
            } else {
                key.to_string()
            };
            let label_text = if compact && *action == "play/open" {
                " play".to_string()
            } else if ultra && *action == "download" {
                " dl".to_string()
            } else if ultra && *action == "lyrics" {
                " lyr".to_string()
            } else if ultra && *action == "max art" {
                " art".to_string()
            } else {
                format!(" {}", action)
            };
            width += display_width(&key_text) + display_width(&label_text);
            spans.push(Span::styled(
                key_text,
                Style::default().fg(t.accent2).add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(label_text, Style::default().fg(t.dim)));
        }
        if !compact {
            spans.push(Span::styled(" ", Style::default()));
            width += 1;
        }
        (spans, width)
    };
    let (mut hint_spans, hint_w) = build_hints(compact, false);
    let status_gap = if compact { 2 } else { 1 };
    // The status is drawn with one leading cell of breathing room, so leave
    // that cell out of the budget as well as the gap to the guide.
    let mut status_budget = (area.width as usize).saturating_sub(hint_w + status_gap + 1);
    if compact && status_budget < 8 {
        let (ultra_spans, ultra_w) = build_hints(true, true);
        // Shorten even when the status still cannot fit: a guide that fits
        // the row is worth more than one clipped mid-word.
        if ultra_w < hint_w {
            hint_spans = ultra_spans;
            status_budget = (area.width as usize).saturating_sub(ultra_w + status_gap + 1);
        }
    }

    // Errors ("Playback error…", "Failed to load…") get the bright red.
    // Otherwise the status picks up palette colours: numbers (the "3" in
    // "Added 3 tracks") go color5 bold like row numbers, lone symbols
    // (· — …) the accent, and ordinary words the title colour.
    let lower = app.status_message.to_lowercase();
    let is_error = lower.contains("error") || lower.contains("fail");
    let truncated = if status_budget >= 8 {
        truncate(&app.status_message, status_budget)
    } else {
        String::new()
    };

    let mut status_spans: Vec<Span> = Vec::new();
    if is_error {
        status_spans.push(Span::styled(truncated, Style::default().fg(t.err)));
    } else {
        for (i, token) in truncated.split_whitespace().enumerate() {
            if i > 0 {
                status_spans.push(Span::raw(" "));
            }
            let style = if token.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                Style::default().fg(t.mark).add_modifier(Modifier::BOLD)
            } else if !token.chars().any(|c| c.is_alphanumeric()) {
                Style::default().fg(t.accent).add_modifier(Modifier::BOLD)
            } else {
                // Ordinary words — including the song title a download is
                // previewing — take the palette's title colour (color6)
                // instead of fading into the dim grey.
                Style::default().fg(t.accent2)
            };
            status_spans.push(Span::styled(token, style));
        }
    }

    if !status_spans.is_empty() {
        let mut status_line = vec![Span::raw(" ")];
        status_line.extend(status_spans);
        f.render_widget(Paragraph::new(Line::from(status_line)), area);
    }
    f.render_widget(
        Paragraph::new(Line::from(hint_spans)).alignment(Alignment::Right),
        area,
    );
}

// ---------------------------------------------------------------------------
// Content pages
// ---------------------------------------------------------------------------

/// The slice of a one-row-per-item list that can actually be seen, centred on
/// the cursor. Building only this window keeps a 1,000-track queue as cheap to
/// draw as a 20-track one; Ratatui would discard the rest anyway.
fn visible_window(len: usize, selected: usize, viewport: u16) -> (usize, usize) {
    if len == 0 || viewport == 0 {
        return (0, 0);
    }
    let viewport = (viewport as usize).min(len);
    let selected = selected.min(len - 1);
    let start = selected.saturating_sub(viewport / 2).min(len - viewport);
    (start, start + viewport)
}

fn draw_content(f: &mut Frame, app: &mut App, area: Rect) {
    match app.current_page {
        Page::Search => draw_search(f, app, area),
        Page::Library => draw_library_detail(f, app, area),
        Page::Browse if app.discovery_page.is_some() && app.discovery_is_root() => {
            draw_discovery(f, app, area)
        }
        Page::Browse => draw_browse(f, app, area),
        Page::Playlist => draw_playlist_page(f, app, area),
        Page::Queue => draw_queue_page(f, app, area),
    }
}

/// Search: a filter-chip line (with the result count on the right), then the
/// result rows.
fn draw_search(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(area);

    // Chip line: filters on the left (only once a search has returned some),
    // the result count on the right.
    let mut spans = Vec::new();
    if !app.search_filters.is_empty() {
        for (i, label) in app.search_filter_labels().iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled("  ", Style::default()));
            }
            let style = if i == app.search_filter {
                Style::default()
                    .fg(t.accent_bright)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(t.dim)
            };
            spans.push(Span::styled(label.clone(), style));
        }
    }
    f.render_widget(Paragraph::new(Line::from(spans)), rows[0]);
    if !app.search_results.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("{} results", app.search_results.len()),
                Style::default().fg(t.dim),
            )))
            .alignment(Alignment::Right),
            rows[0],
        );
    }

    let width = rows[1].width as usize;
    let (start, end) = visible_window(app.search_results.len(), app.selected_index, rows[1].height);
    let items: Vec<ListItem> = if app.search_results.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            " Nothing here yet — press / to search.",
            Style::default().fg(t.dim),
        )))]
    } else {
        app.search_results[start..end]
            .iter()
            .enumerate()
            .map(|(offset, hit)| {
                let i = start + offset;
                let playing = match hit {
                    SearchHit::Track(tr) => {
                        app.current_track.as_ref().is_some_and(|c| c.id == tr.id)
                    }
                    _ => false,
                };
                let pct = match hit {
                    SearchHit::Track(tr) => app.download_pct(&tr.id),
                    _ => None,
                };
                hit_row(t, hit, width, i == app.selected_index, playing, pct)
            })
            .collect()
    };

    let list =
        List::new(items).highlight_style(Style::default().fg(t.selection_fg).bg(t.selection_bg));
    let mut state = ListState::default();
    if !app.search_results.is_empty() {
        state.select(Some(app.selected_index.saturating_sub(start)));
    }
    f.render_stateful_widget(list, rows[1], &mut state);
}

/// Render feed shelves as compact horizontal cards. Only the viewport is
/// rendered, and a focused shelf scrolls horizontally with its selection.
fn draw_discovery(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = &app.theme;
    let mut groups = Vec::new();
    let mut i = 0;
    while i < app.browse_rows.len() {
        let BrowseRow::Header(title) = &app.browse_rows[i] else {
            i += 1;
            continue;
        };
        let start = i + 1;
        let mut end = start;
        while end < app.browse_rows.len() && matches!(app.browse_rows[end], BrowseRow::Item(_)) {
            end += 1;
        }
        if start < end {
            groups.push((title, start, end));
        }
        i = end;
    }
    if groups.is_empty() {
        f.render_widget(Paragraph::new(app.status_message.as_str()), area);
        return;
    }
    let selected_group = groups
        .iter()
        .position(|(_, start, end)| (*start..*end).contains(&app.selected_index))
        .unwrap_or(0);
    let compact = |title: &str| {
        title.eq_ignore_ascii_case("filters")
            || title.eq_ignore_ascii_case("explore")
            || contains_ascii_ci(title, "mood")
            || contains_ascii_ci(title, "genre")
    };
    let group_height = |title: &str| -> usize {
        if area.height < 11 || area.width < 20 {
            4
        } else if compact(title) {
            5
        } else {
            11
        }
    };
    let mut first = selected_group;
    let mut occupied = group_height(groups[first].0);
    while first > 0 && occupied + group_height(groups[first - 1].0) <= usize::from(area.height) {
        first -= 1;
        occupied += group_height(groups[first].0);
    }
    let card_width = usize::from(area.width).clamp(1, 20);
    let columns = (usize::from(area.width) / card_width).max(1);
    // At most one cover is wanted per frame, and only when a fetch could start,
    // so a steady frame allocates no URL strings.
    let fetch_allowed = app.discovery_art_idle();
    let mut wanted_url: Option<&str> = None;
    let mut art_cards = 0usize;
    let mut top = usize::from(area.y);
    for (group_index, (title, start, end)) in groups.iter().enumerate().skip(first) {
        let y = top as u16;
        if y >= area.bottom() {
            break;
        }
        let height = group_height(title);
        let grid_rows = if compact(title) && height > 4 { 3 } else { 1 };
        let slots = columns * grid_rows;
        let count = end - start;
        let chosen = app.selected_index.saturating_sub(*start).min(count - 1);
        let offset = if group_index == selected_group {
            // Slide a row at a time so the selected card is always in view,
            // including in the compact three-row grids.
            chosen.saturating_sub(slots - 1).div_ceil(columns) * columns
        } else {
            0
        };
        let arrows = if count > slots { "  ‹ ›" } else { "" };
        let mut label = title.to_uppercase();
        label.push_str(arrows);
        f.render_widget(
            Paragraph::new(Span::styled(
                label,
                Style::default()
                    .fg(theme.accent_bright)
                    .add_modifier(Modifier::BOLD),
            )),
            Rect::new(area.x, y, area.width, 1),
        );
        for (slot, row) in (*start + offset..*end).take(slots).enumerate() {
            let BrowseRow::Item(hit) = &app.browse_rows[row] else {
                continue;
            };
            let (name, detail, url) = match hit {
                SearchHit::Track(track) => {
                    (&track.title, &track.artist, track.thumbnail_url.as_ref())
                }
                SearchHit::Collection(collection) => (
                    &collection.title,
                    &collection.subtitle,
                    collection.thumbnail_url.as_ref(),
                ),
            };
            let x = area
                .x
                .saturating_add(((slot % columns) * card_width) as u16);
            let width = area.right().saturating_sub(x).min(card_width as u16);
            if width == 0 || y + 1 >= area.bottom() {
                continue;
            }
            let selected = row == app.selected_index && !app.sidebar_focused;
            let style = if selected {
                Style::default()
                    .fg(theme.selection_fg)
                    .bg(theme.selection_bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.fg)
            };
            let art_rows = if height == 11 { 7u16 } else { 0 };
            if art_rows > 0
                && let Some(url) = url
            {
                art_cards += 1;
                // Beyond the cap a card shows the placeholder only, so the
                // visible covers always fit in the cache.
                let shown = art_cards <= crate::app::DISCOVERY_ART_VISIBLE_MAX;
                if shown && fetch_allowed && wanted_url.is_none() && !app.discovery_art_cached(url)
                {
                    wanted_url = Some(url.as_str());
                }
                if let Some(Some(img)) = app.discovery_art.get(url).filter(|_| shown) {
                    let art_width = width.min(16);
                    for py in 0..art_rows {
                        if y + 1 + py >= area.bottom() {
                            break;
                        }
                        let mut spans = Vec::with_capacity(usize::from(art_width));
                        for px in 0..art_width {
                            let x = u32::from(px) * 2;
                            let y = u32::from(py) * 2;
                            spans.push(quadrant_span(&[
                                *img.get_pixel(x, y),
                                *img.get_pixel(x + 1, y),
                                *img.get_pixel(x, y + 1),
                                *img.get_pixel(x + 1, y + 1),
                            ]));
                        }
                        f.render_widget(
                            Paragraph::new(Line::from(spans)),
                            Rect::new(x, y + 1 + py, art_width, 1),
                        );
                    }
                } else {
                    f.render_widget(
                        Paragraph::new("▧").style(Style::default().fg(theme.dim)),
                        Rect::new(x, y + 1, width, 1),
                    );
                }
            }
            let text_y = y + 1 + art_rows + (slot / columns) as u16;
            if text_y < area.bottom() {
                f.render_widget(
                    Paragraph::new(truncate(name, width as usize)).style(style),
                    Rect::new(x, text_y, width, 1),
                );
            }
            if grid_rows == 1 && text_y + 1 < area.bottom() {
                f.render_widget(
                    Paragraph::new(truncate(detail, width as usize))
                        .style(Style::default().fg(theme.dim)),
                    Rect::new(x, text_y + 1, width, 1),
                );
            }
        }
        top += height;
    }
    let wanted_url = wanted_url.map(str::to_owned);
    app.request_discovery_art(wanted_url.as_deref());
}

/// ASCII case-insensitive `contains` that allocates nothing.
fn contains_ascii_ci(haystack: &str, needle: &str) -> bool {
    haystack
        .as_bytes()
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
}

/// "For you": mixes, trending songs, community picks and the mood/genre
/// grids, grouped under section headers. Headers are visible but the cursor
/// only lands on real rows.
fn draw_browse(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    if app.browse_rows.is_empty() {
        let message = if app.browse_loaded {
            " Nothing here right now."
        } else {
            " Loading your mixes…"
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                message,
                Style::default().fg(t.dim),
            ))),
            area,
        );
        return;
    }

    let width = area.width as usize;
    let (start, end) = visible_window(app.browse_rows.len(), app.selected_index, area.height);
    let items: Vec<ListItem> = app.browse_rows[start..end]
        .iter()
        .enumerate()
        .map(|(offset, row)| {
            let i = start + offset;
            match row {
                BrowseRow::Header(title) => ListItem::new(Line::from(vec![
                    Span::styled(
                        format!(" {}", title.to_uppercase()),
                        Style::default()
                            .fg(t.accent_bright)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled("  ", Style::default().fg(t.dim)),
                ])),
                BrowseRow::Item(hit) => {
                    let playing = match hit {
                        SearchHit::Track(track) => {
                            app.current_track.as_ref().is_some_and(|c| c.id == track.id)
                        }
                        _ => false,
                    };
                    let pct = match hit {
                        SearchHit::Track(track) => app.download_pct(&track.id),
                        _ => None,
                    };
                    hit_row(t, hit, width, i == app.selected_index, playing, pct)
                }
            }
        })
        .collect();
    let list =
        List::new(items).highlight_style(Style::default().fg(t.selection_fg).bg(t.selection_bg));
    let mut state = ListState::default();
    state.select(Some(app.selected_index.saturating_sub(start)));
    f.render_stateful_widget(list, area, &mut state);
}

/// Library: the detail view of the highlighted sidebar entry — title, a dim
/// meta line, then its lazily-fetched track list (scrollable with PgUp/PgDn).
fn draw_library_detail(f: &mut Frame, app: &mut App, area: Rect) {
    let t = &app.theme;
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title
            Constraint::Length(1), // meta
            Constraint::Min(1),    // tracks
        ])
        .split(area);

    let width = rows[2].width as usize;
    let playlist = app.playlists.get(app.selected_index).cloned();

    if let Some(playlist) = &playlist {
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    format!("{} ", kind_glyph(playlist.kind)),
                    Style::default().fg(t.accent2).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    truncate(&playlist.title, width.saturating_sub(2)),
                    Style::default().fg(t.bright).add_modifier(Modifier::BOLD),
                ),
            ])),
            rows[0],
        );
        let meta = playlist_meta(playlist);
        if !meta.is_empty() {
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    truncate(&meta, width),
                    Style::default().fg(t.dim),
                ))),
                rows[1],
            );
        }

        let viewport = rows[2].height as usize;
        let showing_preview = app.preview_for.as_deref() == Some(playlist.id.as_str());
        let mut lines: Vec<Line> = Vec::new();
        let mut total_lines = 0usize;

        if showing_preview {
            if app.preview_tracks.is_empty() {
                // A local folder has no request in flight — an empty list
                // is the answer, not a wait.
                total_lines = 1;
                let waiting = !crate::offline::is_local(playlist.id.as_str());
                lines.push(Line::from(Span::styled(
                    if waiting {
                        " Loading…"
                    } else {
                        " Nothing here yet"
                    },
                    Style::default().fg(if waiting { t.warn } else { t.dim }),
                )));
            } else {
                total_lines = app.preview_tracks.len();
            }
        }

        // Record scroll bounds so PgUp/PgDn clamp correctly, then build only
        // the visible rows. The old code formatted every track in the list on
        // every frame, even though Paragraph discards what is off-screen.
        let max = total_lines.saturating_sub(viewport) as u16;
        app.details_scroll_max = max;
        app.details_viewport = viewport as u16;
        if app.details_scroll > max {
            app.details_scroll = max;
        }
        if showing_preview && !app.preview_tracks.is_empty() {
            let start = app.details_scroll as usize;
            let end = (start + viewport).min(total_lines);
            lines.reserve(end.saturating_sub(start));
            for (offset, track) in app.preview_tracks[start..end].iter().enumerate() {
                let playing = app.current_track.as_ref().is_some_and(|c| c.id == track.id);
                lines.push(track_line(
                    t,
                    start + offset + 1,
                    track,
                    width,
                    false,
                    playing,
                    app.download_pct(&track.id),
                ));
            }
        }
        f.render_widget(Paragraph::new(lines), rows[2]);
    } else {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                " Nothing selected.",
                Style::default().fg(t.dim),
            ))),
            rows[0],
        );
    }
}

/// A opened collection: one header line, then its numbered track list.
fn draw_playlist_page(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(area);

    let (title, kind) = app
        .current_playlist
        .as_ref()
        .map(|p| (p.title.clone(), p.kind_label()))
        .unwrap_or_else(|| ("Playlist".to_string(), "Playlist"));
    let width = rows[0].width as usize;
    let glyph = app
        .current_playlist
        .as_ref()
        .map(|p| kind_glyph(p.kind))
        .unwrap_or("≡");
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!("{} ", glyph),
                Style::default().fg(t.accent2).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                truncate(&title, width.saturating_sub(24)),
                Style::default().fg(t.bright).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" · {} · {} tracks", kind, app.playlist_tracks.len()),
                Style::default().fg(t.dim),
            ),
        ])),
        rows[0],
    );

    let list_w = rows[1].width as usize;
    let (start, end) = visible_window(
        app.playlist_tracks.len(),
        app.selected_index,
        rows[1].height,
    );
    let items: Vec<ListItem> = if app.playlist_tracks.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            " Nothing here yet.",
            Style::default().fg(t.dim),
        )))]
    } else {
        app.playlist_tracks[start..end]
            .iter()
            .enumerate()
            .map(|(offset, track)| {
                let i = start + offset;
                let playing = app.current_track.as_ref().is_some_and(|c| c.id == track.id);
                track_row(
                    t,
                    i + 1,
                    track,
                    list_w,
                    i == app.selected_index,
                    playing,
                    app.download_pct(&track.id),
                )
            })
            .collect()
    };
    let list =
        List::new(items).highlight_style(Style::default().fg(t.selection_fg).bg(t.selection_bg));
    let mut state = ListState::default();
    if !app.playlist_tracks.is_empty() {
        state.select(Some(app.selected_index.saturating_sub(start)));
    }
    f.render_stateful_widget(list, rows[1], &mut state);
}

/// The playback queue: a header with the queue's state, then the track
/// list. The currently playing row carries the green marker; Enter jumps
/// to a row, `d` deletes, `J`/`K` reorder.
fn draw_queue_page(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(area);

    let state = format!(
        "repeat {} · shuffle {}",
        app.repeat.label(),
        if app.shuffle { "on" } else { "off" }
    );
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "≡ ",
                Style::default().fg(t.accent2).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "Queue",
                Style::default().fg(t.bright).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" · {} tracks · {}", app.queue.len(), state),
                Style::default().fg(t.dim),
            ),
        ])),
        rows[0],
    );

    let list_w = rows[1].width as usize;
    let (start, end) = visible_window(app.queue.len(), app.selected_index, rows[1].height);
    let items: Vec<ListItem> = if app.queue.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            " Queue is empty — press X or Z on a track to queue it.",
            Style::default().fg(t.dim),
        )))]
    } else {
        app.queue[start..end]
            .iter()
            .enumerate()
            .map(|(offset, track)| {
                let i = start + offset;
                let playing = app.current_track.as_ref().is_some_and(|c| c.id == track.id);
                track_row(
                    t,
                    i + 1,
                    track,
                    list_w,
                    i == app.selected_index,
                    playing,
                    app.download_pct(&track.id),
                )
            })
            .collect()
    };
    let list =
        List::new(items).highlight_style(Style::default().fg(t.selection_fg).bg(t.selection_bg));
    let mut state = ListState::default();
    if !app.queue.is_empty() {
        state.select(Some(app.selected_index.saturating_sub(start)));
    }
    f.render_stateful_widget(list, rows[1], &mut state);
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

fn hit_row(
    theme: &crate::theme::Theme,
    hit: &SearchHit,
    width: usize,
    is_selected: bool,
    playing: bool,
    pct: Option<u8>,
) -> ListItem<'static> {
    match hit {
        SearchHit::Track(track) => track_row(theme, 0, track, width, is_selected, playing, pct),
        SearchHit::Collection(collection) => collection_row(theme, collection, width, is_selected),
    }
}

/// One row in a track list: a coloured index, the title, then a dim artist
/// and a bright duration clustered at the right edge. `number` of 0 renders
/// no index. The currently playing track is marked in green.
fn track_row(
    theme: &crate::theme::Theme,
    number: usize,
    track: &Track,
    width: usize,
    is_selected: bool,
    playing: bool,
    pct: Option<u8>,
) -> ListItem<'static> {
    ListItem::new(track_line(
        theme,
        number,
        track,
        width,
        is_selected,
        playing,
        pct,
    ))
}

fn track_line(
    theme: &crate::theme::Theme,
    number: usize,
    track: &Track,
    width: usize,
    is_selected: bool,
    playing: bool,
    pct: Option<u8>,
) -> Line<'static> {
    // Selected spans leave the foreground unset so they inherit the list's
    // highlight colours.
    let title_style = if is_selected {
        Style::default()
    } else if playing && number == 0 {
        // Numberless rows (search results) mark the playing track in the title.
        Style::default().fg(theme.ok_soft)
    } else {
        // Song titles carry the palette colour of the UI's lines (color6).
        Style::default().fg(theme.accent2)
    };
    let dim_style = if is_selected {
        Style::default()
    } else {
        Style::default().fg(theme.dim)
    };
    // Artists carry a palette colour (color5), like the playback line.
    let artist_style = if is_selected {
        Style::default()
    } else {
        Style::default().fg(theme.mark)
    };

    let mut spans = Vec::new();
    // The index, then straight into the title. The download status — a live
    // percentage while saving, the ✓ once it lands — sits at the title's
    // trailing edge instead, right where the song name ends.
    // Both read in plain bold white, like the index numbers, rather than
    // borrowing a palette accent.
    let status_style = if is_selected {
        Style::default()
    } else {
        Style::default().fg(theme.fg).add_modifier(Modifier::BOLD)
    };
    if number > 0 {
        // The index turns green while that track is playing, white otherwise.
        let index_style = if is_selected {
            Style::default()
        } else if playing {
            Style::default()
                .fg(theme.ok_soft)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg).add_modifier(Modifier::BOLD)
        };
        spans.push(Span::styled(format!("{:>3} ", number), index_style));
    }
    let avail = width.saturating_sub(spans.iter().map(|s| display_width(&s.content)).sum());

    let dur = track.duration.clone().unwrap_or_default();
    let dur_w = display_width(&dur);
    let show_dur = dur_w > 0 && avail >= dur_w + 16;

    let artist_cap = (width / 3).clamp(10, 28);
    let artist = truncate(&track.artist, artist_cap);
    let artist_w = display_width(&artist);
    let show_artist = artist_w > 0 && avail >= artist_w + dur_w + 14;

    // The album sits between the artist and the duration like a table
    // column. It peels off first when the row
    // narrows, then the artist, then the duration — the title always
    // keeps a workable budget.
    let album = truncate(
        track.album.as_deref().unwrap_or(""),
        (width / 3).clamp(8, 24),
    );
    let album_w = display_width(&album);
    let show_album = !album.is_empty() && show_artist && avail >= album_w + artist_w + dur_w + 18;

    let right_w = (if show_dur { dur_w } else { 0 })
        + (if show_artist { artist_w + 2 } else { 0 })
        + (if show_album { album_w + 2 } else { 0 });
    let title_w = avail.saturating_sub(right_w).max(1);
    // The download status hugs the title's trailing edge: ` 42%` while yt-dlp
    // is saving, ` ✓` once the file is on disk. It is carved out of the
    // title's own budget, so the artist/album/duration columns never move.
    let indicator = match (pct, track.downloaded) {
        (Some(p), _) => Some((format!(" {:>3}%", p.min(100)), status_style)),
        (None, true) => Some((" ✓".to_string(), status_style)),
        (None, false) => None,
    };
    let ind_w = indicator
        .as_ref()
        .map_or(0, |(text, _)| display_width(text));
    // On a very narrow row the title itself matters more than the status.
    let show_ind = ind_w > 0 && title_w > ind_w + 4;
    let ind_w = if show_ind { ind_w } else { 0 };
    let title = truncate(&track.title, title_w.saturating_sub(ind_w));

    let used = display_width(&title) + ind_w;
    spans.push(Span::styled(title, title_style));
    if show_ind {
        if let Some((text, style)) = indicator {
            spans.push(Span::styled(text, style));
        }
    }
    // Fill the rest of the title column so the right-hand meta stays put.
    spans.push(Span::styled(
        " ".repeat(title_w.saturating_sub(used)),
        title_style,
    ));
    if show_artist {
        spans.push(Span::styled(artist, artist_style));
        spans.push(Span::styled("  ", dim_style));
    }
    if show_album {
        // Secondary meta — dim, not the artist's colour.
        spans.push(Span::styled(album, dim_style));
        spans.push(Span::styled("  ", dim_style));
    }
    if show_dur {
        // Durations are plain — no palette colour.
        spans.push(Span::styled(dur, Style::default()));
    }
    Line::from(spans)
}

/// One row in a list of collections: kind glyph, title, dim subtitle.
fn collection_row(
    theme: &crate::theme::Theme,
    collection: &Playlist,
    width: usize,
    is_selected: bool,
) -> ListItem<'static> {
    let title_style = if is_selected {
        Style::default()
    } else {
        Style::default().fg(theme.fg)
    };
    let dim_style = if is_selected {
        Style::default()
    } else {
        Style::default().fg(theme.dim)
    };
    let glyph_style = if is_selected {
        Style::default()
    } else {
        Style::default()
            .fg(theme.accent2)
            .add_modifier(Modifier::BOLD)
    };

    let prefix = format!(" {} ", kind_glyph(collection.kind));
    let prefix_w = display_width(&prefix);
    let subtitle = truncate(&collection.subtitle, (width / 3).max(8));
    let sub_w = if subtitle.is_empty() {
        0
    } else {
        display_width(&subtitle) + 2
    };
    let title_w = width.saturating_sub(prefix_w + sub_w + 1).max(1);
    let title = truncate(&collection.title, title_w);

    let mut spans = vec![
        Span::styled(prefix, glyph_style),
        Span::styled(pad(&title, title_w), title_style),
    ];
    if sub_w > 0 {
        spans.push(Span::styled(subtitle, dim_style));
    }
    ListItem::new(Line::from(spans))
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn kind_glyph(kind: PlaylistKind) -> &'static str {
    match kind {
        PlaylistKind::Playlist => "≡",
        PlaylistKind::Album => "▣",
        PlaylistKind::Artist => "♪",
        PlaylistKind::Channel => "▸",
        PlaylistKind::Mood => "◐",
        PlaylistKind::Genre => "◆",
    }
}

/// Meta line for the Library detail header: the API's own subtitle when it
/// has one (it already reads "Playlist • Owner • N tracks"), else kind and
/// track count.
fn playlist_meta(playlist: &Playlist) -> String {
    if !playlist.subtitle.is_empty() {
        return playlist.subtitle.clone();
    }
    let mut parts: Vec<String> = vec![playlist.kind_label().to_string()];
    if let Some(n) = playlist.track_count {
        parts.push(format!("{} tracks", n));
    }
    parts.join(" · ")
}

fn truncate(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if display_width(s) <= max {
        return s.to_string();
    }
    // Truncate to `max - 1` columns, then append an ellipsis.
    let budget = max.saturating_sub(1);
    let mut out = String::new();
    let mut w = 0usize;
    for ch in s.chars() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw > budget {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push('…');
    out
}

/// Display width of a string in terminal columns (CJK/emoji count as two).
fn display_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Greedy word wrap used by the cheatsheet. Always returns at least one line.
fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if current.is_empty() {
            current.push_str(word);
        } else if display_width(&current) + 1 + display_width(word) <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
        }
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }
    lines
}

/// Pad `s` with trailing spaces up to `width` display columns.
fn pad(s: &str, width: usize) -> String {
    let mut out = s.to_string();
    let w = display_width(s);
    if w < width {
        out.push_str(&" ".repeat(width - w));
    }
    out
}

#[cfg(test)]
mod help_tests {
    use super::{App, draw};
    use ratatui::{Terminal, backend::TestBackend};

    fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
        let buffer = terminal.backend().buffer();
        let mut out = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[tokio::test]
    async fn help_overlay_lists_shortcuts_and_scrolls_to_the_end() {
        let mut app = App::new();
        app.open_help();
        let mut terminal = Terminal::new(TestBackend::new(80, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let top = buffer_text(&terminal);
        assert!(top.contains("keybinds"));
        assert!(top.contains("Ctrl+C"));
        assert!(top.contains("Open this cheatsheet"));
        assert!(top.contains("↑ / ↓"));
        assert!(!top.contains("Global"), "section headers are not rendered");

        app.help_end();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let end = buffer_text(&terminal);
        assert!(end.contains("Close the cheatsheet"));
        assert!(end.contains("100%"));
        assert!(
            !end.contains("Cheatsheet"),
            "section headers are not rendered"
        );
    }
}

#[cfg(test)]
mod header_tests {
    use super::header_account;
    use super::lastfm_badge;
    use crate::lastfm::Scrobbler;

    #[test]
    fn header_shows_the_account_name_when_known() {
        assert_eq!(header_account(false, Some("Test Account")), "Test Account");
    }

    #[test]
    fn header_shows_guest_without_a_valid_account() {
        assert_eq!(header_account(false, None), "Guest");
        assert_eq!(header_account(false, Some("")), "Guest");
        assert_eq!(header_account(false, Some("  \t\n ")), "Guest");
    }

    #[test]
    fn header_hides_a_stale_account_name_while_offline() {
        assert_eq!(header_account(true, Some("Test Account")), "Offline");
        assert_eq!(header_account(true, None), "Offline");
    }

    #[test]
    fn header_keeps_malformed_names_on_one_line() {
        assert_eq!(
            header_account(false, Some(" Test\nAccount ")),
            "Test Account"
        );
        assert_eq!(header_account(false, Some("Test\tAccount")), "Test Account");
    }

    /// A configured Last.fm account always shows the badge in the accent
    /// colour — there is no dimmed "off" state.
    #[test]
    fn the_lastfm_badge_is_accent_when_configured() {
        let theme = crate::theme::Theme::default();
        let scrobbler = Scrobbler::new(true, Some("tester".to_string()));
        let span = lastfm_badge(&scrobbler, &theme).expect("badge");
        assert_eq!(span.content, "  · last.fm:tester");
        assert_eq!(span.style.fg, Some(theme.accent));

        // Without a configured account there is no badge at all.
        assert!(lastfm_badge(&Scrobbler::default(), &theme).is_none());
    }
}

#[cfg(test)]
mod discovery_render_tests {
    use super::*;
    use crate::app::DiscoveryPage;
    use ratatui::{Terminal, backend::TestBackend};

    fn collection(title: &str) -> BrowseRow {
        BrowseRow::Item(SearchHit::Collection(Playlist {
            id: title.into(),
            title: title.into(),
            subtitle: "Artists".into(),
            track_count: None,
            thumbnail_url: None,
            kind: PlaylistKind::Playlist,
        }))
    }

    #[tokio::test]
    async fn sidebar_navigation_stays_visible_when_library_scrolls() {
        let mut app = App::new();
        app.current_page = Page::Library;
        app.playlists = (0..40)
            .map(|i| Playlist {
                id: format!("id{i}"),
                title: format!("Playlist {i}"),
                subtitle: String::new(),
                track_count: None,
                thumbnail_url: None,
                kind: PlaylistKind::Playlist,
            })
            .collect();
        app.selected_index = 39;
        let mut terminal = Terminal::new(TestBackend::new(30, 12)).unwrap();
        terminal
            .draw(|frame| draw_sidebar(frame, &app, Rect::new(0, 0, 30, 12)))
            .unwrap();
        let buf = terminal.backend().buffer();
        let text = (0..12)
            .map(|y| (0..30).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("Home") && text.contains("Explore") && text.contains("Library"),
            "{text}"
        );
        assert!(text.contains("Playlist 39"), "{text}");
    }

    #[tokio::test]
    async fn wide_terminals_cap_covers_so_the_cache_cannot_thrash() {
        let mut app = App::new();
        app.current_page = Page::Browse;
        app.discovery_page = Some(DiscoveryPage::Home);
        app.browse_rows
            .push(BrowseRow::Header("Mixed for you".into()));
        for i in 0..30 {
            let url = format!("https://example.invalid/{i}");
            app.browse_rows
                .push(BrowseRow::Item(SearchHit::Collection(Playlist {
                    id: format!("id{i}"),
                    title: format!("Mix {i}"),
                    subtitle: String::new(),
                    track_count: None,
                    thumbnail_url: Some(url.clone()),
                    kind: PlaylistKind::Playlist,
                })));
            app.discovery_art
                .insert(url, Some(image::RgbaImage::new(32, 16)));
        }
        app.selected_index = 1;
        let mut terminal = Terminal::new(TestBackend::new(400, 30)).unwrap();
        terminal
            .draw(|frame| draw_discovery(frame, &mut app, Rect::new(0, 0, 400, 30)))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let cap = crate::app::DISCOVERY_ART_VISIBLE_MAX as u16;
        // Cards are 20 columns wide; the cover starts one row under the title.
        assert_ne!(buffer[(0, 1)].symbol(), "▧");
        assert_ne!(buffer[((cap - 1) * 20, 1)].symbol(), "▧");
        assert_eq!(buffer[(cap * 20, 1)].symbol(), "▧");
        assert!(app.discovery_art_idle(), "all covers were already cached");
    }

    #[tokio::test]
    async fn selected_cards_scroll_into_view_at_the_end_of_long_shelves() {
        let mut app = App::new();
        app.current_page = Page::Browse;
        app.discovery_page = Some(DiscoveryPage::Home);
        app.browse_rows
            .push(BrowseRow::Header("Mixed for you".into()));
        for i in 0..30 {
            let title = format!("Mix {i}");
            app.browse_rows
                .push(BrowseRow::Item(SearchHit::Collection(Playlist {
                    id: title.clone(),
                    title,
                    subtitle: String::new(),
                    track_count: None,
                    thumbnail_url: None,
                    kind: PlaylistKind::Playlist,
                })));
        }
        app.selected_index = 30;
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| draw_discovery(frame, &mut app, Rect::new(0, 0, 80, 24)))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let text = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Mix 29"), "{text}");

        // Compact groups use three rows of cards; the last row must still
        // slide into view when the selection enters it.
        let mut app = App::new();
        app.current_page = Page::Browse;
        app.discovery_page = Some(DiscoveryPage::Home);
        app.browse_rows
            .push(BrowseRow::Header("Moods & genres".into()));
        for i in 0..20 {
            let title = format!("Mood {i}");
            app.browse_rows
                .push(BrowseRow::Item(SearchHit::Collection(Playlist {
                    id: title.clone(),
                    title,
                    subtitle: String::new(),
                    track_count: None,
                    thumbnail_url: None,
                    kind: PlaylistKind::Mood,
                })));
        }
        app.selected_index = 13;
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| draw_discovery(frame, &mut app, Rect::new(0, 0, 80, 24)))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let text = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Mood 12"), "{text}");
    }

    #[tokio::test]
    async fn home_displays_shelf_items_horizontally_and_survives_small_terminals() {
        let mut app = App::new();
        app.current_page = Page::Browse;
        app.discovery_page = Some(DiscoveryPage::Home);
        app.browse_rows = vec![
            BrowseRow::Header("Mixed for you".into()),
            collection("My Mix 1"),
            collection("My Mix 2"),
        ];
        app.selected_index = 1;
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| draw_discovery(frame, &mut app, Rect::new(0, 0, 80, 24)))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let line = (0..80).map(|x| buffer[(x, 8)].symbol()).collect::<String>();
        assert!(
            line.contains("My Mix 1") && line.contains("My Mix 2"),
            "{line}"
        );
        let mut small = Terminal::new(TestBackend::new(18, 4)).unwrap();
        small
            .draw(|frame| draw_discovery(frame, &mut app, Rect::new(0, 0, 18, 4)))
            .unwrap();
    }
}
