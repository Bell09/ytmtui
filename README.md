# ytmtui

A lightning-fast YouTube Music TUI client written in Rust.

## Install

### Arch Linux (AUR)

```bash
paru -S ytmtui
# or
yay -S ytmtui
```

### From source

```bash
git clone https://github.com/Bell09/ytmtui.git
cd ytmtui
cargo build --release
```

## Features

- **Terminal UI** built with `ratatui` + `crossterm`.
- **Interactive authentication wizard** (`ytmtui setup`).
- **YouTube Music (Innertube) search** when signed in; public search fallback otherwise. Search results can be filtered by **Top / Songs / Albums / Artists / playlists**, and albums, artists and playlists are directly openable.
- **Your library** — a master–detail browser of your playlists, subscribed artists and channels, with a lazy track preview. Open any entry (playlists open their full track list; artists open their top songs) and play a track.
- **Simple keyboard navigation** (arrows, `Enter`, `n`/`p`), a real-time progress bar, buffering state, true pause/resume (via `mpv`'s IPC socket), and an `mpv` playback backend.

## Setup & Authentication

```bash
ytmtui setup              # interactive wizard (auto-detects your browser)
ytmtui setup --manual     # skip browser detection, paste headers directly
ytmtui setup --browser firefox
```

The wizard:

1. Shows the `YouTube Music Authentication` banner.
2. If already authenticated, asks whether to re-authenticate.
3. Auto-detects a signed-in browser and extracts cookies; on failure it
   falls back to the manual header paste.
4. Identifies the account and lets you pick between multiple Google accounts.
5. Verifies credentials against YouTube Music and reports the result.

Credentials are stored in `~/.config/ytmtui/auth.json`.

### Browser auto-extraction

Supported browsers (detected in this order): `helium`, `chrome`, `chromium`,
`brave`, `firefox`, `zen`, `edge`, `vivaldi`, `opera`.

- **Firefox family** (Firefox, Zen): reads `cookies.sqlite` directly.
- **Chromium family** (Chrome, Chromium, Brave, Edge, Vivaldi, Opera, Helium):
  reads the `Cookies` database and decrypts values with AES-128-CBC, using the
  "Chrome Safe Storage" secret from the login keyring (`secret-tool`) when
  available, or Chrome's Linux fallback. Native (flatpak/snap) paths are
  handled too.

Browsers are read via a private copy of the database, so a running browser
never blocks extraction.

### Manual header paste

```bash
ytmtui setup --manual
```

1. Open <https://music.youtube.com>, press **F12** → **Network**, and refresh.
2. Click any `/browse` request, then right-click **Request Headers** → **Copy**.
3. Paste into the wizard, then press **Enter** on a blank line.

Only have the `cookie` value? Paste that instead — the wizard fills in the rest.

## Keybindings

| Key | Action |
| :--- | :--- |
| `Up` / `Down` | Move within the current list |
| `Left` / `Right` | Seek ∓10s (only acts while a track is playing or paused) |
| `Enter` | Play the selected track or open the selected collection |
| `Space` | Toggle play/pause |
| `n` / `p` | Next / previous track |
| `/` | Focus the search bar at the top of the window (type, then `Enter` to search) |
| `Tab` / `Shift+Tab` | Cycle search filters (Top, Songs, Albums, …) |
| `PageDown` / `PageUp` | Scroll the Library detail pane |
| `Esc` | Back (out of a collection, or from search to the library) |
| `q` | Quit |

## Views

The window is chrome-free — no box borders, just hairlines, whitespace and
right-aligned metadata (colour comes from your pywal palette). It opens on the
library:

- **Header** — the app name and your signed-in account, the search prompt, and
  a hairline. Press `/` to focus the search line, type a query and press
  `Enter` for authenticated Innertube results; `Tab` / `Shift+Tab` switches
  between the **Top**, **Songs**, **Albums**, **Artists** and playlist
  filters. `Esc` returns to the library.
- **Playlist sidebar** — always visible on the left: your library's playlists,
  albums, artists and channels as compact rows (kind glyph, name, right-aligned
  track count). `Up`/`Down` moves the `▌` cursor and the detail view follows;
  `Enter` opens the selection as a full track list.
- **Detail / results** — the right pane shows the highlighted entry's title,
  meta line and lazily-fetched track list (cached; `PageUp`/`PageDown` scrolls
  long lists), the search results, or the tracks of an opened collection — with
  `Esc` going back.
- **Selection line** — the focused item's full name behind a `▌` cursor.
- **Playback** — indicator, track and time over a full-width progress bar that
  only advances while actually playing. `Left`/`Right` seek ±10s, `n`/`p` skip,
  `Space` pauses.
- **Footer** — the status message on the left, a compact keybinding cheat-sheet
  on the right.

## Theming

Colors are taken from your **pywal** palette at startup
(`$XDG_CACHE_HOME/wal/colors.json`, falling back to the `colors` file). The
accent colour marks whatever is active — the `▌` cursors, the progress bar,
the playing indicator and the current search filter — while everything
secondary (meta lines, counts, hairlines, status text) is dimmed. If no pywal
palette is found, a built-in fallback theme is used. Set a new wallpaper with
`wal` and restart `ytmtui` to pick up the new colors.

## Command-line helpers

Aside from the TUI, the binary exposes a few non-interactive commands:

```bash
ytmtui status            # show the signed-in account
ytmtui playlists         # list library playlists, artists and channels
ytmtui liked             # list liked songs
ytmtui tracks <id>       # list tracks of a playlist/album/artist (VLPL…, VLLM, MPREb…)
```

## Running

```bash
cargo run --release
```
