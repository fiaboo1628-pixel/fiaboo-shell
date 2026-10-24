//! Quick settings: Wi-Fi/Bluetooth/Night light/DND tiles, volume, media.

use crate::*;

// ---------- quick settings ----------
pub(crate) struct QuickState(pub(crate) Option<AnyWindowHandle>);
impl Global for QuickState {}

pub(crate) fn toggle_quick(cx: &mut App) {
    if let Some(handle) = cx.try_global::<QuickState>().and_then(|s| s.0) {
        cx.set_global(QuickState(None));
        if handle.update(cx, |_, window, _| window.remove_window()).is_ok() {
            return;
        }
    }
    let opened = cx.open_window(
        WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: point(px(0.), px(0.)),
                size: size(px(0.), px(0.)),
            })),
            app_id: Some("fiaboo-quick".to_string()),
            window_background: WindowBackgroundAppearance::Transparent,
            kind: WindowKind::LayerShell(LayerShellOptions {
                namespace: "fiaboo-quick".to_string(),
                layer: Layer::Overlay,
                anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                exclusive_zone: Some(px(-1.)),
                keyboard_interactivity: KeyboardInteractivity::Exclusive,
                ..Default::default()
            }),
            ..Default::default()
        },
        |window, cx| cx.new(|cx| Quick::new(window, cx)),
    );
    if let Ok(h) = opened {
        cx.set_global(QuickState(Some(h.into())));
    }
}

pub(crate) fn close_quick(window: &mut Window, cx: &mut App) {
    cx.set_global(QuickState(None));
    window.remove_window();
}

pub(crate) struct Quick {
    pal: Palette,
    sys: Sys,
    focus: FocusHandle,
    last_input: std::time::Instant,
    focus_i: usize,          // keyboard focus among `actions()`
    drag_level: Option<i32>, // last volume set while dragging over the segments
}

/// (tile id, command) in tile order, skipping tiles that cannot act; what Tab/Enter walk.
fn actions(sys: &Sys) -> Vec<(&'static str, String)> {
    let mut v = vec![];
    if sys.wifi_avail {
        v.push(("q-wifi", format!("nmcli radio wifi {}", if sys.wifi_on { "off" } else { "on" })));
    }
    v.push(("q-bt", if sys.bt_on { "bluetoothctl power off".to_string() } else { "rfkill unblock bluetooth; bluetoothctl power on".to_string() }));
    v.push(("q-night", "pkill -x hyprsunset || setsid hyprsunset".to_string()));
    v.push(("q-dnd", "@dnd".to_string()));
    v
}

fn run_action(cmd: &str) {
    if cmd == "@dnd" { notify::toggle_dnd() } else { run_bg(cmd) }
}

impl Quick {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        cx.spawn_in(window, async move |this, cx| loop {
            let sys = cx.background_executor().spawn(async move { read_sys() }).await;
            let idle = this
                .update(cx, |q, cx| {
                    q.sys = sys;
                    cx.notify();
                    q.last_input.elapsed() > Duration::from_secs(60)
                })
                .unwrap_or(true);
            if idle {
                let _ = this.update_in(cx, |_, window, cx| close_quick(window, cx));
                break;
            }
            cx.background_executor().timer(Duration::from_millis(1200)).await;
        })
        .detach();
        Quick { pal: palette(), sys: Sys::default(), focus, last_input: std::time::Instant::now(), focus_i: 0, drag_level: None }
    }
}

impl Render for Quick {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.pal.clone();
        let sys = self.sys.clone();
        let (g_wifi, g_bt, g_vol) = sys_glyphs(&sys);

