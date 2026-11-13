mod api;
mod app;
mod art;
mod auth;
mod auto_queue;
mod browser;
mod ipc;
mod keymap;
mod lastfm;
mod lyrics;
mod models;
mod mpris;
mod offline;
mod player;
mod session;
mod theme;
mod ui;

use app::{App, InputMode};
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use keymap::{Action, Context};
use models::Page;
use ratatui::{Terminal, backend::CrosstermBackend};
use std::{
    env, io,
    io::Write,
    time::{Duration, Instant},
};

fn print_help() {
    println!("ytmtui — a full-featured YouTube Music TUI client");
    println!();
    println!("Usage:");
    println!("  ytmtui                       Launch the interactive TUI");
    println!("  ytmtui setup [--manual]      Interactive setup for YouTube Music and Last.fm");
    println!("  ytmtui setup lastfm          Configure Last.fm scrobbling directly");
    println!("  ytmtui setup --browser NAME  Try extracting cookies from a browser");
    println!("  ytmtui status                Show accounts, Last.fm and playback tooling");
    println!("  ytmtui playlists             List your library playlists");
    println!("  ytmtui liked                 List your liked songs");
    println!("  ytmtui tracks <id>           List tracks of a playlist/album/artist");
    println!("  ytmtui help                  Show this help");
    println!();
    println!("Queue bridge (used by Quickshell's media widget):");
    println!("  ytmtui --json queue          Print the running player's queue");
    println!("  ytmtui jump <index>          Play queue index immediately");
    println!("  ytmtui --json now            Print now-playing info");
    println!("  ytmtui queue clear           Clear the queue");
}

/// Run a fire-and-forget bridge action against the running TUI (friendly
/// message, exit code 1 on failure).
fn bridge_action(command: &str, args: Option<serde_json::Value>) {
    match ipc::request(command, args) {
        Ok(resp) if resp["ok"] == true => {}
        Ok(resp) => bridge_error(resp),
        Err(_) => bridge_offline(),
    }
}

fn bridge_error(resp: serde_json::Value) -> ! {
    eprintln!(
        "Error: {}",
        resp["error"].as_str().unwrap_or("unknown error")
    );
    std::process::exit(1);
}

fn bridge_offline() -> ! {
    eprintln!("ytmtui is not running. Launch with `ytmtui` first.");
    std::process::exit(1);
}

