//! The taskbar: Start, workspaces, running apps, tray, status icons, clock; it drives every background loop.
pub(crate) mod musicline;

use crate::commons::theme::{A_ACTIVE, A_BORDER, A_HOVER, A_STRONG, NF, Palette, palette, PANEL_FILL};
use crate::commons::ui::{avatar, scroll_steps, with_tip};
use crate::commons::util::{run_bg, sh};
use crate::panels::calendar::toggle_calendar;
use crate::panels::clipboard::toggle_clip;
use crate::panels::notifications::{panel_open, sync_notif_popups, toggle_notif_center};
use crate::panels::osd::{hide_osd_if_due, OSD_PENDING, show_osd};
use crate::panels::quick::toggle_quick;
use crate::panels::screenshot::toggle_shot;
use crate::panels::settings::toggle_settings;
use crate::panels::start::toggle_start;
use crate::panels::taskview::toggle_taskview;
use crate::panels::traymenu::open_tray_menu;
use crate::panels::wallpaper::toggle_wp;
use crate::services::apps::launch_app;
use crate::services::hypr::{focus_window, HYPR_DIRTY, read_state, set_layout, State};
use crate::services::ipc::IPC_RX;
use crate::services::{notify, tray, wm};
use crate::services::system::{
    INDICATORS, Indicators, open_updates, parse_im, read_bar_sys, read_volume, Sys, sys_glyphs,
};
use gpui::{
    App, Bounds, Context, div, FontWeight, img, MouseButton, ObjectFit, point, px, rgb, rgba, size, Window,
    WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions,
};
use gpui::layer_shell::{Anchor, KeyboardInteractivity, Layer, LayerShellOptions};
use gpui::prelude::*;
use std::fs;
use std::time::Duration;

pub(crate) const BAR_HEIGHT: f32 = 48.0;
// ---------- view ----------
pub(crate) struct Bar {
    pub(crate) pal: Palette,
    pub(crate) st: State,
    pub(crate) sys: Sys,
    pub(crate) tray: Vec<tray::TrayItem>,
    pub(crate) notif_gen: u64,
    pub(crate) notif_count: usize,
    pub(crate) rec_secs: Option<u64>, // screen recording running for this long (the flag file's age)
    pub(crate) panels_open: bool, // popups were hidden for an open panel
    pub(crate) ind: Indicators,
}

/// A fiaboo panel is open (set by the bar loop): tooltips stay away, the button's panel is on top of them.
pub(crate) static PANELS_OPEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

