# CLAUDE.md

# fiaboo-shell

A Rust GPUI desktop shell for Hyprland 0.56 (Lua config), replacing ML4W/quickshell: taskbar, Start, Task View, tray, quick settings + media, OSD, notifications, clipboard, screenshots, wallpaper, Settings, lunar calendar, music line.
UI text is English. Current state and history live in auto memory.

Back on Arch since 2026-10-27 after a NixOS trial: deployed with cargo again (the `package.nix`/flake path in `~/Projects/nixos-config` is history).

## Never break the running desktop
- The shell is the owner's live desktop, used at the desk; remote use is SSH from an iPhone (Tailscale), VNC is rare, so don't design the UI around touch. Replace things gradually, always keep a fallback (swaync for notifications, fuzzel as launcher fallback, nm-applet).
- Never `pkill -f` (it matches your own command line and kills the wrong thing).
- Back up a file before editing config outside the repo (`<file>.bakN` next to it).

## Layout
- `src/main.rs` (~1.2k lines): palette, icons, compositor/system state, the taskbar `Bar` + volume OSD, IPC and `main()`. One module per panel, each starting with `use crate::*;` and exposing its items `pub(crate)` (main re-exports them with `use <mod>::*`): `quick.rs`, `traymenu.rs`, `calendar.rs`, `notif_ui.rs`, `clip.rs`, `shot.rs`, `wallpaper.rs`, `settings.rs`, `start.rs`, `taskview.rs`. `src/wm.rs` Hyprland backend, `src/viz.rs` music line, `src/notify.rs` notification daemon (stored in `~/.local/state/fiaboo-notifs.json`), `src/tray.rs`, `src/lunar.rs`.
- `scripts/fiaboo-session`: supervisor started by Hyprland (`~/.config/hypr/custom.lua`). `scripts/fiaboo-record`, `scripts/fiaboo-wallpaper`, `scripts/hypr-autostart.lua` (installed as `~/.config/hypr/conf/autostart.lua`).
- Glass: `scripts/hyprglass/` is the vendored hyprglass Hyprland plugin (C++, BSD-3, from omarchy-true-glass 1.0.1); `fiaboo-session` builds it once per Hyprland version into `~/.cache/fiaboo-shell/hyprglass-<ver>.so`, loads it and applies `scripts/glass.lua`, which it also copies to `~/.cache/fiaboo-shell/glass.lua` for `custom.lua` to re-apply on reload (presets per `fiaboo-*` namespace, kitty's black-glass tag). glass.lua must not `hg.commit()` during a reload (`GLASS_IN_RELOAD`). Engine patches are listed in `scripts/hyprglass/PATCHES.md`. The shell itself knows nothing about it. To disable: `hyprctl plugin unload <so>`; a Hyprland crash report naming hyprglass keeps it off until the engine is rebuilt.
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
3. Upgrade the live shell: build release → `cp target/release/fiaboo-shell ~/.local/bin/fiaboo-shell.new && mv ~/.local/bin/fiaboo-shell.new ~/.local/bin/fiaboo-shell` → `pkill -9 -x fiaboo-shell` once. The supervisor restarts it after ~1 s. `scripts/fiaboo-session` is installed the same way as `~/.local/bin/fiaboo-session` (takes effect at the next login).
   - Use `-9`: exit code 143/130 (SIGTERM/SIGINT) means "stopped on purpose" and the supervisor exits, leaving no shell. 137 (`-9`) always restarts at once.
   - If the shell dies any other way within 30 s of starting, the supervisor starts swaync and retries every 30 s (log path in the notification).
   - The supervisor's process name is `bash`, so `pgrep fiaboo-session` finds nothing; use `pgrep -af bin/fiaboo-session`. Never start a second one.
4. Verify the UI with `grim` screenshots. Don't rely on ydotool mouse positioning (inaccurate).

## Git
- Work on a feature branch, merge into `main` with `git merge --no-ff` ("Merge <branch>: <summary>").
- **Do not push.** `origin` is Forgejo on the home server, which the owner has asked not to be
  touched from this machine; and the flake input reads the local checkout, so a push buys nothing
  here. Commit locally and leave it. (`nixos-config` is the opposite: it does go to GitHub.)

## UI/UX skills

Taste, a11y, bans, voice, and icon library for this project live in `.ux-profile.md`.
Run `setup-ui-ux-skills` again only to change those defaults.
Before UI or microcopy work, read `.ux-profile.md`, then the matching domain skill (surfaces, loaders, empty-states, motion, typography, spacing, colour-palette).
Before finishing UI, run `ux-audit`.

## Owner's taste
- One font everywhere: JetBrainsMono Nerd Font, set on every panel root (`.font_family(NF)`); GPUI otherwise falls back to the system sans. Type scale 12 meta / 14 content / 16 titles. Anything larger is a one-off display number (clock, OSD percentage), not a scale step — don't reach for 18 or 20 as "slightly bigger".
- Radii in use: `md` small controls, `lg` inner surfaces and rows, `xl` cards, `2xl` panel shells, `full` pills and dots. Those five, nothing new; the drift that got cleaned up in 2026-10 was single elements picking a neighbour's radius.
- Glass background: constant `GLASS` (alpha 0xC8); the glass plugin sees through it (panels frosted, blur 3, since 2026-10-24; kitty stays clear). Text ≥ 12px, heaviest weight `SEMIBOLD`.
- **Colours are not literals.** `palette()` reads `~/.config/fiaboo-shell/palette.conf` (written by `theme`, `~/.local/bin/theme`, themes in `~/.local/share/fiaboo-theme`) once at startup, with Catppuccin Mocha as the fallback. Take colours from the `Palette` and the `A_*` alpha constants; a new hex in a panel is a bug, because it won't follow the theme.
- **The glass has two fills, keyed on what is behind a surface, not on what it is.** `scripts/glass.lua`: `BAR = 0.7` for the taskbar, whose exclusive zone means only the wallpaper is ever behind it; `PANEL = 0.9` for everything else, which can land over a bright window. Measured over the lightest background a window can produce, body text holds 6.4:1 at 0.9 and falls to 3.9:1 at 0.6. There used to be four levels; they were four complaints answered on four days, not a ladder.
- `fill_key` in glass.lua is a **chroma key** and must equal the colour the panels actually paint, i.e. `palette().bg`. It is rendered into `~/.config/hypr/glass-colors.lua` from the same `colors.toml` for exactly that reason — hand-editing one and not the other makes a panel's body a different colour from its own text.
- Motion must be soft and slow; the owner has rejected "fast/snappy" easing several times (current: `smoothOut` = easeOutQuint).
- Music: a thin line on the taskbar's top edge that grows out from the middle with loudness, pushed a little on each kick; mauve in the middle -> blue -> melting into the bar at the ends; fades out in silence. ~3 % CPU at 30 fps, no measurable GPU. No fullscreen view. Rejected: crossing waves (15 % CPU), bars, spikes, filled rings, blurred cover, vinyl view, fullscreen shaders, WayVes (37-87 % iGPU).
