//! Music visualizer ("studio"): a thin spectrum strip above the taskbar while working (mini),
//! and a fullscreen view on dark glass over the desktop, the cover in the middle and a circular
//! spectrum around it (full). Audio comes from the default sink monitor via `parec`; the spectrum
//! is a plain windowed DFT over log-spaced bands, no extra dependency.

use std::{
    f32::consts::PI,
    io::Read,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::Mutex,
    time::Duration,
};

use gpui::{
    canvas, div, img, linear_color_stop, linear_gradient, layer_shell::*, point, prelude::*, px, rgb, rgba, size, AnyWindowHandle, App, Bounds,
    Context, FocusHandle, FontWeight, Global, KeyDownEvent, MouseButton, ObjectFit, PathBuilder, Pixels, Point, Window,
    WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions,
};

use crate::{home, palette, sh, Palette, BAR_HEIGHT, NF};

pub const BANDS: usize = 48;
const RATE: usize = 44100;
const N: usize = 1024; // DFT window: 43 Hz per bin
const HOP: usize = RATE / 60; // one spectrum per 1/60 s of audio
const F_LO: f32 = 50.;
const F_HI: f32 = 12000.;
// ponytail: fixed level mapping; tune these two if bars sit too low/high on your music
const FLOOR_DB: f32 = -62.; // this level and below draws as 0
const CEIL_DB: f32 = -14.; // this level and above draws full height (music bins rarely reach 0 dB)
const TILT_DB_PER_OCT: f32 = 3.; // lifts the treble, music energy falls ~3 dB per octave
// display easing per frame (~60 fps): how far the drawn level moves toward the new one
const RISE: f32 = 0.09;
const FALL: f32 = 0.035;
const MINI_H: f32 = 72.;

/// Latest band levels 0..1 (bass first), written by the capture thread.
static LEVELS: Mutex<[f32; BANDS]> = Mutex::new([0.; BANDS]);
static CAPTURE: Mutex<Option<Child>> = Mutex::new(None);

/// Band levels 0..1 of `samples` (mono, -1..1, len N), log-spaced from F_LO to F_HI.
pub fn bands(samples: &[f32]) -> [f32; BANDS] {
    let n = samples.len();
    let res = RATE as f32 / n as f32;
    let cos: Vec<f32> = (0..n).map(|i| (2. * PI * i as f32 / n as f32).cos()).collect();
    let sin: Vec<f32> = (0..n).map(|i| (2. * PI * i as f32 / n as f32).sin()).collect();
    let win: Vec<f32> = (0..n).map(|i| samples[i] * (0.5 - 0.5 * cos[i])).collect(); // Hann
    let mag = |k: usize| {
        let (mut re, mut im) = (0., 0.);
        for (i, s) in win.iter().enumerate() {
            let j = (k * i) % n;
            re += s * cos[j];
            im -= s * sin[j];
        }
        (re * re + im * im).sqrt() / (n as f32 / 4.) // a full-scale sine reads 1.0
    };
    let mut out = [0.; BANDS];
    for (b, o) in out.iter_mut().enumerate() {
        let f0 = F_LO * (F_HI / F_LO).powf(b as f32 / BANDS as f32);
        let f1 = F_LO * (F_HI / F_LO).powf((b + 1) as f32 / BANDS as f32);
        let k0 = ((f0 / res) as usize).max(1);
        let k1 = ((f1 / res).ceil() as usize).max(k0 + 1);
        let m = (k0..k1).map(mag).fold(0., f32::max);
        let db = 20. * m.max(1e-9).log10() + TILT_DB_PER_OCT * ((f0 * f1).sqrt() / 1000.).log2();
        *o = ((db - FLOOR_DB) / (CEIL_DB - FLOOR_DB)).clamp(0., 1.);
    }
    out
}

