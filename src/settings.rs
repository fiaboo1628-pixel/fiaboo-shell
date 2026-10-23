//! Settings panel (network, sound, Bluetooth, displays, about).

use crate::*;

// ---------- settings ----------
pub(crate) struct SettingsState(pub(crate) Option<AnyWindowHandle>);
impl Global for SettingsState {}

#[derive(Clone, Default)]
pub(crate) struct SettingsInfo {
    net: Vec<(String, String)>,          // (label, "state · connection")
    sinks: Vec<(String, String, bool)>,  // (name, description, default)
    displays: Vec<String>,
    about: Vec<(&'static str, String)>,
}

/// `bluetoothctl devices …` lines: "Device <MAC> <name>" -> (mac, name)
pub(crate) fn bt_device_lines(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let mut it = l.strip_prefix("Device ")?.splitn(2, ' ');
            let mac = it.next()?.to_string();
            // the MAC goes into shell commands, so accept only AA:BB:CC:DD:EE:FF
            let valid = mac.len() == 17 && mac.char_indices().all(|(i, c)| if i % 3 == 2 { c == ':' } else { c.is_ascii_hexdigit() });
            valid.then(|| (mac.clone(), it.next().unwrap_or(&mac).to_string()))
        })
        .collect()
}

/// Split one `nmcli -t` line: fields are ':'-separated, literal ':' is escaped as "\:".
pub(crate) fn nmcli_fields(line: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.last_mut().unwrap().extend(chars.next()),
            ':' => out.push(String::new()),
            _ => out.last_mut().unwrap().push(c),
        }
    }
    out
}

/// One `nmcli -t -f DEVICE,TYPE,STATE,CONNECTION device` line in plain words: ("Wi-Fi", "Connected · Home").
pub(crate) fn net_row(f: &[String]) -> (String, String) {
    let (dev, kind, state, conn) = (f[0].as_str(), f[1].as_str(), f[2].as_str(), f[3].as_str());
    let label = match kind {
        "wifi" => "Wi-Fi",
        "ethernet" => "Ethernet",
        _ => "VPN",
    };
    let state = match state {
        s if s.starts_with("connected") => "Connected".to_string(), // "(externally)": managed by Tailscale etc.
        "unavailable" if kind == "ethernet" => "Cable unplugged".to_string(),
        "unavailable" => "Unavailable".to_string(),
        s => s[..1].to_uppercase() + &s[1..],
    };
    let conn = if conn == dev || conn.is_empty() { String::new() } else { format!(" · {conn}") };
    (label.to_string(), state + &conn)
}

pub(crate) fn read_settings() -> SettingsInfo {
    let net = sh("nmcli -t -f DEVICE,TYPE,STATE,CONNECTION device")
        .lines()
        .map(nmcli_fields)
        // what a person connects through; container bridges/veths and unmanaged devices are noise here
        .filter(|f| f.len() == 4 && matches!(f[1].as_str(), "wifi" | "ethernet" | "wireguard" | "tun" | "vpn") && f[2] != "unmanaged")
        .filter(|f| !(f[0].starts_with("docker") || f[0].starts_with("br-") || f[0].starts_with("veth")))
        .map(|f| net_row(&f))
        .collect();
    let default_sink = sh("pactl get-default-sink");
    let sinks = serde_json::from_str::<serde_json::Value>(&sh("pactl -f json list sinks"))
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|s| {
            let name = s["name"].as_str()?.to_string();
            let desc = s["description"].as_str().unwrap_or(&name).to_string();
            let is_default = name == default_sink;
            Some((name, desc, is_default))
        })
        .collect();
    let displays = hyprctl_json("monitors")
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|m| {
            format!(
                "{} · {:.0}×{:.0} @ {:.0} Hz · scale {}",
                m["name"].as_str().unwrap_or("?"),
                m["width"].as_f64().unwrap_or(0.0),
                m["height"].as_f64().unwrap_or(0.0),
                m["refreshRate"].as_f64().unwrap_or(0.0),
                m["scale"]
            )
        })
        .collect();
    let about = vec![
        ("Host", sh("cat /etc/hostname")),
        ("Kernel", sh("uname -r")),
        ("Uptime", sh("uptime -p").trim_start_matches("up ").to_string()),
        ("Memory", sh("free -h | awk '/^Mem/ {print $3 \" / \" $2}'")),
        ("Disk /", sh("df -h / | awk 'NR==2 {print $3 \" / \" $2}'")),
    ];
    SettingsInfo { net, sinks, displays, about }
}

