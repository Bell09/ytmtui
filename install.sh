#!/bin/sh
# Install the latest ytmtui release binary and its runtime dependencies.
#
#   curl -fsSL https://github.com/Bell09/ytmtui/releases/latest/download/install.sh | sh
#   curl -fsSL https://raw.githubusercontent.com/Bell09/ytmtui/Main/install.sh | sh
#   sh install.sh --system
#   YTMTUI_VERSION=0.1.20 sh install.sh
#
# The first URL is served from the release assets and works on networks that
# cannot resolve raw.githubusercontent.com.
set -eu

REPO="Bell09/ytmtui"
ASSET="ytmtui-x86_64-linux.tar.gz"
DEPS="mpv yt-dlp ffmpeg"
INSTALL_DIR="${YTMTUI_INSTALL_DIR:-}"
VERSION="${YTMTUI_VERSION:-}"
SYSTEM=0

usage() {
    cat <<'EOF'
Usage: install.sh [--system] [--version VERSION]

  --system            install to /usr/local/bin (uses sudo when needed)
  --version VERSION   install a specific release, for example 0.1.20

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

# --- platform checks --------------------------------------------------------

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

# --- dependency detection ---------------------------------------------------

detect_pkg_manager() {
    # Return the package manager name and install command, one per line.
    # Order matters: prefer the most specific match first.
    if command -v pacman >/dev/null 2>&1; then
        echo "pacman"
        return 0
    fi
    if command -v apt-get >/dev/null 2>&1; then
        echo "apt-get"
        return 0
    fi
    if command -v dnf >/dev/null 2>&1; then
        echo "dnf"
        return 0
    fi
    if command -v yum >/dev/null 2>&1; then
        echo "yum"
        return 0
    fi
    if command -v zypper >/dev/null 2>&1; then
        echo "zypper"
        return 0
    fi
    if command -v apk >/dev/null 2>&1; then
        echo "apk"
        return 0
    fi
    return 1
}

have_sudo() {
    command -v sudo >/dev/null 2>&1
}

run_as_root() {
    if [ "$(id -u)" -eq 0 ]; then
        "$@"
    elif have_sudo; then
        sudo "$@"
    else
        return 1
    fi
}

# Install missing runtime dependencies. Returns 0 on success, 1 on failure.
install_deps() {
    missing=""
    for dep in $DEPS; do
        command -v "$dep" >/dev/null 2>&1 || missing="$missing $dep"
    done
    [ -n "$missing" ] || return 0

    pkg_manager=$(detect_pkg_manager) || {
        echo "install.sh: missing runtime dependencies:$missing" >&2
        echo "install.sh: no supported package manager found (tried pacman, apt-get, dnf, yum, zypper, apk)" >&2
        echo "install.sh: install them manually, for example: sudo <pkg-manager> install$missing" >&2
        return 1
    }

    case "$pkg_manager" in
        pacman)
            # Full upgrade: a partial 'pacman -Sy' can leave installed
            # packages on older libraries and break the system.
            cmd="pacman -Syu --needed --noconfirm$missing"
            ;;
        apt-get)
            cmd="apt-get update && apt-get install -y$missing"
            ;;
        dnf)
            cmd="dnf install -y$missing"
            ;;
        yum)
            cmd="yum install -y$missing"
            ;;
        zypper)
            cmd="zypper install -y$missing"
            ;;
        apk)
            cmd="apk add$missing"
            ;;
        *)
            echo "install.sh: unsupported package manager: $pkg_manager" >&2
            echo "install.sh: install the dependencies manually:$missing" >&2
            return 1
            ;;
    esac

    echo "Installing runtime dependencies:$missing via $pkg_manager..."
    if ! run_as_root sh -c "$cmd"; then
        echo "install.sh: failed to install dependencies:$missing via $pkg_manager" >&2
        echo "install.sh: try installing them manually, then re-run this script" >&2
        return 1
    fi

    # Re-check after install.
    missing=""
    for dep in $DEPS; do
        command -v "$dep" >/dev/null 2>&1 || missing="$missing $dep"
    done
    [ -z "$missing" ] || {
        echo "install.sh: dependencies still missing after install:$missing" >&2
        return 1
    }
    return 0
}

# Verify each dependency actually runs.
verify_deps() {
    for dep in $DEPS; do
        case "$dep" in
            mpv)
                "$dep" --version >/dev/null 2>&1 || {
                    echo "install.sh: $dep is installed but does not run (mpv --version failed)" >&2
                    return 1
                }
                ;;
            yt-dlp)
                "$dep" --version >/dev/null 2>&1 || {
                    echo "install.sh: $dep is installed but does not run (yt-dlp --version failed)" >&2
                    return 1
                }
                ;;
            ffmpeg)
                "$dep" -version >/dev/null 2>&1 || {
                    echo "install.sh: $dep is installed but does not run (ffmpeg -version failed)" >&2
                    return 1
                }
                ;;
        esac
    done
    return 0
}

# --- install ytmtui binary --------------------------------------------------

# Download a URL, reporting DNS/proxy problems in plain language. Some
# networks cannot resolve raw.githubusercontent.com while github.com works.
download() {
    url="$1"
    out="$2"
    err="$tmp/download.err"
    if curl -fL --retry 3 --connect-timeout 20 -o "$out" "$url" 2>"$err"; then
        return 0
    fi
    if grep -q "Could not resolve host" "$err" 2>/dev/null; then
        echo "install.sh: DNS could not resolve the download host ($url)" >&2
        echo "install.sh: try a different host, for example:" >&2
        echo "install.sh:   curl -fL -o ytmtui.tar.gz \\" >&2
        echo "install.sh:     https://github.com/$REPO/releases/latest/download/$ASSET" >&2
        echo "install.sh:   tar -xzf ytmtui.tar.gz && install -Dm755 ytmtui-*/ytmtui ~/.local/bin/ytmtui" >&2
    else
        echo "install.sh: download failed: $url" >&2
        sed 's/^/install.sh: /' "$err" >&2 || true
    fi
    return 1
}

echo "Downloading $ASSET..."
download "$base/$ASSET" "$tmp/$ASSET" || fail "could not download $ASSET"
download "$base/$ASSET.sha256" "$tmp/$ASSET.sha256" || fail "could not download $ASSET.sha256"

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

# --- install and verify runtime dependencies --------------------------------

if ! install_deps; then
    echo "On Arch-based systems: sudo pacman -S --needed $DEPS" >&2
    fail "runtime dependencies could not be installed; see errors above"
fi

if ! verify_deps; then
    fail "runtime dependencies failed verification; see errors above"
fi

echo "Runtime dependencies verified: mpv yt-dlp ffmpeg"

case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *) echo "Add $INSTALL_DIR to PATH to run ytmtui from any directory." ;;
esac

echo "ytmtui is ready. Run 'ytmtui setup' to authenticate, then play music."
