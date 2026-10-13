-- fiaboo-shell glass: hyprglass presets for the fiaboo-* layers.
-- Applied by fiaboo-session after `hyprctl plugin load`, and again by
-- ~/.config/hypr/custom.lua on every Hyprland reload (a reload resets the engine).
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
local BG, FILL = matugen_bg(), 0.14

-- clear glass (n = 1.52), mild colour split at the rim, a hairline glint from the
-- upper left, one thin bevel round every shape
local glass = {
  ior = 1.52, abbe = 8,
  specular_angle = 315, light_elevation = 35, specular_strength = 0.3,
  fresnel_strength = 0.2,
  bevel_strength = 0.3, bevel_size = 1.4, bevel_angle = 315, bevel_shadow = 0.2,
  tint_color = 0x16161606,
}
local function preset(over)
  local p = {}
  for k, v in pairs(glass) do p[k] = v end
  for k, v in pairs(over) do p[k] = v end
  return p
end
hg.preset("glass-bar",    preset({ thickness = 24, bezel_width = 24, blur_strength = 0.0, blur_iterations = 1, adaptive_dim = 0.1,
                                   bevel_angle = 0, bevel_strength = 0.2, bevel_shadow = 0.0 }))
hg.preset("glass-window", preset({ thickness = 30, bezel_width = 26, blur_strength = 0.0, blur_iterations = 1, bevel_strength = 0.0 }))
hg.preset("glass-card",   preset({ thickness = 56, bezel_width = 44, blur_strength = 0.0, blur_iterations = 1, bevel_size = 1.6 }))
hg.preset("glass-small",  preset({ thickness = 20, bezel_width = 16, blur_strength = 0.0, blur_iterations = 1 }))

hg.config({
  enabled = true,
  default_theme = "dark",
  default_preset = "glass-window",
  layers = { enabled = true, live_resample_fps = 30 },
  -- frost 0.5 = auto: clear over the wallpaper, frosted over windows (text stays readable)
  frost = { windows = 0.5, cards = 0.5, blur = 32, milk = 0 },
  low_power = 1, -- Vega iGPU: no rim reflection, one background read
})

-- taskbar at the bottom: glass runs past the screen's bottom and sides, rim only on top
hg.layer("fiaboo-bar", { preset = "glass-bar", shape = { inset = { 0, -80, -80, -80 }, radius = 0 }, adaptive = true,
                         fill_key = BG, fill_alpha = FILL })
-- full-screen overlay layers that draw one card: the glass follows the card's outline
for _, ns in ipairs({ "fiaboo-start", "fiaboo-quick", "fiaboo-calendar", "fiaboo-clip", "fiaboo-notifcenter",
                      "fiaboo-taskview", "fiaboo-settings", "fiaboo-wallpaper", "fiaboo-traymenu", "fiaboo-shot" }) do
  hg.layer(ns, { preset = "glass-card", mask_mode = "contour", mask_threshold = 0.04, shadow = 0.2,
                 fill_key = BG, fill_alpha = FILL })
end
-- toasts and the OSD: thin glass; the toast stack fades in place (one toast leaving is not the stack closing)
for _, ns in ipairs({ "fiaboo-notify", "fiaboo-osd" }) do
  hg.layer(ns, { preset = "glass-small", mask_mode = "contour", mask_threshold = 0.04, shadow = 0.16, held_close = false,
                 fill_key = BG, fill_alpha = FILL })
end
hg.commit()
