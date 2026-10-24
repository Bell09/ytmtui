<div align="center">

# <span style="color:#FF0033">♬♫♪</span> ytmtui

**YouTube Music TUI** · Rust · mpv · lyrics · offline

<img src="https://img.shields.io/badge/Linux-0A0A0A?style=flat-square&logo=linux&logoColor=FCC624"/>
<img src="https://img.shields.io/badge/Rust-000000?style=flat-square&logo=rust&logoColor=white"/>
<img src="https://img.shields.io/badge/mpv-6C3483?style=flat-square"/>
<img src="https://img.shields.io/badge/MIT-6366F1?style=flat-square"/>
<img src="https://img.shields.io/badge/AUR-1793D1?style=flat-square&logo=archlinux&logoColor=white"/>

<br>

<a href="#-install"><img src="https://img.shields.io/badge/install-FF0033?style=flat-square"/></a>
<a href="#-usage"><img src="https://img.shields.io/badge/usage-FF3366?style=flat-square"/></a>
<a href="#-keys"><img src="https://img.shields.io/badge/keys-FF6699?style=flat-square"/></a>
<a href="#-theme"><img src="https://img.shields.io/badge/theme-22C55E?style=flat-square"/></a>

<br>

<img src="assets/screenshots/all-i-wanted.png" width="85%"/>

</div>

---

### features

| | |
|---|---|
| <span style="color:#FF0033">▌</span> **tui** | `ratatui` · chrome-free · hairlines only |
| <span style="color:#FF3366">▌</span> **auth** | browser cookie extract · manual paste |
| <span style="color:#FF6699">▌</span> **search** | innertube · songs · albums · artists · playlists |
| <span style="color:#C084FC">▌</span> **library** | playlists · artists · channels · lazy tracks |
| <span style="color:#6366F1">▌</span> **play** | mpv ipc · pause/resume · seek · queue |
| <span style="color:#22C55E">▌</span> **extra** | lyrics · offline · download mp3 · like |

---

### install

```bash
# aur
paru -S ytmtui

# source
git clone https://github.com/Bell09/ytmtui && cd ytmtui
cargo build --release
```

**deps** — `mpv` required · `yt-dlp` `ffmpeg` `libsecret` optional

```bash
# arch
sudo pacman -S mpv yt-dlp ffmpeg libsecret
# debian
sudo apt install mpv yt-dlp ffmpeg libsecret-tools
# fedora
sudo dnf install mpv yt-dlp ffmpeg libsecret
```

---

### usage

```bash
ytmtui setup          # auth wizard
ytmtui                # launch
ytmtui setup --manual # paste headers
```

creds → `~/.config/ytmtui/auth.json`

---

### keys

| | | | |
|---|---|---|---|
| `↑↓` move | `enter` play/open | `space` pause | `n`/`p` next/prev |
| `←→` seek | `/` search | `tab` filter | `esc` back/queue |
| `d` download | `o` offline | `h` like | `l` lyrics |
| `x`/`z` queue | `ctrl+r` repeat | `ctrl+s` shuffle | `q` quit |

---

### theme

picks up **pywal** · **wallust** · **matugen** live. accent from wallpaper. no palette → fallback.

---

<div align="center">

<img src="assets/screenshots/your-anxiety-buddy.png" width="32%"/>
<img src="assets/screenshots/billie-eilish.png" width="32%"/>
<img src="assets/screenshots/birds-of-a-feather.png" width="32%"/>

<br><br>

MIT © [Bell09](https://github.com/Bell09)  
<span style="color:#FF0033">♬</span> <span style="color:#22C55E">♫</span> <span style="color:#6366F1">♪</span>

</div>
