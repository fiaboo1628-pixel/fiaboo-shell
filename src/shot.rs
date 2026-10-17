//! Screenshot overlay: drag = region, click = window, F = whole screen.

use crate::*;

// ---------- screenshot: drag = region, click = window under the pointer, F = whole screen ----------
pub(crate) struct ShotState(pub(crate) Option<AnyWindowHandle>);
impl Global for ShotState {}

#[derive(Clone)]
pub(crate) struct WinRect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    title: String,
}

/// (x, y, w, h) of the focused monitor, and the windows visible on it (front-most first).
pub(crate) fn shot_geometry() -> ((i32, i32, i32, i32), Vec<WinRect>) {
    let mons = hyprctl_json("monitors");
    let mon = mons
        .as_array()
        .and_then(|m| m.iter().find(|m| m["focused"].as_bool() == Some(true)).or_else(|| m.first()))
        .cloned()
        .unwrap_or_default();
    let g = |k: &str| mon[k].as_i64().unwrap_or(0) as i32;
    let ws = mon["activeWorkspace"]["id"].as_i64().unwrap_or(1);
    let (mx, my) = (g("x"), g("y"));
    let mut clients: Vec<serde_json::Value> = hyprctl_json("clients").as_array().cloned().unwrap_or_default();
    clients.retain(|c| {
        c["workspace"]["id"].as_i64() == Some(ws)
            && c["mapped"].as_bool() != Some(false)
            && c["hidden"].as_bool() != Some(true)
            && !c["class"].as_str().unwrap_or("").starts_with("fiaboo")
    });
    clients.sort_by_key(|c| c["focusHistoryID"].as_i64().unwrap_or(i64::MAX));
    let wins = clients
        .iter()
        .map(|c| WinRect {
            x: c["at"][0].as_f64().unwrap_or(0.) as f32 - mx as f32,
            y: c["at"][1].as_f64().unwrap_or(0.) as f32 - my as f32,
            w: c["size"][0].as_f64().unwrap_or(0.) as f32,
            h: c["size"][1].as_f64().unwrap_or(0.) as f32,
            title: c["title"].as_str().unwrap_or("").to_string(),
        })
        .collect();
    // width/height are physical pixels; windows and grim -g use logical ones (scaled, rotated)
    let scale = mon["scale"].as_f64().filter(|s| *s > 0.).unwrap_or(1.);
    let (w, h) = ((g("width") as f64 / scale).round() as i32, (g("height") as f64 / scale).round() as i32);
    let (w, h) = if mon["transform"].as_i64().unwrap_or(0) % 2 == 1 { (h, w) } else { (w, h) };
    ((mx, my, w, h), wins)
}

pub(crate) fn toggle_shot(cx: &mut App) {
    if let Some(handle) = cx.try_global::<ShotState>().and_then(|s| s.0) {
        cx.set_global(ShotState(None));
        if handle.update(cx, |_, window, _| window.remove_window()).is_ok() {
            return;
        }
    }
    let opened = cx.open_window(
        WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(Bounds { origin: point(px(0.), px(0.)), size: size(px(0.), px(0.)) })),
            app_id: Some("fiaboo-shot".to_string()),
            window_background: WindowBackgroundAppearance::Transparent,
            kind: WindowKind::LayerShell(LayerShellOptions {
                namespace: "fiaboo-shot".to_string(),
                layer: Layer::Overlay,
                anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                exclusive_zone: Some(px(-1.)),
                keyboard_interactivity: KeyboardInteractivity::Exclusive,
                ..Default::default()
            }),
            ..Default::default()
        },
        |window, cx| cx.new(|cx| ShotOverlay::new(window, cx)),
    );
    if let Ok(h) = opened {
        cx.set_global(ShotState(Some(h.into())));
    }
}

pub(crate) fn close_shot(window: &mut Window, cx: &mut App) {
    cx.set_global(ShotState(None));
    window.remove_window();
}

/// Close the overlay, wait for the compositor to remove it, then grab `rect` (global coords) with grim.
pub(crate) fn take_shot(window: &mut Window, cx: &mut App, rect: Option<(i32, i32, i32, i32)>) {
    close_shot(window, cx);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(350));
        let dir = format!("{}/Pictures/shots", home());
        let _ = fs::create_dir_all(&dir);
        let file = format!("{dir}/shot-{}.png", sh("date +%Y%m%d-%H%M%S"));
        let geo = rect.map(|(x, y, w, h)| format!("-g '{x},{y} {w}x{h}'")).unwrap_or_default();
        let script = format!(
            "grim {geo} '{file}' && wl-copy < '{file}' && \
             case \"$(notify-send -a Screenshot -i '{file}' -A default=Open -A edit=Edit -t 6000 'Screenshot copied' '{file}')\" in \
             default) xdg-open '{file}' ;; \
             edit) satty --filename '{file}' --output-filename '{file}' --copy-command wl-copy --early-exit ;; esac"
        );
        spawn_bg(Command::new("setsid").args(["sh", "-c", &script]));
    });
}

pub(crate) struct ShotOverlay {
    pal: Palette,
    focus: FocusHandle,
    mon: (i32, i32, i32, i32),
    wins: Vec<WinRect>,
    start: Option<gpui::Point<gpui::Pixels>>,
    cur: gpui::Point<gpui::Pixels>,
    last_input: std::time::Instant,
}

