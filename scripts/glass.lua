-- fiaboo-shell glass: hyprglass presets for the fiaboo-* layers.
-- Applied by fiaboo-session after `hyprctl plugin load` (hyprctl eval), and again by
-- ~/.config/hypr/custom.lua on every Hyprland reload, which sets GLASS_IN_RELOAD first:
-- during a reload the engine commits by itself once the config is parsed, and an early
-- hg.commit() here would empty its pending presets and layers (the glass then vanished).
local hg = hl.plugin.hyprglass
if not hg then return end

-- the panels paint p.bg at alpha 0xC8: the engine sees through that fill and keeps only
-- fill_alpha of it, so the glass reads underneath. This has to be the same colour
-- palette().bg returns in src/main.rs, or a panel's body ends up a different colour from
-- its own text. nixos-config's `theme` writes glass-colors.lua beside the hypr config when
-- a theme is applied; the literal is the Catppuccin Mocha base, which is both what
-- palette() still hardcodes and what the file holds when the theme is catppuccin.
local ok, themed = pcall(dofile, os.getenv("HOME") .. "/.config/hypr/glass-colors.lua")
local BG = (ok and type(themed) == "table" and themed.bg) or 0x1e1e2e
-- How much body the glass keeps depends on what is BEHIND a surface, not on what it is.
-- There used to be four levels (0.6 decorative, 0.8 lists, 0.9 Start, 0.9 toasts). That was
-- not a ladder, it was four complaints answered on four days: the 0.6 comment still called
-- the calendar and the tray menu "decorative" while both were full of text.
--   bar: its exclusive zone keeps windows above it, so the only thing ever behind it is the
--   wallpaper, which the owner picks. Thin glass is safe.
--   everything else: can land over any window, including a bright one, so it carries its own body.
-- Measured over the lightest background a window can actually produce (#a6a6a6 -- the glass
-- dims a pure-white app by ~35 %): body text holds 6.4:1 at 0.9 and fell to 3.9:1 at 0.6,
-- under WCAG AA. 0.9 is where the owner landed twice unprompted (Start 2026-10-23, toasts
-- 2026-10-25); this only spares the rest from having to be complained about first.
local BAR = 0.7
local PANEL = 0.9

-- clear glass (n = 1.52), strong colour split at the rim (Abbe 4), a glint from the
-- upper left, a clear bevel round every shape; the rim reflects the scene around it
local glass = {
  ior = 1.52, abbe = 4,
  specular_angle = 315, light_elevation = 35, specular_strength = 0.85,
  fresnel_strength = 0.4,
  bevel_strength = 0.5, bevel_size = 1.8, bevel_angle = 315, bevel_shadow = 0.15,
  tint_color = 0x16161606,
}
local function preset(over)
  local p = {}
  for k, v in pairs(glass) do p[k] = v end
  for k, v in pairs(over) do p[k] = v end
  return p
end
-- app windows and the bar: thick glass with a wide rim (the bend is what reads as glass)
hg.preset("glass-window", preset({ thickness = 40, bezel_width = 36, blur_strength = 0.0, blur_iterations = 1, bevel_strength = 0.2 }))
hg.preset("glass-bar",    preset({ thickness = 32, bezel_width = 32, blur_strength = 0.0, blur_iterations = 1, adaptive_dim = 0.1,
                                   bevel_angle = 0, bevel_strength = 0.3, bevel_shadow = 0.0 }))
-- kitty: black glass, a neutral black tint (adaptive_dim took the wallpaper's hue instead);
-- no blur (owner kept it clear, 2026-10-17); unfocused it fades with the window (engine patch in GlassDecoration.cpp)
hg.preset("glass-dark",   preset({ thickness = 40, bezel_width = 36, blur_strength = 0.0, blur_iterations = 1, bevel_strength = 0.2,
                                   tint_color = 0x000000B4, dark = { adaptive_dim = 0.0, saturation = 0.6 } }))
hg.preset("glass-card",   preset({ thickness = 72, bezel_width = 60, blur_strength = 3.0, blur_iterations = 3, bevel_size = 2.0 })) -- frosted (owner, 2026-10-24): clear glass let window text fight the panel text
hg.preset("glass-small",  preset({ thickness = 28, bezel_width = 24, blur_strength = 0.0, blur_iterations = 1 }))

hg.config({
  enabled = true,
  default_theme = "dark",
  layers = { enabled = true, live_resample_fps = 30 },
  -- frost 0 = clear glass everywhere (0.5 = auto: frosted over windows; 1 = frosted)
  frost = { windows = 0.0, cards = 0.0, blur = 32, milk = 0 },
  low_power = 0, -- rim reflection and colour split on (Vega iGPU measured fine)
  dark_glass = 0, -- 1 = smoked glass everywhere (Omarchy's "Dark glass"); off: clear panes, kitty darkened by its own adaptive_dim
})

-- windows get their preset by tag, not default_preset (that is validated during a reload, before presets exist)
hl.window_rule({ name = "glass-window", match = { class = ".*" }, tag = "+hyprglass_preset_glass-window" })
-- kitty: black glass instead
-- (one tag per rule; these run after the rule above, so they win)
hl.window_rule({ name = "glass-kitty-1", match = { class = "^kitty$" }, tag = "-hyprglass_preset_glass-window" })
hl.window_rule({ name = "glass-kitty-2", match = { class = "^kitty$" }, tag = "+hyprglass_preset_glass-dark" })
-- the bar: thinner than the panels, because nothing but the wallpaper is ever behind it
hg.layer("fiaboo-bar", { preset = "glass-bar", shape = { inset = { 0, -80, -80, -80 }, radius = 0 }, adaptive = false,
                         fill_key = BG, fill_alpha = BAR })
-- panels (Start, quick settings, calendar, ...) have no glass since 2026-10-28: the engine follows a
-- panel's painted outline a few frames late, so any in-app motion showed the text running ahead of
-- its glass. They paint an opaque fill (PANEL_FILL in main.rs) instead.
-- the panels open and close without Hyprland's layer animation (the calendar drops in-app)
hl.layer_rule({ match = { namespace = "^fiaboo-(start|quick|calendar|clip|notifcenter|taskview|settings|wallpaper|traymenu|shot)$" },
                no_anim = true, animation = "none" })
-- toasts and the OSD: thin glass; the toast stack fades in place (one toast leaving is not the stack closing)
for _, ns in ipairs({ "fiaboo-notify", "fiaboo-osd" }) do
  hg.layer(ns, { preset = "glass-small", mask_mode = "contour", mask_threshold = 0.04, shadow = 0.16, held_close = false,
                 fill_key = BG, fill_alpha = PANEL })
end
-- The ags desktop card (~/.config/ags/app.ts) gets the panels' clear glass; contour masking
-- because its surface is transparent except the card.
hg.layer("ags-dash", { preset = "glass-window", mask_mode = "contour", mask_threshold = 0.04, shadow = 0.2 })
if not GLASS_IN_RELOAD then hg.commit() end
