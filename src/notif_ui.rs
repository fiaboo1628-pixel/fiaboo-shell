//! Notification popups (toasts) and the notification center.

use crate::*;

// ---------- notifications: popups + center ----------
const NC_ROWS: usize = 4; // cards shown before the center scrolls
pub(crate) const CARD_H: f32 = 108.; // app + summary + 2 body lines (~80px) + padding; 100 clipped the 2nd body line

/// Action buttons ("Reply", "Open"…; "default" is the card click itself).
fn buttons(n: &notify::Notif) -> impl Iterator<Item = &(String, String)> {
    n.actions.iter().filter(|(k, _)| k != "default")
}

/// A card with action buttons is one 32px button row (+ gap) taller.
pub(crate) fn card_h(n: &notify::Notif) -> f32 {
    CARD_H + if buttons(n).next().is_some() { 36. } else { 0. }
}

/// A card; in the center (`center` set) dismissals go through its 5 s undo instead of the store.
pub(crate) fn notif_card(n: &notify::Notif, p: &Palette, center: Option<gpui::Entity<NotifCenter>>, focused: bool, id: usize, today: &str) -> gpui::AnyElement {
    let (nid, nid2, nid3) = (n.id, n.id, n.id);
    let in_center = center.is_some();
    let (c_right, c_x) = (center.clone(), center);
    let icon = find_icon_file(&n.icon).or_else(|| icon_for(&n.app.to_lowercase()));
    div()
        .id(("notif", id))
        .cursor_pointer()
        .w_full()
        .h(px(card_h(n)))
        .flex_none()
        .flex()
        .gap_3()
        .p_3()
        .rounded_xl()
        .bg(rgba((p.bg << 8) | if in_center { 0x00 } else { PANEL_FILL }))
        .when(in_center, |d| d.bg(card_fill(if n.critical { p.err } else { p.accent }, false)).shadow(card_shadow(&p, false)))
        .border_1()
        .border_color(if n.critical { rgba((p.err << 8) | A_BORDER) } else { rgba((p.fg << 8) | A_BORDER) })
        // keyboard focus ring: 2 px fg; one px less padding keeps the content still
        .when(focused, |d| d.border_2().p(px(11.)).border_color(rgb(p.fg)))
        .hover(|s| if in_center { s.bg(card_fill(if n.critical { p.err } else { p.accent }, true)).shadow(card_shadow(&p, true)) } else { s.bg(rgba((p.accent << 8) | A_HOVER)) })
        .text_color(rgb(p.fg))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_mouse_down(MouseButton::Right, move |_, _, cx| {
            cx.stop_propagation();
            match &c_right {
                Some(c) => c.update(cx, |t, cx| t.defer_dismiss(vec![nid2], cx)),
                None => notify::hide_popup(nid2), // a popup's right-click only hides it; the center keeps it
            }
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
                .rounded_md()
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
                .child(div().max_h(px(34.)).line_height(px(17.)).overflow_hidden().line_clamp(2).text_ellipsis().text_size(px(12.)).text_color(rgb(p.fg)).child(n.body.clone()))
                .when(buttons(n).next().is_some(), |d| {
                    d.child(div().mt_1().flex().gap_1().children(buttons(n).enumerate().map(|(j, (key, label))| {
                        let key = key.clone();
                        div()
                            .id(("notif-act", j))
                            .cursor_pointer()
                            .flex_1()
                            .min_w(px(0.))
                            .h(px(32.))
                            .px_2()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_md()
                            .text_size(px(12.))
                            .bg(rgba((p.accent << 8) | A_SURFACE))
                            .hover(|s| s.bg(rgba((p.accent << 8) | A_ACTIVE)))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(move |_, _, cx| {
                                cx.stop_propagation();
                                notify::invoke(nid, &key);
                            })
                            .child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().child(label.clone()))
                    })))
                }),
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
                    match &c_x {
                        Some(c) => c.update(cx, |t, cx| t.defer_dismiss(vec![nid3], cx)),
                        None => notify::hide_popup(nid3),
                    }
                })
                .child("\u{2715}"),
        )
        .into_any_element()
}

