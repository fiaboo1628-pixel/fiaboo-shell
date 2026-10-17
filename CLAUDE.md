# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

# fiaboo-shell

A Rust GPUI desktop shell for Arch + Hyprland 0.56 (Lua config), replacing ML4W/quickshell: taskbar, Start, Task View, tray, quick settings + media, OSD, notifications, clipboard, screenshots, wallpaper, Settings, lunar calendar, music line.
UI text is English. Current state and history live in auto memory.

## Never break the running desktop
- The shell is the owner's live desktop, used at the desk; the owner sometimes works on it over SSH from an iPhone (Tailscale), but remote GUI use (VNC) is rare, so don't design the UI around touch. Replace things gradually, always keep a fallback (swaync for notifications, fuzzel as launcher fallback, nm-applet).
- Never `pkill -f` (it matches your own command line and kills the wrong thing).
- Back up a file before editing config outside the repo (`<file>.bakN` next to it).

## Layout
- `src/main.rs` (~1.2k lines): palette, icons, compositor/system state, the taskbar `Bar` + volume OSD, IPC and `main()`. One module per panel, each starting with `use crate::*;` and exposing its items `pub(crate)` (main re-exports them with `use <mod>::*`): `quick.rs`, `traymenu.rs`, `calendar.rs`, `notif_ui.rs`, `clip.rs`, `shot.rs`, `wallpaper.rs`, `settings.rs`, `start.rs`, `taskview.rs`. `src/wm.rs` Hyprland/niri backend, `src/viz.rs` music line, `src/notify.rs` notification daemon (stored in `~/.local/state/fiaboo-notifs.json`), `src/tray.rs`, `src/lunar.rs`.
- `scripts/fiaboo-session`: supervisor started by Hyprland (`~/.config/hypr/custom.lua`). `scripts/fiaboo-record`, `scripts/fiaboo-wallpaper`, `scripts/hypr-autostart.lua`.
- Glass: `scripts/hyprglass/` is the vendored hyprglass Hyprland plugin (C++, BSD-3, from omarchy-true-glass 1.0.1); `fiaboo-session` builds it once per Hyprland version into `~/.cache/fiaboo-shell/hyprglass-<ver>.so`, loads it and applies `scripts/glass.lua`, which it also copies to `~/.cache/fiaboo-shell/glass.lua` for `custom.lua` to re-apply on reload (presets per `fiaboo-*` namespace, kitty's black-glass tag). glass.lua must not `hg.commit()` during a reload (`GLASS_IN_RELOAD`). Engine patches are listed in `scripts/hyprglass/PATCHES.md`. The shell itself knows nothing about it; Hyprland blur rules for `fiaboo-*` are gone. To disable: `hyprctl plugin unload <so>`; a Hyprland crash report naming hyprglass keeps it off until the engine is rebuilt.
- IPC: `fiaboo-shell <start|taskview|clipboard|shot|wallpaper|settings|calendar|viz>` via `$XDG_RUNTIME_DIR/fiaboo-shell.sock`.

## Architecture
- One process. `main()`: with an argument it is an IPC client (`send_ipc`, exit 1 if no shell listens); otherwise it starts the IPC socket, `notify::start()`, `tray::start()` (D-Bus via zbus, own threads), then opens the taskbar `Bar` as a layer-shell window (`Layer::Top`, exclusive zone).
- Every panel/popup (Start, Task View, quick settings, calendar, clipboard, shot, OSD, notifications…) is its own `WindowKind::LayerShell` window on `Layer::Overlay`. Its handle lives in a GPUI `Global` (`QuickState`, `CalState`, …) and a `toggle_*`/`close_*` pair opens/closes it; follow that pattern for new panels (namespace `fiaboo-*` so the glass presets in `scripts/glass.lua` and the fade layer rule apply).
- Threads → UI: background threads never touch GPUI; they write statics and the `Bar` loop picks them up. Hyprland `.socket2.sock` → `HYPR_DIRTY`; `pactl subscribe` → `OSD_PENDING`; tray polls D-Bus every 1.5 s → `tray::items()`; notify daemon bumps `notify::generation()`; recording = flag file `$XDG_RUNTIME_DIR/fiaboo-recording`.
- `Bar::new` drives everything: one task drains IPC commands into `toggle_*`; one 150 ms loop reads the statics above, re-runs `read_state()` (hyprctl -j) only when `HYPR_DIRTY` is set or every 10 s, `read_sys()` every 8 s, and calls `cx.notify()` only if something changed.
- `notify.rs` is a full `org.freedesktop.Notifications` D-Bus server (zbus); if the shell dies the name is released and D-Bus activation brings swaync back. `tray.rs` is both StatusNotifierWatcher and host.
- System data comes from shelling out (`sh`, `run_bg`: hyprctl, wpctl, pactl, playerctl, cliphist, grim…), not libraries. The music line is its own 8 px layer surface (`fiaboo-musicline`, Layer::Top over the bar's top edge, so the bar never redraws for it); `playerctl --follow status` drives it, and only while a player plays does it exist and run `parec` (default sink monitor), measuring just loudness and bass.

## Build, test, upgrade
1. Long cargo builds: run in the background; on OOM use `-j4`.
2. Test: `dbus-run-session -- cargo test --release` (plain `cargo test` fails the tray tests because the running shell owns the watcher name). One test: `dbus-run-session -- cargo test --release <name>`.
3. Upgrade the live shell: build release → `cp target/release/fiaboo-shell ~/.local/bin/fiaboo-shell.new && mv ~/.local/bin/fiaboo-shell.new ~/.local/bin/fiaboo-shell` → `pkill -9 -x fiaboo-shell` once. The supervisor sees the new binary and restarts it after ~1 s.
   - Use `-9`: exit code 143/130 (SIGTERM/SIGINT) means "stopped on purpose" and the supervisor exits, leaving no shell.
   - If the shell dies within 30 s of starting, the supervisor starts swaync and retries every 30 s (log path in the notification).
4. Verify the UI with `grim` screenshots. Don't rely on ydotool mouse positioning (inaccurate).

## Git
- Work on a feature branch, merge into `main` with `git merge --no-ff` ("Merge <branch>: <summary>"), push to Forgejo `sever/fiaboo-shell`.

## UI/UX skills

Taste, a11y, bans, voice, and icon library for this project live in `.ux-profile.md`.
Run `setup-ui-ux-skills` again only to change those defaults.
Before UI or microcopy work, read `.ux-profile.md`, then the matching domain skill (surfaces, loaders, empty-states, motion, typography, spacing, colour-palette).
Before finishing UI, run `ux-audit`.

## Owner's taste
- One font everywhere: JetBrainsMono Nerd Font, set on every panel root (`.font_family(NF)`); GPUI otherwise falls back to the system sans. Type scale 12 meta / 14 content / 16 titles.
- Glass background: constant `GLASS` (alpha 0xC8); the glass plugin sees through it (no blur: owner keeps panels and kitty clear). Text ≥ 12px, heaviest weight `SEMIBOLD`.
- Motion must be soft and slow; the owner has rejected "fast/snappy" easing several times (current: `smoothOut` = easeOutQuint).
- Music: a thin line on the taskbar's top edge that grows out from the middle with loudness, pushed a little on each kick; mauve in the middle -> blue -> melting into the bar at the ends; fades out in silence (picked 2026-10-17 from Chromium mocks, "A2"). ~3 % CPU at 30 fps, no measurable GPU. No fullscreen view. Rejected: the two crossing waves it replaced (ugly, 15 % CPU), separate bars, spikes, filled rings, blurred cover background, the vinyl view, a fullscreen Shadertoy shader and WayVes itself (ugly and too heavy on the iGPU: 37-87 % busy).