// ---------- bluetooth list (Settings and the quick settings › page) ----------
#[derive(Clone, Default)]
pub(crate) struct BtInfo {
    pub(crate) on: bool,
    pub(crate) paired: Vec<(String, String, bool)>, // (mac, name, connected)
    pub(crate) found: Vec<(String, String)>,        // (mac, name) known to bluez but not paired, named only
}

pub(crate) fn read_bt() -> BtInfo {
    let connected = bt_device_lines(&sh("timeout 2 bluetoothctl devices Connected"));
    let paired: Vec<_> = bt_device_lines(&sh("timeout 2 bluetoothctl devices Paired"))
        .into_iter()
        .map(|(mac, name)| {
            let on = connected.iter().any(|c| c.0 == mac);
            (mac, name, on)
        })
        .collect();
    let found = bt_device_lines(&sh("timeout 2 bluetoothctl devices"))
        .into_iter()
        .filter(|(mac, name)| name.replace('-', ":") != *mac && !paired.iter().any(|p| p.0 == *mac))
        .collect();
    BtInfo { on: sh("timeout 2 bluetoothctl show | grep -c 'Powered: yes'") == "1", paired, found }
}

/// Current on/off of a Bluetooth row: "power" or a device MAC (on = connected).
pub(crate) fn bt_state(info: &BtInfo, key: &str) -> Option<bool> {
    if key == "power" {
        return Some(info.on);
    }
    info.paired.iter().find(|d| d.0 == key).map(|d| d.2)
}

pub(crate) const BT_SCAN_SECS: u64 = 20;

#[derive(Default)]
pub(crate) struct BtUi {
    info: BtInfo,
    /// Row clicked and not settled yet: (row key, target state, since)
    busy: Option<(String, bool, std::time::Instant)>,
    /// End of the current scan; Some once a scan was started in this panel (then found devices show).
    scan_until: Option<std::time::Instant>,
}

impl BtUi {
    fn refresh(&mut self, info: BtInfo) {
        self.info = info;
        if let Some((key, want, since)) = &self.busy {
            // pair + trust + connect can take a while
            if bt_state(&self.info, key) == Some(*want) || since.elapsed() > Duration::from_secs(30) {
                self.busy = None;
            }
        }
    }
}

/// A panel that shows `bt_rows`.
pub(crate) trait HasBt: 'static {
    fn bt(&mut self) -> &mut BtUi;
    fn touch(&mut self);
}

