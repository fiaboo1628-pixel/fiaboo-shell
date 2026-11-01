# Local patches to hyprglass

Engine: upstream hyprnux/hyprglass v1.1.0 (commit 85b8a8e, Hyprland 0.57), BSD-3.
Each change is marked with a `fiaboo:` comment. Re-apply them when updating the engine.

1. `src/GlassDecoration.cpp`, `renderPass()`: `glassAlpha = presentation().alpha().getTotal()`
   instead of `getTotalWithout(WINDOW_ALPHA_ACTIVE)`. Why: with `opacity "1.0 0.3"` on kitty and a
   clear terminal background, the pane is the whole window body; it must fade with focus too.

Until 2026-11 the engine was the Omarchy True Glass fork of hyprglass 1.0.1 (Eddie175), which never
got Hyprland 0.57 support. It also carried slower close springs for its "island" motion; upstream
has no island motion, so that patch is gone (layers use Hyprland's `fade` animation instead).

Config conventions (not patches): `scripts/glass.lua` must not call `hg.commit()` while Hyprland
is reloading (custom.lua sets `GLASS_IN_RELOAD`); fiaboo-session installs glass.lua and the built
.so into `~/.cache/fiaboo-shell/`.
