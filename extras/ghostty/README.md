# Ghostty files

Ghostty is the terminal since 2026-11-01 (kitty stays installed as a fallback). These files live in
the dotfiles, not in this repo; this folder keeps the reviewed copies next to the shell that shares
their look (same font, same blur, same theme pipeline).

| file | installed at | role |
|---|---|---|
| `config` | `~/.config/ghostty/config` | the terminal: font, padding, 85 % black body, blur, cursor, shader |
| `shaders/cursor-neon.glsl` | `~/.config/ghostty/shaders/cursor-neon.glsl` | the trail in use: neon comet |
| `shaders/cursor-trail.glsl` | `~/.config/ghostty/shaders/cursor-trail.glsl` | plain kitty-style trail (diamond), kept as the fallback |
| `ghostty.conf.tpl` | `~/.local/share/fiaboo-theme/templates/ghostty.conf.tpl` | rendered by `theme` to `~/.config/ghostty/current-theme.conf` |

## Measured facts (do not undo)

- Hyprland 0.57 only blurs Ghostty with `background-blur = true`; the blur rule in
  `~/.config/hypr/custom.lua` is not enough on its own.
- Ghostty ignores a `--class` without a dot, so the classes are `fiaboo.dropterm` and
  `fiaboo.tui-float-*`.
- `config-file` includes load after the main file, so the theme template never sets `background`;
  the body stays the black from `config`.
- `theme` reloads Ghostty with SIGUSR2 (tested safe).
- The trail must feel like kitty's `cursor_trail 1`, `cursor_trail_decay 0.1 0.5`: each corner
  slides home with exp2 decay, leading corners in 0.1 s, trailing in 0.5 s.

## Neon comet (2026-11-01, owner's pick)

`cursor-neon.glsl` replaced the kitty-style trail in `config`. A solid comet in the cursor's colour
runs from the old cursor's back edge (tail, 0.5 s) to the new one's front edge (head, 0.1 s), with
a blue glow; faint for a typed character, full from 4 cells up. Tried and dropped on the way:
three separate neon strands (their own timing made the trail look stiff), the diamond plus a glow
(the owner wanted a comet), and a comet drawn between the cursor centres (a typed character left no
visible tail: the cursor covered it). Test trails by typing real keys in fish, not only with jumps.
The neon colour is a constant in the shader, not from `theme`.

## What changed in these copies

- `config`: comments only, plus two commented-out split lines at the end. No value changed.
- `ghostty.conf.tpl`: one line added, `split-divider-color = {{ muted }}`, so the divider follows
  the theme like everything else. Nothing else changed; it still sets no `background`.
- `shaders/cursor-trail.glsl`: same motion, same numbers. Two additions:
  - a bounding-box early-out before the triangle and edge maths, so the ~99 % of pixels nowhere
    near the trail cost one compare instead of eight distance/cross products while a trail is up;
  - `if (iCursorVisible == 0) return;` (uniform present in Ghostty 1.4's shader prefix), so a TUI
    that hides its cursor while it draws gets no trail to a cursor nobody sees. This one is not
    measured on the desk; delete the line if a program you use looks worse with it.

## The dropped frame every 12-14 frames

Measured: Ghostty animates the shader at ~110 fps on the 120 Hz screen and drops a frame every
12-14 frames; kitty never does. The cause is in Ghostty 1.4's renderer, read from the source:

- `src/renderer/generic.zig`: `pub const draw_interval_ms: u64 = 8;` ("the cadence of continuous
  (draw-only) animation wakes, i.e. 120fps"). `animationWake()` returns this fixed interval while
  custom shaders animate (`.true` while focused, `.always` always). It never reads the display's
  refresh rate.
- `src/renderer/Thread.zig`: the animation timer is re-armed "after every frame update or
  animation draw", so the real period is 8 ms plus the draw itself, about 9 ms: ~110 fps.
- `hasVsync()` returns `false` everywhere but macOS (`DisplayLink` is `void` elsewhere), and
  `window-vsync` is documented as "only supported currently on macOS". On Linux the render thread
  is a free-running timer; the GTK side presents whatever the newest DMABUF is at each 120 Hz
  vblank. Two cadences, 8.33 ms and ~9 ms, beat against each other: once every ~12 vblanks there
  is no new frame to show and the previous one repeats. kitty instead draws from the compositor's
  frame callbacks, so it is locked to the screen.

So: there is no Ghostty 1.4 setting that removes it (no frame-rate option, no vsync on Linux,
`custom-shader-animation` only picks never / focused / always), and no shader trick can change
when the renderer draws. What a shader can do is make the skip harmless: compute everything from
`iTime` (not from the previous frame), which this shader already does, so a repeated frame is one
missing step of the slide, never a stall that catches up with a jump. The real fix is upstream:
pace animation wakes from the apprt's frame clock on Linux the way the macOS display link does.
That is the request to file against ghostty-org/ghostty; discussion #8818 ("Better performance
for custom-shader-animation") is the closest open thread.

## Splits and the tuios look

tuios draws a frame per pane (green focused, red unfocused) and a title in each. What Ghostty 1.4
splits can do, from the config reference and the 1.4 release notes, nothing more:

- `split-divider-color` (1.1.0+): one colour for every divider line. There is no focused versus
  unfocused divider colour, and no border around a pane, only the shared 1 px divider.
- `unfocused-split-opacity` (minimum 0.15; `1` disables) and `unfocused-split-fill` (defaults to
  the background colour): the only focus cue Ghostty has is dimming the panes that are not focused.
  With the 85 % black body this reads as the unfocused panes going darker, the same cue Hyprland
  gives unfocused windows at 0.5.
- `split-preserve-zoom` (1.3.0) and `drag-handle = auto|always|never` (1.4.0): zoom behaviour and
  mouse rearranging of splits. Cosmetic only in the sense that the handle is visible.
- Titles: `title`, `window-subtitle = working-directory` and `window-title-font-family` are per
  window (the GTK title bar, which this setup turns off), never per split. There is no per-pane
  title or status line.

So Ghostty alone gets you "the focused pane is the bright one and the dividers are theme-coloured".
Coloured frames and a title per pane need a multiplexer inside one Ghostty window: tmux with
`pane-border-style`, `pane-active-border-style` (red/green) and `pane-border-status top` with
`pane-border-format`, or tuios itself. The config here leaves `unfocused-split-opacity` commented
out because dimming also dims the content you are reading in the other pane; try it before keeping
it.
