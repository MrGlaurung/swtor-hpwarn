#!/usr/bin/env bash
# Installs hpwarn's icon and a .desktop entry for the current user, so the
# taskbar/app launcher shows the real icon instead of a generic fallback.
#
# On Wayland (KDE Plasma included), a window can't hand the compositor its
# icon directly the way it can on X11 - the taskbar instead looks up the
# icon via the window's app_id matching an installed .desktop file's
# `Icon=` entry. This script sets that up.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"
ICON_DIR="$SCRIPT_DIR/assets/icons"
DESKTOP_SRC="$SCRIPT_DIR/assets/hpwarn.desktop"

if [ ! -d "$ICON_DIR" ] || [ ! -f "$DESKTOP_SRC" ]; then
    echo "error: run this from the hpwarn project directory (expected assets/icons and assets/hpwarn.desktop)" >&2
    exit 1
fi

# Figure out which hpwarn binary to point Exec= at: prefer one already on
# PATH, otherwise fall back to a release build next to this script.
if command -v hpwarn >/dev/null 2>&1; then
    HPWARN_BIN="hpwarn"
elif [ -x "$SCRIPT_DIR/target/release/hpwarn" ]; then
    HPWARN_BIN="$SCRIPT_DIR/target/release/hpwarn"
else
    echo "warning: couldn't find an hpwarn binary on PATH or at target/release/hpwarn;" >&2
    echo "         the installed .desktop entry's Exec= will just say 'hpwarn' - make sure" >&2
    echo "         it ends up on your PATH (e.g. ~/.local/bin), or edit the entry afterwards." >&2
    HPWARN_BIN="hpwarn"
fi

ICON_THEME_DIR="$HOME/.local/share/icons/hicolor"
APPS_DIR="$HOME/.local/share/applications"
mkdir -p "$APPS_DIR"

for size in 16 32 48 64 128 256 512; do
    src="$ICON_DIR/hpwarn-$size.png"
    [ -f "$src" ] || continue
    dest_dir="$ICON_THEME_DIR/${size}x${size}/apps"
    mkdir -p "$dest_dir"
    install -m644 "$src" "$dest_dir/hpwarn.png"
done

sed "s#^Exec=.*#Exec=$HPWARN_BIN#" "$DESKTOP_SRC" > "$APPS_DIR/hpwarn.desktop"
chmod 644 "$APPS_DIR/hpwarn.desktop"

echo "Installed icon set under $ICON_THEME_DIR/*/apps/hpwarn.png"
echo "Installed $APPS_DIR/hpwarn.desktop (Exec=$HPWARN_BIN)"

# Best-effort cache refreshes so the new icon shows up without logging out.
command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q -t -f "$ICON_THEME_DIR" 2>/dev/null || true
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q "$APPS_DIR" 2>/dev/null || true
command -v kbuildsycoca6 >/dev/null 2>&1 && kbuildsycoca6 >/dev/null 2>&1 || true
command -v kbuildsycoca5 >/dev/null 2>&1 && kbuildsycoca5 >/dev/null 2>&1 || true

echo "Done. You may need to restart hpwarn (and possibly plasmashell/your taskbar) to see the new icon."
