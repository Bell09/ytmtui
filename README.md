<div align="center">

# ♪ YTMTUI ♫

**Minimal YouTube Music TUI** — Rust · mpv · offline · synced lyrics

<br>

[![Rust](https://img.shields.io/badge/Rust-1.89%2B-0d1117?style=flat-square&logo=rust&logoColor=22c55e&labelColor=161b22)](https://www.rust-lang.org/)
[![mpv](https://img.shields.io/badge/mpv-backend-0d1117?style=flat-square&logo=mpv&logoColor=4ade80&labelColor=161b22)](https://mpv.io/)
[![License](https://img.shields.io/badge/License-MIT-0d1117?style=flat-square&logo=opensourceinitiative&logoColor=22c55e&labelColor=161b22)](LICENSE)
[![AUR](https://img.shields.io/badge/AUR-ytmtui-0d1117?style=flat-square&logo=archlinux&logoColor=4ade80&labelColor=161b22)](https://aur.archlinux.org/packages/ytmtui)

<br>

![ytmtui](assets/screenshots/all-i-wanted.png)

</div>

> Solo project. Issues welcome. PRs not accepted.

---

## Features

| | |
| :--- | :--- |
| 🖥️ **Terminal UI** | `ratatui` + `crossterm` — chrome-free, hairlines only |
| 🔐 **Auth** | `ytmtui setup` — auto cookie extraction or manual paste |
| 🎧 **Last.fm** | Automatic Now Playing + scrobbling |
| 🔍 **Search** | Signed-in Innertube · Top / Songs / Albums / Artists / Playlists |
| 📚 **Library** | Playlists, artists, channels with lazy track preview |
| ✨ **For you** | Mixes, subscriptions, charts, moods, genres |
| ▶️ **Playback** | Progress bar, true pause, prefetch, retry · `m` for cover + lyrics |
| 🔁 **Auto Queue** | Similar songs before the queue ends |
| 💫 **Extras** | Offline mode · session resume · like · playlists · MP3 + `.lrc` download |

---

## Screenshots

<div align="center">

<img src="assets/screenshots/2026-11-06-210522_hyprshot.png" width="48%" alt="girl in red — we fell in love in october" />
<img src="assets/screenshots/2026-11-06-213858_hyprshot.png" width="48%" alt="Billie Eilish — i love you · Discover Mix" />

<img src="assets/screenshots/2026-11-06-213210_hyprshot.png" width="48%" alt="Avril Lavigne — I'm with You · browse shelves" />
<img src="assets/screenshots/slow%20dancing%20in%20the%20dark.png" width="48%" alt="Joji — SLOW DANCING IN THE DARK" />

<img src="assets/screenshots/2026-11-06-214330_hyprshot.png" width="48%" alt="liana flores — rises the moon" />
<img src="assets/screenshots/no%20Vacation.png" width="48%" alt="Yam Yam — No Vacation" />

</div>

---

## Install

```bash
curl -fsSL https://github.com/Bell09/ytmtui/releases/latest/download/install.sh | sh
```

| Option | Result |
|--------|--------|
| Default | `~/.local/bin` |
| `--system` | `/usr/local/bin` |
| `YTMTUI_VERSION=0.1.21` | Pin version |

The script installs `mpv`, `yt-dlp`, and `ffmpeg` if missing.

**Arch & derivatives**

```ini
# /etc/pacman.conf
[ytmtui]
SigLevel = Optional TrustAll
Server = https://bell09.github.io/ytmtui/$arch
```

```bash
sudo pacman -Sy ytmtui
```

**From source**

```bash
git clone https://github.com/Bell09/ytmtui.git
cd ytmtui && cargo build --release
```

---

## Dependencies

| Required | Purpose |
| :--- | :--- |
| **mpv** | Playback (IPC) |
| **yt-dlp** | Stream extraction |
| **Rust** ≥ 1.89 | Build only |

| Optional | Purpose |
| :--- | :--- |
| **ffmpeg** | MP3 downloads |
| **Deno** / **Node** | yt-dlp JS challenges |

```bash
sudo pacman -S mpv yt-dlp ffmpeg deno   # Arch
```

---

## Terminals

True images via Kitty graphics protocol. Others fall back to Unicode blocks.

| Terminal | Images | 2× lyrics |
|----------|--------|-----------|
| **Kitty** ≥ 0.40 | ✅ | ✅ |
| **WezTerm** | ✅ | ✅ |
| **Ghostty** | ✅ | ✅ |
| **Konsole** | ✅ | ❌ |

---

## Quick start

```bash
ytmtui setup              # auth wizard
ytmtui                    # launch
```

```bash
ytmtui setup --manual          # paste headers
ytmtui setup --browser firefox # force browser
ytmtui setup lastfm            # Last.fm only
```

---

## Setup

### YouTube Music

1. `ytmtui setup`
2. Auto-detects browser cookies (or paste headers with `--manual`)
3. Credentials → `~/.config/ytmtui/auth.json`

Supported: `helium` · `chrome` · `chromium` · `brave` · `firefox` · `zen` · `edge` · `vivaldi` · `opera`

### Last.fm (optional)

```bash
ytmtui setup lastfm
```

Needs API key + shared secret from [last.fm/api/account/create](https://www.last.fm/api/account/create).  
Session key preferred (or one-time password hash exchange).  
Stored in `~/.config/ytmtui/lastfm.json` (0600). Scrobbling is automatic once connected.

---

## Keybindings

| Key | Action |
| :--- | :--- |
| `↑` `↓` | Navigate |
| `Enter` | Play / open |
| `Space` | Play / Pause |
| `←` `→` | Seek ±5s |
| `n` `p` | Next / previous |
| `1`–`5` | Trending / Mix / Community / Moods / Genres |
| `/` | Search |
| `Tab` | Cycle search filters |
| `Esc` | Back |
| `d` | Download MP3 + cover + `.lrc` |
| `a` | Add to playlist |
| `c` | Create playlist |
| `r` | Remove from playlist |
| `o` | Offline mode |
| `L` | Like / unlike |
| `l` | Lyrics pane |
| `m` | Maximized now-playing |
| `X` `Z` | Queue next / append |
| `Ctrl+A` | Toggle Auto Queue |
| `Ctrl+R` | Repeat |
| `Ctrl+S` | Shuffle |
| `q` | Quit |

---

## Views

Chrome-free. Colours come from your wallpaper palette (pywal / wallust / matugen).

- **Header** — account + search (`/` + `Tab`)
- **Sidebar** — library (playlists, artists, channels)
- **Detail** — track list
- **For you** — `b` or `1`–`5`
- **Playback bar** — progress only while playing
- **Now playing** — `m` for large cover + full lyrics
- **Footer** — status + keys

---

## Theming

Auto-detects:

- **pywal** → `$XDG_CACHE_HOME/wal/colors.json`
- **wallust** → `$XDG_CACHE_HOME/wallust/colors.json`
- **matugen** → its `colors.json` / configured output

Accent = cursors, progress, playing indicator.  
Secondary = meta, hairlines.  
Live reload on wallpaper change. Falls back to green.

---

## CLI

```bash
ytmtui status            # account + tools
ytmtui playlists         # library
ytmtui liked             # liked songs
ytmtui tracks <id>       # playlist / album / artist tracks
```

---

## License

MIT © [Bell09](https://github.com/Bell09)