impl ShotOverlay {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        cx.spawn_in(window, async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let idle = this.update(cx, |o, _| o.last_input.elapsed() > Duration::from_secs(60)).unwrap_or(true);
            if idle {
                let _ = this.update_in(cx, |_, window, cx| close_shot(window, cx));
                break;
            }
        })
        .detach();
        let (mon, wins) = shot_geometry();
        ShotOverlay { pal: palette(), focus, mon, wins, start: None, cur: point(px(0.), px(0.)), last_input: std::time::Instant::now() }
    }

    fn hovered(&self) -> Option<&WinRect> {
        let (x, y) = (f32::from(self.cur.x), f32::from(self.cur.y));
        self.wins.iter().find(|w| x >= w.x && x < w.x + w.w && y >= w.y && y < w.y + w.h)
    }

    /// Selection rectangle in window coords while dragging.
    fn drag_rect(&self) -> Option<(f32, f32, f32, f32)> {
        let s = self.start?;
        let (sx, sy, cx_, cy) = (f32::from(s.x), f32::from(s.y), f32::from(self.cur.x), f32::from(self.cur.y));
        Some((sx.min(cx_), sy.min(cy), (sx - cx_).abs(), (sy - cy).abs()))
    }
}

impl Render for ShotOverlay {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.pal.clone();
        let dim = rgba(0x00000066);
        // the rectangle that stays bright: the drag, otherwise the hovered window
        let drag = self.drag_rect().filter(|(_, _, w, h)| *w > 4. || *h > 4.);
        let hole = drag.or_else(|| self.hovered().map(|w| (w.x, w.y, w.w, w.h)));
        let (mw, mh) = (self.mon.2 as f32, self.mon.3 as f32);
        let label = match (&drag, self.hovered()) {
            (Some((_, _, w, h)), _) => format!("{} × {}", *w as i32, *h as i32),
            (None, Some(win)) => format!("{}  ({} × {})", win.title, win.w as i32, win.h as i32),
            _ => "Drag to select a region · click a window · F for the whole screen".to_string(),
        };
        let mut root = div().font_family(NF).font_weight(FontWeight::MEDIUM)
            .size_full()
            .relative()
            .cursor_crosshair()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                this.last_input = std::time::Instant::now();
                match ev.keystroke.key.as_str() {
                    "escape" => close_shot(window, cx),
                    "f" | "enter" => take_shot(window, cx, Some(this.mon)),
                    _ => {}
                }
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, ev: &gpui::MouseDownEvent, _, cx| {
                this.start = Some(ev.position);
                this.cur = ev.position;
                cx.notify();
            }))
            .on_mouse_move(cx.listener(|this, ev: &gpui::MouseMoveEvent, _, cx| {
                this.last_input = std::time::Instant::now();
                this.cur = ev.position;
                cx.notify();
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, ev: &gpui::MouseUpEvent, window, cx| {
                this.cur = ev.position;
                let (mx, my, _, _) = this.mon;
                let global = |x: f32, y: f32, w: f32, h: f32| Some((mx + x as i32, my + y as i32, w.max(1.) as i32, h.max(1.) as i32));
                let rect = match this.drag_rect() {
                    Some((x, y, w, h)) if w > 4. || h > 4. => global(x, y, w, h),
                    _ => this.hovered().and_then(|w| global(w.x, w.y, w.w, w.h)),
                };
                this.start = None;
                match rect {
                    Some(r) => take_shot(window, cx, Some(r)),
                    None => cx.notify(),
                }
            }))
            .on_mouse_down(MouseButton::Right, |_, window, cx| close_shot(window, cx));
        match hole {
            None => root = root.child(div().absolute().top_0().left_0().size_full().bg(dim)),
            Some((x, y, w, h)) => {
                let (x, y) = (x.max(0.), y.max(0.));
                let (w, h) = (w.min(mw - x), h.min(mh - y));
                root = root
                    .child(div().absolute().left_0().top_0().w(px(mw)).h(px(y)).bg(dim))
                    .child(div().absolute().left_0().top(px(y + h)).w(px(mw)).h(px((mh - y - h).max(0.))).bg(dim))
                    .child(div().absolute().left_0().top(px(y)).w(px(x)).h(px(h)).bg(dim))
                    .child(div().absolute().left(px(x + w)).top(px(y)).w(px((mw - x - w).max(0.))).h(px(h)).bg(dim))
                    .child(
                        div()
                            .absolute()
                            .left(px(x))
                            .top(px(y))
                            .w(px(w))
                            .h(px(h))
                            .border_2()
                            .border_color(rgb(p.accent)),
                    );
            }
        }
        root.child(
            div()
                .absolute()
                .bottom(px(70.))
                .left_0()
                .w_full()
                .flex()
                .justify_center()
                .child(
                    div()
                        .px_4()
                        .py_2()
                        .rounded_full()
                        .bg(rgba((p.bg << 8) | GLASS))
                        .border_1()
                        .border_color(rgba((p.dim << 8) | 0x50))
                        .text_color(rgb(p.fg))
                        .text_size(px(14.))
                        .child(label),
                ),
        )
    }
}