fn capture_start() {
    let mut cap = CAPTURE.lock().unwrap();
    if cap.is_some() {
        return;
    }
    let child = Command::new("parec")
        .args(["-d", "@DEFAULT_MONITOR@", "--format=s16le", "--rate=44100", "--channels=1", "--latency-msec=20"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else { return };
    let mut out = child.stdout.take().unwrap();
    *cap = Some(child);
    std::thread::spawn(move || {
        let mut ring = vec![0f32; N];
        let mut buf = vec![0u8; HOP * 2];
        while out.read_exact(&mut buf).is_ok() {
            ring.drain(..HOP);
            ring.extend(buf.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.));
            let now = bands(&ring);
            *LEVELS.lock().unwrap() = now;
        }
        *LEVELS.lock().unwrap() = [0.; BANDS]; // parec gone (killed or sound server restarted)
    });
}

fn capture_stop() {
    if let Some(mut c) = CAPTURE.lock().unwrap().take() {
        let _ = c.kill();
        let _ = c.wait();
    }
}

fn levels() -> [f32; BANDS] {
    *LEVELS.lock().unwrap()
}

/// Ease the drawn levels toward the latest spectrum: neighbours blended, slow rise, slower fall.
fn ease(shown: &mut [f32; BANDS], rise: f32, fall: f32) {
    let mut t = levels();
    for _ in 0..4 {
        let s = t;
        for i in 0..BANDS {
            t[i] = 0.25 * s[i.saturating_sub(1)] + 0.5 * s[i] + 0.25 * s[(i + 1).min(BANDS - 1)];
        }
    }
    for (v, t) in shown.iter_mut().zip(t) {
        *v += (t - *v) * if t > *v { rise } else { fall };
    }
}

/// Level for slot `i` of `2 * BANDS` going round: bass at both ends of the run, treble in the middle.
fn mirrored(lv: &[f32; BANDS], i: usize) -> f32 {
    lv[if i < BANDS { i } else { 2 * BANDS - 1 - i }]
}

/// Smooth curve through `pts` (quadratic segments between midpoints); `closed` joins the ends.
fn smooth(pb: &mut PathBuilder, pts: &[Point<Pixels>], closed: bool, start: bool) {
    let mid = |a: Point<Pixels>, b: Point<Pixels>| point((a.x + b.x) / 2., (a.y + b.y) / 2.);
    let n = pts.len();
    if closed {
        pb.move_to(mid(pts[n - 1], pts[0]));
        for i in 0..n {
            pb.curve_to(mid(pts[i], pts[(i + 1) % n]), pts[i]);
        }
        pb.close();
    } else {
        if start {
            pb.move_to(pts[0]);
        } else {
            pb.line_to(pts[0]);
        }
        for i in 1..n - 1 {
            pb.curve_to(mid(pts[i], pts[i + 1]), pts[i]);
        }
        pb.line_to(pts[n - 1]);
    }
}

// ---------- now playing + cover art ----------
#[derive(Clone, Default, PartialEq)]
struct Track {
    playing: bool,
    art: String,
    artist: String,
    title: String,
}

/// `playerctl metadata --format '{{status}}\t{{mpris:artUrl}}\t{{artist}}\t{{title}}'`
fn parse_track(line: &str) -> Option<Track> {
    let mut f = line.splitn(4, '\t');
    let (status, art, artist, title) = (f.next()?, f.next()?, f.next()?, f.next()?);
    Some(Track { playing: status == "Playing", art: art.into(), artist: artist.trim().into(), title: title.trim().into() })
}

/// `file:///a%20b.jpg` -> `/a b.jpg`
fn file_url_path(url: &str) -> Option<String> {
    let s = url.strip_prefix("file://")?.as_bytes();
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'%' && i + 2 < s.len() {
            if let Ok(b) = u8::from_str_radix(std::str::from_utf8(&s[i + 1..i + 3]).unwrap_or(""), 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(s[i]);
        i += 1;
    }
    // absolute only: the artUrl comes from any MPRIS player, never let it become a magick flag or coder ("-x", "msl:")
    String::from_utf8(out).ok().filter(|p| p.starts_with('/'))
}

#[derive(Clone)]
struct Cover {
    sharp: PathBuf,
    accent: Option<u32>,
}

/// Square cover and matugen accent for an artUrl, cached by URL hash.
fn load_cover(url: &str) -> Option<Cover> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut h);
    let dir = format!("{}/.cache/fiaboo-shell/viz", home());
    let key = format!("{dir}/{:016x}", h.finish());
    let (sharp, acc) = (format!("{key}.jpg"), format!("{key}.accent"));
    if !std::path::Path::new(&acc).exists() {
        let _ = std::fs::create_dir_all(&dir);
        let src = match file_url_path(url) {
            Some(p) => p,
            None if url.starts_with("http") => {
                let tmp = format!("{key}.src");
                Command::new("curl").args(["-sfL", "--max-time", "10", "-o", &tmp, url]).status().ok()?.success().then_some(tmp)?
            }
            None => return None,
        };
        let ok = Command::new("magick")
            .args([&src, "-resize", "600x600^", "-gravity", "center", "-extent", "600x600", &sharp])
            .status()
            .ok()?
            .success();
        if !ok {
            return None;
        }
        let json = sh(&format!("matugen image '{sharp}' --dry-run -j hex --prefer saturation 2>/dev/null"));
        let hex = serde_json::from_str::<serde_json::Value>(&json)
            .ok()
            .and_then(|v| v["colors"]["primary"]["dark"]["color"].as_str().map(String::from))
            .unwrap_or_default();
        let _ = std::fs::write(&acc, hex);
    }
    let accent = std::fs::read_to_string(&acc).ok().and_then(|h| u32::from_str_radix(h.trim().trim_start_matches('#'), 16).ok());
    Some(Cover { sharp: sharp.into(), accent })
}