impl Bar {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        if let Some(mut rx) = IPC_RX.lock().unwrap().take() {
            cx.spawn(async move |_, cx| {
                use futures::StreamExt;
                while let Some(cmd) = rx.next().await {
                    let _ = cx.update(|cx| match cmd.as_str() {
                        "start" => toggle_start(cx),
                        "taskview" => toggle_taskview(cx),
                        "clipboard" => toggle_clip(cx),
                        "shot" => toggle_shot(cx),
                        "wallpaper" => toggle_wp(cx),
                        "settings" => toggle_settings(cx),
                        "calendar" => toggle_calendar(cx),
                        "viz" => musicline::toggle_mini(cx),
                        "quick" => toggle_quick(cx),
                        "notifications" => toggle_notif_center(cx),
                        c => {
                            if let Some(arg) = c.strip_prefix("layout ") {
                                set_layout(arg.trim());
                            }
                        }
                    });
                }
            })
            .detach();
        }
        cx.spawn(async move |this, cx| {
            use std::sync::atomic::Ordering;
            let long_ago = std::time::Instant::now() - Duration::from_secs(60);
            let (mut last_full, mut last_sys) = (long_ago, long_ago);
            let mut last_minute = 0;
            loop {
                // the clock rides on read_state, which otherwise runs on a Hyprland change or every 10 s:
                // without this a minute could turn up to 10 s late on the bar
                let minute = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() / 60);
                let new_minute = minute != last_minute;
                last_minute = minute;
                // window/workspace state: only when Hyprland reports a change (plus a slow refresh for the clock and pins)
                let st = if HYPR_DIRTY.swap(false, Ordering::Relaxed) || new_minute || last_full.elapsed() > Duration::from_secs(10) {
                    last_full = std::time::Instant::now();
                    Some(cx.background_executor().spawn(async { read_state() }).await)
                } else {
                    None
                };
                let sys = if last_sys.elapsed() > Duration::from_secs(8) {
                    let first = last_sys == long_ago;
                    last_sys = std::time::Instant::now();
                    Some(cx.background_executor().spawn(async move { (read_bar_sys(), first.then(read_volume)) }).await)
                } else {
                    None
                };
                let tray_now = tray::items();
                let rec_flag = format!("{}/fiaboo-recording", std::env::var("XDG_RUNTIME_DIR").unwrap_or_default());
                let alive = this.update(cx, |bar, cx| {
                    let mut changed = false;
                    if let Some(st) = st.filter(|st| *st != bar.st) {
                        bar.st = st;
                        changed = true;
                    }
                    if let Some(((wifi_on, bt_on), vol)) = sys {
                        let (vol, muted) = vol.unwrap_or((bar.sys.vol, bar.sys.muted));
                        let sys = Sys { wifi_on, bt_on, vol, muted, ..Sys::default() };
                        if sys != bar.sys {
                            bar.sys = sys;
                            changed = true;
                        }
                    }
                    if let Some((vol, muted)) = OSD_PENDING.lock().unwrap().take() {
                        show_osd(cx, vol, muted);
                        bar.sys.vol = vol;
                        bar.sys.muted = muted;
                        changed = true;
                    }
                    hide_osd_if_due(cx);
                    let panels = panel_open(cx);
                    // hidden behind a panel, a toast must not run out unseen
                    if panels { notify::hold_popups() } else { notify::expire_popups() }
                    let g = notify::generation();
                    PANELS_OPEN.store(panels, std::sync::atomic::Ordering::Relaxed);
                    if g != bar.notif_gen || panels != bar.panels_open {
                        bar.notif_gen = g;
                        bar.panels_open = panels;
                        bar.notif_count = notify::unseen();
                        sync_notif_popups(cx);
                        changed = true;
                    }
                    let rec_secs = fs::metadata(&rec_flag)
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .map(|t| t.elapsed().unwrap_or_default().as_secs());
                    if rec_secs != bar.rec_secs {
                        bar.rec_secs = rec_secs; // once a second while recording: the timer ticks
                        changed = true;
                    }
                    let ind = INDICATORS.lock().unwrap().clone();
                    if ind != bar.ind {
                        bar.ind = ind;
                        changed = true;
                    }
                    if tray_now != bar.tray {
                        // the bar never closes: icons that left the tray must leave GPUI's asset cache by hand
                        for old in bar.tray.iter().filter_map(|t| t.icon.clone()) {
                            if !tray_now.iter().any(|t| t.icon.as_ref() == Some(&old)) {
                                let res = gpui::Resource::from(old);
                                if let Some(Ok(image)) = cx.fetch_asset::<gpui::ImgResourceLoader>(&res) {
                                    cx.drop_image(image, None);
                                }
                                cx.remove_asset::<gpui::ImgResourceLoader>(&res);
                            }
                        }
                        bar.tray = tray_now;
                        changed = true;
                    }
                    if changed {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
                cx.background_executor().timer(Duration::from_millis(150)).await;
            }
        })
        .detach();
        Bar { pal: palette(), st: State::default(), sys: Sys::default(), tray: tray::items(), notif_gen: u64::MAX, notif_count: 0, rec_secs: None, panels_open: false, ind: Indicators::default() }
    }
}

impl Render for Bar {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        // a bar reopened while its monitor was unplugged came back 1080 px tall and covered every
        // window (transparent); GPUI echoes each configure back as the requested size, so ask again
        let b = window.bounds().size;
        if b.height != px(BAR_HEIGHT) {
            window.resize(size(b.width, px(BAR_HEIGHT)));
        }
        let p = self.pal.clone();
        let st = &self.st;

        let dots = (1..=5).map(|id| {
            let is_active = id == st.active_ws;
            let used = st.occupied.contains(&id);
            with_tip(div()
                .id(("ws", id as usize))
                .py(px(12.))
                .px(px(8.)) // a 24 px wide target round an 8 px dot
                .rounded_md()
                .hover(|s| s.bg(rgba((p.dim << 8) | A_HOVER)))
                .on_click(move |_, _, _| wm::focus_workspace(id))
                .child(div()
                .w(if is_active { px(22.) } else { px(8.) })
                .h(px(8.))
                .rounded_full()
                .bg(if is_active {
                    rgb(p.accent)
                } else if used {
                    rgb(p.dim)
                } else {
                    rgba((p.dim << 8) | A_STRONG) // empty workspace: still >= 3:1 on the bar
                })), format!("Workspace {id}"))
        });

