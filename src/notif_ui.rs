//! Notification popups (toasts) and the notification center.

use crate::*;

// ---------- notifications: popups + center ----------
const NC_ROWS: usize = 4; // cards shown before the center scrolls
pub(crate) const CARD_H: f32 = 108.; // app + summary + 2 body lines (~80px) + padding; 100 clipped the 2nd body line

pub(crate) fn notif_card(n: &notify::Notif, p: &Palette, in_center: bool, id: usize, today: &str) -> gpui::AnyElement {
    let (nid, nid2, nid3) = (n.id, n.id, n.id);
    let icon = find_icon_file(&n.icon).or_else(|| icon_for(&n.app.to_lowercase()));
    div()
        .id(("notif", id))
        .cursor_pointer()
        .w_full()
        .h(px(CARD_H))
        .flex_none()
        .flex()
        .gap_3()
        .p_3()
        .rounded_xl()
        .bg(rgba((p.bg << 8) | if in_center { 0x00 } else { GLASS }))
        .when(in_center, |d| d.bg(rgba((p.dim << 8) | A_SURFACE)))
        .border_1()
        .border_color(if n.critical { rgba((p.err << 8) | 0xcc) } else { rgba((p.fg << 8) | A_BORDER) })
        .hover(|s| s.bg(rgba((p.accent << 8) | A_HOVER)))
        .text_color(rgb(p.fg))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_mouse_down(MouseButton::Right, move |_, _, cx| {
            cx.stop_propagation();
            notify::dismiss(nid2);
        })
        .on_click(move |_, _, _| notify::activate(nid))
        .child(match icon {
            Some(path) => img(path).size(px(36.)).flex_none().object_fit(ObjectFit::Contain).into_any_element(),
            None => div()
                .size(px(36.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded_lg()
                .bg(rgba((p.dim << 8) | A_ACTIVE))
                .font_family(NF).font_weight(FontWeight::MEDIUM)
                .child("\u{f009c}")
                .into_any_element(),
        })
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .text_size(px(12.))
                        .text_color(rgb(p.dim))
                        .child(div().flex_1().overflow_hidden().whitespace_nowrap().text_ellipsis().child(n.app.clone()))
                        .child(notify::short_time(&n.time, today)),
                )
                .child(
                    div()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_size(px(14.))
                        .child(n.summary.clone()),
                )
                .child(div().max_h(px(34.)).line_height(px(17.)).overflow_hidden().text_size(px(12.)).text_color(rgb(p.fg)).child(n.body.clone())),
        )
        .child(
            div()
                .id(("notif-x", id))
                .flex_none()
                .size(px(32.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_md()
                .text_color(rgb(p.dim))
                .hover(|s| s.bg(rgba((p.dim << 8) | A_ACTIVE)).text_color(rgb(p.fg)))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    if in_center { notify::dismiss(nid3) } else { notify::hide_popup(nid3) }
                })
                .child("\u{2715}"),
        )
        .into_any_element()
}

pub(crate) struct NotifPopupState(pub(crate) Option<(gpui::WindowHandle<NotifPopup>, usize)>);
impl Global for NotifPopupState {}

pub(crate) struct NotifPopup {
    pal: Palette,
    list: Vec<notify::Notif>,
}

impl Render for NotifPopup {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.pal.clone();
        let today = today_dm();
        div()
            .font_family(NF).font_weight(FontWeight::MEDIUM)
            .size_full()
            .flex()
            .flex_col()
            .gap_2()
            .text_size(px(14.))
            .children(self.list.iter().enumerate().map(|(i, n)| notif_card(n, &p, false, i, &today)))
    }
}

/// Open, resize (by reopening) or close the popup window so it matches notify::popups().
/// Any shell panel or the screenshot overlay is open.
pub(crate) fn panel_open(cx: &App) -> bool {
    fn on<G: Global>(cx: &App, open: impl Fn(&G) -> bool) -> bool {
        cx.try_global::<G>().is_some_and(open)
    }
    on::<QuickState>(cx, |s| s.0.is_some())
        || on::<TrayMenuState>(cx, |s| s.0.is_some())
        || on::<CalState>(cx, |s| s.0.is_some())
        || on::<NotifCenterState>(cx, |s| s.0.is_some())
        || on::<ClipState>(cx, |s| s.0.is_some())
        || on::<ShotState>(cx, |s| s.0.is_some())
        || on::<WpState>(cx, |s| s.0.is_some())
        || on::<SettingsState>(cx, |s| s.0.is_some())
        || on::<StartState>(cx, |s| s.0.is_some())
        || on::<TaskViewState>(cx, |s| s.0.is_some())
}

pub(crate) fn sync_notif_popups(cx: &mut App) {
    // popups would cover the open panel, or end up in a screenshot; they come back when it closes
    let list = if panel_open(cx) { Vec::new() } else { notify::popups() };
    let cur = cx.try_global::<NotifPopupState>().and_then(|s| s.0);
    if let Some((h, n)) = cur {
        if n == list.len() && h.update(cx, |v, _, cx| { v.list = list.clone(); cx.notify(); }).is_ok() {
            return;
        }
        let _ = h.update(cx, |_, window, _| window.remove_window());
        cx.set_global(NotifPopupState(None));
    }
    if list.is_empty() {
        return;
    }
    let n = list.len();
    let h = n as f32 * (CARD_H + 8.);
    let opened = cx.open_window(
        WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(Bounds { origin: point(px(0.), px(0.)), size: size(px(380.), px(h)) })),
            app_id: Some("fiaboo-notify".to_string()),
            window_background: WindowBackgroundAppearance::Transparent,
            kind: WindowKind::LayerShell(LayerShellOptions {
                namespace: "fiaboo-notify".to_string(),
                layer: Layer::Overlay,
                anchor: Anchor::BOTTOM | Anchor::RIGHT,
                margin: Some((px(0.), px(10.), px(8.), px(0.))),
                keyboard_interactivity: KeyboardInteractivity::None,
                ..Default::default()
            }),
            ..Default::default()
        },
        move |_, cx| cx.new(|_| NotifPopup { pal: palette(), list }),
    );
    if let Ok(h) = opened {
        cx.set_global(NotifPopupState(Some((h, n))));
    }
}