/// Power, paired devices (click: connect/disconnect), scan, found devices (click: pair + connect).
pub(crate) fn bt_rows<T: HasBt>(ui: &BtUi, p: &Palette, cx: &mut Context<T>) -> Vec<gpui::AnyElement> {
    let (accent, dim) = (p.accent, p.dim);
    let busy = ui.busy.as_ref().map(|b| b.0.clone());
    let row = |id: gpui::ElementId, key: String, label: String, glyph: &'static str, on: bool, want: bool, cmd: String| {
        let label = if busy.as_deref() == Some(key.as_str()) { format!("{label} · working…") } else { label };
        div()
            .id(id)
            .cursor_pointer()
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .py_2p5()
            .rounded_lg()
            // selected stays the strongest fill; hover on an unselected tile must not outshine it
            .bg(if on { rgba((accent << 8) | A_ACTIVE) } else { rgba((dim << 8) | A_SURFACE) })
            .hover(move |s| s.bg(rgba((accent << 8) | if on { A_ACTIVE } else { A_HOVER })))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.touch();
                this.bt().busy = Some((key.clone(), want, std::time::Instant::now()));
                run_bg(&cmd); // the next refresh shows the new state and clears busy
                cx.notify();
            }))
            .child(div().font_family(NF).font_weight(FontWeight::MEDIUM).text_color(if on { rgb(accent) } else { rgb(dim) }).child(glyph))
            .child(div().flex_1().min_w(px(0.)).overflow_hidden().whitespace_nowrap().text_ellipsis().child(label))
            .into_any_element()
    };
    let info = &ui.info;
    let power_cmd = if info.on { "bluetoothctl power off" } else { "rfkill unblock bluetooth; bluetoothctl power on" };
    let glyph = |on: bool| if on { "\u{f00af}" } else { "\u{f00b2}" };
    let mut rows = vec![row("bt-power".into(), "power".into(), (if info.on { "On" } else { "Off" }).into(), glyph(info.on), info.on, !info.on, power_cmd.into())];
    if !info.on {
        return rows;
    }
    rows.extend(info.paired.iter().enumerate().map(|(i, (mac, name, on))| {
        let cmd = format!("bluetoothctl {} {mac}", if *on { "disconnect" } else { "connect" });
        row(("bt-dev", i).into(), mac.clone(), format!("{name} · {}", if *on { "connected" } else { "paired" }), glyph(*on), *on, !*on, cmd)
    }));
    let scanning = ui.scan_until.is_some_and(|t| t > std::time::Instant::now());
    rows.push(
        div()
            .id("bt-scan")
            .cursor_pointer()
            .px_3()
            .py_2p5()
            .rounded_lg()
            .text_color(rgb(if scanning { dim } else { p.fg }))
            .bg(rgba((dim << 8) | A_HOVER))
            .hover(|s| s.bg(rgba((accent << 8) | A_ACTIVE)))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.touch();
                if !scanning {
                    this.bt().scan_until = Some(std::time::Instant::now() + Duration::from_secs(BT_SCAN_SECS));
                    run_bg(&format!("bluetoothctl --timeout {BT_SCAN_SECS} scan on"));
                }
                cx.notify();
            }))
            .child(if scanning { "Scanning for new devices…" } else { "Scan for new devices" })
            .into_any_element(),
    );
    if ui.scan_until.is_some() {
        rows.extend(info.found.iter().enumerate().map(|(i, (mac, name))| {
            // shortcut: devices that ask for a PIN or passkey are not handled, pair those with `bluetoothctl` in a terminal
            // pairable only while pairing, so nearby devices can't pair on their own afterwards
            let cmd = format!("bluetoothctl pairable on; bluetoothctl --timeout 25 pair {mac} && bluetoothctl trust {mac}; bluetoothctl pairable off; bluetoothctl connect {mac}");
            row(("bt-new", i).into(), mac.clone(), format!("{name} · pair"), "\u{f00b1}", false, true, cmd)
        }));
    }
    rows
}

pub(crate) fn toggle_settings(cx: &mut App) {
    if let Some(handle) = cx.try_global::<SettingsState>().and_then(|s| s.0) {
        cx.set_global(SettingsState(None));
        if handle.update(cx, |_, window, _| window.remove_window()).is_ok() {
            return;
        }
    }
    let opened = cx.open_window(
        WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(Bounds { origin: point(px(0.), px(0.)), size: size(px(0.), px(0.)) })),
            app_id: Some("fiaboo-settings".to_string()),
            window_background: WindowBackgroundAppearance::Transparent,
            kind: WindowKind::LayerShell(LayerShellOptions {
                namespace: "fiaboo-settings".to_string(),
                layer: Layer::Overlay,
                anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                exclusive_zone: Some(px(-1.)),
                keyboard_interactivity: KeyboardInteractivity::Exclusive,
                ..Default::default()
            }),
            ..Default::default()
        },
        |window, cx| cx.new(|cx| SettingsPanel::new(window, cx)),
    );
    if let Ok(h) = opened {
        cx.set_global(SettingsState(Some(h.into())));
    }
}

pub(crate) fn close_settings(window: &mut Window, cx: &mut App) {
    cx.set_global(SettingsState(None));
    window.remove_window();
}

pub(crate) struct SettingsPanel {
    pal: Palette,
    focus: FocusHandle,
    info: SettingsInfo,
    last_input: std::time::Instant,
    bt: BtUi,
}

impl HasBt for SettingsPanel {
    fn bt(&mut self) -> &mut BtUi {
        &mut self.bt
    }
    fn touch(&mut self) {
        self.last_input = std::time::Instant::now();
    }
}

impl SettingsPanel {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        // refresh every few seconds; also the idle safety net (the panel holds exclusive keyboard focus)
        cx.spawn_in(window, async move |this, cx| loop {
            let (info, bt) = cx.background_executor().spawn(async { (read_settings(), read_bt()) }).await;
            let idle = this
                .update(cx, |s, cx| {
                    s.info = info;
                    s.bt.refresh(bt);
                    cx.notify();
                    s.last_input.elapsed() > Duration::from_secs(120)
                })
                .unwrap_or(true);
            if idle {
                let _ = this.update_in(cx, |_, window, cx| close_settings(window, cx));
                break;
            }
            cx.background_executor().timer(Duration::from_secs(3)).await;
        })
        .detach();
        SettingsPanel { pal: palette(), focus, info: SettingsInfo::default(), last_input: std::time::Instant::now(), bt: BtUi::default() }
    }
}

