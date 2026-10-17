//! Wallpaper picker.

use crate::*;

// ---------- wallpaper picker ----------
pub(crate) struct WpState(pub(crate) Option<AnyWindowHandle>);
impl Global for WpState {}

pub(crate) const WP_COLS: usize = 4;

pub(crate) fn wp_dir() -> String {
    format!("{}/Pictures/wallpapers", home())
}

pub(crate) fn wp_list() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(wp_dir())
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    v.retain(|p| {
        p.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| matches!(e.to_lowercase().as_str(), "jpg" | "jpeg" | "png" | "webp"))
    });
    v.sort();
    v
}

/// Small cached copy of a wallpaper for the grid (full-size images are far too heavy to draw 100 of).
pub(crate) fn wp_thumb(src: &std::path::Path) -> PathBuf {
    let name = src.file_name().and_then(|s| s.to_str()).unwrap_or("wp"); // full name: x.jpg and x.png differ
    PathBuf::from(format!("{}/.cache/fiaboo-shell/wp/{name}.jpg", home()))
}

/// The thumbnail generator is running (the picker re-renders only while it is).
pub(crate) static WP_THUMBS_RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(crate) fn wp_make_thumbs(list: Vec<PathBuf>) {
    // reopening the picker while an earlier run is still going must not start a second writer
    if WP_THUMBS_RUNNING.swap(true, std::sync::atomic::Ordering::AcqRel) {
        return;
    }
    let dir = format!("{}/.cache/fiaboo-shell/wp", home());
    let _ = fs::create_dir_all(&dir);
    // thumbnails of removed wallpapers (and the old stem-only names)
    let wanted: Vec<PathBuf> = list.iter().map(|s| wp_thumb(s)).collect();
    for e in fs::read_dir(&dir).into_iter().flatten().flatten() {
        if !wanted.contains(&e.path()) {
            let _ = fs::remove_file(e.path());
        }
    }
    for src in list {
        let t = wp_thumb(&src);
        if !t.exists() {
            // the picker shows a thumbnail once it exists: only a finished file may take its name
            let tmp = t.with_extension("tmp.jpg");
            let ok = Command::new("magick")
                .arg(format!("{}[0]", src.display()))
                .args(["-thumbnail", "400x225^", "-gravity", "center", "-extent", "400x225", "-quality", "82"])
                .arg(&tmp)
                .status()
                .is_ok_and(|s| s.success());
            if ok {
                let _ = fs::rename(&tmp, &t);
            } else {
                let _ = fs::remove_file(&tmp);
            }
        }
    }
    WP_THUMBS_RUNNING.store(false, std::sync::atomic::Ordering::Release);
}

pub(crate) fn wp_current() -> String {
    fs::read_to_string(format!("{}/.cache/fiaboo-shell/wallpaper/current", home())).unwrap_or_default().trim().to_string()
}

pub(crate) fn wp_apply(path: &std::path::Path) {
    spawn_bg(Command::new("setsid").arg(format!("{}/.local/bin/fiaboo-wallpaper", home())).arg(path));
}

pub(crate) fn toggle_wp(cx: &mut App) {
    if let Some(handle) = cx.try_global::<WpState>().and_then(|s| s.0) {
        cx.set_global(WpState(None));
        if handle.update(cx, |_, window, _| window.remove_window()).is_ok() {
            return;
        }
    }
    let opened = cx.open_window(
        WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(Bounds { origin: point(px(0.), px(0.)), size: size(px(0.), px(0.)) })),
            app_id: Some("fiaboo-wallpaper".to_string()),
            window_background: WindowBackgroundAppearance::Transparent,
            kind: WindowKind::LayerShell(LayerShellOptions {
                namespace: "fiaboo-wallpaper".to_string(),
                layer: Layer::Overlay,
                anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                exclusive_zone: Some(px(-1.)),
                keyboard_interactivity: KeyboardInteractivity::Exclusive,
                ..Default::default()
            }),
            ..Default::default()
        },
        |window, cx| cx.new(|cx| WpPicker::new(window, cx)),
    );
    if let Ok(h) = opened {
        cx.set_global(WpState(Some(h.into())));
    }
}

pub(crate) fn close_wp(window: &mut Window, cx: &mut App) {
    cx.set_global(WpState(None));
    window.remove_window();
}

pub(crate) struct WpPicker {
    pal: Palette,
    focus: FocusHandle,
    list: Vec<PathBuf>,
    current: String,
    sel: usize,
    scroll: gpui::ScrollHandle,
    last_input: std::time::Instant,
    thumbs_pending: bool, // the generator was running at the last poll
}

