<div align="center">

# ♪ YTMTUI ♫

**A minimal, lightning-fast YouTube Music TUI written in Rust**  
featuring an **mpv** backend · download songs with Embedded album art + lyrics · synced lyrics · offline mode · straight forward keybindings

<br>

[![Rust](https://img.shields.io/badge/Rust-1.89%2B-0d1117?style=for-the-badge&logo=rust&logoColor=22c55e&labelColor=161b22)](https://www.rust-lang.org/)
[![mpv](https://img.shields.io/badge/mpv-backend-0d1117?style=for-the-badge&logo=mpv&logoColor=4ade80&labelColor=161b22)](https://mpv.io/)
[![License](https://img.shields.io/badge/License-MIT-yellow.svg?style=for-the-badge)](LICENSE)
[![AUR](https://img.shields.io/badge/AUR-ytmtui-0d1117?style=for-the-badge&logo=archlinux&logoColor=4ade80&labelColor=161b22)](https://aur.archlinux.org/packages/ytmtui)
[![Linux](https://img.shields.io/badge/Linux-only-0d1117?style=for-the-badge&logo=linux&logoColor=22c55e&labelColor=161b22)](https://github.com/Bell09/ytmtui)

<br>

[![INSTALL](https://img.shields.io/badge/INSTALL-22c55e?style=for-the-badge&labelColor=0d1117&color=22c55e)](#-install)
[![QUICK%20START](https://img.shields.io/badge/QUICK%20START-4ade80?style=for-the-badge&labelColor=0d1117&color=4ade80)](#-quick-start)
[![KEYBINDINGS](https://img.shields.io/badge/KEYBINDINGS-86efac?style=for-the-badge&labelColor=0d1117&color=86efac)](#%EF%B8%8F-keybindings)
[![THEMING](https://img.shields.io/badge/THEMING-bbf7d0?style=for-the-badge&labelColor=0d1117&color=bbf7d0)](#-theming)
[![LICENSE](https://img.shields.io/badge/LICENSE-dcfce7?style=for-the-badge&labelColor=0d1117&color=dcfce7)](#-license)

<br>

![ytmtui](assets/screenshots/all-i-wanted.png)

</div>

> **Solo project** — Built and maintained by me.  
> Pull requests are not accepted. Issues for bugs are welcome.  

---

## ✨ Features

| | |
| :--- | :--- |
| 🖥️ **Terminal UI** | Built with `ratatui` + `crossterm` — chrome-free, hairlines & whitespace only |
| 🔐 **Auth wizard** | `ytmtui setup` configures YouTube Music, Last.fm, or both — browser cookie extraction included |
| 🎧 **Last.fm scrobbling** | Now Playing updates and automatic scrobbles with background retries — always on once set up |
| 🔍 **Innertube search** | Signed-in search with Top / Songs / Albums / Artists / Playlists filters |
| 📚 **Your library** | Master–detail browser of playlists, artists & channels with lazy track preview |
| ✨ **For you** | Personal mixes, subscriptions, trending songs, community playlists, activity mixes, moods, genres, and country/global charts |
| ▶️ **Playback** | Real-time progress bar, true pause/resume via `mpv` IPC, next-track prefetch, · `m` for a maximized cover + synced lyrics view |
| 🔁 **Auto Queue** | Fetches similar songs whenever the queue ends |
| 💫 **Extras** | Synced lyrics, queue, offline mode, session resume, like/unlike, remove song from playlist, create a playlist, MP3 download with embedded cover art and `.lrc` lyrics |

---

## 📸 Screenshots

<div align="center">

<img src="assets/screenshots/2026-11-06-210522_hyprshot.png" width="48%" alt="girl in red — we fell in love in october" />
<img src="assets/screenshots/2026-11-06-213858_hyprshot.png" width="48%" alt="Billie Eilish — i love you · Discover Mix" />

<img src="assets/screenshots/2026-11-06-213210_hyprshot.png" width="48%" alt="Avril Lavigne — I'm with You · browse shelves" />
<img src="assets/screenshots/slow%20dancing%20in%20the%20dark.png" width="48%" alt="Joji — SLOW DANCING IN THE DARK" />

<img src="assets/screenshots/2026-11-06-214330_hyprshot.png" width="48%" alt="liana flores — rises the moon" />
<img src="assets/screenshots/no%20Vacation.png" width="48%" alt="Yam Yam — No Vacation" />

</div>

---

## 📦 Install

### One-line install

```bash
curl -fsSL https://github.com/Bell09/ytmtui/releases/latest/download/install.sh | sh
```

If your network cannot resolve `raw.githubusercontent.com`, use the command
above (it is served from the release assets) or fetch the script through the
API:

```bash
curl -fsSL 'https://api.github.com/repos/Bell09/ytmtui/contents/install.sh?ref=Main' \
  | python3 -c 'import sys,json,base64;sys.stdout.write(base64.b64decode(json.load(sys.stdin)["content"]).decode())' | sh
```

Without the installer at all:

```bash
curl -fL -o ytmtui.tar.gz https://github.com/Bell09/ytmtui/releases/latest/download/ytmtui-x86_64-linux.tar.gz
tar -xzf ytmtui.tar.gz
install -Dm755 ytmtui-*/ytmtui ~/.local/bin/ytmtui
```

<!-- coderabbit-ai-generated:v1 -->

<!-- coderabbit-ai-generated-section:v1:b2a4474c2d90877de2632ab4a1fecafef1c8300c0e1a073d016104c4db1fc18a -->

| Option | Result |
|--------|--------|
| Default | Installs to `~/.local/bin` |
| `sh install.sh --system` | Installs to `/usr/local/bin` |
| `YTMTUI_VERSION=0.1.23` | Pins a specific version |

The script supports Linux x86_64, verifies the download checksum before installing, and automatically installs missing `mpv`, `yt-dlp`, and `ffmpeg` dependencies using a supported package manager. It verifies that all three dependencies run before reporting success.

<!-- /coderabbit-ai-generated:v1 -->

### Release binaries

Download `ytmtui-x86_64-linux.tar.gz` from the [latest release](https://github.com/Bell09/ytmtui/releases/latest) and place the binary on your `PATH`.

### Arch Linux & derivatives

Supported distributions:

- **Arch Linux**
- **Garuda Linux**
- **CachyOS**
- **EndeavourOS**
- **Manjaro**
- **Artix Linux**
- **BlackArch Linux**

Add the custom repository to `/etc/pacman.conf`:

```ini
[ytmtui]
SigLevel = Optional TrustAll
Server = https://bell09.github.io/ytmtui/$arch
```

Then install:

```bash
sudo pacman -Sy ytmtui
```

> Packages are currently unsigned (`TrustAll`). Future updates are picked up with `pacman -Syu`.

### From source

```bash
git clone https://github.com/Bell09/ytmtui.git
cd ytmtui
cargo build --release
```

Binary lands in `target/release/ytmtui`.

---

## 📋 Dependencies

### Runtime (required)

| Dependency | Purpose |
| :--- | :--- |
| **mpv** | Playback backend (controlled via IPC) |
| **yt-dlp** | Stream extraction (`ytmtui` uses it directly and mpv uses it as a fallback) |
| **Rust** ≥ 1.89 | Only needed when building from source |

Install on Arch-based systems:

```bash
sudo pacman -S mpv yt-dlp
```

yt-dlp evolves with YouTube; keep it reasonably current. `ytmtui status` reports whether the required tools are on `PATH` and whether a JavaScript runtime is available. ytmtui uses yt-dlp's documented defaults (no forced player client, no bundled PO Token generator), so advanced options and PO Token providers belong in yt-dlp's own configuration.

### Optional (recommended)

| Dependency | Purpose |
| :--- | :--- |
| **ffmpeg** | Used when downloading songs as MP3 |
| **Deno** or **Node** | JavaScript runtime for yt-dlp when a video needs YouTube's JS challenges |

```bash
sudo pacman -S ffmpeg deno
```

ytmtui never bundles or silently installs these runtimes; it only detects them and points at the missing piece.

### Rust crates (main)

| Crate | Purpose |
| :--- | :--- |
| `ratatui` + `crossterm` | Terminal UI |
| `tokio` | Async runtime |
| `reqwest` | HTTP / Innertube API |
| `serde` / `serde_json` | Serialization |
| `rusqlite` | Local offline database |
| `lofty` | Audio metadata & cover art |
| `zbus` | MPRIS support |
| `image` | Cover art processing |

---

## >_ Supported Terminals

Album art is rendered as **true high-resolution images** on terminals that support the **Kitty graphics protocol**. All other terminals fall back to Unicode quadrant blocks.

| Terminal     | True images | 2× text scaling (lyrics) |
|--------------|-------------|---------------------------|
| **Kitty**    | ✅          | ✅ (≥ 0.40)              |
| **WezTerm**  | ✅          | ✅                        |
| **Ghostty**  | ✅          | ✅                        |
| **Konsole**  | ✅          | ❌                        |

**Kitty ≥ 0.40** is recommended for the full experience (images + 2× lyrics text in the maximized view).

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
ytmtui setup lastfm            # configure Last.fm directly
```

</details>

---

## 🔑 Setup & Authentication

Run `ytmtui setup` and pick what to configure:

```
[1] YouTube Music
[2] Last.fm
[3] Both
```

`--manual`, `--browser`, and `setup lastfm` skip the menu and go straight to the matching wizard, so existing instructions still work.

### ▶️ YouTube Music

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
| 🐺 **Firefox / Zen** | Reads `cookies.sqlite` directly |
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

### 🎧 Last.fm scrobbling

Last.fm is optional and works alongside YouTube Music. Create an API account at [last.fm/api/account/create](https://www.last.fm/api/account/create), then:

```bash
ytmtui setup lastfm    # or choose Last.fm / Both from `ytmtui setup`
```

The wizard asks for the **API key**, **shared secret**, **username**, and then either:

- a **session key** (recommended — verified with `user.getInfo`), or
- an **MD5 password hash**, exchanged once for a session key via `auth.getMobileSession`.

Secret prompts never echo what you type, and the password hash is not kept after the exchange. Credentials are written to `~/.config/ytmtui/lastfm.json` with owner-only (`0600`) permissions; API secrets, password hashes and session keys are never displayed or logged.

Running the wizard again shows the current connection and lets you update credentials or disconnect (which deletes the stored file).

Once connected, scrobbling runs automatically — there is no switch to flip:

- **Now Playing** is sent when a track actually starts playing (not while buffering or paused).
- A track is scrobbled after half its duration, capped at four minutes; tracks under 30 seconds are ignored. Unknown durations use the four-minute cap.
- Artist, title, album and duration are submitted when known.
- Only genuinely playing time counts — pauses and UI stalls do not.
- Each track is marked submitted before the request is sent, so retries can never duplicate a scrobble.
- Requests run in the background with short timeouts and light retries for transient Last.fm/network errors. Playback is never blocked or interrupted.

As long as `~/.config/ytmtui/lastfm.json` holds valid credentials, every track is scrobbled. The header shows `· last.fm:<username>` in the accent colour while connected. Run `ytmtui setup lastfm` and choose **Disconnect** to stop scrobbling.

---

## ⌨️ Keybindings

Press `?` in the app for the complete, scrollable cheatsheet, including every
screen, picker and text-entry mode. The everyday keys are:

| Key | Action |
| :--- | :--- |
| `↑` / `↓` | Navigate lists |
| `Enter` | Play / open selection |
| `Space` | Play / Pause |
| `←` / `→` | Seek -10s / +10s |
| `n` / `p` | Next / previous track |
| `1`–`5` | Jump to **Trending / Mix / Community / Moods / Genres** |
| `/` | Focus search bar |
| `Tab` / `⇧Tab` | Cycle search filters |
| `b` | Open the Browse shelf picker |
| `z` | Open the queue (`Esc` returns) |
| `J` / `K` | Move the selected queue track down / up |
| `Esc` | Back / leave the maximized view |
| `d` | Download highlighted song as MP3 with cover art and synced `.lrc` (or delete in queue) |
| `a` | Add highlighted song to a library playlist — arrows scroll, `Enter` adds |
| `c` | Create a new private playlist — type a name, `Enter` creates it |
| `r` | Remove highlighted song from the open playlist (rolls back on failure) |
| `o` | Toggle offline mode and online mode |
| `L` | Like / unlike current song — in Liked Music it targets the highlighted row |
| `l` | Toggle lyrics pane |
| `m` | Toggle maximized now playing view (large cover + full-height lyrics) |
| `X` / `Z` | Queue next / append to queue |
| `Ctrl+A` | Toggle Auto Queue for this session |
| `Ctrl+R` | Cycle repeat mode |
| `Ctrl+S` | Toggle shuffle |
| `PgUp` / `PgDn` | Scroll lyrics or detail pane |
| `?` | Open the keyboard cheatsheet |
| `q` | Quit |
| `Ctrl+C` | Quit from any screen or text field |

## 🖼️ Views

The window is deliberately chrome-free — no box borders, just hairlines, whitespace and right-aligned metadata. Colour comes from your **wallpaper** palette.

| Area | Description |
|------|-------------|
| 📌 **Header** | App name, signed-in account and search prompt. Press `/` to search · `Tab` / `⇧Tab` cycles **Top · Songs · Albums · Artists · Playlists** |
| 📂 **Playlist sidebar** | Always-visible library of playlists, albums, artists & channels. `↑`/`↓` moves the `▌` cursor · `Enter` opens the selection |
| 📄 **Detail / results** | Title, meta line and lazily-fetched track list (cached) |
| ✨ **For you** | Press `b` for the shelf picker. `1`–`5` jump straight to the first five shelves |
| 🎚️ **Playback bar** | Indicator, track, time and a full-width progress bar that only advances while playing |
| 🖼️ **Now playing** | Press `m` for a large centred cover beside full-height synced lyrics (2× text in Kitty ≥ 0.40). `PgUp` / `PgDn` scrolls · `m` or `Esc` returns |
| ℹ️ **Footer** | Status message and compact keybinding guide · press `?` for the full scrollable cheatsheet |

**For you shelves** (accessible via `b` or hotkeys `1`–`5`):

Quick picks · Your daily discover · Featured playlists for you · My subscriptions · Forgotten favorites · From the community · Moods & genres · Activity mixes · Discovery mix · Mixed for you · New releases · Covers and remixes · Trending community playlists · Trending songs · Country charts · Global charts

---

## 🎨 Theming

YTMTUI automatically picks up the newest color palette written by your wallpaper tools.

| Tool        | Paths searched |
|-------------|----------------|
| **pywal**   | `$XDG_CACHE_HOME/wal/colors.json` <br> or the plain `colors` file |
| **wallust** | `$XDG_CACHE_HOME/wallust/colors.json` (or `wallust/colors`) <br> + any JSON `target` / `dst` declared in `~/.config/wallust/wallust.toml` |
| **matugen** | `colors.json` in its cache / config / data directories <br> + any JSON `output_path` declared in `~/.config/matugen/config.toml` |

Matugen’s Material role JSON (flat or nested) and base16 JSON are both supported.

**How colors are used**

| Role        | Applied to |
|-------------|------------|
| **Accent**  | Active elements — `▌,'.~‎♪♬.༺❜´˚` cursors, progress bar, playing indicator, text, current search filter |
| **Secondary** | Meta text, counts, hairlines, status messages (dimmed) |

The palette is watched live — change your wallpaper and the UI repaints without a restart.  
If no palette is found, a built-in **green** fallback is used.

---

## 🛠️ CLI helpers

```bash
ytmtui status            # show the signed-in account, Last.fm and playback tooling
ytmtui playlists         # list library playlists, artists and channels
ytmtui liked             # list liked songs
ytmtui tracks <id>       # list tracks of a playlist / album / artist
                         # (VLPL…, VLLM…, MPREb…)
```

---

## 📄 License

YTMTUI is licensed under the **MIT License**.

Copyright (c) 2026 Quim Ashley M. Castillo

See [LICENSE](LICENSE) for details.
