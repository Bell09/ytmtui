#!/bin/sh
# Install the latest ytmtui release binary.
#
#   curl -fsSL https://raw.githubusercontent.com/Bell09/ytmtui/main/install.sh | sh
#   sh install.sh --system
#   YTMTUI_VERSION=0.1.8 sh install.sh
set -eu

REPO="Bell09/ytmtui"
ASSET="ytmtui-x86_64-linux.tar.gz"
INSTALL_DIR="${YTMTUI_INSTALL_DIR:-}"
VERSION="${YTMTUI_VERSION:-}"
SYSTEM=0

usage() {
    cat <<'EOF'
Usage: install.sh [--system] [--version VERSION]

  --system            install to /usr/local/bin (uses sudo when needed)
  --version VERSION   install a specific release, for example 0.1.8

Environment:
  YTMTUI_INSTALL_DIR  destination directory (default: ~/.local/bin)
  YTMTUI_VERSION      same as --version
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --system) SYSTEM=1 ;;
        --version)
            shift
            [ $# -gt 0 ] || { echo "install.sh: --version needs a value" >&2; exit 1; }
            VERSION="$1"
            ;;
        -h|--help) usage; exit 0 ;;
        *) echo "install.sh: unknown option $1" >&2; usage >&2; exit 1 ;;
    esac
    shift
done

fail() {
    echo "install.sh: $*" >&2
    exit 1
}

[ "$(uname -s)" = "Linux" ] || fail "only Linux is supported"
case "$(uname -m)" in
    x86_64|amd64) ;;
    *) fail "prebuilt binaries are x86_64 only, build from source instead" ;;
esac

command -v curl >/dev/null 2>&1 || fail "curl is required"
command -v sha256sum >/dev/null 2>&1 || fail "sha256sum is required"
command -v tar >/dev/null 2>&1 || fail "tar is required"

if [ -z "$INSTALL_DIR" ]; then
    if [ "$SYSTEM" -eq 1 ]; then
        INSTALL_DIR="/usr/local/bin"
    else
        INSTALL_DIR="$HOME/.local/bin"
    fi
fi

if [ -z "$VERSION" ]; then
    base="https://github.com/$REPO/releases/latest/download"
else
    base="https://github.com/$REPO/releases/download/v${VERSION#v}"
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM

echo "Downloading $ASSET..."
curl -fL --retry 3 --connect-timeout 20 -o "$tmp/$ASSET" "$base/$ASSET"
curl -fL --retry 3 --connect-timeout 20 -o "$tmp/$ASSET.sha256" "$base/$ASSET.sha256"

(cd "$tmp" && sha256sum -c "$ASSET.sha256")

tar -xzf "$tmp/$ASSET" -C "$tmp"
bin=$(find "$tmp" -type f -name ytmtui | head -n 1)
[ -n "$bin" ] || fail "the archive did not contain a ytmtui binary"

mkdir -p "$INSTALL_DIR"
if [ -w "$INSTALL_DIR" ]; then
    install -Dm755 "$bin" "$INSTALL_DIR/ytmtui"
else
    command -v sudo >/dev/null 2>&1 || fail "$INSTALL_DIR is not writable and sudo is missing"
    sudo install -Dm755 "$bin" "$INSTALL_DIR/ytmtui"
fi

echo "Installed $("$INSTALL_DIR/ytmtui" --version 2>/dev/null || echo ytmtui) to $INSTALL_DIR/ytmtui"

missing=""
for dep in mpv yt-dlp ffmpeg; do
    command -v "$dep" >/dev/null 2>&1 || missing="$missing $dep"
done
if [ -n "$missing" ]; then
    echo "Runtime dependencies still missing:$missing" >&2
    echo "On Arch: sudo pacman -S --needed$missing" >&2
fi

case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *) echo "Add $INSTALL_DIR to PATH to run ytmtui from any directory." ;;
esac