// ---------- windows ----------
#[derive(Default)]
struct VizState {
    mini: Option<AnyWindowHandle>,
    full: Option<AnyWindowHandle>,
}
impl Global for VizState {}

fn state(cx: &mut App) -> &mut VizState {
    if !cx.has_global::<VizState>() {
        cx.set_global(VizState::default());
    }
    cx.global_mut::<VizState>()
}

fn layer_window<V: 'static + Render>(
    cx: &mut App,
    ns: &str,
    layer: Layer,
    anchor: Anchor,
    height: f32,
    keyboard: KeyboardInteractivity,
    build: impl FnOnce(&mut Window, &mut Context<V>) -> V + 'static,
) -> Option<AnyWindowHandle> {
    cx.open_window(
        WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(Bounds { origin: point(px(0.), px(0.)), size: size(px(0.), px(height)) })),
            app_id: Some(ns.to_string()),
            window_background: WindowBackgroundAppearance::Transparent,
            kind: WindowKind::LayerShell(LayerShellOptions {
                namespace: ns.to_string(),
                layer,
                anchor,
                exclusive_zone: (layer == Layer::Overlay).then_some(px(-1.)), // fullscreen covers the taskbar too
                keyboard_interactivity: keyboard,
                ..Default::default()
            }),
            ..Default::default()
        },
        |window, cx| cx.new(|cx| build(window, cx)),
    )
    .ok()
    .map(|h| h.into())
}

fn close(handle: Option<AnyWindowHandle>, cx: &mut App) {
    if let Some(h) = handle {
        let _ = h.update(cx, |_, window, _| window.remove_window());
    }
    let s = state(cx);
    if s.mini.is_none() && s.full.is_none() {
        capture_stop();
    }
}

/// Present while the strip is on, so it comes back after a shell restart or a new login.
fn mini_flag() -> String {
    format!("{}/.cache/fiaboo-shell/viz/mini-on", home())
}

/// Reopen the strip if it was on when the shell last stopped.
pub fn restore(cx: &mut App) {
    if std::path::Path::new(&mini_flag()).exists() && state(cx).mini.is_none() {
        toggle_mini(cx);
    }
}

/// `fiaboo-shell viz`: show/hide the strip above the taskbar.
pub fn toggle_mini(cx: &mut App) {
    if let Some(h) = state(cx).mini.take() {
        let _ = std::fs::remove_file(mini_flag());
        return close(Some(h), cx);
    }
    let _ = std::fs::create_dir_all(format!("{}/.cache/fiaboo-shell/viz", home()));
    let _ = std::fs::write(mini_flag(), "");
    capture_start();
    let h = layer_window(cx, "fiaboo-viz", Layer::Bottom, Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT, MINI_H, KeyboardInteractivity::None, |w, cx| {
        Mini::new(w, cx)
    });
    state(cx).mini = h;
}

/// `fiaboo-shell viz-full` (or double-click the strip): fullscreen studio; Esc leaves.
pub fn toggle_full(cx: &mut App) {
    if let Some(h) = state(cx).full.take() {
        return close(Some(h), cx);
    }
    capture_start();
    let h = layer_window(
        cx,
        "fiaboo-viz-full",
        Layer::Overlay,
        Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
        0.,
        KeyboardInteractivity::Exclusive,
        |w, cx| Full::new(w, cx),
    );
    state(cx).full = h;
}

