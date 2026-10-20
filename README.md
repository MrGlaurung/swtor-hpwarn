# hpwarn

A tiny, single-purpose SWTOR overlay. It does exactly one thing:

> Show nothing, unless your HP is at or below a threshold (default 50%).
> The moment it first drops to/below that threshold, show **HITPOINTS: NN%**
> on screen (updating live) and play a one-shot warning sound. It stays
> quiet again until you recover above the threshold and drop below it again.

It reads your SWTOR combat log file directly (the same file DPS meters like
[Baras](https://github.com/baras-app/baras) use), so no in-game addon or
memory access is needed. It's a native Linux binary (Rust + egui), no
Wine/Proton tricks required for the overlay itself.

## How it finds your HP

Every combat log line carries the acting/target entity's current and max HP
as `(cur/max)`. hpwarn identifies *you* using the same trick every SWTOR log
parser uses: the `AreaEntered` and `DisciplineChanged` events are only ever
logged for the local player, so whoever appears on those lines is "you".
From then on it watches every line for your HP and ignores everyone else
(including your own companion, whose entity tag is `YourTag/CompanionName`,
never mistaken for your own `YourTag`).

## Building

Requires a Rust toolchain (`rustup.rs`) and ALSA dev headers for audio:

```sh
# Debian/Ubuntu
sudo apt install libasound2-dev pkg-config

# openSUSE
sudo zypper install alsa-devel pkg-config

cargo build --release
```

The binary is `target/release/hpwarn`. Copy it anywhere on your `$PATH`, e.g.:

```sh
install -Dm755 target/release/hpwarn ~/.local/bin/hpwarn
```

### Giving it a proper icon

Without this step, your taskbar/app switcher will likely show a generic
fallback icon instead of hpwarn's own. hpwarn's windows do carry an
embedded icon, but on Wayland a window can't hand the compositor pixel data
for its own icon (there's no protocol for it) - the taskbar instead looks
one up via a `.desktop` file's `Icon=` entry, matched to the window. Run:

```sh
./install-icon.sh
```

once (after building, so it can find `target/release/hpwarn`, or after
installing hpwarn onto your `$PATH` as above). It installs the icon into
`~/.local/share/icons/hicolor/*/apps/hpwarn.png` and a matching entry into
`~/.local/share/applications/hpwarn.desktop`, and tries to refresh the
relevant caches. You may need to restart hpwarn (and occasionally
`plasmashell`) to see it update.

## First-time setup

```sh
hpwarn --configure
```

This opens a small settings window where you set:

- **Log file or folder** — either point it directly at a specific
  `combat_*.txt`, or (recommended) point it at your SWTOR `CombatLogs`
  folder. In folder mode, hpwarn always follows the newest `combat_*.txt`
  in there and automatically switches to a new one the moment the game
  starts a fresh log (e.g. on relogging), so you never have to re-point it.
  A common Proton path looks like:
  `~/.local/share/Steam/steamapps/compatdata/1286830/pfx/drive_c/users/steamuser/Documents/Star Wars - The Old Republic/CombatLogs`
  hpwarn guesses this automatically if it exists, but check it's right.
- **Threshold** — show/beep at or below this HP%.
- **Font** — Monospace/Proportional, or point `font_file` at any `.ttf`/`.otf`
  for a fully custom font.
- **Font size** and **colour**.
- **Position** — on X11, also settable just by dragging the overlay around
  once it's running (saved automatically on release). On Wayland's
  layer-shell overlay, dragging isn't available, so set `pos_x`/`pos_y` here
  instead.

Settings save to `~/.config/hpwarn/config.toml` and are **hot-reloaded** by
the running overlay (checked roughly once a second), so you can tweak
things live without restarting it.

## Running

```sh
hpwarn
```

This opens the **dashboard** (see below) and, in the background, starts the
log watcher and the HP overlay itself — one command does everything. Put it
in whatever you use to launch SWTOR (a Steam launch script, your window
manager's autostart, etc.) if you want it running automatically.

If you'd rather not have the dashboard window open (e.g. a minimal
autostart setup), pass `--no-dashboard` to get just the overlay, the same as
older versions of hpwarn.

### The dashboard

By default, running `hpwarn` opens a window split into two parts:

- **Configuration** (left) — the same settings as before: log path,
  threshold, font, colour, size, position. Changes save immediately and
  take effect right away (the overlay doesn't need restarting).
- **History** (right) — above, a curve graph of your HP% over time; below,
  a scrollable list of every HP change, newest first, each showing the
  amount and (when the log identifies it) the ability and source, e.g.:

  ```
  20261020 17:30:42.12  +10496 hp from Shae Vizla Kolto Drip ability
  20261020 17:30:48.90  -6503 hp from Annihilation Droid XRR-3 Burning ability
  ```

  The timestamp is `YYYYMMDD HH:MM:SS.cc` (centiseconds), using the log
  line's own time-of-day and today's date.

  The curve only gets a new point when your HP actually changes — it isn't
  sampled on a timer — and both the curve and the list keep the most recent
  500 changes.

`hpwarn --configure` still works as a lightweight settings-only window (no
overlay, no watcher, no history) if you just want to tweak the config file
without the game running.

### A note on "overlay" — X11 vs Wayland

**On X11**, this is a real, separate OS window placed on top via
window-manager always-on-top, not an in-game render injection — the same
approach every non-cheating SWTOR tool uses. You'll want SWTOR in
**borderless windowed** mode rather than exclusive fullscreen, since exclusive
fullscreen bypasses the window manager entirely. Click-and-drag anywhere on
the overlay to reposition it; the position is saved on release. The window
occupies a small fixed region wherever you place it (it doesn't shrink to
nothing while hidden), so put it somewhere you don't need to click during
fights, e.g. a screen corner.

**On Wayland** (`hpwarn` detects this automatically via `$WAYLAND_DISPLAY`),
ordinary windows have no way to ask to stay "always on top" — there's no such
protocol, by design — and even a compositor-level trick like KDE's Window
Rules "Keep above" only affects normal windows: compositors routinely stop
compositing entirely for any window that *looks* fullscreen (which SWTOR's
window does under Wine/Proton even in "Borderless Windowed" mode), and at
that point nothing can draw above it. So on Wayland, hpwarn instead creates a
**layer-shell surface** (`zwlr_layer_shell_v1`, on the `Overlay` layer) — the
same mechanism panels, docks and on-screen-displays use, and the same
technique the [Baras](https://github.com/baras-app/baras) DPS meter's own
Linux overlay uses. That layer composites above everything, including
fullscreen content, by protocol definition, so it works regardless of
Wine/Proton's fullscreen hinting. It's also fully click-through, so it can
never block a click in the game underneath it.

Trade-offs of the Wayland path: it requires a compositor that implements
`wlr-layer-shell` — true of KDE Plasma and wlroots-based compositors
(Hyprland, Sway, …), **not** true of GNOME/Mutter — and, because it isn't a
normal window, it doesn't support click-and-drag repositioning; set its
position with the `pos_x`/`pos_y` fields in `--configure` instead (hot-reloaded
like everything else). If your compositor doesn't support it, hpwarn prints
why and automatically falls back to the plain X11-style window described
above (pass `--no-layer-shell` to force that fallback yourself). On a Wayland
compositor that falls back, you're subject to the same fullscreen-compositing
caveat X11 has.

## Latency

hpwarn polls the log file every 50ms for new lines, parses them immediately,
and updates the on-screen state the moment a relevant line is found — there
is no batching delay on the hpwarn side. The remaining delay is however long
the game itself takes to flush the new combat log line to disk, which is
outside hpwarn's control but is normally well under a second.

## Files

- `src/parser.rs` — combat log line parsing, "who am I" detection, and ability/source attribution for HP changes.
- `src/watcher.rs` — file tailing, HP-crossing / beep-trigger logic, and history/curve recording.
- `src/state.rs` — state shared between the watcher thread and the UI windows (HP%, history, curve, config knobs).
- `src/audio.rs` — synthesizes the warning tone (no external sound file).
- `src/overlay.rs` — the plain transparent draggable overlay window (X11, or Wayland fallback).
- `src/wayland_overlay.rs` — the native Wayland `wlr-layer-shell` overlay.
- `src/dashboard.rs` — the default always-on window: configuration + HP history/curve.
- `src/configure.rs` — the `--configure`-only lightweight settings window.
- `src/config.rs` — config file (TOML) load/save/defaults.

Run `cargo test` to run the unit tests. One of them renders the Wayland
overlay's text through the exact same code path the real overlay uses and
saves it to `$TMPDIR/hpwarn_wayland_overlay_test.png`, since there's no
compositor in a typical CI/build environment to check it against live.