pub(crate) struct NotifPopupState(pub(crate) Option<(gpui::WindowHandle<NotifPopup>, f32)>); // (window, its height)
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
            .image_cache(gpui::retain_all("notif-popup-images")) // freed with the window; plain img() stays in GPUI's global cache forever
            .font_family(NF).font_weight(FontWeight::MEDIUM)
            .size_full()
            .flex()
            .flex_col()
            .gap_2()
            .text_size(px(14.))
            .children(self.list.iter().enumerate().map(|(i, n)| notif_card(n, &p, None, false, i, &today)))
    }
}

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

/// Open, resize (by reopening) or close the popup window so it matches notify::popups().
pub(crate) fn sync_notif_popups(cx: &mut App) {
    // popups would cover the open panel, or end up in a screenshot; they come back when it closes
    let list = if panel_open(cx) { Vec::new() } else { notify::popups() };
    let cur = cx.try_global::<NotifPopupState>().and_then(|s| s.0);
    let h = list.iter().map(|n| card_h(n) + 8.).sum::<f32>();
    if let Some((win, cur_h)) = cur {
        if cur_h == h && win.update(cx, |v, _, cx| { v.list = list.clone(); cx.notify(); }).is_ok() {
            return;
        }
        let _ = win.update(cx, |_, window, _| window.remove_window());
        cx.set_global(NotifPopupState(None));
    }
    if list.is_empty() {
        return;
    }
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
    if let Ok(win) = opened {
        cx.set_global(NotifPopupState(Some((win, h))));
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
        crate::overlay_options("fiaboo-notifcenter"),
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
    focus_i: Option<usize>, // keyboard focus in the visible list; None (ring hidden) until the first Tab or arrow
    hidden: std::collections::HashSet<u32>, // dismissed, still undoable
    pending: Option<(Vec<u32>, u64)>,       // (ids waiting for the store, token of the timer that commits them)
    undo_token: u64,
}

const UNDO_SECS: u64 = 5;

impl NotifCenter {
    /// Hide `ids` now; they leave the store after UNDO_SECS unless `undo` runs first.
    fn defer_dismiss(&mut self, ids: Vec<u32>, cx: &mut Context<Self>) {
        self.commit_pending();
        self.hidden.extend(ids.iter().copied());
        self.undo_token += 1;
        let token = self.undo_token;
        self.pending = Some((ids, token));
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(UNDO_SECS)).await;
            let _ = this.update(cx, |t, cx| {
                if t.pending.as_ref().is_some_and(|p| p.1 == token) {
                    t.commit_pending();
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn commit_pending(&mut self) {
        if let Some((ids, _)) = self.pending.take() {
            for id in &ids {
                self.hidden.remove(id);
            }
            notify::dismiss_many(&ids);
        }
    }

    fn undo(&mut self) {
        if let Some((ids, _)) = self.pending.take() {
            for id in ids {
                self.hidden.remove(&id);
            }
        }
    }
}

// closing the center (Esc, bell click, idle) must not resurrect what the owner deleted
impl Drop for NotifCenter {
    fn drop(&mut self) {
        self.commit_pending();
    }
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
        NotifCenter { pal: palette(), focus, gen: notify::generation(), confirm_clear: false, focus_i: None, hidden: Default::default(), pending: None, undo_token: 0 }
    }
}

impl Render for NotifCenter {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.pal.clone();
        let list: Vec<notify::Notif> = notify::all().into_iter().filter(|n| !self.hidden.contains(&n.id)).collect();
        let today = today_dm();
        let dnd = notify::dnd();
        let me = cx.entity();
        let focus_i = self.focus_i.map(|i| i.min(list.len().saturating_sub(1)));
        let pill = |id: &'static str, label: String, on: bool| button(id, label, p.accent2, &p).bg(card_fill(p.accent2, on));
        div()
            .image_cache(gpui::retain_all("notif-center-images")) // freed with the window; plain img() stays in GPUI's global cache forever
            .font_family(NF).font_weight(FontWeight::MEDIUM)
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                let shown: Vec<u32> = notify::all().iter().map(|n| n.id).filter(|id| !this.hidden.contains(id)).collect();
                let n = shown.len();
                let k = &ev.keystroke;
                match k.key.as_str() {
                    "escape" => close_notif_center(window, cx),
                    "tab" if k.modifiers.shift => this.focus_i = step_focus(this.focus_i, n, true),
                    "tab" | "down" => this.focus_i = step_focus(this.focus_i, n, false),
                    "up" => this.focus_i = step_focus(this.focus_i, n, true),
                    // with no ring yet these act on the first card, as before
                    "delete" | "backspace" if n > 0 => {
                        let id = shown[this.focus_i.unwrap_or(0).min(n - 1)];
                        this.defer_dismiss(vec![id], cx);
                    }
                    "enter" if n > 0 => notify::activate(shown[this.focus_i.unwrap_or(0).min(n - 1)]),
                    "z" if k.modifiers.control => this.undo(),
                    _ => {}
                }
                cx.notify();
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
                            .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).text_size(px(16.)).child("Notifications"))
                            // the state in words (a check), not only in the fill; Quick calls it the same
                            .child(pill("nc-dnd", if dnd { "\u{f012c} Do not disturb".into() } else { "Do not disturb".into() }, dnd).on_click(
                                cx.listener(|_, _, _, cx| {
                                    notify::toggle_dnd();
                                    cx.notify();
                                }),
                            ))
                            // only with something to clear, and not while the row below asks
                            .when(!self.confirm_clear && !list.is_empty(), |d| {
                                d.child(pill("nc-clear", "Clear".into(), false).on_click(cx.listener(|this, _, _, cx| {
                                    this.confirm_clear = true;
                                    cx.notify();
                                })))
                            }),
                    )
                    .when(self.confirm_clear && !list.is_empty(), |d| {
                        let n = list.len();
                        d.child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).child(if n == 1 { "Clear 1 notification?".to_string() } else { format!("Clear {n} notifications?") }))
                                .child(pill("nc-keep", "Cancel".into(), false).on_click(cx.listener(|this, _, _, cx| {
                                    this.confirm_clear = false;
                                    cx.notify();
                                })))
                                .child(button("nc-clear-yes", "Clear", p.err, &p).on_click(cx.listener(|this, _, _, cx| {
                                    let ids: Vec<u32> = notify::all().iter().map(|n| n.id).filter(|id| !this.hidden.contains(id)).collect();
                                    this.confirm_clear = false;
                                    this.defer_dismiss(ids, cx);
                                }))),
                        )
                    })
                    .child(
                        div()
                            .id("nc-list")
                            // whole cards: max_h on the panel left a fifth card cut by the border
                            .h(px(list.iter().take(NC_ROWS).map(|n| card_h(n) + 8.).sum::<f32>().max(CARD_H + 8.) - 8.))
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .children(list.iter().enumerate().map(|(i, n)| notif_card(n, &p, Some(me.clone()), focus_i == Some(i), i, &today)))
                            .when(list.is_empty(), |d| {
                                d.child(div().py_6().flex().justify_center().text_color(rgb(p.dim)).child("No notifications"))
                            }),
                    )
                    // a deletion waits UNDO_SECS here before it is final
                    .when_some(self.pending.as_ref().map(|p| p.0.len()), |d, n| {
                        d.child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_size(px(12.))
                                .text_color(rgb(p.dim))
                                .child(div().flex_1().child(if n == 1 { "Cleared 1 notification".to_string() } else { format!("Cleared {n} notifications") }))
                                .child(pill("nc-undo", "Undo".into(), true).on_click(cx.listener(|this, _, _, cx| {
                                    this.undo();
                                    cx.notify();
                                }))),
                        )
                    })
                    .when(!list.is_empty(), |d| {
                        d.child(div().text_size(px(12.)).text_color(rgb(p.dim)).child("Enter: open · Del: clear · Esc: close"))
                    }),
            )
    }
}