        let apps = st.apps.iter().enumerate().map(|(i, a)| {
            let address = a.address.clone();
            let launch = a.launch.clone();
            let running = a.running;
            let letter = a.class.chars().find(|c| c.is_alphabetic()).unwrap_or('?').to_uppercase().to_string();
            let icon = match &a.icon {
                Some(path) => div()
                    .size(px(26.))
                    .child(img(path.clone()).size(px(26.)).object_fit(ObjectFit::Contain))
                    .into_any_element(),
                None => div()
                    .size(px(26.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .bg(rgba((p.dim << 8) | A_ACTIVE))
                    .text_color(rgb(p.fg))
                    .child(letter)
                    .into_any_element(),
            };
            let name = a.name.clone();
            with_tip(div()
                .id(("app", i))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_0p5()
                .px_2()
                .h(px(36.))
                .rounded_md()
                .hover(|s| s.bg(rgba((p.dim << 8) | A_HOVER)))
                .on_mouse_down(MouseButton::Middle, {
                    let launch = launch.clone();
                    move |_, _, _| {
                        if let Some(stem) = &launch {
                            launch_app(stem)
                        }
                    }
                })
                .on_click(move |_, _, _| {
                    if running {
                        focus_window(&address)
                    } else if let Some(stem) = &launch {
                        launch_app(stem)
                    }
                })
                .child(icon)
                .child(
                    div()
                        .h(px(3.))
                        .w(if a.active { px(14.) } else { px(5.) })
                        .rounded_full()
                        .bg(if a.active {
                            rgb(p.accent)
                        } else if a.running {
                            rgba((p.dim << 8) | A_STRONG)
                        } else {
                            rgba(0x00000000)
                        }),
                ), name)
        });

        let (clock_time, clock_date) = st.time.split_once('|').unwrap_or((st.time.as_str(), ""));
        div()
            .font_family(NF).font_weight(FontWeight::MEDIUM)
            .size_full()
            .relative()
            .flex()
            .flex_row()
            .items_center()
            .px_3()
            .bg(rgba((p.bg << 8) | PANEL_FILL))
            .border_t_1()
            .border_color(rgba((p.fg << 8) | A_BORDER))
            .text_color(rgb(p.fg))
            .text_size(px(14.))
            // left: workspaces
            .child(
                div()
                    .w(px(340.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .id("ws-dots")
                            .flex()
                            .flex_row()
                            .items_center()
                            .on_scroll({
                                let active = st.active_ws;
                                move |ev, _, _| {
                                    let n = scroll_steps(&ev.delta) as i64;
                                    let to = (active + n).clamp(1, active.max(5)); // past the dots (ws 7): never jump back to 5
                                    if n != 0 && to != active {
                                        wm::focus_workspace(to);
                                    }
                                }
                            })
                            .children(dots),
                    )
                    .when(!st.layout.is_empty(), |d| {
                        let glyph = match st.layout.as_str() {
                            "master" => "\u{f0bcc}",    // md-view_split_vertical
                            "scrolling" => "\u{f0573}", // md-view_column
                            _ => "\u{f0575}",           // md-view_quilt
                        };
                        d.child(with_tip(div()
                            .id("layout-button")
                            .size(px(36.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_md()
                            .font_family(NF).font_weight(FontWeight::MEDIUM)
                            .text_size(px(16.))
                            .hover(|s| s.bg(rgba((p.dim << 8) | A_HOVER)))
                            .on_click(|_, _, _| set_layout("next"))
                            .on_mouse_down(MouseButton::Right, |_, _, _| set_layout("dwindle"))
                            .on_scroll(|ev, _, _| match scroll_steps(&ev.delta) {
                                0 => {}
                                n if n > 0 => set_layout("next"),
                                _ => set_layout("prev"),
                            })
                            .child(glyph),
                            format!("Layout: {} · click: next · right-click: dwindle · `layout <name>` in a terminal", st.layout)))
                    }),
            )
            // center (Win11): Start, Task View, apps
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_center()
                    .gap_1()
                    .child(
                        with_tip(
                            div()
                                .id("start-button")
                                .size(px(36.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_md()
                                .hover(|s| s.bg(rgba((p.dim << 8) | A_HOVER)))
                                .on_click(|_, _, cx| toggle_start(cx))
                                .child(avatar(30., &p)),
                            "Start",
                        ),
                    )
                    .child(
                        with_tip(
                            div()
                                .id("taskview-button")
                                .size(px(36.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_md()
                                .font_family(NF).font_weight(FontWeight::MEDIUM)
                                .text_size(px(28.)) // the glyph's ink is small for its em box
                                .hover(|s| s.bg(rgba((p.dim << 8) | A_HOVER)))
                                .on_click(|_, _, cx| toggle_taskview(cx))
                                .child("\u{f0574}"),
                            "Task View",
                        ),
                    )
                    .children(apps),
            )
            // right: tray, status icons, clock, notifications
            .child(
                div()
                    .w(px(340.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_end()
                    .gap_2()
                    .child(
                div().flex().items_center().gap_1().children(self.tray.iter().enumerate().map(|(i, it)| {
                    let (left, right) = (it.clone(), it.clone());
                    let title = if it.title.is_empty() { "Tray".to_string() } else { it.title.clone() };
                    with_tip(div()
                        .id(("tray", i))
                        .size(px(36.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_md()
                        .hover(|s| s.bg(rgba((p.dim << 8) | A_HOVER)))
                        .on_mouse_down(MouseButton::Left, move |_, _, _| {
                            let it = left.clone();
                            std::thread::spawn(move || tray::activate(&it));
                        })
                        .on_mouse_down(MouseButton::Right, move |ev: &gpui::MouseDownEvent, _, cx| open_tray_menu(cx, right.clone(), ev.position.x.into()))
                        .child(match &it.icon {
                            Some(path) => img(path.clone()).size(px(18.)).object_fit(ObjectFit::Contain).into_any_element(),
                            None => div()
                                .size(px(18.))
                                .rounded_full()
                                .bg(rgba((p.dim << 8) | A_STRONG))
                                .into_any_element(),
                        }), title)
                })),
            )
                    .when(self.ind.updates > 0, |d| {
                        d.child(with_tip(
                            div()
                                .id("updates")
                                .px_2()
                                .h(px(36.))
                                .rounded_md()
                                .flex()
                                .items_center()
                                .gap_1()
                                .font_family(NF).font_weight(FontWeight::MEDIUM)
                                .text_size(px(16.))
                                .hover(|s| s.bg(rgba((p.dim << 8) | A_HOVER)))
                                .on_click(|_, _, _| open_updates())
                                .child("\u{f03d5}")
                                .child(div().text_size(px(12.)).line_height(px(16.)).child(self.ind.updates.to_string())),
                            "Updates · click to install",
                        ))
                    })
                    .when(self.ind.mic, |d| {
                        d.child(with_tip(
                            div()
                                .id("mic")
                                .size(px(36.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .font_family(NF).font_weight(FontWeight::MEDIUM)
                                .text_size(px(16.))
                                .text_color(rgb(p.accent))
                                .child("\u{f036c}"),
                            "Microphone in use",
                        ))
                    })
                    .when_some(self.rec_secs, |d, secs| {
                        d.child(with_tip(
                            div()
                                .id("rec")
                                .px_2()
                                .h(px(36.))
                                .rounded_md()
                                .flex()
                                .items_center()
                                .gap_1()
                                .font_family(NF).font_weight(FontWeight::MEDIUM)
                                .text_size(px(16.))
                                .text_color(rgb(p.err))
                                .hover(|s| s.bg(rgba((p.dim << 8) | A_HOVER)))
                                .on_click(|_, _, _| run_bg("fiaboo-record"))
                                .child("\u{f044a}")
                                .child(div().text_size(px(12.)).line_height(px(16.)).child(format!("{:02}:{:02}", secs / 60, secs % 60))),
                            "Recording · click to stop",
                        ))
                    })
                    .when_some(self.ind.vi, |d, vi| {
                        d.child(with_tip(
                            div()
                                .id("input-method")
                                .min_w(px(36.))
                                .px_2()
                                .h(px(36.))
                                .rounded_md()
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(px(12.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .hover(|s| s.bg(rgba((p.dim << 8) | A_HOVER)))
                                .on_click(|_, _, _| {
                                    std::thread::spawn(|| INDICATORS.lock().unwrap().vi = parse_im(&sh("fcitx5-remote -t; fcitx5-remote -n 2>/dev/null")));
                                })
                                .child(if vi { "VI" } else { "EN" }),
                            "Input method · click to switch",
                        ))
                    })
                    .child({
                let (g_wifi, g_bt, g_vol) = sys_glyphs(&self.sys);
                with_tip(div()
                    .id("sys-button")
                    .on_scroll(|ev, _, _| {
                        let n = scroll_steps(&ev.delta);
                        if n != 0 {
                            let dir = if n > 0 { "-" } else { "+" };
                            run_bg(&format!("wpctl set-volume -l 1 @DEFAULT_AUDIO_SINK@ {}%{dir}", 5 * n.abs()));
                        }
                    })
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_2()
                    .h(px(36.))
                    .rounded_md()
                    .font_family(NF).font_weight(FontWeight::MEDIUM)
                    .text_size(px(16.))
                    .hover(|s| s.bg(rgba((p.dim << 8) | A_HOVER)))
                    .on_click(|_, _, cx| toggle_quick(cx))
                    .child(div().text_color(if self.sys.wifi_on { rgb(p.fg) } else { rgb(p.dim) }).child(g_wifi))
                    .child(div().text_color(if self.sys.bt_on { rgb(p.fg) } else { rgb(p.dim) }).child(g_bt))
                    .child(div().text_color(if self.sys.muted { rgb(p.dim) } else { rgb(p.fg) }).child(g_vol)), "Quick settings · scroll: volume")
            })
                    .child(
                        with_tip(div()
                            .id("clock")
                            .rounded_md()
                            .h(px(36.))
                            .justify_center()
                            .hover(|s| s.bg(rgba((p.dim << 8) | A_HOVER)))
                            .on_click(|_, _, cx| toggle_calendar(cx))
                            .flex()
                            .flex_col()
                            .items_end()
                            .px_2()
                            // whole-pixel line heights (16+16 in a 36 px box): GPUI has no hinting, so a fractional
                            // baseline is what makes small text look soft
                            .child(div().text_size(px(14.)).line_height(px(16.)).font_weight(FontWeight::SEMIBOLD).child(clock_time.to_string()))
                            .child(div().text_size(px(12.)).line_height(px(16.)).text_color(rgb(p.dim)).child(clock_date.to_string())), "Calendar"),
                    )
                    .child(
                        with_tip(div()
                            .id("bell")
                            .font_family(NF).font_weight(FontWeight::MEDIUM)
                            .text_size(px(16.))
                            .px_2()
                            .h(px(36.))
                            .rounded_md()
                            .hover(|s| s.bg(rgba((p.dim << 8) | A_HOVER)))
                            .on_click(|_, _, cx| toggle_notif_center(cx))
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(if notify::dnd() { "\u{f009b}" } else { "\u{f009c}" })
                            .when(self.notif_count > 0, |d| {
                                d.child(
                                    div()
                                        .text_size(px(12.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(rgb(p.accent))
                                        .child(self.notif_count.to_string()),
                                )
                            }), "Notifications"),
                    ),
            )
    }
}

pub(crate) fn open_bar(cx: &mut App) {
    if let Err(e) = cx.open_window(
        WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: point(px(0.), px(0.)),
                size: size(px(0.), px(BAR_HEIGHT)), // width 0 = compositor stretches it (anchored left+right)
            })),
            app_id: Some("fiaboo-shell".to_string()),
            window_background: WindowBackgroundAppearance::Transparent,
            kind: WindowKind::LayerShell(LayerShellOptions {
                namespace: "fiaboo-bar".to_string(),
                layer: Layer::Top,
                anchor: Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                exclusive_zone: Some(px(BAR_HEIGHT)), // full-width taskbar at the bottom, windows tile above it
                margin: None,
                keyboard_interactivity: KeyboardInteractivity::None,
                ..Default::default()
            }),
            ..Default::default()
        },
        |_, cx| cx.new(Bar::new),
    ) {
        eprintln!("fiaboo-shell: taskbar did not open: {e:#}"); // e.g. the monitor vanished again mid re-plug
    }
}
