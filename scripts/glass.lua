-- fiaboo-shell glass: hyprglass (upstream hyprnux 1.1.0, Hyprland 0.57) presets for the taskbar,
-- toasts, OSD and windows. fiaboo-session installs this file to ~/.cache/fiaboo-shell/glass.lua and
-- loads the plugin; ~/.config/hypr/custom.lua runs it on every config parse. The engine collects
-- hg.preset/hg.layer calls while the config is parsed and applies them once it is done (Hyprland
-- also reparses after every plugin load), so nothing here has to commit.

-- Hyprland layer rules come first: they must hold with the plugin off too (a crash keeps it off).
-- Panels (Start, quick settings, calendar, ...) have no glass since 2026-10-28: glass follows a
-- panel's painted outline a few frames late, so in-app motion showed text running ahead of it.
-- They paint PANEL_FILL over Hyprland's own blur, drawn in the same frame; ignore_alpha keeps the
-- blur to the painted card, decoration xray keeps window text out of it. No Hyprland layer
-- animation either (the calendar drops in-app).
hl.layer_rule({ match = { namespace = "^fiaboo-(start|quick|calendar|clip|notifcenter|taskview|settings|wallpaper|traymenu|shot)$" },
                no_anim = true, animation = "none", blur = true, ignore_alpha = 0.5 })
-- Without the plugin the glass surfaces would be a thin wash over the bare wallpaper: give them
-- Hyprland's blur. With it, the engine replaces that blur on the layers it glasses.
hl.layer_rule({ match = { namespace = "^fiaboo-(bar|notify|osd)$" }, blur = true, ignore_alpha = 0.04 })

local hg = hl.plugin.hyprglass
if not hg then return end

-- The glass surfaces paint a thin wash of the palette's bg (GLASS in src/main.rs) and the glass
-- carries the rest of their body as its tint, so the rim and bend sit above the colour instead of
-- under a solid fill (the old engine keyed that fill out; this one has no fill_key). The tint must
-- be the bg palette() returns or a surface's body drifts from its own text; `theme` renders it.
local ok, themed = pcall(dofile, os.getenv("HOME") .. "/.config/hypr/glass-colors.lua")
local BG = (ok and type(themed) == "table" and themed.bg) or 0x1e1e2e
local WASH = 0x80 / 0xFF -- GLASS in src/main.rs
-- How much body a surface has depends on what is BEHIND it, not on what it is:
--   bar: its exclusive zone keeps windows above it, only the wallpaper is ever behind it. 0.7.
--   toasts, OSD: can land over a bright window. 0.9 kept body text at 6.4:1 over the lightest
--   background a window produces (#a6a6a6); 0.6 fell to 3.9:1, under WCAG AA.
local function body(total) -- tint for a total body of `total` over the wash
  local a = math.floor((total - WASH) / (1 - WASH) * 0xFF + 0.5)
  return (BG << 8) | a
end

-- clear glass, a glint from the upper left, a thin lit bevel round every shape
hg.preset("fiaboo", {
  blur_strength = 0.0, blur_iterations = 1,
  specular_angle = 315, specular_strength = 0.85,
  fresnel_strength = 0.4,
  bevel_strength = 0.5, bevel_size = 1.8, bevel_angle = 315, bevel_shadow = 0.15,
  tint_color = 0x16161606,
})
-- app windows: a wide soft rim (only translucent windows show it; opaque ones are skipped)
hg.preset("fiaboo-window", { inherits = "fiaboo", edge_thickness_px = 36, bevel_strength = 0.2 })
-- kitty, the one loud piece: an 85 % black core under a wide bright rim that runs into the corners
-- (owner, 2026-10-28: "lớp đen bên dưới rõ lên, lớp kính phủ lên rồi chảy dài về bên góc"). The rim's
-- light colour is mixed in after the black tint, so the rim stays glassy while the core stays black.
-- A light blur under it (owner, 2026-11-01: "thêm 1 ít blur nền"; was Hyprland's size 4 / 2 passes,
-- which the engine now turns off on glassed windows, so the glass blurs instead: 1.0 = 12 px).
-- Unfocused it fades with the window (engine patch, PATCHES.md).
hg.preset("fiaboo-kitty", { inherits = "fiaboo", edge_thickness_px = 52, blur_strength = 1.0, blur_iterations = 2,
  bevel_strength = 0.6, fresnel_strength = 1.0, specular_strength = 1.0, fresnel_color = 0xC8D8FFB0,
  tint_color = 0x000000D9, dark = { adaptive_dim = 0.0, saturation = 0.6 } })
-- taskbar: light from straight above, so only its top edge catches it
hg.preset("fiaboo-bar", { inherits = "fiaboo", edge_thickness_px = 32, adaptive_dim = 0.1,
  bevel_angle = 0, bevel_strength = 0.3, bevel_shadow = 0.0, tint_color = body(0.7) })
hg.preset("fiaboo-small", { inherits = "fiaboo", edge_thickness_px = 24, tint_color = body(0.9) })

hg.config({
  enabled = true,
  default_theme = "dark",
  layers = { enabled = true, live_resample_fps = 30 },
})

-- windows get their preset by tag; kitty swaps it for its own (one tag per rule, later rules win)
hl.window_rule({ name = "glass-window", match = { class = ".*" }, tag = "+hyprglass_preset_fiaboo-window" })
hl.window_rule({ name = "glass-kitty-1", match = { class = "^kitty$" }, tag = "-hyprglass_preset_fiaboo-window" })
hl.window_rule({ name = "glass-kitty-2", match = { class = "^kitty$" }, tag = "+hyprglass_preset_fiaboo-kitty" })

-- listing a layer whitelists it: only these three get glass. The bar's shape reaches past the
-- screen on three sides, so its rim shows only along the top.
hg.layer("fiaboo-bar", { preset = "fiaboo-bar", shape = { inset = { 0, -80, -80, -80 }, radius = 0 } })
for _, ns in ipairs({ "fiaboo-notify", "fiaboo-osd" }) do
  hg.layer(ns, { preset = "fiaboo-small", mask_mode = "contour", mask_threshold = 0.04 })
end
