#!/bin/sh
# Installs the binary, the desktop entry and the icon for the current user.
set -e
cd "$(dirname "$0")"
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
