# fiaboo-shell

A Rust GPUI desktop shell for Arch + Hyprland 0.56 (Lua config), replacing ML4W/quickshell: taskbar, Start, Task View, tray, quick settings + media, OSD, notifications, clipboard, screenshots, wallpaper, Settings, lunar calendar, music visualizer.
UI text is English. Current state and history live in auto memory.

## Never break the running desktop
- The shell is the owner's live desktop, often used remotely from an iPhone (Moonlight/SSH over Tailscale). Replace things gradually, always keep a fallback (swaync for notifications, rofi as launcher fallback, nm-applet).
- Never touch Wi-Fi/network. Never reboot unless asked.
- Never `pkill -f` (it matches your own command line and kills the wrong thing).
- Back up a file before editing config outside the repo (`<file>.bakN` next to it).

## Layout
- `src/main.rs` (~4.2k lines): almost everything. `src/viz.rs` visualizer, `src/notify.rs` notification store (`~/.local/state/fiaboo-notifs.json`), `src/tray.rs`, `src/lunar.rs`.
- `scripts/fiaboo-session`: supervisor started by Hyprland (`~/.config/hypr/custom.lua`). `scripts/fiaboo-record`, `scripts/fiaboo-wallpaper`, `scripts/hypr-autostart.lua`.
- IPC: `fiaboo-shell <start|taskview|clipboard|shot|wallpaper|settings|calendar|viz|viz-full>` via `$XDG_RUNTIME_DIR/fiaboo-shell.sock`.
- Keybindings: `~/.config/hypr/conf/keybindings/default.lua` (dotfiles: `dot add/commit/push`).

## Build, test, upgrade
1. Long cargo builds: run in the background; on OOM use `-j4`.
2. Test: `dbus-run-session -- cargo test --release` (plain `cargo test` fails the tray tests because the running shell owns the watcher name).
3. Upgrade the live shell: build release → `cp target/release/fiaboo-shell ~/.local/bin/fiaboo-shell.new && mv ~/.local/bin/fiaboo-shell.new ~/.local/bin/fiaboo-shell` → `pkill -9 -x fiaboo-shell` once. The supervisor sees the new binary and restarts it after ~1 s.
   - Use `-9`: exit code 143/130 (SIGTERM/SIGINT) means "stopped on purpose" and the supervisor exits, leaving no shell.
   - If the shell dies within 30 s of starting, the supervisor starts swaync and retries every 30 s (log path in the notification).
4. Verify the UI with `grim` screenshots. Don't rely on ydotool mouse positioning (inaccurate).
5. Env when running from Claude Code: `WAYLAND_DISPLAY=wayland-1`, `HYPRLAND_INSTANCE_SIGNATURE=$(command ls -t /run/user/1000/hypr | head -1)`, `DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus`.

## Git
- Work on a feature branch, merge into `main` with `git merge --no-ff` ("Merge <branch>: <summary>"), push to Forgejo `sever/fiaboo-shell`.

## Owner's taste
- One font everywhere: JetBrainsMono Nerd Font.
- Glass background: constant `GLASS` (alpha 0xC8) so Hyprland blur shows. Text ≥ 12px, heaviest weight `SEMIBOLD`.
- Motion must be soft and slow; the owner has rejected "fast/snappy" easing several times (current: `smoothOut` = easeOutQuint).
- Visualizer: soft borderless wave strip above the taskbar; fullscreen = black glass, vinyl with the thumbnail as label, thin rays. Rejected: separate bars, spikes, filled rings, blurred cover background.
