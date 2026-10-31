//! Task View (Hyprland window thumbnails).

use crate::*;

// ---------- Task View ----------
pub(crate) struct TaskViewState(pub(crate) Option<AnyWindowHandle>);
impl Global for TaskViewState {}

pub(crate) struct TvWin {
    address: String,
    title: String,
    ws: i64,
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
                thumb: PathBuf::from(format!(
                    "{}/.cache/fiaboo-shell/tv-{}-{}.jpg",
                    home(),
                    stable_id,
                    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
                )),
                title,
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
    pending_close: Option<(usize, std::time::Instant)>, // card armed by one Del / right-click, and when
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
        TaskView { pal: palette(), focus, wins, sel: 0, last_input: std::time::Instant::now(), pending_close: None }
    }
}

/// Second Del / right-click on the same card within this window closes it; one press only arms it.
const CLOSE_CONFIRM: Duration = Duration::from_secs(3);

impl TaskView {
    fn request_close(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.last_input = std::time::Instant::now();
        match self.pending_close {
            Some((j, t)) if j == i && t.elapsed() < CLOSE_CONFIRM => {
                if let Some(w) = self.wins.get(i) {
                    wm::close_window(&w.address);
                }
                close_taskview(window, cx);
            }
            _ => {
                let armed = std::time::Instant::now();
                self.pending_close = Some((i, armed));
                self.sel = i;
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(CLOSE_CONFIRM).await;
                    let _ = this.update(cx, |t, cx| {
                        if t.pending_close.is_some_and(|(_, at)| at == armed) {
                            t.pending_close = None; // the card goes back to showing its title
                            cx.notify();
                        }
                    });
                })
                .detach();
                cx.notify();
            }
        }
    }
}

impl Render for TaskView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.pal.clone();
        let empty = self.wins.is_empty();
        let sel = self.sel;
        let pending = self.pending_close.filter(|(_, t)| t.elapsed() < CLOSE_CONFIRM).map(|(i, _)| i);
        let cards = self.wins.iter().enumerate().map(|(i, w)| {
            let focus_addr = w.address.clone();
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
                .bg(card_fill(if i % 2 == 0 { p.accent } else { p.accent2 }, i == sel))
                .shadow(card_shadow(&p, i == sel))
                .border_1()
                .border_color(rgba((p.fg << 8) | A_BORDER))
                .when(i == sel, |d| d.border_2().border_color(rgb(p.accent)))
                // armed to close: the border carries the warning, because the line below says so
                // in fg. err at 14 px measures 3.4:1 over the glass, under AA, and a border is
                // not the only cue -- the text changes too.
                .when(pending == Some(i), |d| d.border_2().border_color(rgb(p.err)))
                .hover(|s| s.bg(card_fill(if i % 2 == 0 { p.accent } else { p.accent2 }, true)).shadow(card_shadow(&p, true)))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                // The ✕ lived in a header row that is gone, so right-click is the mouse path to
                // closing now. Still two presses: request_close only arms on the first.
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.request_close(i, window, cx);
                    }),
                )
                .on_click(move |_, window, cx| {
                    focus_window(&focus_addr);
                    close_taskview(window, cx);
                })
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
                        // the confirmation used to sit in the header row; with that row gone this
                        // is the only place left to say it, and saying nothing would make one Del
                        // arm a close with no sign of it on screen
                        .child(if pending == Some(i) { "Again: close · Esc: keep".to_string() } else { w.title.clone() }),
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
                    "escape" if this.pending_close.is_some() => {
                        this.pending_close = None; // keep the window
                        cx.notify();
                    }
                    "escape" => close_taskview(window, cx),
                    "delete" | "backspace" if n > 0 => {
                        let i = this.sel;
                        this.request_close(i, window, cx);
                    }
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
                div().pt_4().text_size(px(12.)).text_color(rgb(p.dim)).child("Enter: switch · Del or right-click twice: close window · Esc: close"),
            )
    }
}
