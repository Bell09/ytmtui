//! Minimal, chrome-free rendering: no box borders anywhere. Structure comes
//! from thin hairlines, whitespace and right-aligned metadata; a one-cell
//! gutter keeps everything off the terminal edges; colour comes from the
//! pywal palette (accent for active things, dim for everything secondary).

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};
use std::time::Duration;
use crate::app::{App, InputMode, PlaybackState};
use crate::models::{Page, Playlist, PlaylistKind, SearchHit, Track};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

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

pub fn draw(f: &mut Frame, app: &mut App) {
    // Title line, main (search line above the list and content, with
    // the lyrics pane running their full height beside them), one
    // playback line (track + connector bar), footer — compact: no
    // padding on the top or bottom edge, a one-cell gutter left and
    // right.
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
            Constraint::Length(1), // footer
        ])
        .split(area);

    let sidebar_w = sidebar_width(rows[1].width);
    // With the lyrics pane open it claims the right edge — the
    // `[playlists | content | lyrics]` layout, borderless beside the content.
    // It only appears when there is room for a readable pane plus the
    // content's minimum width.
    let lyrics_w = if app.lyrics_open {
        rows[1]
            .width
            .saturating_sub(sidebar_w + 1 + 10)
            .min(40)
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

    // Left of the lyrics the search line and its rule take the top two
    // rows of the main area; the list and content keep their rows below
    // them. The pane runs the main area's full height — its first line
    // lands level with the search input, not under the header.
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

    // Sidebar column: the list on top, cover art below it — the art sits
    // directly above the playback line while a track is loaded, and the
    // list reclaims the space whenever there is no art to show.
    let mut list_area = body[0];
    let mut art_area: Option<Rect> = None;
    let current_id = app.current_track.as_ref().map(|t| t.id.as_str());
    if app.art_track.as_deref().is_some_and(|id| Some(id) == current_id) {
        // A floating look: the picture keeps a two-cell margin of empty
        // space around its block plus a one-cell inset inside it, and is
        // capped small enough to read as a card, never touching the list
        // above or the playback line below.
        const PAD: u16 = 2;
        const MAX_W: u16 = 22;
        let (cw, ch) = (body[0].width, body[0].height);
        let art_w = cw.saturating_sub(PAD * 2).min(MAX_W) & !1; // even → clean aspect
        // The square core: the picture keeps one empty column of side
        // margin inside the block, and its height in rows follows the
        // terminal's real cell proportions — cols·cell_w = rows·cell_h —
        // so the cover lands square on screen. Cells twice as tall as
        // wide (the classic assumption) give the familiar rows = cols/2;
        // a proportional font's taller cells get fewer rows, which is
        // what keeps kitty from letterboxing dead bands around it.
        let core = art_w.saturating_sub(2);
        let (cell_w, cell_h) = crate::art::cell_size();
        let art_rows = ((f32::from(core) * cell_w / cell_h).round() as u16)
            .max(1)
            .min(ch / 2);
        if art_rows >= 4 && ch >= art_rows + PAD * 2 + 6 {
            let parts = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(4),                       // list
                    Constraint::Length(1),                    // gap
                    Constraint::Length(art_rows + PAD * 2),   // padded block
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
        draw_lyrics(f, app, cols[3]);
    }

    draw_playback_bar(f, app, rows[2]);
    draw_footer_bar(f, app, rows[3]);
}

/// Header: the title line (app name + signed-in account) and the search
/// input box. They are separate areas — the title spans the full width,
/// the search line only the columns left of the lyrics pane, so the
/// pane's first line lands level with the input.
fn draw_header(f: &mut Frame, app: &App, title: Rect, search: Rect) {
    let t = &app.theme;
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // search input line
            Constraint::Length(1), // underline rule
        ])
        .split(search);

    // Title line: app name left, account name right. The offline chip sits
    // against the app name while it's on, so the source of music on screen
    // is never in doubt.
    let mut title_spans = vec![Span::styled(
        " ytmtui",
        Style::default().fg(t.brand).add_modifier(Modifier::BOLD),
    )];
    if app.offline {
        title_spans.push(Span::styled(
            "  offline",
            Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(title_spans)), title);
    if let Some(name) = &app.account_name {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("{} ", name),
                Style::default().fg(t.dim),
            )))
            .alignment(Alignment::Right),
            title,
        );
    }

    // The search input — a text line sitting on an underline rule; no
    // left/right/top borders. The rule keeps a fixed width and never
    // stretches across the header.
    let focused = app.input_mode == InputMode::Search;
    let input_w = usize::from(rows[0].width).min(136);

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
    // label + arrow reserve nine columns; keep a margin before the rule's
    // end so the text never sits flush against it.
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

    // The underline rule below the input.
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "─".repeat(input_w),
            Style::default().fg(t.accent2),
        ))),
        rows[1],
    );
}

