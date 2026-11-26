//! Tray context menu (dbusmenu).

use crate::*;

// ---------- tray context menu (dbusmenu) ----------
pub(crate) struct TrayMenuState(pub(crate) Option<AnyWindowHandle>);
impl Global for TrayMenuState {}

pub(crate) fn close_tray_menu_global(cx: &mut App) -> bool {
    if let Some(handle) = cx.try_global::<TrayMenuState>().and_then(|s| s.0) {
        cx.set_global(TrayMenuState(None));
        return handle.update(cx, |_, window, _| window.remove_window()).is_ok();
    }
    false
}

/// `x`: where on the bar the icon was right-clicked; the menu opens centred above it.
pub(crate) fn open_tray_menu(cx: &mut App, item: tray::TrayItem, x: f32) {
    let was_open = close_tray_menu_global(cx);
    if was_open {
        return;
    }
    // a click while an earlier menu is still loading supersedes it (else two menus, one orphaned)
    static OPENING: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let ticket = OPENING.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    cx.spawn(async move |cx| {
        let it = item.clone();
        let entries = cx.background_executor().spawn(async move { tray::fetch_menu(&it) }).await;
        if OPENING.load(std::sync::atomic::Ordering::Relaxed) != ticket {
            return;
        }
        let _ = cx.update(|cx| {
            if entries.is_empty() {
                // no dbusmenu: ask the app to show its own menu
                std::thread::spawn(move || tray::context_menu(&item));
                return;
            }
            show_tray_menu(cx, item, entries, x);
        });
    })
    .detach();
}

pub(crate) fn show_tray_menu(cx: &mut App, item: tray::TrayItem, entries: Vec<tray::MenuEntry>, x: f32) {
    let opened = cx.open_window(
        crate::overlay_options("fiaboo-traymenu"),
        |window, cx| cx.new(|cx| TrayMenu::new(window, cx, item, entries, x)),
    );
    if let Ok(h) = opened {
        cx.set_global(TrayMenuState(Some(h.into())));
    }
}

/// Run a menu entry and close the menu.
fn tray_click(item: &tray::TrayItem, id: i32, window: &mut Window, cx: &mut App) {
    let it = item.clone();
    std::thread::spawn(move || tray::menu_click(&it, id));
    cx.set_global(TrayMenuState(None));
    window.remove_window();
}

const MENU_W: f32 = 250.;

pub(crate) struct TrayMenu {
    pal: Palette,
    item: tray::TrayItem,
    entries: Vec<tray::MenuEntry>,
    focus: FocusHandle,
    x: f32,
    focus_i: Option<usize>, // keyboard focus among the clickable entries (`clickable()`)
    scroll: gpui::ScrollHandle,
}

impl TrayMenu {
    /// Indices into `entries` that do something: not separators, not submenu headers, not disabled.
    fn clickable(&self) -> Vec<usize> {
        self.entries.iter().enumerate().filter(|(_, e)| !e.separator && e.enabled && !e.submenu).map(|(i, _)| i).collect()
    }

    fn new(window: &mut Window, cx: &mut Context<Self>, item: tray::TrayItem, entries: Vec<tray::MenuEntry>, x: f32) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        // safety net: this menu holds exclusive keyboard focus, so never keep it longer than 30 s
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(30)).await;
            let _ = this.update_in(cx, |_, window, cx| {
                cx.set_global(TrayMenuState(None));
                window.remove_window();
            });
        })
        .detach();
        TrayMenu { pal: palette(), item, entries, focus, x, focus_i: None, scroll: gpui::ScrollHandle::new() }
    }
}

impl Render for TrayMenu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.pal.clone();
        let screen = window.bounds().size;
        let (screen_w, screen_h): (f32, f32) = (screen.width.into(), screen.height.into());
        let left = (self.x - MENU_W / 2.).clamp(8., (screen_w - MENU_W - 8.).max(8.));
        let focused = self.focus_i.and_then(|i| self.clickable().get(i).copied());
        // one check lane for every row once any entry has a check, so the labels line up
        let checks = self.entries.iter().any(|e| e.check.is_some());
        let rows = self.entries.iter().enumerate().map(|(i, e)| {
            if e.separator {
                return div().h(px(9.)).flex_none().flex().items_center().child(div().w_full().h(px(1.)).bg(rgba((p.dim << 8) | A_ACTIVE))).into_any_element();
            }
            // a submenu parent is a header over its (indented) entries, not a button
            let (item, id, enabled) = (self.item.clone(), e.id, e.enabled && !e.submenu);
            div()
                .id(("tm", i))
                .h(px(34.))
                .flex_none()
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .pl(px(12. + 16. * e.depth as f32))
                .rounded_lg()
                .text_color(if enabled { rgb(p.fg) } else { rgb(p.dim) })
                .when(e.submenu, |d| d.text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).text_color(rgb(p.dim)))
                .when(enabled, |d| d.cursor_pointer().hover(|s| s.bg(rgba((p.accent << 8) | A_HOVER))))
                .when(focused == Some(i), |d| d.bg(rgba((p.accent << 8) | A_HOVER)))
                .on_click(move |_, window, cx| {
                    if enabled {
                        tray_click(&item, id, window, cx);
                    }
                })
                .when(checks, |d| d.child(div().w(px(16.)).flex_none().child(if e.check == Some(true) { "\u{f012c}" } else { "" })))
                .child(e.label.clone())
                .into_any_element()
        });
        div()
            .font_family(NF).font_weight(FontWeight::MEDIUM)
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                let k = &ev.keystroke;
                let rows = this.clickable();
                match k.key.as_str() {
                    "escape" => {
                        cx.set_global(TrayMenuState(None));
                        window.remove_window();
                    }
                    "down" | "up" | "tab" => {
                        this.focus_i = step_focus(this.focus_i, rows.len(), k.key == "up" || k.modifiers.shift);
                        if let Some(&i) = this.focus_i.and_then(|f| rows.get(f)) {
                            this.scroll.scroll_to_item(i);
                        }
                        cx.notify();
                    }
                    "enter" | "space" => {
                        if let Some(e) = this.focus_i.and_then(|f| rows.get(f)).map(|&i| &this.entries[i]) {
                            tray_click(&this.item, e.id, window, cx);
                        }
                    }
                    _ => {}
                }
            }))
            .flex()
            .flex_col()
            .justify_end()
            .items_start()
            .pl(px(left))
            .pb(px(56.))
            .on_mouse_down(MouseButton::Left, |_, window, cx| { cx.set_global(TrayMenuState(None)); window.remove_window(); }).on_mouse_down(MouseButton::Right, |_, window, cx| { cx.set_global(TrayMenuState(None)); window.remove_window(); })
            .child(
                div()
                    .id("tm-list")
                    .w(px(MENU_W))
                    // long menus (a browser's tray, a mail client's folders) scroll from 60 % of the screen
                    .h(px((self.entries.iter().map(|e| if e.separator { 9.0 } else { 34.0 }).sum::<f32>() + 20.).min(screen_h * 0.6)))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                    .flex()
                    .flex_col()
            .p_2()
            .rounded_xl()
            .bg(rgba((p.bg << 8) | PANEL_FILL))
            .border_1()
            .border_color(rgba((p.fg << 8) | A_BORDER))
            .text_size(px(14.))
            .children(rows)
            )
    }
}