/// Redraw at ~60 fps while the window lives.
fn tick<V: 'static>(window: &mut Window, cx: &mut Context<V>, mut every_second: impl FnMut(&mut V, &mut Context<V>) + 'static) {
    cx.spawn_in(window, async move |this, cx| {
        let mut n = 0u32;
        loop {
            cx.background_executor().timer(Duration::from_millis(16)).await;
            let alive = this
                .update(cx, |v, cx| {
                    if n % 60 == 0 {
                        every_second(v, cx);
                    }
                    cx.notify();
                })
                .is_ok();
            if !alive {
                break;
            }
            n = n.wrapping_add(1);
        }
    })
    .detach();
}

struct Mini {
    pal: Palette,
    shown: [f32; BANDS],
}

impl Mini {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        tick(window, cx, |m: &mut Mini, _| m.pal = palette());
        Mini { pal: palette(), shown: [0.; BANDS] }
    }
}

impl Render for Mini {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        ease(&mut self.shown, RISE, FALL);
        let (lv, accent) = (self.shown, self.pal.accent);
        div()
            .size_full()
            .on_mouse_down(MouseButton::Left, |ev, _, cx| {
                if ev.click_count == 2 {
                    toggle_full(cx);
                }
            })
            .child(
                canvas(
                    |_, _, _| (),
                    move |b, _, window, _| {
                        // one soft wave across the screen (fill only): treble at the edges, bass in the middle
                        let (x0, w) = (f32::from(b.origin.x), f32::from(b.size.width));
                        let bottom = f32::from(b.origin.y) + f32::from(b.size.height);
                        let n = BANDS * 2;
                        let pts: Vec<_> = (0..n)
                            .map(|i| {
                                let v = lv[if i < BANDS { BANDS - 1 - i } else { i - BANDS }];
                                point(px(x0 + w * i as f32 / (n - 1) as f32), px(bottom - 2. - v * (MINI_H - 10.)))
                            })
                            .collect();
                        let mut area = PathBuilder::fill();
                        area.move_to(point(px(x0), px(bottom)));
                        smooth(&mut area, &pts, false, false);
                        area.line_to(point(px(x0 + w), px(bottom)));
                        area.close();
                        if let Ok(p) = area.build() {
                            window.paint_path(
                                p,
                                linear_gradient(
                                    180.,
                                    linear_color_stop(rgba((accent << 8) | 0x10), 0.),
                                    linear_color_stop(rgba((accent << 8) | 0x48), 1.),
                                ),
                            );
                        }
                    },
                )
                .size_full(),
            )
    }
}

struct Full {
    pal: Palette,
    focus: FocusHandle,
    track: Track,
    cover: Option<Cover>,
    shown: [f32; BANDS],
    slow: [f32; BANDS], // lags behind `shown`, drawn as the faint outer halo
}

impl Full {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        tick(window, cx, |f: &mut Full, cx| {
            let line = sh("playerctl metadata --format '{{status}}\t{{mpris:artUrl}}\t{{artist}}\t{{title}}' 2>/dev/null");
            let t = parse_track(&line).unwrap_or_default();
            if t.art != f.track.art {
                f.cover = None;
                let url = t.art.clone();
                cx.spawn(async move |this, cx| {
                    let u = url.clone();
                    let cover = cx.background_executor().spawn(async move { load_cover(&u) }).await;
                    let _ = this.update(cx, |f, _| {
                        if f.track.art == url {
                            f.cover = cover;
                        }
                    });
                })
                .detach();
            }
            f.track = t;
        });
        Full { pal: palette(), focus, track: Track::default(), cover: None, shown: [0.; BANDS], slow: [0.; BANDS] }
    }
}

