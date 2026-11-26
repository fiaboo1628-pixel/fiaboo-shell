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
        // also a replaced source: a thumb older than its wallpaper is stale
        let mtime = |f: &std::path::Path| fs::metadata(f).and_then(|m| m.modified()).ok();
        if mtime(&t) < mtime(&src) {
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
    // by name, not ~/.local/bin: on Arch the script was installed there, here it is in the
    // store and on PATH. The hardcoded path simply did not exist, and spawn_bg swallows that,
    // so picking a wallpaper did nothing and said nothing.
    spawn_bg(Command::new("setsid").arg("fiaboo-wallpaper").arg(path));
}

pub(crate) fn toggle_wp(cx: &mut App) {
    if let Some(handle) = cx.try_global::<WpState>().and_then(|s| s.0) {
        cx.set_global(WpState(None));
        if handle.update(cx, |_, window, _| window.remove_window()).is_ok() {
            return;
        }
    }
    let opened = cx.open_window(
        crate::overlay_options("fiaboo-wallpaper"),
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
    query: String,        // typed filter on the file name, mirrors `field`
    field: gpui::Entity<SearchField>,
}

impl WpPicker {
    fn visible(&self) -> Vec<PathBuf> {
        let q = self.query.to_lowercase();
        self.list.iter().filter(|p| q.is_empty() || p.file_name().is_some_and(|n| n.to_string_lossy().to_lowercase().contains(&q))).cloned().collect()
    }
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
        cx.on_release(|p, cx| {
            // GPUI would keep every decoded thumb forever, and a regenerated one keeps its name
            for path in &p.list {
                gpui::ImageSource::from(wp_thumb(path)).remove_asset(cx);
            }
        })
        .detach();
        let scroll = gpui::ScrollHandle::new();
        scroll.scroll_to_item(sel);
        let field = cx.new(|_| SearchField::new(focus.clone(), "", palette().dim));
        cx.observe(&field, |p, field, cx| {
            p.last_input = std::time::Instant::now();
            if p.query != field.read(cx).text {
                p.query = field.read(cx).text.clone();
                p.sel = 0;
                cx.notify();
            }
        })
        .detach();
        WpPicker { pal: palette(), focus, list, current, sel, scroll, last_input: std::time::Instant::now(), thumbs_pending: true, query: String::new(), field }
    }
}

impl Render for WpPicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.pal.clone();
        let shown = self.visible();
        let sel = self.sel.min(shown.len().saturating_sub(1));
        let rows = self.list.len().div_ceil(WP_COLS).clamp(1, 5) as f32; // the whole list: filtering must not resize the panel
        let cells = shown.iter().enumerate().map(|(i, path)| {
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
                .relative()
                .bg(rgba((p.dim << 8) | A_HOVER))
                .shadow(card_shadow(&p, i == sel))
                .hover(|s| s.shadow(card_shadow(&p, true)))
                .border_2()
                .border_color(if i == sel {
                    rgb(p.accent)
                } else if is_cur {
                    rgba((p.accent << 8) | A_STRONG)
                } else {
                    rgba(0x00000000)
                })
                .on_click(move |_, window, cx| {
                    wp_apply(&target);
                    close_wp(window, cx);
                })
                .when(thumb.exists(), |d| d.child(img(thumb).size_full().object_fit(ObjectFit::Cover)))
                // "current" as a mark of its own; the border alone told it from "selected" by alpha only
                .when(is_cur, |d| {
                    d.child(
                        div()
                            .absolute()
                            .top(px(6.))
                            .right(px(6.))
                            .size(px(22.))
                            .rounded_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(rgba((p.bg << 8) | PANEL_FILL))
                            .font_family(NF)
                            .text_size(px(14.))
                            .text_color(rgb(p.accent))
                            .child("\u{f012c}"),
                    )
                })
        });
        let sel_name = shown.get(sel).and_then(|f| f.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        div()
            .font_family(NF).font_weight(FontWeight::MEDIUM)
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                this.last_input = std::time::Instant::now();
                let n = this.visible().len();
                let k = &ev.keystroke;
                let go = |this: &mut Self, to: usize, cx: &mut Context<Self>| {
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
                    "r" if k.modifiers.control => {
                        run_bg("setsid fiaboo-wallpaper --random");
                        close_wp(window, cx);
                    }
                    "enter" => {
                        if let Some(path) = this.visible().get(this.sel) {
                            wp_apply(path);
                        }
                        close_wp(window, cx);
                    }
                    "backspace" => this.field.update(cx, |f, cx| f.backspace(cx)),
                    _ => {} // typed text arrives through `field`
                }
            }))
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_move(cx.listener(|this, _: &gpui::MouseMoveEvent, _, _| this.last_input = std::time::Instant::now()))
            .on_scroll_wheel(cx.listener(|this, _: &gpui::ScrollWheelEvent, _, _| this.last_input = std::time::Instant::now()))
            .on_mouse_down(MouseButton::Left, |_, window, cx| close_wp(window, cx))
            .on_mouse_down(MouseButton::Right, |_, window, cx| close_wp(window, cx))
            .child(
                div()
                    .w(px(WP_COLS as f32 * 190. + (WP_COLS as f32 - 1.) * 8. + 34.)) // the grid's own width plus p_4 and the 1 px border: centred, no trailing gap
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                    .flex()
                    .flex_col()
                    .gap_3()
                    .p_4()
                    .rounded_2xl()
                    .bg(rgba((p.bg << 8) | PANEL_FILL))
                    .border_1()
                    .border_color(rgba((p.fg << 8) | A_BORDER))
                    .text_color(rgb(p.fg))
                    .text_size(px(14.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().font_weight(FontWeight::SEMIBOLD).text_size(px(16.)).child("Wallpaper"))
                            // the selected file's name: what the filter matches on
                            .child(div().flex_1().min_w(px(0.)).overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(px(12.)).text_color(rgb(p.dim)).child(sel_name))
                            .child(button("wp-random", "Shuffle", p.accent, &p).on_click(|_, window, cx| {
                                run_bg("setsid fiaboo-wallpaper --random");
                                close_wp(window, cx);
                            })),
                    )
                    .child(
                        div()
                            .id("wp-grid")
                            // whole thumbnail rows, up to five (flex_1 left a sixth cut by the footer); fewer images, a shorter panel
                            .h(px(rows * 107. + (rows - 1.) * 8.))
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll)
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .content_start()
                            .gap_2()
                            .children(cells)
                            .when(shown.is_empty() && !self.query.is_empty(), |d| {
                                d.child(div().w_full().h(px(107.)).flex().items_center().justify_center().text_color(rgb(p.dim)).child(format!("Nothing matches \u{201c}{}\u{201d}", self.query)))
                            }),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(p.dim))
                            .flex()
                            .when(!self.query.is_empty(), |d| d.child("Filter: "))
                            .child(SearchLine(self.field.clone())) // empty: zero wide, but the IME still needs it painted
                            .child(if self.query.is_empty() {
                                format!("{} images · Type: filter · Enter: apply · Ctrl+R: shuffle · Esc: close", self.list.len())
                            } else {
                                format!(" · {} of {} · Backspace: edit · Enter: apply · Esc: close", shown.len(), self.list.len())
                            }),
                    ),
            )
    }
}
