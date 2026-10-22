<div align="center">

# 🎵 ytmtui

**A minimal, lightning-fast YouTube Music terminal player**  
written in Rust · lightweight · keyboard-driven

<br>

[![Rust](https://img.shields.io/badge/rust-1.75%2B-orange?style=flat-square&logo=rust)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-MIT-blue?style=flat-square)](LICENSE)
[![AUR](https://img.shields.io/aur/version/ytmtui?style=flat-square&logo=archlinux&label=AUR)](https://aur.archlinux.org/packages/ytmtui)

<br>

![ytmtui — All I Wanted](assets/screenshots/all-i-wanted.png)

</div>

---

## ✨ Features

| | |
| :--- | :--- |
| 🖥️ **Terminal UI** | Built with `ratatui` + `crossterm` — chrome-free, hairlines & whitespace only |
| 🔐 **Auth wizard** | `ytmtui setup` auto-detects your browser and extracts cookies |
| 🔍 **Innertube search** | Signed-in search with Top / Songs / Albums / Artists / Playlists filters |
| 📚 **Your library** | Master–detail browser of playlists, artists & channels with lazy track preview |
| ▶️ **Playback** | Real-time progress bar, true pause/resume via `mpv` IPC, buffering state |
| 💫 **Extras** | Synced lyrics, queue, offline mode, like/unlike, MP3 download with cover art |

---

## 📸 Screenshots

<div align="center">

| | |
| :---: | :---: |
| ![Billie Eilish](assets/screenshots/billie-eilish.png) | ![Birds of a Feather](assets/screenshots/birds-of-a-feather.png) |
| *Billie Eilish playlist + lyrics* | *BIRDS OF A FEATHER playing* |

![Your Anxiety Buddy](assets/screenshots/your-anxiety-buddy.png)

*Your Anxiety Buddy — full library view*

</div>

---

## 📦 Install

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
# binary lands in target/release/ytmtui
```

---

## 🚀 Quick start

```bash
ytmtui setup              # interactive auth wizard (auto-detects browser)
ytmtui                    # launch the TUI
```

<details>
<summary><strong>⚙️ Setup options</strong></summary>

```bash
ytmtui setup --manual          # skip browser detection, paste headers
ytmtui setup --browser firefox # force a specific browser
```

</details>

---

## 🔑 Setup & Authentication

The wizard walks you through signing in:

1. Shows the **YouTube Music Authentication** banner
2. If already authenticated, asks whether to re-authenticate
3. Auto-detects a signed-in browser and extracts cookies (falls back to manual paste)
4. Identifies the account and lets you pick between multiple Google accounts
5. Verifies credentials against YouTube Music

Credentials live in `~/.config/ytmtui/auth.json`.

### 🌐 Browser auto-extraction

Supported browsers (checked in order):

`helium` · `chrome` · `chromium` · `brave` · `firefox` · `zen` · `edge` · `vivaldi` · `opera`

| Family | Method |
| :--- | :--- |
| 🦊 **Firefox / Zen** | Reads `cookies.sqlite` directly |
| 🧭 **Chromium family** | Decrypts the Cookies DB with AES-128-CBC using the login keyring (`secret-tool`) or Chrome’s Linux fallback |

Databases are copied privately, so a running browser never blocks extraction. Flatpak / snap paths are handled too.

### 📋 Manual header paste

```bash
ytmtui setup --manual
```

1. Open [music.youtube.com](https://music.youtube.com) → **F12** → **Network** → refresh
2. Click any `/browse` request → right-click **Request Headers** → **Copy**
3. Paste into the wizard, then press **Enter** on a blank line

Only have the `cookie` value? Paste that — the wizard fills in the rest.

---

## ⌨️ Keybindings

| Key | Action |
| :--- | :--- |
| `↑` / `↓` | Move within the current list |
| `←` / `→` | Seek ∓10 s (while playing / paused) |
| `Enter` | Play selected track or open collection |
| `Space` | Toggle play / pause |
| `n` / `p` | Next / previous track |
| `/` | Focus search bar |
| `Tab` / `⇧Tab` | Cycle search filters |
| `Esc` | Back / open queue |
| `d` | Download highlighted song as MP3 (or delete in queue) |
| `o` | Toggle offline mode |
| `h` | Like / unlike current song |
| `l` | Toggle lyrics pane |
| `X` / `Z` | Queue next / append to queue |
| `Ctrl+R` | Cycle repeat mode |
| `Ctrl+S` | Toggle shuffle |
| `PgUp` / `PgDn` | Scroll lyrics or detail pane |
| `q` | Quit |

---

## 🖼️ Views

The window is deliberately chrome-free — no box borders, just hairlines, whitespace and right-aligned metadata. Colour comes from your **pywal** palette.

- 📌 **Header** — app name, signed-in account, search prompt  
  Press `/` to search · `Tab` / `⇧Tab` switches **Top · Songs · Albums · Artists · Playlists**
- 📂 **Playlist sidebar** — always-visible library of playlists, albums, artists & channels  
  `↑`/`↓` moves the `▌` cursor · `Enter` opens the selection
- 📄 **Detail / results** — title, meta line and lazily-fetched track list (cached)
- 🎚️ **Playback bar** — indicator, track, time and a full-width progress bar that only advances while playing
- ℹ️ **Footer** — status message + compact keybinding cheat-sheet

---

## 🎨 Theming

Colors are taken from your **pywal** palette at startup:

```
$XDG_CACHE_HOME/wal/colors.json   # preferred
$XDG_CACHE_HOME/wal/colors        # fallback
```

The accent colour marks everything active — the `▌` cursors, progress bar, playing indicator and current search filter. Secondary text (meta, counts, hairlines, status) is dimmed. No pywal palette found → a built-in fallback is used.

Set a new wallpaper with `wal` and restart `ytmtui` to pick up the new colours.

---

## 🛠️ CLI helpers

```bash
ytmtui status            # show the signed-in account
ytmtui playlists         # list library playlists, artists and channels
ytmtui liked             # list liked songs
ytmtui tracks <id>       # list tracks of a playlist / album / artist
                         # (VLPL…, VLLM…, MPREb…)
```

---

## 📄 License

MIT © [Bell09](https://github.com/Bell09)
