//! Task View (Hyprland window thumbnails).

use crate::*;

// ---------- Task View ----------
pub(crate) struct TaskViewState(pub(crate) Option<AnyWindowHandle>);
impl Global for TaskViewState {}

pub(crate) struct TvWin {
    address: String,
    title: String,
    class: String,
    ws: i64,
    icon: Option<PathBuf>,
    stable_id: String,
    thumb: PathBuf, // filled in by a background `grim -T` (Hyprland's stableId is the ext-foreign-toplevel identifier)
}

/// Capture every window (hidden workspaces too) into small JPEGs. Each open uses fresh file
/// names so GPUI's image cache never shows an old frame; the previous set is deleted.
pub(crate) fn tv_capture(wins: Vec<(String, PathBuf)>) {
    let dir = PathBuf::from(format!("{}/.cache/fiaboo-shell", home()));
    let keep: Vec<PathBuf> = wins.iter().map(|w| w.1.clone()).collect();
    if let Ok(rd) = fs::read_dir(&dir) {
        for e in rd.flatten() {
            if e.file_name().to_string_lossy().starts_with("tv-") && !keep.contains(&e.path()) {
                let _ = fs::remove_file(e.path());
            }
        }
    }
    let jobs: Vec<_> = wins
        .into_iter()
        .map(|(id, file)| {
            std::thread::spawn(move || {
                let tmp = file.with_extension("tmp");
                let ok = Command::new("grim")
                    .args(["-T", &id, "-s", "0.3", "-t", "jpeg", "-q", "75"])
                    .arg(&tmp)
                    .status()
                    .is_ok_and(|s| s.success());
                if ok {
                    let _ = fs::rename(&tmp, &file); // appears whole, never half-written
                }
            })
        })
        .collect();
    for j in jobs {
        let _ = j.join();
    }
}

pub(crate) fn tv_windows() -> Vec<TvWin> {
    let mut v: Vec<TvWin> = hyprctl_json("clients")
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|c| {
            let class = c["class"].as_str()?.to_string();
            let title = c["title"].as_str().unwrap_or("").to_string();
            let ws = c["workspace"]["id"].as_i64()?;
            if class.is_empty() || ws < 1 || class == "fiaboo-shell" {
                return None;
            }
            let stable_id = c["stableId"].as_str().unwrap_or("0").to_string();
            Some(TvWin {
                address: c["address"].as_str()?.to_string(),
                icon: icon_for(&class),
                thumb: PathBuf::from(format!(
                    "{}/.cache/fiaboo-shell/tv-{}-{}.jpg",
                    home(),
                    stable_id,
                    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
                )),
                title,
                class,
                ws,
                stable_id,
            })
        })
        .collect();
    v.sort_by_key(|w| w.ws);
    v
}

pub(crate) fn toggle_taskview(cx: &mut App) {
    if let Some(handle) = cx.try_global::<TaskViewState>().and_then(|s| s.0) {
        cx.set_global(TaskViewState(None));
        if handle.update(cx, |_, window, _| window.remove_window()).is_ok() {
            return;
        }
    }
    let opened = cx.open_window(
        WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(Bounds { origin: point(px(0.), px(0.)), size: size(px(0.), px(0.)) })),
            app_id: Some("fiaboo-taskview".to_string()),
            window_background: WindowBackgroundAppearance::Transparent,
            kind: WindowKind::LayerShell(LayerShellOptions {
                namespace: "fiaboo-taskview".to_string(),
                layer: Layer::Overlay,
                anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                exclusive_zone: Some(px(-1.)),
                keyboard_interactivity: KeyboardInteractivity::Exclusive,
                ..Default::default()
            }),
            ..Default::default()
        },
        |window, cx| cx.new(|cx| TaskView::new(window, cx)),
    );
    if let Ok(h) = opened {
        cx.set_global(TaskViewState(Some(h.into())));
    }
}

pub(crate) fn close_taskview(window: &mut Window, cx: &mut App) {
    cx.set_global(TaskViewState(None));
    window.remove_window();
}

pub(crate) struct TaskView {
    pal: Palette,
    focus: FocusHandle,
    wins: Vec<TvWin>,
    sel: usize,
    last_input: std::time::Instant,
}