impl Render for Full {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.pal.clone();
        let accent = self.cover.as_ref().and_then(|c| c.accent).unwrap_or(p.accent);
        ease(&mut self.shown, RISE, FALL);
        ease(&mut self.slow, RISE / 3., FALL / 2.);
        let (lv, slow) = (self.shown, self.slow);
        const COVER: f32 = 300.;
        const RING: f32 = COVER + 2. * (14. + 120. + 8.); // cover + gap + widest halo, each side
        let center = match &self.cover {
            Some(c) => img(c.sharp.clone()).size(px(COVER)).rounded_full().object_fit(ObjectFit::Cover).into_any_element(),
            None => div()
                .size(px(COVER))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba((p.bg << 8) | 0xC0))
                .font_family(NF)
                .text_size(px(96.))
                .text_color(rgb(accent))
                .child("\u{f075a}")
                .into_any_element(),
        };
        let (title, artist) = if self.track.title.is_empty() {
            ("Nothing playing".to_string(), "Play something to see it here".to_string())
        } else {
            (self.track.title.clone(), self.track.artist.clone())
        };
        div()
            .size_full()
            .relative()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|_, ev: &KeyDownEvent, _, cx| match ev.keystroke.key.as_str() {
                "escape" => toggle_full(cx),
                "space" => crate::run_bg("playerctl play-pause"),
                "left" => crate::run_bg("playerctl previous"),
                "right" => crate::run_bg("playerctl next"),
                _ => {}
            }))
            .on_mouse_down(MouseButton::Right, |_, _, cx| toggle_full(cx))
            .bg(rgba(0x0000008c)) // dark glass: Hyprland's layer blur shows the desktop through it
            .child(
                div()
                    .absolute()
                    .size_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .child(
                        // ring and cover share one square so they stay concentric
                        div()
                            .relative()
                            .size(px(RING))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                canvas(
                    |_, _, _| (),
                    move |b, _, window, _| {
                        let (cx0, cy0) = (f32::from(b.origin.x) + f32::from(b.size.width) / 2., f32::from(b.origin.y) + f32::from(b.size.height) / 2.);
                        // a soft ring that breathes with the music: bass at the top, mirrored left/right
                        let n = BANDS * 2;
                        let r0 = COVER / 2. + 10.;
                        let ring = |lv: &[f32; BANDS], amp: f32| -> Vec<Point<Pixels>> {
                            (0..n)
                                .map(|i| {
                                    let a = -PI / 2. + i as f32 * 2. * PI / n as f32;
                                    let r = r0 + 4. + mirrored(lv, i) * amp;
                                    point(px(cx0 + r * a.cos()), px(cy0 + r * a.sin()))
                                })
                                .collect()
                        };
                        let halo = ring(&slow, 120.);
                        let main = ring(&lv, 80.);
                        for (pts, alpha) in [(&halo, 0x0c), (&main, 0x16)] {
                            let mut pb = PathBuilder::fill();
                            smooth(&mut pb, pts, true, true);
                            if let Ok(p) = pb.build() {
                                window.paint_path(p, rgba((accent << 8) | alpha));
                            }
                        }
                        let mut edge = PathBuilder::stroke(px(2.));
                        smooth(&mut edge, &main, true, true);
                        if let Ok(p) = edge.build() {
                            window.paint_path(p, rgba((accent << 8) | 0xd8));
                        }
                    },
                )
                                .absolute()
                                .size_full(),
                            )
                            .child(div().id("viz-cover").cursor_pointer().on_click(|_, _, _| crate::run_bg("playerctl play-pause")).child(center)),
                    )
                    .child(
                        div()
                            .max_w(px(900.))
                            .text_size(px(28.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(0xffffff))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(title),
                    )
                    .child(div().mt_1().text_size(px(17.)).text_color(rgba(0xffffffb0)).child(artist)),
            )
            .child(
                div()
                    .absolute()
                    .bottom(px(BAR_HEIGHT / 2.))
                    .w_full()
                    .flex()
                    .justify_center()
                    .text_size(px(12.))
                    .text_color(rgba(0xffffff80))
                    .child("Space: play/pause   ·   ← →: previous/next   ·   Esc: back"),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sine_peaks_in_its_band() {
        let tone = |f: f32| (0..N).map(|i| (2. * PI * f * i as f32 / RATE as f32).sin() * 0.5).collect::<Vec<_>>();
        let peak = |v: [f32; BANDS]| (0..BANDS).max_by(|&a, &b| v[a].total_cmp(&v[b])).unwrap();
        let (lo, hi) = (peak(bands(&tone(100.))), peak(bands(&tone(5000.))));
        assert!(lo < 10 && hi > 35, "100 Hz -> band {lo}, 5 kHz -> band {hi}");
        assert!(bands(&vec![0.; N]).iter().all(|&v| v == 0.));
    }
    #[test]
    fn track_and_file_url() {
        let t = parse_track("Playing\tfile:///tmp/a%20b.jpg\tArtist\tSong").unwrap();
        assert!(t.playing && t.title == "Song" && t.artist == "Artist");
        assert_eq!(file_url_path(&t.art).as_deref(), Some("/tmp/a b.jpg"));
        assert_eq!(file_url_path("https://x/y.jpg"), None);
        assert_eq!(file_url_path("file://-density"), None);
    }
}
