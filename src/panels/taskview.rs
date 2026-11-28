//! Task View (Hyprland window thumbnails).
use crate::commons::theme::{A_BORDER, A_STRONG, card_fill, card_shadow, NF, Palette, palette, PANEL_FILL};
use crate::commons::ui::overlay_options;
use crate::commons::util::home;
use crate::services::hypr::focus_window;
use crate::services::wm;
use crate::services::wm::hypr_json as hyprctl_json;
use gpui::{
    AnyWindowHandle, App, Context, div, FocusHandle, FontWeight, Global, img, KeyDownEvent, MouseButton,
    ObjectFit, px, rgb, rgba, Window,
};
use gpui::prelude::*;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

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

/// Capture every listed window (other workspaces too, not special ones) into small JPEGs. Each open uses fresh file
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
        overlay_options("fiaboo-taskview"),
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
    scroll: gpui::ScrollHandle,
    thumbs_done: Arc<std::sync::atomic::AtomicBool>, // capture finished: a missing thumbnail failed, it is not still loading
}

impl TaskView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let wins = tv_windows();
        let jobs: Vec<(String, PathBuf)> = wins.iter().map(|w| (w.stable_id.clone(), w.thumb.clone())).collect();
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (flag, thumbs_done) = (done.clone(), done.clone());
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
        // exclusive keyboard focus: close after 90 s without a key press or mouse use, like the other panels
        cx.spawn_in(window, async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let idle = this.update(cx, |t, _| t.last_input.elapsed() > Duration::from_secs(90)).unwrap_or(true);
            if idle {
                let _ = this.update_in(cx, |_, window, cx| close_taskview(window, cx));
                break;
            }
        })
        .detach();
        // every open has fresh file names, so GPUI's asset cache would otherwise keep each decoded thumbnail forever
        cx.on_release(|t, cx| {
            for w in &t.wins {
                gpui::ImageSource::from(w.thumb.clone()).remove_asset(cx);
            }
        })
        .detach();
        TaskView { pal: palette(), focus, wins, sel: 0, last_input: std::time::Instant::now(), pending_close: None, scroll: gpui::ScrollHandle::new(), thumbs_done }
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.pal.clone();
        let screen_h: f32 = window.bounds().size.height.into();
        let empty = self.wins.is_empty();
        let sel = self.sel;
        let pending = self.pending_close.filter(|(_, t)| t.elapsed() < CLOSE_CONFIRM).map(|(i, _)| i);
        let thumbs_done = self.thumbs_done.load(std::sync::atomic::Ordering::Relaxed);
        let cards = self.wins.iter().enumerate().map(|(i, w)| {
            let focus_addr = w.address.clone();
            div()
                .id(("tv", i))
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
                .when(i == sel || pending == Some(i), |d| d.border_2().p(px(11.)).border_color(rgb(p.accent))) // 1 px less padding: the content stays put
                // armed to close: the border carries the warning, because the line below says so
                // in fg. err at 14 px measured 3.4:1 over the old glass, under AA, and a border is
                // not the only cue -- the text changes too.
                .when(pending == Some(i), |d| d.border_color(rgb(p.err)))
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
                        .map(|d| {
                            if w.thumb.exists() {
                                d.child(img(w.thumb.clone()).size_full().object_fit(ObjectFit::Contain))
                            } else {
                                d.flex()
                                    .items_center()
                                    .justify_center()
                                    .text_size(px(12.))
                                    .text_color(rgb(p.dim))
                                    .child(if thumbs_done { "No preview" } else { "Loading\u{2026}" })
                            }
                        }),
                )
                .child(
                    div()
                        .flex()
                        .items_baseline()
                        .gap_2()
                        .text_size(px(14.))
                        .font_weight(FontWeight::SEMIBOLD)
                        // the confirmation used to sit in the header row; with that row gone this
                        // is the only place left to say it, and saying nothing would make one Del
                        // arm a close with no sign of it on screen. It names the window; only the
                        // title is ellipsized, never the instruction.
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(if pending == Some(i) { format!("Close \u{201c}{}", w.title) } else { w.title.clone() }),
                        )
                        .child(if pending == Some(i) {
                            div().flex_none().child("\u{201d}? Del again")
                        } else {
                            // cards from different workspaces look alike without it
                            div().flex_none().text_size(px(12.)).font_weight(FontWeight::MEDIUM).text_color(rgb(p.dim)).child(format!("Workspace {}", w.ws))
                        }),
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
                    "tab" if n > 0 && ev.keystroke.modifiers.shift => {
                        this.sel = (this.sel + n - 1) % n;
                        this.scroll.scroll_to_item(this.sel);
                        cx.notify();
                    }
                    "right" | "down" | "tab" if n > 0 => {
                        this.sel = (this.sel + 1) % n;
                        this.scroll.scroll_to_item(this.sel);
                        cx.notify();
                    }
                    "left" | "up" if n > 0 => {
                        this.sel = (this.sel + n - 1) % n;
                        this.scroll.scroll_to_item(this.sel);
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
            .bg(rgba((p.bg << 8) | PANEL_FILL))
            .text_color(rgb(p.fg))
            .on_mouse_move(cx.listener(|this, _: &gpui::MouseMoveEvent, _, _| this.last_input = std::time::Instant::now()))
            .on_scroll(cx.listener(|this, _: &gpui::ScrollEvent, _, _| this.last_input = std::time::Instant::now()))
            .on_mouse_down(MouseButton::Left, |_, window, cx| close_taskview(window, cx))
            .on_mouse_down(MouseButton::Right, |_, window, cx| close_taskview(window, cx))
            .child(
                div()
                    .id("tv-grid")
                    // more than ~12 windows: the grid scrolls instead of running off the screen (hint row ~40 px, margins)
                    .when(screen_h > 0., |d| d.max_h(px(screen_h - 96.)))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .max_w(px(1600.))
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .content_start()
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
