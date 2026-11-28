//! Shared UI pieces for every panel: overlay window options, buttons, focus stepping, tooltips, avatar.
use crate::bar::{BAR_HEIGHT, PANELS_OPEN};
use crate::commons::theme::{A_ACTIVE, A_BORDER, card_fill, card_shadow, NF, Palette, palette, PANEL_FILL};
use crate::commons::util::home;
use gpui::{
    App, Bounds, Context, div, FontWeight, Global, img, MouseButton, ObjectFit, point, px, rgb, rgba, size,
    Window, WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions,
};
use gpui::layer_shell::{Anchor, KeyboardInteractivity, Layer, LayerShellOptions};
use gpui::prelude::*;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

/// Every panel: a full-screen transparent overlay layer that takes the keyboard; the panel
/// draws its own shell inside it and closes on a click outside.
pub(crate) fn overlay_options(namespace: &str) -> WindowOptions {
    WindowOptions {
        titlebar: None,
        window_bounds: Some(WindowBounds::Windowed(Bounds { origin: point(px(0.), px(0.)), size: size(px(0.), px(0.)) })),
        app_id: Some(namespace.to_string()),
        window_background: WindowBackgroundAppearance::Transparent,
        kind: WindowKind::LayerShell(LayerShellOptions {
            namespace: namespace.to_string(),
            layer: Layer::Overlay,
            anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
            exclusive_zone: Some(px(-1.)),
            keyboard_interactivity: KeyboardInteractivity::Exclusive,
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// The one text button (header actions, footers, confirmations): 36 px raised card tinted with
/// `tone` (`p.err` for destructive ones), hover = lifted. The transparent 2 px border is where a
/// keyboard focus ring goes (`.border_color(rgb(p.fg))`) without the button changing size.
pub(crate) fn button(id: impl Into<gpui::ElementId>, label: impl Into<gpui::SharedString>, tone: u32, p: &Palette) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_none()
        .h(px(36.))
        .px(px(10.))
        .border_2()
        .border_color(rgba(0x00000000))
        .flex()
        .items_center()
        .justify_center()
        .rounded_lg()
        .text_size(px(14.))
        .bg(card_fill(tone, false))
        .shadow(card_shadow(p, false))
        .hover(|s| s.bg(card_fill(tone, true)).shadow(card_shadow(p, true)))
        .child(label.into())
}

/// Keyboard focus after one Tab/arrow press over `n` stops (None for n = 0). It starts at None, so
/// the ring shows only once the keyboard is used, not when a panel is opened with the mouse.
pub(crate) fn step_focus(cur: Option<usize>, n: usize, back: bool) -> Option<usize> {
    if n == 0 {
        return None;
    }
    Some(match cur {
        None if back => n - 1,
        None => 0,
        Some(i) if back => (i.min(n - 1) + n - 1) % n,
        Some(i) => (i.min(n - 1) + 1) % n,
    })
}

/// Turn wheel events into whole steps (+1 = scroll down). Touchpads send many small pixel
/// deltas, so those are accumulated until they add up to one notch.
pub(crate) fn scroll_steps(delta: &gpui::ScrollDelta) -> i32 {
    static ACC: Mutex<f32> = Mutex::new(0.);
    let dy = match delta {
        gpui::ScrollDelta::Lines(p) if p.y == 0. => return 0, // horizontal tilt; 0.0.signum() is 1
        gpui::ScrollDelta::Lines(p) => return -p.y.signum() as i32,
        gpui::ScrollDelta::Pixels(p) => f32::from(p.y),
    };
    let mut acc = ACC.lock().unwrap();
    *acc += dy;
    let steps = (*acc / 40.).trunc();
    *acc -= steps * 40.;
    -steps as i32
}

// ---------- tooltips (bar buttons) ----------
// An overlay window of its own: the bar is 48 px tall, so GPUI's in-window tooltip would sit on the icons.
pub(crate) struct TipState(pub(crate) Option<gpui::WindowHandle<Tip>>);
impl Global for TipState {}
pub(crate) static TIP_TOKEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(crate) struct Tip {
    pub(crate) pal: Palette,
    pub(crate) text: String,
}

impl Render for Tip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.pal.clone();
        div()
            .font_family(NF).font_weight(FontWeight::MEDIUM)
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .rounded_lg()
            .bg(rgba((p.bg << 8) | PANEL_FILL))
            .border_1()
            .border_color(rgba((p.fg << 8) | A_BORDER))
            .text_size(px(12.))
            .text_color(rgb(p.fg))
            .child(self.text.clone())
    }
}

pub(crate) fn hide_tip(cx: &mut App) {
    TIP_TOKEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed); // a pending show is now stale
    if let Some(h) = cx.try_global::<TipState>().and_then(|s| s.0) {
        let _ = h.update(cx, |_, window, _| window.remove_window());
        cx.set_global(TipState(None));
    }
}

