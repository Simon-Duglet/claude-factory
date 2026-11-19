#!/bin/sh
# Installs the binary, the desktop entry and the icon for the current user.
set -e
cd "$(dirname "$0")"

# Build prerequisites. Window system libraries (Wayland, X11, xkbcommon) load at runtime, so the build
# only needs a C compiler. Each check is "command-to-test apt-pkg dnf-pkg pacman-pkg zypper-pkg".
missing_apt="" missing_dnf="" missing_pacman="" missing_zypper="" missing_names=""
need() {
    command -v "$1" >/dev/null 2>&1 && return
    missing_names="$missing_names $1"
    missing_apt="$missing_apt $2" missing_dnf="$missing_dnf $3"
    missing_pacman="$missing_pacman $4" missing_zypper="$missing_zypper $5"
}
need cc build-essential gcc base-devel gcc
if [ -n "$missing_names" ]; then
    echo "Missing build tools:$missing_names" >&2
    if command -v apt-get >/dev/null; then cmd="apt-get install -y$missing_apt"
    elif command -v dnf >/dev/null; then cmd="dnf install -y$missing_dnf"
    elif command -v pacman >/dev/null; then cmd="pacman -S --needed --noconfirm$missing_pacman"
    elif command -v zypper >/dev/null; then cmd="zypper install -y$missing_zypper"
    else cmd=""
    fi
    if [ -n "$cmd" ]; then
        echo "Installing them with: sudo $cmd" >&2
    fi
    if [ -z "$cmd" ] || ! sudo $cmd; then
        echo "Could not install them. Install them yourself, then run ./install.sh again:" >&2
        echo "  Debian/Ubuntu: sudo apt-get install$missing_apt" >&2
        echo "  Fedora:        sudo dnf install$missing_dnf" >&2
        echo "  Arch:          sudo pacman -S$missing_pacman" >&2
        echo "  openSUSE:      sudo zypper install$missing_zypper" >&2
        exit 1
    fi
fi

cargo install --path .
bin="$(command -v claude-factory || echo "$HOME/.cargo/bin/claude-factory")"
apps="$HOME/.local/share/applications"
icons="$HOME/.local/share/icons/hicolor/256x256/apps"
mkdir -p "$apps" "$icons"
# Absolute Exec path: ~/.cargo/bin is often missing from the desktop session's PATH.
sed "s|^Exec=.*|Exec=$bin|" claude-factory.desktop > "$apps/claude-factory.desktop"
cp res/logo-256.png "$icons/claude-factory.png"
gtk-update-icon-cache -q "$HOME/.local/share/icons/hicolor" 2>/dev/null || true
update-desktop-database -q "$apps" 2>/dev/null || true
# KDE: a running Plasma keeps its old icon lookups (blank taskbar icon) until told the cache changed.
kbuildsycoca6 >/dev/null 2>&1 || true
dbus-send --session --type=signal /KIconLoader org.kde.KIconLoader.iconChanged int32:0 2>/dev/null || true
command -v claude >/dev/null || echo "Warning: \`claude\` is not on PATH. Install Claude Code, or the team sessions cannot start." >&2
echo "Installed $bin, $apps/claude-factory.desktop and $icons/claude-factory.png"
