mod models;
mod api;
mod player;
mod app;
mod ui;
mod auth;
mod browser;
mod theme;
mod art;
mod mpris;
mod ipc;
mod lyrics;
mod offline;

use std::{env, io, io::Write, time::Duration};
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use app::{App, InputMode};
use models::Page;

fn print_help() {
    println!("ytmtui — a full-featured YouTube Music TUI client");
    println!();
    println!("Usage:");
    println!("  ytmtui                       Launch the interactive TUI");
    println!("  ytmtui setup [--manual]      Interactive authentication wizard");
    println!("  ytmtui setup --browser NAME  Try extracting cookies from a browser");
    println!("  ytmtui status                Show the signed-in account");
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

#[tokio::main]
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
                let mut i = 2;
                while i < args.len() {
                    match args[i].as_str() {
                        "--manual" => manual = true,
                        "--browser" if i + 1 < args.len() => {
                            browser = Some(args[i + 1].clone());
                            i += 1;
                        }
                        other => {
                            eprintln!("Unknown setup option: {}", other);
                            std::process::exit(1);
                        }
                    }
                    i += 1;
                }
                if let Err(e) = auth::run_setup(manual, browser).await {
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

    // Create app state instantly
    let mut app = App::new();

    // Load the library so the sidebar has playlist data, then show the
    // detail of the first entry.
    app.load_library().await;
    app.sync_preview();

    // Main event loop
    while !app.should_quit {
        app.update();
        terminal.draw(|f| ui::draw(f, &mut app))?;
        // Kitty album art travels as escape bytes written after the frame
        // (the image floats above the cells ratatui keeps drawing).
        if !app.art_output.is_empty() {
            let mut out = io::stdout();
            let _ = out.write_all(&app.art_output);
            let _ = out.flush();
            app.art_output.clear();
        }

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                // Global hotkeys
                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                    app.should_quit = true;
                    break;
                }

                // Text-entry mode captures all keys.
                if app.input_mode == InputMode::Search {
                    match key.code {
                        KeyCode::Enter => {
                            app.input_mode = InputMode::Normal;
                            app.perform_search().await;
                        }
                        KeyCode::Esc => {
                            app.input_mode = InputMode::Normal;
                            app.search_query.clear();
                        }
                        KeyCode::Backspace => {
                            app.search_query.pop();
                        }
                        KeyCode::Char(c) => {
                            app.search_query.push(c);
                        }
                        _ => {}
                    }
                    continue;
                }

                match key.code {
                    // Quit.
                    KeyCode::Char('q') => {
                        app.should_quit = true;
                    }
                    // Move within the current list.
                    KeyCode::Down => app.next(),
                    KeyCode::Up => app.previous(),
                    // Seek ±10s (only acts while music is playing or paused).
                    KeyCode::Left => app.seek_relative(-10),
                    KeyCode::Right => app.seek_relative(10),
                    // Skip through the queue.
                    KeyCode::Char('n') => app.play_next(),
                    KeyCode::Char('p') => app.play_previous(),
                    // Play/pause.
                    KeyCode::Char(' ') => app.toggle_play(),
                    // The queue page: `z` opens it, Esc leaves it.
                    KeyCode::Char('z') => app.open_queue(),
                    // Offline mode: the local music folder instead of the
                    // account's library. Press again to go back online.
                    KeyCode::Char('o') => app.toggle_offline().await,
                    // Queue the highlighted track:
                    // X plays it next, Z appends it.
                    KeyCode::Char('X') => app.queue_focused_next(),
                    KeyCode::Char('Z') => {
                        if let Some(t) = app.selected_track() {
                            app.queue_append(t);
                        }
                    }
                    // Queue page operations.
                    KeyCode::Char('d') => {
                        // On the queue `d` deletes the row — its long-standing
                        // meaning. Anywhere else a song is highlighted, `d`
                        // saves it for offline and marks it with a check.
                        if app.current_page == Page::Queue {
                            app.queue_remove_selected();
                        } else {
                            app.download_selected();
                        }
                    }
                    KeyCode::Char('J') => app.queue_move(true),
                    KeyCode::Char('K') => app.queue_move(false),
                    // Like the current song — the heart in the playback
                    // bar fills in (the like toggle, on `h`).
                    KeyCode::Char('h') => app.toggle_like(),
                    // Toggle the lyrics pane on the right.
                    KeyCode::Char('l') => app.toggle_lyrics(),
                    // Repeat / shuffle.
                    KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.cycle_repeat()
                    }
                    KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.toggle_shuffle()
                    }
                    // Play the selected track or open the selected collection.
                    KeyCode::Enter => app.activate_selected().await,
                    // Cycle the search filters (search page only).
                    KeyCode::Tab if app.current_page == Page::Search => {
                        app.cycle_search_filter(true).await;
                    }
                    KeyCode::BackTab if app.current_page == Page::Search => {
                        app.cycle_search_filter(false).await;
                    }
                    // Scroll the lyrics pane while it's open, otherwise the
                    // Library detail pane.
                    KeyCode::PageDown => {
                        if app.lyrics_open {
                            app.scroll_lyrics(10);
                        } else if app.current_page == Page::Library {
                            app.scroll_details(1);
                        }
                    }
                    KeyCode::PageUp => {
                        if app.lyrics_open {
                            app.scroll_lyrics(-10);
                        } else if app.current_page == Page::Library {
                            app.scroll_details(-1);
                        }
                    }
                    // Back from a collection; from search, back to the library.
                    KeyCode::Esc => {
                        if app.current_page == Page::Playlist {
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
                    // Focus the search bar.
                    KeyCode::Char('/') => {
                        app.current_page = Page::Search;
                        app.input_mode = InputMode::Search;
                        app.search_query.clear();
                    }
                    _ => {}
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
