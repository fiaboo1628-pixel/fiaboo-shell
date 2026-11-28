//! Volume OSD: `pactl subscribe` watcher and the OSD popup.
use crate::commons::theme::{A_ACTIVE, A_BORDER, NF, Palette, palette, PANEL_FILL};
use crate::commons::util::die_with_shell;
use crate::services::system::{read_mic, read_volume};
use gpui::{
    App, Bounds, Context, div, FontWeight, Global, point, px, rgb, rgba, size, Window,
    WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions,
};
use gpui::layer_shell::{Anchor, KeyboardInteractivity, Layer, LayerShellOptions};
use gpui::prelude::*;
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

// ---------- volume OSD ----------
pub(crate) static OSD_PENDING: Mutex<Option<(f32, bool)>> = Mutex::new(None);

/// Follow `pactl subscribe`: when the default sink's volume or mute state changes (media keys,
/// quick settings, apps), queue an OSD; when a recording stream changes, recheck the mic
/// indicator. Event-driven, no polling.
pub(crate) fn start_volume_watch() {
    use std::io::{BufRead, BufReader};
    std::thread::spawn(|| loop {
        let mut last = read_volume();
        if let Ok(mut child) = die_with_shell(Command::new("pactl").arg("subscribe")).stdout(std::process::Stdio::piped()).spawn() {
            read_mic();
            if let Some(out) = child.stdout.take() {
                for line in BufReader::new(out).lines().map_while(Result::ok) {
                    if line.contains("on source-output #") {
                        read_mic();
                    }
                    if line.contains("on sink #") {
                        let v = read_volume();
                        if v != last {
                            last = v;
                            *OSD_PENDING.lock().unwrap() = Some(v);
                        }
                    }
                }
            }
            let _ = child.wait();
        }
        std::thread::sleep(Duration::from_secs(5)); // pipewire restarted: resubscribe
    });
}

pub(crate) struct OsdState(pub(crate) Option<(gpui::WindowHandle<Osd>, std::time::Instant)>);
impl Global for OsdState {}

pub(crate) struct Osd {
    pub(crate) pal: Palette,
    pub(crate) vol: f32,
    pub(crate) muted: bool,
}

impl Render for Osd {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.pal.clone();
        let pct = (self.vol * 100.).round() as i32;
        let frac = if self.muted { 0. } else { self.vol.clamp(0., 1.) };
        let glyph = if self.muted || pct == 0 { "\u{f0581}" } else if pct < 34 { "\u{f057f}" } else if pct < 67 { "\u{f0580}" } else { "\u{f057e}" };
        div()
            .font_family(NF).font_weight(FontWeight::MEDIUM)
            .size_full()
            .flex()
            .items_center()
            .gap_3()
            .px_4()
            .rounded_2xl()
            .bg(rgba((p.bg << 8) | PANEL_FILL))
            .border_1()
            .border_color(rgba((p.fg << 8) | A_BORDER))
            .text_color(rgb(p.fg))
            .child(div().font_family(NF).font_weight(FontWeight::MEDIUM).text_size(px(20.)).w(px(24.)).child(glyph))
            .child(
                div()
                    .flex_1()
                    .h(px(6.))
                    .rounded_full()
                    .bg(rgba((p.dim << 8) | A_ACTIVE))
                    .child(div().h_full().w(gpui::relative(frac)).rounded_full().bg(rgb(p.accent))),
            )
            .child(
                div()
                    .w(px(44.))
                    .flex()
                    .justify_end()
                    .text_size(px(14.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(if self.muted { "Muted".to_string() } else { format!("{pct}%") }),
            )
    }
}

/// Show or refresh the OSD; it hides itself 1.5 s after the last change (see hide_osd_if_due).
pub(crate) fn show_osd(cx: &mut App, vol: f32, muted: bool) {
    let until = std::time::Instant::now() + Duration::from_millis(1500);
    if let Some((h, _)) = cx.try_global::<OsdState>().and_then(|s| s.0) {
        if h.update(cx, |o, _, cx| { o.vol = vol; o.muted = muted; cx.notify(); }).is_ok() {
            cx.set_global(OsdState(Some((h, until))));
            return;
        }
    }
    let opened = cx.open_window(
        WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(Bounds { origin: point(px(0.), px(0.)), size: size(px(320.), px(52.)) })),
            app_id: Some("fiaboo-osd".to_string()),
            window_background: WindowBackgroundAppearance::Transparent,
            kind: WindowKind::LayerShell(LayerShellOptions {
                namespace: "fiaboo-osd".to_string(),
                layer: Layer::Overlay,
                anchor: Anchor::BOTTOM,
                margin: Some((px(0.), px(0.), px(16.), px(0.))),
                keyboard_interactivity: KeyboardInteractivity::None,
                ..Default::default()
            }),
            ..Default::default()
        },
        move |_, cx| cx.new(|_| Osd { pal: palette(), vol, muted }),
    );
    cx.set_global(OsdState(opened.ok().map(|h| (h, until))));
}

pub(crate) fn hide_osd_if_due(cx: &mut App) {
    if let Some((h, until)) = cx.try_global::<OsdState>().and_then(|s| s.0) {
        if std::time::Instant::now() >= until {
            let _ = h.update(cx, |_, window, _| window.remove_window());
            cx.set_global(OsdState(None));
        }
    }
}