        // left click runs `cmd`; right click opens `app` (if any) to pick a network/device
        // `on`: None = the device is not there; the tile explains itself and does nothing
        let focused_id = actions(&sys).get(self.focus_i).map(|a| a.0);
        let tile = |id: &'static str, glyph: &'static str, title: &'static str, sub: String, on: Option<bool>, cmd: String, app: Option<&'static str>| {
            let avail = on.is_some();
            let on = on.unwrap_or(false);
            div()
                .id(id)
                .when(avail, |d| d.cursor_pointer())
                .flex_1()
                // keyboard focus: a 1 px accent ring (always a border so the tile keeps its size)
                .border_1()
                .border_color(if focused_id == Some(id) { rgb(p.accent) } else { rgba(0x00000000) })
                .flex()
                .items_center()
                .gap_3()
                .px_3()
                .py_3()
                .rounded_xl()
                .bg(if on { rgba((p.accent << 8) | A_HOVER) } else { rgba((p.dim << 8) | A_SURFACE) })
                .when(avail, |d| {
                    d.hover(|s| s.bg(rgba((p.accent << 8) | A_ACTIVE))).on_click(cx.listener(move |this, _, _, cx| {
                        this.last_input = std::time::Instant::now();
                        run_action(&cmd);
                        cx.notify();
                    }))
                })
                .when_some(app.filter(|_| avail), |d, app| {
                    d.on_mouse_down(MouseButton::Right, move |_, window, cx| {
                        cx.stop_propagation();
                        run_bg(&format!("setsid {app}"));
                        close_quick(window, cx);
                    })
                })
                .child(
                    div()
                        .font_family(NF).font_weight(FontWeight::MEDIUM)
                        .text_size(px(22.))
                        .text_color(if on { rgb(p.accent) } else if avail { rgb(p.fg) } else { rgb(p.dim) })
                        .child(glyph),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .child(div().font_weight(FontWeight::SEMIBOLD).text_size(px(14.)).when(!avail, |d| d.text_color(rgb(p.dim))).child(title))
                        .child(div().text_size(px(12.)).text_color(rgb(p.dim)).child(sub)),
                )
        };

        let act = |id: &str| actions(&sys).into_iter().find(|a| a.0 == id).map(|a| a.1).unwrap_or_default();
        let (wifi_cmd, bt_cmd) = (act("q-wifi"), act("q-bt"));