impl Render for SettingsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.pal.clone();
        let info = self.info.clone();
        let section = |title: &'static str| {
            div().pt_4().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).text_color(rgb(p.dim)).child(title)
        };
        let row = |label: String, value: String| {
            div()
                .flex()
                .gap_3()
                .px_3()
                .py_2p5()
                .rounded_lg()
                .bg(rgba((p.dim << 8) | A_SURFACE))
                .child(div().w(px(170.)).flex_none().overflow_hidden().whitespace_nowrap().text_ellipsis().child(label))
                .child(div().flex_1().min_w(px(0.)).overflow_hidden().whitespace_nowrap().text_ellipsis().text_color(rgb(p.dim)).child(value))
        };
        let sinks = info.sinks.iter().enumerate().map(|(i, (name, desc, is_default))| {
            let name = name.clone();
            div()
                .id(("sink", i))
                .cursor_pointer()
                .flex()
                .items_center()
                .gap_3()
                .px_3()
                .py_2p5()
                .rounded_lg()
                .bg(if *is_default { rgba((p.accent << 8) | A_ACTIVE) } else { rgba((p.dim << 8) | A_SURFACE) })
                .hover(move |s| s.bg(rgba((p.accent << 8) | if *is_default { A_ACTIVE } else { A_HOVER })))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.last_input = std::time::Instant::now();
                    run_bg(&format!("pactl set-default-sink '{name}'"));
                    for s in this.info.sinks.iter_mut() {
                        s.2 = s.0 == name; // show it now, the next refresh confirms
                    }
                    cx.notify();
                }))
                .child(div().font_family(NF).font_weight(FontWeight::MEDIUM).text_color(if *is_default { rgb(p.accent) } else { rgb(p.dim) }).child(if *is_default { "\u{f0133}" } else { "\u{f0130}" }))
                .child(div().flex_1().min_w(px(0.)).overflow_hidden().whitespace_nowrap().text_ellipsis().child(desc.clone()))
        });
        let btn = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .cursor_pointer()
                .px_3()
                .py_2p5()
                .rounded_lg()
                .text_size(px(14.))
                .bg(rgba((p.dim << 8) | A_HOVER))
                .hover(|s| s.bg(rgba((p.accent << 8) | A_ACTIVE)))
                .child(label)
        };
        div()
            .font_family(NF).font_weight(FontWeight::MEDIUM)
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                this.last_input = std::time::Instant::now();
                if ev.keystroke.key == "escape" {
                    close_settings(window, cx);
                }
            }))
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(MouseButton::Left, |_, window, cx| close_settings(window, cx))
            .on_mouse_down(MouseButton::Right, |_, window, cx| close_settings(window, cx))
            .child(
                div()
                    .w(px(640.))
                    .h(px(700.))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_4()
                    .rounded_2xl()
                    .bg(rgba((p.bg << 8) | GLASS))
                    .border_1()
                    .border_color(rgba((p.fg << 8) | A_BORDER))
                    .text_color(rgb(p.fg))
                    .text_size(px(14.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).text_size(px(16.)).child("Settings"))
                            .child(btn("s-wp", "Change wallpaper").on_click(|_, window, cx| {
                                close_settings(window, cx);
                                toggle_wp(cx);
                            })),
                    )
                    .child(
                        div()
                            .id("settings-body")
                            .flex_1()
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(section("Network"))
                            .children(info.net.iter().map(|(a, b)| row(a.clone(), b.clone())))
                            .child(section("Sound output"))
                            .children(sinks)
                            .child(section("Bluetooth"))
                            .children(bt_rows(&self.bt, &p, cx))
                            .child(section("Displays"))
                            .children(info.displays.iter().map(|d| row(d.split(" · ").next().unwrap_or("").to_string(), d.split_once(" · ").map(|x| x.1).unwrap_or("").to_string())))
                            .child(section("About"))
                            .children(info.about.iter().map(|(a, b)| row(a.to_string(), b.clone()))),
                    )

            )
    }
}