/// The always-visible Library playlist list. Rows carry a kind glyph, the
/// name, and a right-aligned track count; the selected row gets a `▌` cursor
/// and the pywal selection colours.
fn draw_sidebar(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;

    // The cursor only makes sense on the Library page, where the list is what
    // the selection controls; elsewhere the sidebar is informational.
    let show_cursor = app.current_page == Page::Library && !app.playlists.is_empty();

    // Item text width: minus the 2-column cursor column and a left pad.
    let text_w = (area.width as usize).saturating_sub(3);

    let mut items = vec![ListItem::new(Line::from(vec![
        Span::styled(" LIBRARY", Style::default().fg(t.bright).add_modifier(Modifier::BOLD)),
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
        let is_selected = show_cursor && i == app.selected_index;
        items.push(sidebar_item(t, playlist, text_w, is_selected));
    }

    let list = List::new(items).highlight_style(
        Style::default()
            .fg(t.selection_fg)
            .bg(t.selection_bg)
            .add_modifier(Modifier::BOLD),
    ).highlight_symbol("▌");
    let mut state = ListState::default();
    if show_cursor {
        state.select(Some(app.selected_index + 1)); // +1 for the title row
    }
    f.render_stateful_widget(list, area, &mut state);
}

fn sidebar_item(
    theme: &crate::theme::Theme,
    playlist: &Playlist,
    text_w: usize,
    is_selected: bool,
) -> ListItem<'static> {
    // When selected, spans leave the foreground unset so they inherit the
    // highlight colours set by the list.
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
    // The track count, right-aligned.
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

/// Cover art in the sidebar column, flush above the playback line. In a
/// kitty terminal it is a true inline image (graphics protocol); in
/// other terminals it is drawn with quadrant blocks at two pixels per
/// cell in both axes. The cover keeps its own colours — it is a photo,
/// not a palette element. While the fetch is still in flight the space
/// holds a dim cover glyph.
fn draw_album_art(f: &mut Frame, app: &mut App, area: Rect) {
    let (w, rows) = (area.width, area.height);

    if crate::art::graphics_mode() == crate::art::Graphics::Kitty {
        // True inline image: transmit once per (track, cell size, terminal
        // size); kitty keeps it placed between frames — but drops every
        // placement when the window is resized, so a fullscreen toggle
        // must retransmit. The escape bytes are flushed after each frame
        // is written (see main's event loop).
        if let Some(id) = app.art_track.clone() {
            if let Some(src) = app.art_source.as_ref() {
                let frame = f.area();
                let key = (id, w, rows, frame.width, frame.height);
                if app.art_kitty_sent.as_ref() != Some(&key) {
                    // The square cover shows in full — it occupies the
                    // block's square core (cells are ~2:1), letterboxed
                    // by the empty columns on either side.
                    let core = w.saturating_sub(2);
                    let px = crate::art::fit_square(src, 720, 720);
                    if let Some(png) = crate::art::png_bytes(&px) {
                        app.art_output = crate::art::kitty_transfer(
                            area.x + (w - core) / 2,
                            area.y,
                            core,
                            rows,
                            720,
                            720,
                            &png,
                        );
                        app.art_kitty_sent = Some(key);
                    }
                }
            } else {
                draw_art_placeholder(f, app, area);
            }
        }
        return;
    }

    // Quadrant blocks: the canvas carries two pixels per cell in both
    // axes. The square cover fills the block's square core — letterboxed
    // by untouched (background-coloured) cells on either side.
    let core = w.saturating_sub(2);
    if app.art_fitted.as_ref().map(|(key, _)| *key) != Some((core, rows)) {
        app.art_fitted = None;
    }
    if app.art_fitted.is_none() {
        app.art_fitted = app.art_source.as_ref().map(|src| {
            (
                (core, rows),
                crate::art::fit_square(
                    src,
                    u32::from(core) * 2,
                    u32::from(rows) * 2,
                ),
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

/// One terminal cell assembled from four pixels: the majority colour
/// becomes the background, the minority pixels are painted with a
/// quadrant (or half-block) glyph in the foreground. Three-of-a-kind
/// flips the roles so a lone pixel can still be shown exactly.
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

/// Playback, all on one line: indicator + track, with the progress bar
/// running out of the artist's name into the time readout — the bar *is*
/// the connector. The bar is always present: dim and empty when nothing is
/// playing, so the line keeps its shape. The title gives up characters
/// before the bar ever gets too short to read.
fn draw_playback_bar(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let width = area.width as usize;

    // The indicator and its colour come straight from the playback state:
    // buffering = yellow, playing = bright green, paused = bright yellow,
    // stopped = red.
    let (indicator, indicator_style) = match app.playback_state {
        PlaybackState::Buffering => (
            "⏳",
            Style::default().fg(t.warn).add_modifier(Modifier::BOLD),
        ),
        PlaybackState::Playing => (
            "▶",
            Style::default().fg(t.ok).add_modifier(Modifier::BOLD),
        ),
        PlaybackState::Paused => (
            "⏸",
            Style::default().fg(t.warn_bright).add_modifier(Modifier::BOLD),
        ),
        PlaybackState::Stopped => (
            "⏹",
            Style::default().fg(t.stop).add_modifier(Modifier::BOLD),
        ),
    };

    let (title, artist, time_label, ratio) = if let Some(ref cur) = app.current_track {
        let total_elapsed = app.position_secs();
        let total = app.playback_duration_secs;
        // The elapsed clock only advances while actually playing (see above),
        // so the ratio can be computed directly — including while paused.
        let ratio = if total > 0 {
            (total_elapsed as f64 / total as f64).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let time = match app.playback_state {
            PlaybackState::Buffering => "buffering…".to_string(),
            _ => format!(
                "{:02}:{:02} / {}",
                total_elapsed / 60,
                total_elapsed % 60,
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

    // Track text gets everything left over once the time and a minimum
    // connector length are reserved — the bar never shrinks below this.
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
        // The heart: the like indicator — accent when liked,
        // muted otherwise (`h` toggles it). It owns two columns of the
        // text budget.
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
            spans.push(Span::styled(" — ", Style::default().fg(t.dim)));
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
            spans.push(Span::styled(
                "─".repeat(bar_w),
                Style::default().fg(t.dim),
            ));
        }
    }
    spans.push(Span::styled(right, right_style));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The lyrics pane — the right sidebar, borderless beside the
/// content: a one-line status while loading/missing, otherwise the lines
/// with the current one highlighted and the view scrolled to centre it.
/// The section hugs its content — those rows only, never stretched to
/// the pane's full height.
fn draw_lyrics(f: &mut Frame, app: &mut App, area: Rect) {
    let t = &app.theme;

    if area.height == 0 || area.width == 0 {
        return;
    }

    // No header — the pane opens straight with the status or the lines.
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

    let crate::lyrics::LyricsState::Ready(lyrics) = app.lyrics_state.clone() else {
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

    // Long lines wrap; the row maths below counts wrapped rows so the
    // autoscroll can centre the current line.
    let wrap_w = inner.width as usize;
    let text_rows = |text: &str| -> usize { display_width(text).div_ceil(wrap_w).max(1) };

    let (spans, centre_row, rows_total): (Vec<Line>, usize, usize) = match &lyrics {
        crate::lyrics::Lyrics::Synced(lines) => {
            // Bisect the timestamps for the current line.
            let pos = app.position_secs() as f64;
            let idx = lines.partition_point(|(ts, _)| *ts <= pos) as i64 - 1;
            // A blank row only where the song pauses — verses stay
            // together, sections get air — instead of doubling every line.
            // Instrumental `♪` markers always close a section.
            let gap_after: Vec<bool> = (0..lines.len().saturating_sub(1))
                .map(|i| {
                    let (ts, text) = &lines[i];
                    lines[i + 1].0 - ts >= 6.0
                        || text.trim() == "♪"
                        || text.trim().is_empty()
                })
                .collect();
            let gaps_before = |n: usize| gap_after.iter().take(n).filter(|g| **g).count();
            let centre_row = lines
                .iter()
                .take(idx.max(0) as usize)
                .map(|(_, text)| text_rows(text))
                .sum::<usize>()
                + gaps_before(idx.max(0) as usize);
            let rows_total: usize = lines.iter().map(|(_, text)| text_rows(text)).sum::<usize>()
                + gap_after.iter().filter(|g| **g).count();
            let mut spans = Vec::with_capacity(lines.len() + 1);
            for (i, (_, text)) in lines.iter().enumerate() {
                let style = if (i as i64) < idx {
                    Style::default().fg(t.dim) // played
                } else if i as i64 == idx {
                    Style::default()
                        .fg(t.accent)
                        .add_modifier(Modifier::BOLD) // current
                } else {
                    Style::default().fg(t.fg) // upcoming
                };
                spans.push(Line::from(Span::styled(text.clone(), style)));
                if gap_after.get(i).copied().unwrap_or(false) {
                    spans.push(Line::from(""));
                }
            }
            (spans, centre_row, rows_total)
        }
        crate::lyrics::Lyrics::Unsynced(lines) => {
            // Plain lyrics carry their own stanza breaks. Only when the
            // source is one dense block do we separate every line.
            let dense = !lines.iter().any(|l| l.trim().is_empty());
            let gap = usize::from(dense);
            let rows_total: usize = lines.iter().map(|t| text_rows(t)).sum::<usize>()
                + lines.len().saturating_sub(1) * gap;
            let mut spans = Vec::with_capacity(lines.len() * 2);
            for (i, text) in lines.iter().enumerate() {
                spans.push(Line::from(Span::styled(
                    text.clone(),
                    Style::default().fg(t.fg),
                )));
                if i + 1 < lines.len() && dense {
                    spans.push(Line::from(""));
                }
            }
            (spans, 0, rows_total)
        }
    };

    // The section hugs its content: the block is only as tall as the
    // wrapped lines (capped by the pane), so short lyrics and every
    // state leave the rest of the column free instead of padding out
    // to full height.
    let inner = Rect {
        height: rows_total.min(inner.height as usize) as u16,
        ..inner
    };
    let offset = lyrics_row_offset(app, centre_row, rows_total, inner.height as usize);
    app.lyrics_offset = offset;
    f.render_widget(
        Paragraph::new(spans)
            .wrap(Wrap { trim: false })
            .scroll((offset, 0)),
        inner,
    );
}

/// The row offset to render: a fresh manual scroll (autoscroll pauses
/// for 3s after you scroll) clamped to the content, otherwise
/// centred on the current line.
fn lyrics_row_offset(app: &mut App, centre_row: usize, rows_total: usize, viewport: usize) -> u16 {
    let max_off = rows_total.saturating_sub(viewport) as u16;
    match app.lyrics_scroll {
        Some((off, at)) if at.elapsed() < Duration::from_secs(3) => off.min(max_off),
        Some(_) => {
            app.lyrics_scroll = None;
            centre_row
                .saturating_sub(viewport / 2)
                .min(max_off as usize) as u16
        }
        None => centre_row
            .saturating_sub(viewport / 2)
            .min(max_off as usize) as u16,
    }
}

/// Footer: status message left (red on errors), a compact keybinding
/// cheat-sheet right with the keys picked out in the glyph accent.
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
    let mode: (&str, &str) = if app.offline {
        ("o", "online")
    } else {
        ("o", "offline")
    };
    let guide: Vec<(&str, &str)> = vec![
        ("d", "download"),
        ("space", "pause"),
        ("←→", "seek"),
        ("n p", "skip"),
        ("h", "like"),
        ("l", "lyrics"),
        ("q", "quit"),
    ];
    let hints: Vec<(&str, &str)> = match app.current_page {
        Page::Queue => vec![
            ("enter", "play"),
            ("←→", "seek"),
            ("d", "del"),
            ("J K", "move"),
            ("esc", "back"),
            ("h", "like"),
            ("l", "lyrics"),
            ("q", "quit"),
        ],
        Page::Search => {
            let mut hints = vec![("/", "search"), ("tab", "filter"), ("esc", "back")];
            hints.extend(guide);
            hints
        }
        Page::Library => {
            let mut hints = vec![("esc", "back"), ("/", "search"), mode];
            hints.extend(guide);
            hints
        }
        // Every collection page: a song is highlighted, so `d` is live here.
        // Offline the mode toggle takes the slot before the permanent guide.
        _ => {
            let mut hints = vec![("esc", "back"), ("/", "search")];
            if app.offline {
                hints.push(mode);
            }
            hints.extend(guide);
            hints
        }
    };
    let mut hint_spans = vec![Span::styled(" ", Style::default())];
    let mut hint_w = 1usize;
    for (i, (key, action)) in hints.iter().enumerate() {
        if i > 0 {
            hint_spans.push(Span::styled("  ", Style::default()));
            hint_w += 2;
        }
        let key_text = key.to_string();
        let label_text = format!(" {}", action);
        hint_w += display_width(&key_text) + display_width(&label_text);
        hint_spans.push(Span::styled(
            key_text,
            Style::default().fg(t.accent2).add_modifier(Modifier::BOLD),
        ));
        hint_spans.push(Span::styled(label_text, Style::default().fg(t.dim)));
    }
    hint_spans.push(Span::styled(" ", Style::default()));
    hint_w += 1;

    // Errors ("Playback error…", "Failed to load…") get the bright red.
    // Otherwise the status picks up palette colours: numbers (the "3" in
    // "Added 3 tracks") go color5 bold like row numbers, lone symbols
    // (· — …) the accent, and ordinary words the title colour.
    let lower = app.status_message.to_lowercase();
    let is_error = lower.contains("error") || lower.contains("fail");
    let status_budget = (area.width as usize).saturating_sub(hint_w + 2);
    let truncated = truncate(&app.status_message, status_budget);

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

    let mut status_line = vec![Span::raw(" ")];
    status_line.extend(status_spans);
    f.render_widget(Paragraph::new(Line::from(status_line)), area);
    f.render_widget(
        Paragraph::new(Line::from(hint_spans)).alignment(Alignment::Right),
        area,
    );
}

// ---------------------------------------------------------------------------
// Content pages
// ---------------------------------------------------------------------------

fn draw_content(f: &mut Frame, app: &mut App, area: Rect) {
    match app.current_page {
        Page::Search => draw_search(f, app, area),
        Page::Library => draw_library_detail(f, app, area),
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
                Style::default().fg(t.accent_bright).add_modifier(Modifier::BOLD)
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
    let items: Vec<ListItem> = if app.search_results.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            " Nothing here yet — press / to search.",
            Style::default().fg(t.dim),
        )))]
    } else {
        app.search_results
            .iter()
            .enumerate()
            .map(|(i, hit)| {
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

    let list = List::new(items).highlight_style(
        Style::default()
            .fg(t.selection_fg)
            .bg(t.selection_bg),
    );
    let mut state = ListState::default();
    if !app.search_results.is_empty() {
        state.select(Some(app.selected_index));
    }
    f.render_stateful_widget(list, rows[1], &mut state);
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
                    Style::default()
                        .fg(t.accent2)
                        .add_modifier(Modifier::BOLD),
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

        let mut lines: Vec<Line> = Vec::new();
        if app.preview_for.as_deref() == Some(playlist.id.as_str()) {
            if app.preview_tracks.is_empty() {
                // A local folder has no request in flight — an empty list
                // is the answer, not a wait.
                let waiting = !crate::offline::is_local(playlist.id.as_str());
                lines.push(Line::from(Span::styled(
                    if waiting { " Loading…" } else { " Nothing here yet" },
                    Style::default().fg(if waiting { t.warn } else { t.dim }),
                )));
            } else {
                for (i, track) in app.preview_tracks.iter().enumerate() {
                    let playing = app.current_track.as_ref().is_some_and(|c| c.id == track.id);
                    lines.push(track_line(
                        t,
                        i + 1,
                        track,
                        width,
                        false,
                        playing,
                        app.download_pct(&track.id),
                    ));
                }
            }
        }

        // Record scroll bounds so PgUp/PgDn clamp correctly.
        let max = lines.len().saturating_sub(rows[2].height as usize) as u16;
        app.details_scroll_max = max;
        app.details_viewport = rows[2].height;
        if app.details_scroll > max {
            app.details_scroll = max;
        }
        f.render_widget(
            Paragraph::new(lines).scroll((app.details_scroll, 0)),
            rows[2],
        );
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
                format!(
                    " · {} · {} tracks",
                    kind,
                    app.playlist_tracks.len()
                ),
                Style::default().fg(t.dim),
            ),
        ])),
        rows[0],
    );

    let list_w = rows[1].width as usize;
    let items: Vec<ListItem> = if app.playlist_tracks.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            " Nothing here yet.",
            Style::default().fg(t.dim),
        )))]
    } else {
        app.playlist_tracks
            .iter()
            .enumerate()
            .map(|(i, track)| {
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
    let list = List::new(items).highlight_style(
        Style::default()
            .fg(t.selection_fg)
            .bg(t.selection_bg),
    );
    let mut state = ListState::default();
    if !app.playlist_tracks.is_empty() {
        state.select(Some(app.selected_index));
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
    let items: Vec<ListItem> = if app.queue.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            " Queue is empty — press X or Z on a track to queue it.",
            Style::default().fg(t.dim),
        )))]
    } else {
        app.queue
            .iter()
            .enumerate()
            .map(|(i, track)| {
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
    let list = List::new(items).highlight_style(
        Style::default()
            .fg(t.selection_fg)
            .bg(t.selection_bg),
    );
    let mut state = ListState::default();
    if !app.queue.is_empty() {
        state.select(Some(app.selected_index));
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
        SearchHit::Track(track) => {
            track_row(theme, 0, track, width, is_selected, playing, pct)
        }
        SearchHit::Collection(collection) => {
            collection_row(theme, collection, width, is_selected)
        }
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
    ListItem::new(track_line(theme, number, track, width, is_selected, playing, pct))
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
    // When selected, spans leave the foreground unset so they inherit the
    // highlight colours set by the list.
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
            Style::default().fg(theme.ok_soft).add_modifier(Modifier::BOLD)
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
    let album = truncate(track.album.as_deref().unwrap_or(""), (width / 3).clamp(8, 24));
    let album_w = display_width(&album);
    let show_album =
        !album.is_empty() && show_artist && avail >= album_w + artist_w + dur_w + 18;

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

/// Pad `s` with trailing spaces up to `width` display columns.
fn pad(s: &str, width: usize) -> String {
    let mut out = s.to_string();
    let w = display_width(s);
    if w < width {
        out.push_str(&" ".repeat(width - w));
    }
    out
}
