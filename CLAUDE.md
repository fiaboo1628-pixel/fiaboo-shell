# CLAUDE.md

# fiaboo-shell

A Rust GPUI desktop shell for Hyprland 0.57 (Lua config), replacing ML4W/quickshell: taskbar, Start, Task View, tray, quick settings + media, OSD, notifications, clipboard, screenshots, wallpaper, Settings, lunar calendar, music line.
UI text is English. Current state and history live in auto memory.

Back on Arch since 2026-10-27 after a NixOS trial: deployed with cargo again (the `package.nix`/flake path in `~/Projects/nixos-config` is history).

## Never break the running desktop
- The shell is the owner's live desktop, used at the desk; remote use is SSH from an iPhone (Tailscale), VNC is rare, so don't design the UI around touch. Replace things gradually, always keep a fallback (swaync for notifications, fuzzel as launcher fallback, nm-applet).
- Never `pkill -f` (it matches your own command line and kills the wrong thing).
- Back up a file before editing config outside the repo (`<file>.bakN` next to it).

## Layout
- `src/main.rs` (~1.2k lines): palette, icons, compositor/system state, the taskbar `Bar` + volume OSD, IPC and `main()`. One module per panel, each starting with `use crate::*;` and exposing its items `pub(crate)` (main re-exports them with `use <mod>::*`): `quick.rs`, `traymenu.rs`, `calendar.rs`, `notif_ui.rs`, `clip.rs`, `shot.rs`, `wallpaper.rs`, `settings.rs`, `start.rs`, `taskview.rs`. `src/wm.rs` Hyprland backend, `src/viz.rs` music line, `src/notify.rs` notification daemon (stored in `~/.local/state/fiaboo-notifs.json`), `src/tray.rs`, `src/lunar.rs`.
- `scripts/fiaboo-session`: supervisor started by Hyprland (`~/.config/hypr/custom.lua`). `scripts/fiaboo-record`, `scripts/fiaboo-wallpaper`, `scripts/hypr-autostart.lua` (installed as `~/.config/hypr/conf/autostart.lua`).
- No glass plugin since 2026-11-01 (hyprglass was vendored 2026-10-13..11-01; it needed porting with every Hyprland release). Every surface paints `PANEL_FILL` over Hyprland's blur; the layer rules (blur + `ignore_alpha`, no layer animation for panels) live in `~/.config/hypr/custom.lua`.
- IPC: `fiaboo-shell <start|taskview|clipboard|shot|wallpaper|settings|calendar|viz>` via `$XDG_RUNTIME_DIR/fiaboo-shell.sock`.

## Architecture
- One process. `main()`: with an argument it is an IPC client (`send_ipc`, exit 1 if no shell listens); otherwise it starts the IPC socket, `notify::start()`, `tray::start()` (D-Bus via zbus, own threads), then opens the taskbar `Bar` as a layer-shell window (`Layer::Top`, exclusive zone).
- Every panel/popup (Start, Task View, quick settings, calendar, clipboard, shot, OSD, notifications…) is its own `WindowKind::LayerShell` window on `Layer::Overlay`. Its handle lives in a GPUI `Global` (`QuickState`, `CalState`, …) and a `toggle_*`/`close_*` pair opens/closes it; follow that pattern for new panels (namespace `fiaboo-*`, and add it to both panel regexes in `~/.config/hypr/custom.lua` so it gets the blur and no Hyprland layer animation).
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
- Push `main` to Forgejo `sever/fiaboo-shell` after merging (the owner OK'd pushing from Arch again, 2026-10-27; the NixOS-era "do not push" rule is gone).

## UI/UX skills

Taste, a11y, bans, voice, and icon library for this project live in `.ux-profile.md`.
Run `setup-ui-ux-skills` again only to change those defaults.
Before UI or microcopy work, read `.ux-profile.md`, then the matching domain skill (surfaces, loaders, empty-states, motion, typography, spacing, colour-palette).
Before finishing UI, run `ux-audit`.

## Owner's taste
- One font everywhere: JetBrainsMono Nerd Font, set on every panel root (`.font_family(NF)`); GPUI otherwise falls back to the system sans. Type scale 12 meta / 14 content / 16 titles. Anything larger is a one-off display number (clock, OSD percentage), not a scale step — don't reach for 18 or 20 as "slightly bigger".
- Radii in use: `md` small controls, `lg` inner surfaces and rows, `xl` cards, `2xl` panel shells, `full` pills and dots. Those five, nothing new; the drift that got cleaned up in 2026-10 was single elements picking a neighbour's radius.
- **One surface style, no glass** (panels since 2026-10-28, bar/toasts/OSD since 2026-11-01): `PANEL_FILL` (0xCC) over Hyprland's own blur (`layer_rule` blur + `ignore_alpha 0.5` in custom.lua; decoration `xray` blurs only the wallpaper, so window text never shows through; blur size 3). Glass followed a panel's painted contour a few frames late, so in-app motion showed text running ahead of it; the owner asked for the same treatment everywhere. kitty and the drop-down kitty: an 85 % black body (kitty.conf) over the same blur, 1 px border, unfocused opacity 0.5. Text ≥ 12px, heaviest weight `SEMIBOLD`.
- **Raised cards** (since 2026-10-31, owner asked for a "3D dashboard" look with colour): resting surfaces inside panels take `card_fill(colour, strong)` (a 155° tint of accent/accent2) + `card_shadow(&p, lifted)` (lit top edge, dark bottom edge, inner bloom, soft drop shadow; hover = lifted). Grids alternate accent/accent2. No perspective tilt (GPUI can't), no glow.
- **Colours are not literals.** `palette()` reads `~/.config/fiaboo-shell/palette.conf` (written by `theme`, `~/.local/bin/theme`, themes in `~/.local/share/fiaboo-theme`) once at startup, with Catppuccin Mocha as the fallback. Take colours from the `Palette` and the `A_*` alpha constants; a new hex in a panel is a bug, because it won't follow the theme.
- Motion must be soft and slow; the owner has rejected "fast/snappy" easing several times (current: `smoothOut` = easeOutQuint).
- Music: a thin line on the taskbar's top edge that grows out from the middle with loudness, pushed a little on each kick; mauve in the middle -> blue -> melting into the bar at the ends; fades out in silence. ~3 % CPU at 30 fps, no measurable GPU. No fullscreen view. Rejected: crossing waves (15 % CPU), bars, spikes, filled rings, blurred cover, vinyl view, fullscreen shaders, WayVes (37-87 % iGPU).
