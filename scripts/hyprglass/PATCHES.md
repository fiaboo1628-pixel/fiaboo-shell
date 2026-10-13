# Local patches to hyprglass

Engine: hyprglass 1.0.1 as vendored by omarchy-true-glass (Eddie175/omarchy-true-glass, `engine/`).
Each change is marked with a `fiaboo:` comment. Re-apply them when updating the engine.

1. `src/motion/tokens.hpp`, `islandInPlace()`: close-side spring durations and delays doubled
   (Near/Far/Sides 0.24 → 0.48, Scale 0.2 → 0.4, Alpha 0.08 → 0.16, Glass 0.18 → 0.36; delays ×2).
   Why: the owner wants slow, soft motion; the stock close is ~0.25 s.
2. `src/GlassDecoration.cpp`, `draw()`: `glassAlpha = window->alphaTotal()` instead of
   `alphaTotalWithout(WINDOW_ALPHA_ACTIVE)`. Why: with `opacity "1.0 0.3"` on kitty and a clear
   terminal background, the pane is the whole window body; it must fade with focus too.

Config conventions (not patches): `scripts/glass.lua` must not call `hg.commit()` while Hyprland
is reloading (custom.lua sets `GLASS_IN_RELOAD`); fiaboo-session installs glass.lua and the built
.so into `~/.cache/fiaboo-shell/`.