        let level = (sys.vol * 100.0).round() as i32;
        let segments = (1..=20).map(|i| {
            let filled = !sys.muted && (i * 5) <= level + 2;
            div()
                .id(("vol", i as usize))
                .cursor_pointer()
                .flex_1()
                .h(px(22.))
                .flex()
                .items_center()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.last_input = std::time::Instant::now();
                    this.drag_level = None;
                    run_bg(&format!("wpctl set-volume @DEFAULT_AUDIO_SINK@ {}%", i * 5));
                    cx.notify();
                }))
                // button held while moving across the row: a drag in 5 % steps
                .on_mouse_move(cx.listener(move |this, ev: &gpui::MouseMoveEvent, _, cx| {
                    if ev.pressed_button == Some(MouseButton::Left) && this.drag_level != Some(i * 5) {
                        this.last_input = std::time::Instant::now();
                        this.drag_level = Some(i * 5);
                        run_bg(&format!("wpctl set-volume @DEFAULT_AUDIO_SINK@ {}%", i * 5));
                        cx.notify();
                    }
                }))
                .child(
                    div()
                        .w_full()
                        .h(if filled { px(8.) } else { px(5.) })
                        .rounded_full()
                        .bg(if filled { rgb(p.accent) } else { rgba((p.dim << 8) | A_ACTIVE) }),
                )
        });

        let media_btn = |id: &'static str, glyph: &'static str, cmd: &'static str| {
            div()
                .id(id)
                .cursor_pointer()
                .size(px(40.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .font_family(NF).font_weight(FontWeight::MEDIUM)
                .text_size(px(18.))
                .hover(|s| s.bg(rgba((p.accent << 8) | A_ACTIVE)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.last_input = std::time::Instant::now();
                    run_bg(&format!("playerctl {cmd}"));
                    if let (Some(m), "play-pause") = (this.sys.media.as_mut(), cmd) {
                        m.playing = !m.playing; // show it now, the next poll confirms
                    }
                    cx.notify();
                }))
                .child(glyph)
        };
        let media = sys.media.as_ref().map(|m| {
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .py_2()
                .rounded_xl()
                .bg(rgba((p.dim << 8) | A_SURFACE))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .flex()
                        .flex_col()
                        .child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().font_weight(FontWeight::SEMIBOLD).text_size(px(14.)).child(m.title.clone()))
                        .child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(px(12.)).text_color(rgb(p.dim)).child(m.artist.clone())),
                )
                .child(media_btn("m-prev", "\u{f04ae}", "previous"))
                .child(media_btn("m-play", if m.playing { "\u{f03e4}" } else { "\u{f040a}" }, "play-pause"))
                .child(media_btn("m-next", "\u{f04ad}", "next"))
        });

        div()
            .font_family(NF).font_weight(FontWeight::MEDIUM)
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                this.last_input = std::time::Instant::now();
                let acts = actions(&this.sys);
                let n = acts.len();
                let k = &ev.keystroke;
                match k.key.as_str() {
                    "escape" => close_quick(window, cx),
                    "tab" if k.modifiers.shift => this.focus_i = (this.focus_i + n - 1) % n,
                    "tab" | "right" | "down" => this.focus_i = (this.focus_i + 1) % n,
                    "left" | "up" => this.focus_i = (this.focus_i + n - 1) % n,
                    "enter" | "space" => {
                        if let Some((_, cmd)) = acts.get(this.focus_i) {
                            run_action(cmd);
                        }
                    }
                    _ => {}
                }
                cx.notify();
            }))
            .flex()
            .flex_col()
            .justify_end()
            .items_end()
            .pr(px(10.0))
            .pb(px(56.))
            .on_mouse_down(MouseButton::Left, |_, window, cx| close_quick(window, cx)).on_mouse_down(MouseButton::Right, |_, window, cx| close_quick(window, cx))
            .child(
                div()
                    .w(px(400.)) // same width as the notification center; the hint row needs it
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                    .flex()
                    .flex_col()
            .gap_3()
            .p_4()
            .rounded_2xl()
            .bg(rgba((p.bg << 8) | GLASS))
            .border_1()
            .border_color(rgba((p.fg << 8) | A_BORDER))
            .text_color(rgb(p.fg))
            .text_size(px(14.))
            .child(div().font_weight(FontWeight::SEMIBOLD).text_size(px(16.)).child("Quick settings"))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(tile(
                        "q-wifi",
                        g_wifi,
                        "Wi-Fi",
                        if !sys.wifi_avail {
                            "Unavailable".to_string()
                        } else if !sys.wifi_on {
                            "Off".to_string()
                        } else if sys.ssid.is_empty() {
                            "Not connected".to_string()
                        } else {
                            sys.ssid.clone()
                        },
                        sys.wifi_avail.then_some(sys.wifi_on),
                        wifi_cmd,
                        Some("~/.local/bin/fiaboo tui --float Wi-Fi nmtui connect"),
                    ))
                    .child(tile(
                        "q-bt",
                        g_bt,
                        "Bluetooth",
                        if sys.bt_on { "On" } else { "Off" }.to_string(),
                        Some(sys.bt_on),
                        bt_cmd,
                        Some("~/.local/bin/fiaboo tui --float Bluetooth bluetui"),
                    )),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(tile(
                        "q-night",
                        "\u{f0594}",
                        "Night light",
                        if sys.night { "On" } else { "Off" }.to_string(),
                        Some(sys.night),
                        act("q-night"),
                        None,
                    ))
                    .child(tile(
                        "q-dnd",
                        "\u{f009b}",
                        "Do not disturb",
                        if sys.dnd { "On" } else { "Off" }.to_string(),
                        Some(sys.dnd),
                        act("q-dnd"),
                        None,
                    )),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .rounded_xl()
                    .bg(rgba((p.dim << 8) | A_SURFACE))
                    .child(
                        div()
                            .id("q-mute")
                            .cursor_pointer()
                            .font_family(NF).font_weight(FontWeight::MEDIUM)
                            .text_size(px(22.))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.last_input = std::time::Instant::now();
                                run_bg("wpctl set-mute @DEFAULT_AUDIO_SINK@ toggle");
                                cx.notify();
                            }))
                            .child(g_vol),
                    )
                    .child(div().flex_1().flex().gap_0p5().children(segments))
                    .child(div().w(px(40.)).text_size(px(12.)).text_color(rgb(p.dim)).child(
                        if sys.muted { "Muted".to_string() } else { format!("{level}%") },
                    )),
            )
            .children(media)
            .child(
                div()
                    .pt_1()
                    .text_size(px(12.))
                    .text_color(rgb(p.dim))
                    .child("Tab, Enter: toggle · Right-click: app · Esc: close"),
            ))
    }
}