pub(crate) struct NotifCenterState(pub(crate) Option<AnyWindowHandle>);
impl Global for NotifCenterState {}

pub(crate) fn toggle_notif_center(cx: &mut App) {
    if let Some(handle) = cx.try_global::<NotifCenterState>().and_then(|s| s.0) {
        cx.set_global(NotifCenterState(None));
        if handle.update(cx, |_, window, _| window.remove_window()).is_ok() {
            return;
        }
    }
    notify::mark_seen();
    let opened = cx.open_window(
        WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(Bounds { origin: point(px(0.), px(0.)), size: size(px(0.), px(0.)) })),
            app_id: Some("fiaboo-notifcenter".to_string()),
            window_background: WindowBackgroundAppearance::Transparent,
            kind: WindowKind::LayerShell(LayerShellOptions {
                namespace: "fiaboo-notifcenter".to_string(),
                layer: Layer::Overlay,
                anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                exclusive_zone: Some(px(-1.)),
                keyboard_interactivity: KeyboardInteractivity::Exclusive,
                ..Default::default()
            }),
            ..Default::default()
        },
        |window, cx| cx.new(|cx| NotifCenter::new(window, cx)),
    );
    if let Ok(h) = opened {
        cx.set_global(NotifCenterState(Some(h.into())));
    }
}

pub(crate) fn close_notif_center(window: &mut Window, cx: &mut App) {
    cx.set_global(NotifCenterState(None));
    window.remove_window();
}

pub(crate) struct NotifCenter {
    pal: Palette,
    focus: FocusHandle,
    gen: u64,
    confirm_clear: bool, // "Clear all" asks once before deleting every notification
}

impl NotifCenter {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        // re-render when the list changes (new notification, dismissed from a popup...)
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_millis(300)).await;
            let g = notify::generation();
            let alive = this
                .update(cx, |c, cx| {
                    if c.gen != g {
                        c.gen = g;
                        cx.notify();
                    }
                })
                .is_ok();
            if !alive {
                break;
            }
        })
        .detach();
        NotifCenter { pal: palette(), focus, gen: notify::generation(), confirm_clear: false }
    }
}

impl Render for NotifCenter {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.pal.clone();
        let list = notify::all();
        let today = today_dm();
        let dnd = notify::dnd();
        let pill = |id: &'static str, label: String, on: bool| {
            div()
                .id(id)
                .cursor_pointer()
                .px_3()
                .py_1()
                .rounded_lg()
                .text_size(px(12.))
                .bg(if on { rgba((p.accent << 8) | A_ACTIVE) } else { rgba((p.dim << 8) | A_HOVER) })
                .hover(|s| s.bg(rgba((p.accent << 8) | A_ACTIVE)))
                .child(label)
        };
        div()
            .font_family(NF).font_weight(FontWeight::MEDIUM)
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|_, ev: &KeyDownEvent, window, cx| {
                if ev.keystroke.key == "escape" {
                    close_notif_center(window, cx);
                }
            }))
            .flex()
            .flex_col()
            .justify_end()
            .items_end()
            .pr(px(10.))
            .pb(px(56.))
            .on_mouse_down(MouseButton::Left, |_, window, cx| close_notif_center(window, cx))
            .on_mouse_down(MouseButton::Right, |_, window, cx| close_notif_center(window, cx))
            .child(
                div()
                    .w(px(400.))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
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
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).text_size(px(16.)).child("Notifications"))
                            .child(pill("nc-dnd", if dnd { "\u{f009b} DND".into() } else { "\u{f009c} DND".into() }, dnd).on_click(
                                cx.listener(|_, _, _, cx| {
                                    notify::toggle_dnd();
                                    cx.notify();
                                }),
                            ))
                            .children(if self.confirm_clear {
                                let n = notify::all().len();
                                vec![
                                    pill("nc-keep", "Keep".into(), false).on_click(cx.listener(|this, _, _, cx| {
                                        this.confirm_clear = false;
                                        cx.notify();
                                    })),
                                    pill("nc-clear-yes", if n == 1 { "Delete 1".into() } else { format!("Delete all {n}") }, false)
                                        .bg(rgba((p.err << 8) | 0xcc))
                                        .text_color(rgb(p.bg)) // on light error red: dark text (white was ~1.7:1)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            notify::clear_all();
                                            this.confirm_clear = false;
                                            cx.notify();
                                        })),
                                ]
                            } else {
                                vec![pill("nc-clear", "Clear all".into(), false).on_click(cx.listener(|this, _, _, cx| {
                                    this.confirm_clear = true;
                                    cx.notify();
                                }))]
                            }),
                    )
                    .child(
                        div()
                            .id("nc-list")
                            // whole cards: max_h on the panel left a fifth card cut by the border
                            .h(px(list.len().clamp(1, NC_ROWS) as f32 * (CARD_H + 8.) - 8.))
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .children(list.iter().enumerate().map(|(i, n)| notif_card(n, &p, true, i, &today)))
                            .when(list.is_empty(), |d| {
                                d.child(div().py_6().flex().justify_center().text_color(rgb(p.dim)).child("No notifications"))
                            }),
                    ),
            )
    }
}