impl WpPicker {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let list = wp_list();
        let current = wp_current();
        let sel = list.iter().position(|p| p.to_string_lossy() == current).unwrap_or(0);
        let todo = list.clone();
        std::thread::spawn(move || wp_make_thumbs(todo));
        // thumbnails appear as the background thread produces them; also the idle safety net
        cx.spawn_in(window, async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_millis(700)).await;
            let keep = this
                .update_in(cx, |p, window, cx| {
                    if p.last_input.elapsed() > Duration::from_secs(120) {
                        close_wp(window, cx);
                        return false;
                    }
                    // while thumbnails are being made (one last time after): a failed one must not re-render forever
                    let running = WP_THUMBS_RUNNING.load(std::sync::atomic::Ordering::Acquire);
                    if running || std::mem::replace(&mut p.thumbs_pending, running) {
                        cx.notify();
                    }
                    true
                })
                .unwrap_or(false);
            if !keep {
                break;
            }
        })
        .detach();
        let scroll = gpui::ScrollHandle::new();
        WpPicker { pal: palette(), focus, list, current, sel, scroll, last_input: std::time::Instant::now(), thumbs_pending: true }
    }
}

impl Render for WpPicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.pal.clone();
        let sel = self.sel;
        let cells = self.list.iter().enumerate().map(|(i, path)| {
            let thumb = wp_thumb(path);
            let target = path.clone();
            let is_cur = path.to_string_lossy() == self.current;
            div()
                .id(("wp", i))
                .cursor_pointer()
                .w(px(190.))
                .h(px(107.))
                .flex_none()
                .rounded_lg()
                .overflow_hidden()
                .bg(rgba((p.dim << 8) | 0x30))
                .border_2()
                .border_color(if i == sel {
                    rgb(p.accent)
                } else if is_cur {
                    rgba((p.accent << 8) | 0x70)
                } else {
                    rgba(0x00000000)
                })
                .on_click(move |_, window, cx| {
                    wp_apply(&target);
                    close_wp(window, cx);
                })
                .when(thumb.exists(), |d| d.child(img(thumb).size_full().object_fit(ObjectFit::Cover)))
        });
        div()
            .font_family(NF).font_weight(FontWeight::MEDIUM)
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                this.last_input = std::time::Instant::now();
                let n = this.list.len();
                let mut go = |this: &mut Self, to: usize, cx: &mut Context<Self>| {
                    this.sel = to;
                    this.scroll.scroll_to_item(to);
                    cx.notify();
                };
                match ev.keystroke.key.as_str() {
                    "escape" => close_wp(window, cx),
                    "right" if n > 0 => go(this, (this.sel + 1) % n, cx),
                    "left" if n > 0 => go(this, (this.sel + n - 1) % n, cx),
                    "down" if n > 0 => go(this, (this.sel + WP_COLS).min(n - 1), cx),
                    "up" if n > 0 => go(this, this.sel.saturating_sub(WP_COLS), cx),
                    "r" => {
                        run_bg("setsid ~/.local/bin/fiaboo-wallpaper --random");
                        close_wp(window, cx);
                    }
                    "enter" => {
                        if let Some(path) = this.list.get(this.sel) {
                            wp_apply(path);
                        }
                        close_wp(window, cx);
                    }
                    _ => {}
                }
            }))
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(MouseButton::Left, |_, window, cx| close_wp(window, cx))
            .on_mouse_down(MouseButton::Right, |_, window, cx| close_wp(window, cx))
            .child(
                div()
                    .w(px(840.))
                    .h(px(640.))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                    .flex()
                    .flex_col()
                    .gap_3()
                    .p_4()
                    .rounded_2xl()
                    .bg(rgba((p.bg << 8) | GLASS))
                    .border_1()
                    .border_color(rgba((p.dim << 8) | 0x70))
                    .text_color(rgb(p.fg))
                    .text_size(px(14.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).text_size(px(16.)).child("Wallpaper"))
                            .child(
                                div()
                                    .id("wp-random")
                                    .cursor_pointer()
                                    .px_3()
                                    .py_1()
                                    .rounded_lg()
                                    .text_size(px(12.))
                                    .bg(rgba((p.dim << 8) | 0x28))
                                    .hover(|s| s.bg(rgba((p.accent << 8) | 0x50)))
                                    .on_click(|_, window, cx| {
                                        run_bg("setsid ~/.local/bin/fiaboo-wallpaper --random");
                                        close_wp(window, cx);
                                    })
                                    .child("Shuffle"),
                            ),
                    )
                    .child(
                        div()
                            .id("wp-grid")
                            .flex_1()
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll)
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .content_start()
                            .gap_2()
                            .children(cells),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(p.dim))
                            .child(format!("{} images   ·   Arrows + Enter or click: apply   ·   R: shuffle   ·   Esc: close", self.list.len())),
                    ),
            )
    }
}