// Two runtime threads are plenty for a UI that mostly waits on the network,
// and each fewer thread means one fewer glibc malloc arena holding on to
// freed pages.
#[tokio::main(worker_threads = 2)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // `--json` is accepted anywhere in the argv, so both
    // `ytmtui --json queue` and `ytmtui queue --json` work.
    let json_output = env::args().any(|a| a == "--json");
    let args: Vec<String> = env::args().filter(|a| a != "--json").collect();

    if args.len() > 1 {
        match args[1].as_str() {
            "setup" => {
                let mut manual = false;
                let mut browser: Option<String> = None;
                let mut service: Option<&str> = None;
                let mut i = 2;
                while i < args.len() {
                    match args[i].as_str() {
                        "--manual" => manual = true,
                        "--browser" if i + 1 < args.len() => {
                            browser = Some(args[i + 1].clone());
                            i += 1;
                        }
                        "lastfm" => service = Some("lastfm"),
                        "youtube" | "ytmusic" => service = Some("youtube"),
                        other => {
                            eprintln!("Unknown setup option: {}", other);
                            std::process::exit(1);
                        }
                    }
                    i += 1;
                }
                let result = if service == Some("lastfm") {
                    auth::run_lastfm_setup().await
                } else {
                    auth::run_setup(manual, browser).await
                };
                if let Err(e) = result {
                    eprintln!("Error: {}", e);
                    std::process::exit(1);
                }
                return Ok(());
            }
            "help" | "--help" | "-h" => {
                print_help();
                return Ok(());
            }
            "status" | "whoami" => {
                match auth::Auth::current_account().await {
                    Some(name) => println!("Signed in as: {}", name),
                    None => println!("Not signed in. Run `ytmtui setup` to sign in."),
                }
                println!("{}", lastfm::status_line());
                for line in player::tool_status_lines() {
                    println!("{line}");
                }
                return Ok(());
            }
            "playlists" | "library" => {
                let api = api::YtmApi::new();
                match api.library_playlists().await {
                    Ok(playlists) => {
                        for p in playlists {
                            let count = p
                                .track_count
                                .map(|n| n.to_string())
                                .unwrap_or_else(|| "-".to_string());
                            println!("{:>4}  {:<45}  {}", count, p.title, p.id);
                        }
                    }
                    Err(e) => {
                        eprintln!("Error: {}", e);
                        std::process::exit(1);
                    }
                }
                return Ok(());
            }
            "liked" => {
                let api = api::YtmApi::new();
                match api.liked_tracks().await {
                    Ok(tracks) => {
                        for t in tracks {
                            let duration = t.duration.unwrap_or_default();
                            println!("{:>6}  {} — {}", duration, t.title, t.artist);
                        }
                    }
                    Err(e) => {
                        eprintln!("Error: {}", e);
                        std::process::exit(1);
                    }
                }
                return Ok(());
            }
            "tracks" => {
                let Some(id) = args.get(2) else {
                    eprintln!("Usage: ytmtui tracks <playlistId>");
                    std::process::exit(1);
                };
                let api = api::YtmApi::new();
                match api.playlist_tracks(id).await {
                    Ok(tracks) => {
                        for t in tracks {
                            let duration = t.duration.unwrap_or_default();
                            println!("{:>6}  {} — {}", duration, t.title, t.artist);
                        }
                    }
                    Err(e) => {
                        eprintln!("Error: {}", e);
                        std::process::exit(1);
                    }
                }
                return Ok(());
            }
            // The widget bridge: query or steer the running TUI's queue.
            "queue" => {
                if args.iter().any(|a| a == "clear") {
                    bridge_action("queue_clear", None);
                    if !json_output {
                        println!("Queue cleared.");
                    }
                } else {
                    match ipc::request("queue", None) {
                        Ok(resp) if resp["ok"] == true => {
                            let data = &resp["data"];
                            if json_output {
                                println!("{}", serde_json::to_string_pretty(data)?);
                            } else {
                                println!(
                                    "queue: {} tracks · current {} · repeat {} · shuffle {}",
                                    data["length"],
                                    data["current_index"],
                                    data["repeat"].as_str().unwrap_or("?"),
                                    data["shuffle"]
                                );
                            }
                        }
                        Ok(resp) => bridge_error(resp),
                        Err(_) => bridge_offline(),
                    }
                }
                return Ok(());
            }
            "jump" => {
                let Some(index) = args.get(2).and_then(|s| s.parse::<u64>().ok()) else {
                    eprintln!("Usage: ytmtui jump <index>");
                    std::process::exit(1);
                };
                bridge_action("jump", Some(serde_json::json!({ "index": index })));
                return Ok(());
            }
            "now" => {
                match ipc::request("now", None) {
                    Ok(resp) if resp["ok"] == true => {
                        println!("{}", serde_json::to_string_pretty(&resp["data"])?);
                    }
                    Ok(resp) => bridge_error(resp),
                    Err(_) => bridge_offline(),
                }
                return Ok(());
            }
            "--version" | "-V" => {
                println!("ytmtui {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            other => {
                eprintln!("Unknown command: {}", other);
                print_help();
                std::process::exit(1);
            }
        }
    }

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app state instantly. The library fetch runs in the background
    // so the first frame appears immediately — an empty sidebar fills in when
    // the account's playlists arrive.
    let mut app = App::new();
    // Resume the previous queue and playback position before the first frame.
    app.restore_session();
    app.start_library_load();
    // Warm the shelf cache in the background so the first `b` opens at once.
    app.start_browse_refresh();

    // Draw whenever something animates (playback clock, buffering,
    // downloads) or a key arrives; an idle screen only needs a periodic
    // refresh so background results still appear promptly. This keeps the
    // 10Hz tick cheap without hurting input latency.
    let mut last_draw = Instant::now();
    let mut needs_redraw = true;

    // Main event loop
    while !app.should_quit {
        app.update();
        let animating = app.animating();
        if needs_redraw || animating || last_draw.elapsed() >= Duration::from_secs(1) {
            // Scaled lyrics are multicell glyphs; erase the previous block before
            // ratatui redraws underneath it once the scaled view is left.
            let size = terminal.size()?;
            ui::prepare_lyrics(&mut app, (size.width, size.height));
            if !app.lyrics_clear.is_empty() {
                let mut out = io::stdout();
                let _ = out.write_all(&app.lyrics_clear);
                let _ = out.flush();
                app.lyrics_clear.clear();
            }
            terminal.draw(|f| ui::draw(f, &mut app))?;
            // Kitty album art travels as escape bytes written after the frame
            // (the image floats above the cells ratatui keeps drawing).
            if !app.art_output.is_empty() {
                let mut out = io::stdout();
                let _ = out.write_all(&app.art_output);
                let _ = out.flush();
                app.art_output.clear();
            }
            // Changed scaled lyrics: clear the old block and paint the new one in
            // one write so the text never blinks out in between.
            if !app.lyrics_clear.is_empty() || !app.lyrics_output.is_empty() {
                let mut buf = Vec::with_capacity(app.lyrics_clear.len() + app.lyrics_output.len());
                buf.extend_from_slice(&app.lyrics_clear);
                buf.extend_from_slice(&app.lyrics_output);
                let mut out = io::stdout();
                let _ = out.write_all(&buf);
                let _ = out.flush();
                app.lyrics_clear.clear();
                app.lyrics_output.clear();
            }
            last_draw = Instant::now();
            needs_redraw = false;
        }

        if event::poll(Duration::from_millis(100))? {
            needs_redraw = true;
            if let Event::Key(key) = event::read()? {
                // Ctrl+C is global; every other key is resolved by the
                // context that currently owns the keyboard. The binding table
                // in `keymap` is shared with the `?` cheatsheet.
                let context = if app.help_open {
                    Context::Help
                } else if app.input_mode == InputMode::Search {
                    Context::Search
                } else if app.input_mode == InputMode::CreatePlaylist {
                    Context::CreatePlaylist
                } else if app.picker_open {
                    Context::AddPicker
                } else if app.browse_picker_open {
                    Context::BrowsePicker
                } else {
                    Context::Normal
                };
                let action = keymap::action_for(Context::Global, key.code, key.modifiers)
                    .or_else(|| keymap::action_for(context, key.code, key.modifiers));
                let Some(action) = action else { continue };

                match action {
                    Action::Quit => app.should_quit = true,
                    Action::OpenHelp => app.open_help(),
                    Action::CloseHelp => app.close_help(),
                    Action::HelpLineUp => app.scroll_help(-1),
                    Action::HelpLineDown => app.scroll_help(1),
                    Action::HelpPageUp => app.scroll_help_page(-1),
                    Action::HelpPageDown => app.scroll_help_page(1),
                    Action::HelpTop => app.help_home(),
                    Action::HelpBottom => app.help_end(),
                    Action::NextRow => app.next(),
                    Action::PreviousRow => app.previous(),
                    Action::Activate => app.activate_selected().await,
                    Action::Back => {
                        if app.sidebar_focused {
                            app.sidebar_focused = false;
                        } else if app.now_playing {
                            app.now_playing = false;
                        } else if app.current_page == Page::Browse {
                            app.leave_browse();
                        } else if app.current_page == Page::Playlist {
                            app.current_page = app.playlist_return_page;
                            app.selected_index = app.playlist_return_index;
                            app.sync_preview();
                        } else if app.current_page == Page::Queue {
                            app.leave_queue();
                            app.sync_preview();
                        } else if app.current_page == Page::Search {
                            app.current_page = Page::Library;
                            app.selected_index = 0;
                            app.sync_preview();
                        }
                    }
                    Action::OpenBrowseSection(section) => {
                        app.open_browse_section(section).await;
                    }
                    Action::OpenBrowsePicker => app.open_browse_picker().await,
                    Action::FocusSearch => {
                        app.current_page = Page::Search;
                        app.input_mode = InputMode::Search;
                        app.search_query.clear();
                    }
                    Action::CycleSearchFilter(forward) => {
                        if app.current_page == Page::Search {
                            app.cycle_search_filter(forward);
                        } else {
                            app.toggle_sidebar_focus();
                        }
                    }
                    Action::DiscoveryItem(forward) => app.discovery_item_step(forward),
                    Action::PlayDiscoveryShelf => app.play_discovery_shelf(),
                    Action::ScrollPage(delta) => {
                        if app.lyrics_open || app.now_playing {
                            app.scroll_lyrics(i32::from(delta) * 10);
                        } else if app.current_page == Page::Library {
                            app.scroll_details(isize::from(delta));
                        }
                    }
                    Action::ToggleOffline => app.toggle_offline(),
                    Action::TogglePlay => app.toggle_play(),
                    Action::SeekBack => app.seek_relative(-10),
                    Action::SeekForward => app.seek_relative(10),
                    Action::PlayNext => app.play_next(),
                    Action::PlayPrevious => app.play_previous(),
                    Action::ToggleLike => app.toggle_like(),
                    Action::ToggleLyrics => app.toggle_lyrics(),
                    Action::ToggleNowPlaying => app.toggle_now_playing(),
                    Action::ToggleAutoQueue => app.toggle_auto_queue(),
                    Action::CycleRepeat => app.cycle_repeat(),
                    Action::ToggleShuffle => app.toggle_shuffle(),
                    Action::OpenQueue => app.open_queue(),
                    Action::QueuePlayNext => app.queue_focused_next(),
                    Action::QueueAppend => {
                        if let Some(track) = app.selected_track() {
                            app.queue_append(track);
                        }
                    }
                    Action::QueueMoveDown => app.queue_move(true),
                    Action::QueueMoveUp => app.queue_move(false),
                    Action::DownloadOrDelete => {
                        // On the queue `d` deletes the row — its long-standing
                        // meaning. Anywhere else a song is highlighted, `d`
                        // saves it for offline and marks it with a check.
                        if app.current_page == Page::Queue {
                            app.queue_remove_selected();
                        } else {
                            app.download_selected();
                        }
                    }
                    Action::OpenAddPicker => app.open_playlist_picker(),
                    Action::StartCreatePlaylist => {
                        app.input_mode = InputMode::CreatePlaylist;
                        app.create_playlist_query.clear();
                    }
                    Action::RemoveFromPlaylist => app.remove_selected_from_playlist(),
                    Action::SearchSubmit => {
                        app.input_mode = InputMode::Normal;
                        app.perform_search();
                    }
                    Action::SearchCancel => {
                        app.input_mode = InputMode::Normal;
                        app.search_query.clear();
                    }
                    Action::SearchBackspace => {
                        app.search_query.pop();
                    }
                    Action::SearchInsert => {
                        if let KeyCode::Char(c) = key.code {
                            app.search_query.push(c);
                        }
                    }
                    Action::CreateSubmit => {
                        let title = std::mem::take(&mut app.create_playlist_query);
                        app.input_mode = InputMode::Normal;
                        app.create_playlist_submit(title);
                    }
                    Action::CreateCancel => {
                        app.input_mode = InputMode::Normal;
                        app.create_playlist_query.clear();
                    }
                    Action::CreateBackspace => {
                        app.create_playlist_query.pop();
                    }
                    Action::CreateInsert => {
                        if let KeyCode::Char(c) = key.code {
                            app.create_playlist_query.push(c);
                        }
                    }
                    Action::AddPickerNext => app.picker_next(),
                    Action::AddPickerPrevious => app.picker_previous(),
                    Action::AddPickerConfirm => app.picker_add_selected(),
                    Action::AddPickerCancel => app.close_playlist_picker(),
                    Action::BrowsePickerNext => app.browse_picker_next(),
                    Action::BrowsePickerPrevious => app.browse_picker_previous(),
                    Action::BrowsePickerConfirm => app.browse_picker_select().await,
                    Action::BrowsePickerCancel => app.close_browse_picker(),
                }
            }
        }
    }

    // Remove any inline album art, then restore the terminal.
    if app.art_kitty_sent.is_some() {
        let mut out = io::stdout();
        let _ = out.write_all(&crate::art::kitty_cleanup());
        let _ = out.flush();
    }
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}
