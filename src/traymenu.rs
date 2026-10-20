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

pub(crate) fn open_tray_menu(cx: &mut App, item: tray::TrayItem) {
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
            show_tray_menu(cx, item, entries);
        });
    })
    .detach();
}

pub(crate) fn show_tray_menu(cx: &mut App, item: tray::TrayItem, entries: Vec<tray::MenuEntry>) {
    let rows = entries.iter().map(|e| if e.separator { 9.0 } else { 34.0 }).sum::<f32>();
    let opened = cx.open_window(
        WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: point(px(0.), px(0.)),
                size: size(px(0.), px(0.)),
            })),
            app_id: Some("fiaboo-traymenu".to_string()),
            window_background: WindowBackgroundAppearance::Transparent,
            kind: WindowKind::LayerShell(LayerShellOptions {
                namespace: "fiaboo-traymenu".to_string(),
                layer: Layer::Overlay,
                anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                exclusive_zone: Some(px(-1.)),
                keyboard_interactivity: KeyboardInteractivity::Exclusive,
                ..Default::default()
            }),
            ..Default::default()
        },
        |window, cx| cx.new(|cx| TrayMenu::new(window, cx, item, entries)),
    );
    if let Ok(h) = opened {
        cx.set_global(TrayMenuState(Some(h.into())));
    }
}

pub(crate) struct TrayMenu {
    pal: Palette,
    item: tray::TrayItem,
    entries: Vec<tray::MenuEntry>,
    focus: FocusHandle,
}

impl TrayMenu {
    fn new(window: &mut Window, cx: &mut Context<Self>, item: tray::TrayItem, entries: Vec<tray::MenuEntry>) -> Self {
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
        TrayMenu { pal: palette(), item, entries, focus }
    }
}

impl Render for TrayMenu {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.pal.clone();
        let rows = self.entries.iter().enumerate().map(|(i, e)| {
            if e.separator {
                return div().h(px(9.)).flex().items_center().child(div().w_full().h(px(1.)).bg(rgba((p.dim << 8) | A_ACTIVE))).into_any_element();
            }
            let (item, id, enabled) = (self.item.clone(), e.id, e.enabled);
            div()
                .id(("tm", i))
                .h(px(34.))
                .flex()
                .items_center()
                .px_3()
                .rounded_lg()
                .text_color(if e.enabled { rgb(p.fg) } else { rgba((p.dim << 8) | 0x90) })
                .when(e.enabled, |d| d.cursor_pointer().hover(|s| s.bg(rgba((p.accent << 8) | A_HOVER))))
                .on_click(move |_, window, cx| {
                    if enabled {
                        let it = item.clone();
                        std::thread::spawn(move || tray::menu_click(&it, id));
                        cx.set_global(TrayMenuState(None));
                        window.remove_window();
                    }
                })
                .child(e.label.clone())
                .into_any_element()
        });
        fade_in("traymenu", div()
            .font_family(NF).font_weight(FontWeight::MEDIUM)
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|_, ev: &KeyDownEvent, window, cx| {
                if ev.keystroke.key == "escape" {
                    cx.set_global(TrayMenuState(None));
                    window.remove_window();
                }
            }))
            .flex()
            .flex_col()
            .justify_end()
            .items_end()
            .pr(px(190.0))
            .pb(px(56.))
            .on_mouse_down(MouseButton::Left, |_, window, cx| { cx.set_global(TrayMenuState(None)); window.remove_window(); }).on_mouse_down(MouseButton::Right, |_, window, cx| { cx.set_global(TrayMenuState(None)); window.remove_window(); })
            .child(
                div()
                    .w(px(250.0))
                    .h(px(self.entries.iter().map(|e| if e.separator { 9.0 } else { 34.0 }).sum::<f32>() + 20.))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                    .flex()
                    .flex_col()
            .p_2()
            .rounded_xl()
            .bg(rgba((p.bg << 8) | GLASS))
            .border_1()
            .border_color(rgba((p.dim << 8) | A_STRONG))
            .text_size(px(14.))
            .children(rows)
            ))
    }
}