/// A 26 px pill just above the bar, centred on `x` (the pointer's x when the hover settled),
/// kept inside the `screen_w` wide screen. It hides by itself after 2.5 s: a panel opened from the
/// button takes the pointer and the bar never sees the leave.
pub(crate) fn show_tip(cx: &mut App, text: String, x: f32, screen_w: f32) {
    hide_tip(cx);
    let w = (text.chars().count() as f32 * 7.3 + 20.).round(); // 12 px JetBrainsMono ≈ 7.2 px per glyph
    let left = (x - w / 2.).clamp(8., (screen_w - w - 16.).max(8.));
    let opened = cx.open_window(
        WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(Bounds { origin: point(px(0.), px(0.)), size: size(px(w), px(26.)) })),
            app_id: Some("fiaboo-tip".to_string()),
            window_background: WindowBackgroundAppearance::Transparent,
            kind: WindowKind::LayerShell(LayerShellOptions {
                namespace: "fiaboo-tip".to_string(),
                layer: Layer::Overlay,
                anchor: Anchor::BOTTOM | Anchor::LEFT,
                margin: Some((px(0.), px(0.), px(BAR_HEIGHT + 6.), px(left))),
                keyboard_interactivity: KeyboardInteractivity::None,
                ..Default::default()
            }),
            ..Default::default()
        },
        move |_, cx| cx.new(|_| Tip { pal: palette(), text }),
    );
    let token = TIP_TOKEN.load(std::sync::atomic::Ordering::Relaxed);
    cx.set_global(TipState(opened.ok()));
    cx.spawn(async move |cx| {
        cx.background_executor().timer(Duration::from_millis(2500)).await;
        let _ = cx.update(|cx| {
            if TIP_TOKEN.load(std::sync::atomic::Ordering::Relaxed) == token {
                hide_tip(cx);
            }
        });
    })
    .detach();
}

/// The owner's picture (Start button, Start footer).
/// A missing file makes img() draw nothing, and the button is 30px of empty space you can
/// still click -- which is exactly what happened when ~/Pictures/fiaboo was not copied to this
/// machine and Start went invisible for days. Check first and fall back to a glyph, so a lost
/// image leaves a button that looks wrong rather than one that looks absent.
pub(crate) fn avatar(size_px: f32, p: &Palette) -> gpui::AnyElement {
    let face = PathBuf::from(format!("{}/Pictures/fiaboo/cauvang.jpg", home()));
    if face.exists() {
        img(face).size(px(size_px)).rounded_full().object_fit(ObjectFit::Cover).into_any_element()
    } else {
        div()
            .size(px(size_px))
            .flex_none()
            .rounded_full()
            .bg(rgba((p.accent << 8) | A_ACTIVE))
            .flex()
            .items_center()
            .justify_center()
            .text_color(rgb(p.fg))
            .child("\u{2630}")
            .into_any_element()
    }
}

/// Hover for 600 ms shows `text` under the pointer; leaving or pressing hides it.
pub(crate) fn with_tip<E: StatefulInteractiveElement>(el: E, text: impl Into<String>) -> E {
    let text = text.into();
    el.on_hover(move |hovered, window, cx| {
        if *hovered {
            let x: f32 = window.mouse_position().x.into();
            let screen_w: f32 = window.bounds().size.width.into(); // the bar spans the screen
            let token = TIP_TOKEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            let text = text.clone();
            cx.spawn(async move |cx| {
                cx.background_executor().timer(Duration::from_millis(600)).await;
                let _ = cx.update(|cx| {
                    if TIP_TOKEN.load(std::sync::atomic::Ordering::Relaxed) == token
                        && !PANELS_OPEN.load(std::sync::atomic::Ordering::Relaxed)
                    {
                        show_tip(cx, text, x, screen_w);
                    }
                });
            })
            .detach();
        } else {
            hide_tip(cx);
        }
    })
    .on_mouse_down(MouseButton::Left, |_, _, cx| hide_tip(cx))
}
