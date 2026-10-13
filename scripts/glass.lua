-- fiaboo-shell glass: hyprglass presets for the fiaboo-* layers.
-- Applied by fiaboo-session after `hyprctl plugin load` (hyprctl eval), and again by
-- ~/.config/hypr/custom.lua on every Hyprland reload, which sets GLASS_IN_RELOAD first:
-- during a reload the engine commits by itself once the config is parsed, and an early
-- hg.commit() here would empty its pending presets and layers (the glass then vanished).
local hg = hl.plugin.hyprglass
if not hg then return end

-- the panels paint p.bg (matugen kitty background) at alpha 0xC8: the engine sees
-- through that fill and keeps only FILL of it, so the glass reads underneath
local function matugen_bg()
  local f = io.open(os.getenv("HOME") .. "/.config/kitty/colors-matugen.conf")
  if not f then return 0x111418 end
  local hex
  for line in f:lines() do
    hex = line:match("^background%s+#(%x%x%x%x%x%x)")
    if hex then break end
  end
  f:close()
  return hex and tonumber(hex, 16) or 0x111418
end
local BG, FILL = matugen_bg(), 0.0 -- 0: the panel fill is fully seen through, only the glass remains

-- clear glass (n = 1.52), strong colour split at the rim (Abbe 4), a glint from the
-- upper left, a clear bevel round every shape; the rim reflects the scene around it
local glass = {
  ior = 1.52, abbe = 4,
  specular_angle = 315, light_elevation = 35, specular_strength = 0.5,
  fresnel_strength = 0.4,
  bevel_strength = 0.5, bevel_size = 1.8, bevel_angle = 315, bevel_shadow = 0.3,
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
-- kitty: the window pane with a dark tint on top of the smoke, so the focused window reads solid;
-- unfocused it fades to 45 % with the window (engine patch in GlassDecoration.cpp)
hg.preset("glass-dark",   preset({ thickness = 40, bezel_width = 36, blur_strength = 0.0, blur_iterations = 1, bevel_strength = 0.2,
                                   tint_color = 0x000000A0 }))
hg.preset("glass-card",   preset({ thickness = 72, bezel_width = 60, blur_strength = 0.0, blur_iterations = 1, bevel_size = 2.0 }))
hg.preset("glass-small",  preset({ thickness = 28, bezel_width = 24, blur_strength = 0.0, blur_iterations = 1 }))

hg.config({
  enabled = true,
  default_theme = "dark",
  layers = { enabled = true, live_resample_fps = 30 },
  -- frost 0 = clear glass everywhere (0.5 = auto: frosted over windows; 1 = frosted)
  frost = { windows = 0.0, cards = 0.0, blur = 32, milk = 0 },
  low_power = 0, -- rim reflection and colour split on (Vega iGPU measured fine)
  dark_glass = 1, -- smoked glass everywhere (Omarchy's "Dark glass"): what is behind is dimmed, brights most
})

-- windows get their preset by tag, not default_preset (that is validated during a reload, before presets exist)
hl.window_rule({ name = "glass-window", match = { class = ".*" }, tag = "+hyprglass_preset_glass-window" })
-- kitty: dark glass instead (one tag per rule; these run after the rule above, so they win)
hl.window_rule({ name = "glass-kitty-1", match = { class = "^kitty$" }, tag = "-hyprglass_preset_glass-window" })
hl.window_rule({ name = "glass-kitty-2", match = { class = "^kitty$" }, tag = "+hyprglass_preset_glass-dark" })
-- the bar keeps 70 % of its fill, the same as kitty's background_opacity, so the two match where they meet
hg.layer("fiaboo-bar", { preset = "glass-bar", shape = { inset = { 0, -80, -80, -80 }, radius = 0 }, adaptive = false,
                         fill_key = BG, fill_alpha = 0.7 })
-- full-screen overlay layers that draw one card: the glass follows the card's outline
for _, ns in ipairs({ "fiaboo-start", "fiaboo-quick", "fiaboo-calendar", "fiaboo-clip", "fiaboo-notifcenter",
                      "fiaboo-taskview", "fiaboo-settings", "fiaboo-wallpaper", "fiaboo-traymenu", "fiaboo-shot" }) do
  hg.layer(ns, { preset = "glass-card", mask_mode = "contour", mask_threshold = 0.04, shadow = 0.2,
                 fill_key = BG, fill_alpha = FILL })
end
-- the panels close with the engine's own motion (from the card it held): Hyprland's layer
-- fade on top of it showed two panels closing, so it is off for them
hl.layer_rule({ match = { namespace = "^fiaboo-(start|quick|calendar|clip|notifcenter|taskview|settings|wallpaper|traymenu|shot)$" },
                no_anim = true, animation = "none" })
-- toasts and the OSD: thin glass; the toast stack fades in place (one toast leaving is not the stack closing)
for _, ns in ipairs({ "fiaboo-notify", "fiaboo-osd" }) do
  hg.layer(ns, { preset = "glass-small", mask_mode = "contour", mask_threshold = 0.04, shadow = 0.16, held_close = false,
                 fill_key = BG, fill_alpha = FILL })
end
if not GLASS_IN_RELOAD then hg.commit() end
