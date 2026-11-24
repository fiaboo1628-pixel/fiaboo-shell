-- fiaboo-shell on Hyprland 0.57 (Lua). Installed as ~/.config/hypr/custom.lua and required at the
-- end of hyprland.lua, after dms/binds.lua, so the binds here replace the stock ones.
-- Glass: hyprliquid plugin (AUR `hyprliquid`); rebuild it after every Hyprland upgrade.

-- the rest of the session (wallpaper, polkit, hypridle, cliphist) is in conf/autostart.lua
hl.on("hyprland.start", function()
	hl.exec_cmd("~/.local/bin/fiaboo-session")
end)

-- === Surfaces ===
-- Glass panels are full-screen transparent layers with one rounded shell inside, so the
-- plugin finds the shell from alpha (vdf_map_mode 1). `always` rebuilds the map every frame:
-- panels move in-app, and an async map lagged behind the contour (the reason hyprglass went).
local glass_panels = "^fiaboo-(start|quick|calendar|clip|settings|wallpaper|notifcenter|notify|osd|traymenu|tip)$"
-- shot (frozen screen to select from), taskview and the music line stay on plain blur
local blur_only = "^fiaboo-(shot|taskview|musicline)$"

hl.layer_rule({ match = { namespace = "^fiaboo-.*" }, no_anim = true })
hl.layer_rule({ match = { namespace = blur_only }, blur = true, ignore_alpha = 0.5 })

hl.plugin.load("/usr/lib/libhyprliquid.so")
if hl.plugin.hyprliquid then
	hl.layer_rule({
		name = "glass-bar",
		match = { namespace = "^fiaboo-bar$" },
		blur = true,
		["hyprliquid:effect"] = "liquid_glass",
		["hyprliquid:corner_radius"] = 0,
		["hyprliquid:tint_color"] = "rgba(0x11, 0x11, 0x1b, 0.35)",
		["hyprliquid:highlight_style"] = 3,
	})
	hl.layer_rule({
		name = "glass-panels",
		match = { namespace = glass_panels },
		-- Hyprland blur under the glass (as in the plugin's wlogout example): frosted, so text
		-- never sits on a sharp wallpaper
		blur = true,
		ignore_alpha = 0.5,
		["hyprliquid:effect"] = "liquid_glass",
		["hyprliquid:corner_radius"] = 16,
		["hyprliquid:tint_color"] = "rgba(0x11, 0x11, 0x1b, 0.35)",
		["hyprliquid:highlight_style"] = 3,
		["hyprliquid:vdf_map_mode"] = 1,
		["hyprliquid:vdf_map_update_policy"] = "always",
	})
	hl.window_rule({
		name = "glass-terminals",
		match = { class = "^(com\\.mitchellh\\.ghostty|fiaboo\\..*|kitty)$" },
		no_blur = true,
		["hyprliquid:effect"] = "liquid_glass",
		["hyprliquid:highlight_style"] = 3,
	})
else
	-- plugin missing or not rebuilt for this Hyprland: same look as before glass
	hl.layer_rule({ match = { namespace = "^fiaboo-.*" }, blur = true, ignore_alpha = 0.5 })
end

-- === Binds (replace the DMS ones that called `dms ipc`) ===
local sh = function(cmd) return hl.dsp.exec_cmd(cmd) end
-- a second hl.bind on a key adds a bind (both would fire), so free the stock one first
local function bind(key, action, opts) hl.unbind(key); hl.bind(key, action, opts) end
bind("SUPER + space", sh("fiaboo-shell start"))
bind("SUPER + X", sh("fiaboo-shell start"))
bind("SUPER + SHIFT + E", sh("fiaboo-shell start"))
bind("SUPER + T", sh("ghostty"))
bind("SUPER + Return", sh("ghostty"))
bind("SUPER + V", sh("fiaboo-shell clipboard"))
bind("SUPER + comma", sh("fiaboo-shell settings"))
bind("SUPER + A", sh("fiaboo-shell quick"))
bind("SUPER + CTRL + N", sh("fiaboo-shell notifications"))
bind("SUPER + D", sh("fiaboo-shell calendar"))
bind("SUPER + Y", sh("fiaboo-shell wallpaper"))
bind("SUPER + SHIFT + W", sh("fiaboo-shell wallpaper"))
bind("SUPER + TAB", sh("fiaboo-shell taskview"))
bind("SUPER + O", sh("fiaboo-shell taskview"))
bind("SUPER + SHIFT + C", sh("hyprpicker -a"))
bind("SUPER + ALT + L", sh("loginctl lock-session"))
bind("CTRL + ALT + Delete", sh("ghostty --class=fiaboo.tui-float-btop -e btop"))
bind("CTRL + ALT + M", sh("ghostty --class=fiaboo.tui-float-btop -e btop"))
bind("Print", sh("fiaboo-shell shot"))
bind("CTRL + Print", sh("fiaboo-shell shot"))
bind("ALT + Print", sh("fiaboo-shell shot"))

-- the OSD follows `pactl subscribe`, so plain wpctl/playerctl show it
bind("XF86AudioRaiseVolume", sh("wpctl set-volume -l 1 @DEFAULT_SINK@ 3%+"), { locked = true, repeating = true })
bind("XF86AudioLowerVolume", sh("wpctl set-volume @DEFAULT_SINK@ 3%-"), { locked = true, repeating = true })
bind("XF86AudioMute", sh("wpctl set-mute @DEFAULT_SINK@ toggle"), { locked = true })
bind("XF86AudioMicMute", sh("wpctl set-mute @DEFAULT_SOURCE@ toggle"), { locked = true })
bind("XF86AudioPlay", sh("playerctl play-pause"), { locked = true })
bind("XF86AudioPause", sh("playerctl play-pause"), { locked = true })
bind("XF86AudioPrev", sh("playerctl previous"), { locked = true })
bind("XF86AudioNext", sh("playerctl next"), { locked = true })

-- DMS-only features with no fiaboo counterpart (night light and keep awake live in quick settings)
for _, key in ipairs({
	"ALT + space", "SUPER + M", "SUPER + N", "SUPER + SHIFT + N", "SUPER + ALT + N", "SUPER + ALT + I",
	"SUPER + SHIFT + Slash", "SUPER + ALT + R", "SUPER + P", "CTRL + XF86AudioRaiseVolume",
	"CTRL + XF86AudioLowerVolume", "XF86MonBrightnessUp", "XF86MonBrightnessDown",
}) do
	hl.unbind(key)
end

hl.window_rule({ match = { class = "^(fiaboo\\.tui-float-.*)$" }, float = true, size = "1100 700", center = true })
