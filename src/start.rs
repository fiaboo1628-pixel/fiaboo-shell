//! Start menu.

use crate::*;

// ---------- start menu ----------
#[derive(Clone)]
pub(crate) struct Entry {
    name: String,
    stem: String,
    icon: Option<PathBuf>,
    settings: bool,
}

pub(crate) struct StartState(pub(crate) Option<AnyWindowHandle>);
impl Global for StartState {}

/// (button, confirmation question, command)
pub(crate) const POWER: [(&str, &str, &str); 3] = [
    ("Log out", "Log out now?", "sh -c 'if [ -n \"$NIRI_SOCKET\" ]; then niri msg action quit --skip-confirmation; else hyprctl dispatch \"hl.dsp.exit()\"; fi'"),
    ("Restart", "Restart now?", "systemctl reboot"),
    ("Shut down", "Shut down now?", "systemctl poweroff"),
];

pub(crate) fn entry_of(d: &Desktop) -> Entry {
    Entry { name: d.name.clone(), stem: d.stem.clone(), icon: find_icon_file(&d.icon), settings: d.settings }
}

pub(crate) fn toggle_start(cx: &mut App) {
    if let Some(handle) = cx.try_global::<StartState>().and_then(|s| s.0) {
        cx.set_global(StartState(None));
        if handle.update(cx, |_, window, _| window.remove_window()).is_ok() {
            return; // was open, now closed
        }
    }
    let opened = cx.open_window(
        WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: point(px(0.), px(0.)),
                size: size(px(0.), px(0.)),
            })),
            app_id: Some("fiaboo-start".to_string()),
            window_background: WindowBackgroundAppearance::Transparent,
            kind: WindowKind::LayerShell(LayerShellOptions {
                namespace: "fiaboo-start".to_string(),
                layer: Layer::Overlay,
                anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                exclusive_zone: Some(px(-1.)),
                keyboard_interactivity: KeyboardInteractivity::Exclusive,
                ..Default::default()
            }),
            ..Default::default()
        },
        |window, cx| cx.new(|cx| StartMenu::new(window, cx)),
    );
    if let Ok(h) = opened {
        cx.set_global(StartState(Some(h.into())));
    }
}

pub(crate) struct StartMenu {
    pal: Palette,
    query: String,
    focus: FocusHandle,
    all: Vec<Entry>,
    pins: Vec<Entry>,
    sel: usize, // highlighted result (arrow keys); with no query it walks the all-apps list
    scroll: gpui::ScrollHandle,
    last_input: std::time::Instant,
    power: bool,            // power row shown
    confirm: Option<usize>, // POWER action waiting for confirmation
}

impl StartMenu {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let mut all: Vec<Entry> = apps().0.iter().map(entry_of).collect();
        // apps A–Z first, then configuration tools (network editor, input method setup…) A–Z
        all.sort_by_key(|e| (e.settings, e.name.to_lowercase()));
        all.dedup_by(|a, b| a.stem == b.stem);
        let pins = pinned().iter().map(|(_, d)| entry_of(d)).collect();
        // safety net: close after 90 s without a key press (the menu holds exclusive keyboard focus)
        cx.spawn_in(window, async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let idle = this
                .update(cx, |m, _| m.last_input.elapsed() > Duration::from_secs(90))
                .unwrap_or(true);
            if idle {
                let _ = this.update_in(cx, |_, window, cx| close_start(window, cx));
                break;
            }
        })
        .detach();
        StartMenu {
            pal: palette(),
            query: String::new(),
            focus,
            all,
            pins,
            sel: 0,
            scroll: gpui::ScrollHandle::new(),
            last_input: std::time::Instant::now(),
            power: false,
            confirm: None,
        }
    }

    fn results(&self) -> Vec<Entry> {
        let q = self.query.to_lowercase();
        self.all
            .iter()
            .filter(|e| e.name.to_lowercase().contains(&q) || e.stem.contains(&q))
            .cloned()
            .collect()
    }
}

pub(crate) fn close_start(window: &mut Window, cx: &mut App) {
    cx.set_global(StartState(None));
    window.remove_window();
}

