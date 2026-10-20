#!/usr/bin/env bash
# Installs hpwarn's icon and .desktop entries for the current user, so the
# launcher, taskbar, window titlebar AND alt-tab/task-switcher all show the
# real icon instead of a generic fallback.
#
# On Wayland (KDE Plasma included), a window can't hand the compositor its
# icon directly the way it can on X11 - there's no protocol for it, and
# winit (hpwarn's GUI toolkit) doesn't implement the newer
# xdg_toplevel_icon_v1 extension that some compositors support for this.
# Instead, KWin (and other Wayland shells) resolve a *running window's*
# icon by matching its app_id to an installed .desktop file - and for that
# match to work reliably, the .desktop file's own filename has to equal
# "<app_id>.desktop", not just contain a matching StartupWMClass field.
# hpwarn has three windows with three different app_ids (so a window rule
# like "always on top" only catches the one it's meant for - see
# src/main.rs), so there are three small .desktop files here, one per
# window, all pointing at the same icon.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"
ICON_DIR="$SCRIPT_DIR/assets/icons"
DESKTOP_FILES=(hpwarn-dashboard.desktop hpwarn-settings.desktop hpwarn.desktop)

if [ ! -d "$ICON_DIR" ] || [ ! -f "$SCRIPT_DIR/assets/${DESKTOP_FILES[0]}" ]; then
    echo "error: run this from the hpwarn project directory (expected assets/icons and assets/*.desktop)" >&2
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
    echo "         the installed .desktop entries' Exec= will just say 'hpwarn' - make sure" >&2
    echo "         it ends up on your PATH (e.g. ~/.local/bin), or edit the entries afterwards." >&2
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
echo "Installed icon set under $ICON_THEME_DIR/*/apps/hpwarn.png"

for name in "${DESKTOP_FILES[@]}"; do
    # Only rewrite the binary itself (the "hpwarn" right after "Exec="),
    # keeping any trailing flags like " --configure" intact.
    sed "s#^Exec=hpwarn#Exec=$HPWARN_BIN#" "$SCRIPT_DIR/assets/$name" > "$APPS_DIR/$name"
    chmod 644 "$APPS_DIR/$name"
    echo "Installed $APPS_DIR/$name"
done

# Best-effort cache refreshes so the new icons show up without logging out.
command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q -t -f "$ICON_THEME_DIR" 2>/dev/null || true
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q "$APPS_DIR" 2>/dev/null || true
command -v kbuildsycoca6 >/dev/null 2>&1 && kbuildsycoca6 >/dev/null 2>&1 || true
command -v kbuildsycoca5 >/dev/null 2>&1 && kbuildsycoca5 >/dev/null 2>&1 || true

echo "Done. Fully quit hpwarn and start it again to see the new icon in the"
echo "titlebar and alt-tab (KWin resolves a running window's icon once, when"
echo "it first appears, so an already-running hpwarn won't update live)."
