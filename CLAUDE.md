# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

# fiaboo-shell

A Rust GPUI desktop shell for Arch + Hyprland 0.56 (Lua config), replacing ML4W/quickshell: taskbar, Start, Task View, tray, quick settings + media, OSD, notifications, clipboard, screenshots, wallpaper, Settings, lunar calendar, music visualizer.
UI text is English. Current state and history live in auto memory.

## Never break the running desktop
- The shell is the owner's live desktop, often used remotely from an iPhone (Moonlight/SSH over Tailscale). Replace things gradually, always keep a fallback (swaync for notifications, fuzzel as launcher fallback, nm-applet).
- Never `pkill -f` (it matches your own command line and kills the wrong thing).
- Back up a file before editing config outside the repo (`<file>.bakN` next to it).

## Layout
- `src/main.rs` (~4.2k lines): almost everything. `src/viz.rs` visualizer, `src/notify.rs` notification daemon (stored in `~/.local/state/fiaboo-notifs.json`), `src/tray.rs`, `src/lunar.rs`.
- `scripts/fiaboo-session`: supervisor started by Hyprland (`~/.config/hypr/custom.lua`). `scripts/fiaboo-record`, `scripts/fiaboo-wallpaper`, `scripts/hypr-autostart.lua`.
- Glass: `scripts/hyprglass/` is the vendored hyprglass Hyprland plugin (C++, BSD-3, from omarchy-true-glass 1.0.1); `fiaboo-session` builds it once per Hyprland version into `~/.cache/fiaboo-shell/hyprglass-<ver>.so`, loads it and applies `scripts/glass.lua` (presets per `fiaboo-*` namespace; `custom.lua` re-applies it on reload). The shell itself knows nothing about it; Hyprland blur rules for `fiaboo-*` are gone. To disable: `hyprctl plugin unload <so>`; a Hyprland crash report naming hyprglass keeps it off until the engine is rebuilt.
- IPC: `fiaboo-shell <start|taskview|clipboard|shot|wallpaper|settings|calendar|viz>` via `$XDG_RUNTIME_DIR/fiaboo-shell.sock`.

## Architecture
- One process. `main()`: with an argument it is an IPC client (`send_ipc`, exit 1 if no shell listens); otherwise it starts the IPC socket, `notify::start()`, `tray::start()` (D-Bus via zbus, own threads), then opens the taskbar `Bar` as a layer-shell window (`Layer::Top`, exclusive zone).
- Every panel/popup (Start, Task View, quick settings, calendar, clipboard, shot, OSD, notifications…) is its own `WindowKind::LayerShell` window on `Layer::Overlay`. Its handle lives in a GPUI `Global` (`QuickState`, `CalState`, …) and a `toggle_*`/`close_*` pair opens/closes it; follow that pattern for new panels (namespace `fiaboo-*` so the glass presets in `scripts/glass.lua` and the fade layer rule apply).
- Threads → UI: background threads never touch GPUI; they write statics and the `Bar` loop picks them up. Hyprland `.socket2.sock` → `HYPR_DIRTY`; `pactl subscribe` → `OSD_PENDING`; tray polls D-Bus every 1.5 s → `tray::items()`; notify daemon bumps `notify::generation()`; recording = flag file `$XDG_RUNTIME_DIR/fiaboo-recording`.
- `Bar::new` drives everything: one task drains IPC commands into `toggle_*`; one 150 ms loop reads the statics above, re-runs `read_state()` (hyprctl -j) only when `HYPR_DIRTY` is set or every 10 s, `read_sys()` every 8 s, and calls `cx.notify()` only if something changed.
- `notify.rs` is a full `org.freedesktop.Notifications` D-Bus server (zbus); if the shell dies the name is released and D-Bus activation brings swaync back. `tray.rs` is both StatusNotifierWatcher and host.
- System data comes from shelling out (`sh`, `run_bg`: hyprctl, wpctl, pactl, playerctl, cliphist, grim…), not libraries. The visualizer is a GPUI entity drawn inside the bar; `playerctl --follow status` turns it on, and only while a player plays does it run `parec` (default sink monitor) and its own DFT.

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
- Glass background: constant `GLASS` (alpha 0xC8) so Hyprland blur shows. Text ≥ 12px, heaviest weight `SEMIBOLD`.
- Motion must be soft and slow; the owner has rejected "fast/snappy" easing several times (current: `smoothOut` = easeOutQuint).
- Visualizer: only a small WayVes-style pair of crossing waves (wallpaper primary -> foreground -> tertiary, from matugen) inside the taskbar, behind the icons, shown while music plays. No fullscreen view. Rejected: separate bars, spikes, filled rings, blurred cover background, the vinyl view, a fullscreen Shadertoy shader and WayVes itself (ugly and too heavy on the iGPU: 37-87 % busy).