impl TaskView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let wins = tv_windows();
        let jobs: Vec<(String, PathBuf)> = wins.iter().map(|w| (w.stable_id.clone(), w.thumb.clone())).collect();
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = done.clone();
        std::thread::spawn(move || {
            tv_capture(jobs);
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        });
        // repaint while thumbnails land
        cx.spawn_in(window, async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_millis(120)).await;
            let finished = done.load(std::sync::atomic::Ordering::Relaxed);
            if this.update(cx, |_, cx| cx.notify()).is_err() || finished {
                break;
            }
        })
        .detach();
        // exclusive keyboard focus: close after 90 s without a key press, like the other panels
        cx.spawn_in(window, async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let idle = this.update(cx, |t, _| t.last_input.elapsed() > Duration::from_secs(90)).unwrap_or(true);
            if idle {
                let _ = this.update_in(cx, |_, window, cx| close_taskview(window, cx));
                break;
            }
        })
        .detach();
        TaskView { pal: palette(), focus, wins, sel: 0, last_input: std::time::Instant::now() }
    }
}

impl Render for TaskView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.pal.clone();
        let empty = self.wins.is_empty();
        let sel = self.sel;
        let cards = self.wins.iter().enumerate().map(|(i, w)| {
            let (focus_addr, close_addr) = (w.address.clone(), w.address.clone());
            div()
                .id(("tv", i))
                .cursor_pointer()
                .w(px(300.))
                .h(px(236.))
                .p_3()
                .flex()
                .flex_col()
                .gap_2()
                .rounded_xl()
                .bg(rgba((p.dim << 8) | A_SURFACE))
                .border_1()
                .border_color(rgba((p.dim << 8) | A_STRONG))
                .when(i == sel, |d| d.border_2().border_color(rgb(p.accent)))
                .hover(|s| s.bg(rgba((p.accent << 8) | A_HOVER)))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_mouse_down(MouseButton::Right, move |_, window, cx| {
                    cx.stop_propagation();
                    wm::close_window(&close_addr);
                    close_taskview(window, cx);
                })
                .on_click(move |_, window, cx| {
                    focus_window(&focus_addr);
                    close_taskview(window, cx);
                })
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(match &w.icon {
                            Some(path) => img(path.clone()).size(px(28.)).object_fit(ObjectFit::Contain).into_any_element(),
                            None => div().size(px(28.)).into_any_element(),
                        })
                        .child(div().flex_1().text_size(px(12.)).text_color(rgb(p.dim)).child(w.class.clone()))
                        .child(div().text_size(px(12.)).text_color(rgb(p.accent)).child(format!("WS {}", w.ws))),
                )
                .child(
                    div()
                        .flex_1()
                        .rounded_lg()
                        .overflow_hidden()
                        .bg(rgba((p.bg << 8) | A_STRONG))
                        .when(w.thumb.exists(), |d| d.child(img(w.thumb.clone()).size_full().object_fit(ObjectFit::Contain))),
                )
                .child(
                    div()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_size(px(14.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(w.title.clone()),
                )
        });
        div()
            .font_family(NF).font_weight(FontWeight::MEDIUM)
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                this.last_input = std::time::Instant::now();
                let n = this.wins.len();
                match ev.keystroke.key.as_str() {
                    "escape" => close_taskview(window, cx),
                    "right" | "down" | "tab" if n > 0 => {
                        this.sel = (this.sel + 1) % n;
                        cx.notify();
                    }
                    "left" | "up" if n > 0 => {
                        this.sel = (this.sel + n - 1) % n;
                        cx.notify();
                    }
                    "enter" => {
                        if let Some(w) = this.wins.get(this.sel) {
                            focus_window(&w.address);
                        }
                        close_taskview(window, cx);
                    }
                    _ => {}
                }
            }))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .bg(rgba((p.bg << 8) | 0xB0))
            .text_color(rgb(p.fg))
            .on_mouse_down(MouseButton::Left, |_, window, cx| close_taskview(window, cx))
            .on_mouse_down(MouseButton::Right, |_, window, cx| close_taskview(window, cx))
            .child(
                div()
                    .max_w(px(1600.))
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .justify_center()
                    .gap_4()
                    .children(cards)
                    .when(empty, |d| d.child(div().text_color(rgb(p.dim)).child("No open windows"))),
            )
            .child(
                div().pt_4().text_size(px(12.)).text_color(rgb(p.dim)).child("Enter: switch   ·   Right-click: close window   ·   Esc: close"),
            )
    }
}
