-- Autostart (fiaboo). Runs once at Hyprland start; fiaboo-session starts the shell (custom.lua).
hl.on("hyprland.start", function ()
    -- Export variables to systemd, then restart portals so they catch them. One command: separate
    -- exec_cmd calls run concurrently, so start could win over stop and leave the portals down.
    hl.exec_cmd("dbus-update-activation-environment --systemd WAYLAND_DISPLAY XDG_CURRENT_DESKTOP; "
        .. "systemctl --user stop xdg-desktop-portal xdg-desktop-portal-hyprland; "
        .. "systemctl --user start xdg-desktop-portal-hyprland xdg-desktop-portal")

    -- Wallpaper daemon + last wallpaper
    hl.exec_cmd("awww-daemon")
    hl.exec_cmd("fiaboo-wallpaper --restore")

    -- Load cursor
    hl.exec_cmd("hyprctl setcursor Bibata-Modern-Ice 24")

    -- Network tray icon
    hl.exec_cmd("nm-applet --indicator")

    -- Start polkit daemon
    hl.exec_cmd("/usr/lib/polkit-gnome/polkit-gnome-authentication-agent-1")

    -- Load GTK settings
    hl.exec_cmd("~/.config/hypr/scripts/gtk.sh")

    -- Start hypridle
    hl.exec_cmd("hypridle")

    -- Load cliphist history
    hl.exec_cmd("wl-paste --watch cliphist store")

    -- Start autostart cleanup
    hl.exec_cmd("~/.config/hypr/scripts/cleanup.sh")
end)