pub(crate) fn tile_icon(e: &Entry, size_px: f32, p: &Palette) -> gpui::AnyElement {
    match &e.icon {
        Some(path) => div()
            .size(px(size_px))
            .child(img(path.clone()).size(px(size_px)).object_fit(ObjectFit::Contain))
            .into_any_element(),
        None => div()
            .size(px(size_px))
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .bg(rgba((p.dim << 8) | A_ACTIVE))
            .child(e.name.chars().next().unwrap_or('?').to_uppercase().to_string())
            .into_any_element(),
    }
}

impl Render for StartMenu {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.pal.clone();
        let searching = !self.query.is_empty();

        let search = div()
            .h(px(44.))
            .flex()
            .items_center()
            .px_4()
            .rounded_xl()
            .bg(rgba((p.dim << 8) | A_HOVER))
            .text_size(px(16.))
            .gap_2()
            .child(div().text_color(rgb(p.dim)).child("\u{f002}"))
            .child(if searching {
                div().text_color(rgb(p.fg)).child(format!("{}\u{258f}", self.query))
            } else {
                div().text_color(rgb(p.dim)).child("Search apps…")
            });

        let section = |title: &'static str| {
            div().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).text_color(rgb(p.dim)).child(title)
        };

        let row = |e: &Entry, i: usize, first: bool| {
            let stem = e.stem.clone();
            let pin_stem = e.stem.clone();
            div()
                .id(("row", i))
                .on_mouse_down(MouseButton::Right, cx.listener(move |this, _, _, cx| {
                    toggle_pin(&pin_stem);
                    this.pins = pinned().iter().map(|(_, d)| entry_of(d)).collect();
                    cx.notify();
                }))
                .cursor_pointer()
                .flex()
                .items_center()
                .gap_3()
                .px_3()
                .py_1p5()
                .rounded_lg()
                .when(first, |d| d.bg(rgba((p.accent << 8) | A_HOVER)))
                .hover(|s| s.bg(rgba((p.dim << 8) | A_HOVER)))
                .on_click(move |_, window, cx| {
                    launch_app(&stem);
                    close_start(window, cx);
                })
                .child(tile_icon(e, 26., &p))
                .child(div().child(e.name.clone()))
        };

        let body = if searching {
            let list: Vec<_> =
                self.results().iter().take(40).enumerate().map(|(i, e)| row(e, i, i == self.sel)).collect();
            div()
                .id("results")
                .flex_1()
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .flex()
                .flex_col()
                .children(list)
                .into_any_element()
        } else {
            let tiles = self.pins.iter().enumerate().map(|(i, e)| {
                let stem = e.stem.clone();
                let pin_stem = e.stem.clone();
                div()
                    .id(("pin", i))
                    .on_mouse_down(MouseButton::Right, cx.listener(move |this, _, _, cx| {
                        toggle_pin(&pin_stem);
                        this.pins = pinned().iter().map(|(_, d)| entry_of(d)).collect();
                        cx.notify();
                    }))
                    .cursor_pointer()
                    .w(px(92.))
                    .py_2()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_1()
                    .rounded_lg()
                    .hover(|s| s.bg(rgba((p.dim << 8) | A_HOVER)))
                    .on_click(move |_, window, cx| {
                        launch_app(&stem);
                        close_start(window, cx);
                    })
                    .child(tile_icon(e, 38., &p))
                    .child(
                        div()
                            .max_w(px(86.))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(px(12.))
                            .child(e.name.clone()),
                    )
            });
            let all: Vec<_> = self.all.iter().enumerate().map(|(i, e)| row(e, 1000 + i, i == self.sel)).collect();
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap_2()
                .overflow_hidden()
                .child(section("Pinned"))
                .child(div().flex().flex_row().flex_wrap().children(tiles))
                .child(section("All apps").pt_2())
                .child(
                    div().id("all").flex_1().overflow_y_scroll().track_scroll(&self.scroll).flex().flex_col().children(all),
                )
                .into_any_element()
        };

        let footer_btn = |id: &'static str, label: &'static str, cmd: &'static str| {
            div()
                .id(id)
                .cursor_pointer()
                .px_3()
                .py_2p5()
                .rounded_lg()
                .text_size(px(14.))
                .bg(rgba((p.dim << 8) | A_HOVER))
                .hover(|s| s.bg(rgba((p.accent << 8) | A_ACTIVE)))
                .on_click(move |_, window, cx| {
                    spawn_bg(Command::new("sh").args(["-c", &format!("setsid {cmd} >/dev/null 2>&1 &")]));
                    close_start(window, cx);
                })
                .child(label)
        };

        let small_btn = |id: &'static str, label: &'static str| {
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
        let power_row = match self.confirm {
            Some(i) => {
                let (label, question, cmd) = POWER[i];
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).child(question))
                    .child(footer_btn("p-ok", label, cmd).bg(rgba((p.err << 8) | A_STRONG)))
                    .child(small_btn("p-cancel", "Cancel").on_click(cx.listener(|this, _, _, cx| {
                        this.confirm = None;
                        cx.notify();
                    })))
            }
            None => div().flex().justify_end().gap_3().children(POWER.iter().enumerate().map(|(i, &(label, _, _))| {
                small_btn(label, label).on_click(cx.listener(move |this, _, _, cx| {
                    this.confirm = Some(i);
                    cx.notify();
                }))
            })),
        };

        let footer = div()
            .flex()
            .items_center()
            .gap_3()
            .pt_2()
            .child(
                img(PathBuf::from(format!("{}/Pictures/fiaboo/cauvang.jpg", home())))
                    .size(px(32.))
                    .rounded_full()
                    .object_fit(ObjectFit::Cover),
            )
            .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).child("fiaboo"))
            .child(small_btn("f-settings", "Settings").on_click(|_, window, cx| {
                close_start(window, cx);
                toggle_settings(cx);
            }))
            .child(footer_btn("f-lock", "Lock", "hyprlock"))
            .child(small_btn("f-power", "\u{23fb}").font_family(NF).font_weight(FontWeight::MEDIUM).on_click(cx.listener(|this, _, _, cx| {
                this.power = !this.power;
                this.confirm = None;
                cx.notify();
            })));

        fade_in("start", div()
            .font_family(NF).font_weight(FontWeight::MEDIUM)
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                this.last_input = std::time::Instant::now();
                let k = &ev.keystroke;
                match k.key.as_str() {
                    "escape" => close_start(window, cx),
                    "backspace" => {
                        this.query.pop();
                        this.sel = 0;
                        cx.notify();
                    }
                    "down" | "up" => {
                        // a search shows its first 40 hits; with no query the arrows walk the whole All apps list
                        let n = if this.query.is_empty() { this.all.len() } else { this.results().len().min(40) };
                        if n > 0 {
                            this.sel = if k.key == "down" { (this.sel + 1) % n } else { (this.sel + n - 1) % n };
                            this.scroll.scroll_to_item(this.sel);
                            cx.notify();
                        }
                    }
                    "enter" => {
                        if let Some(e) = this.results().get(this.sel) {
                            launch_app(&e.stem);
                            close_start(window, cx);
                        }
                    }
                    _ => {
                        if !k.modifiers.control && !k.modifiers.alt && !k.modifiers.platform {
                            if let Some(ch) = &k.key_char {
                                this.query.push_str(ch);
                                this.sel = 0;
                                cx.notify();
                            }
                        }
                    }
                }
            }))
            .flex()
            .flex_col()
            .justify_end()
            .items_center() // Win11: opens centered above the Start button
            .pb(px(56.))
            .on_mouse_down(MouseButton::Left, |_, window, cx| close_start(window, cx)).on_mouse_down(MouseButton::Right, |_, window, cx| close_start(window, cx))
            .child(
                div()
                    .w(px(640.0))
                    .h(px(620.))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                    .flex()
                    .flex_col()
            .gap_3()
            .p_4()
            .rounded_2xl()
            .bg(rgba((p.bg << 8) | GLASS))
            .border_1()
            .border_color(rgba((p.dim << 8) | A_STRONG))
            .text_color(rgb(p.fg))
            .text_size(px(14.))
            .child(search)
            .child(body)
            .when(self.power, |d| d.child(power_row))
            .child(footer)
            ))
    }
}
