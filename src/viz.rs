//! Music visualizer: two crossing waves inside the taskbar while working (mini), and a fullscreen
//! shader view (full: `fiaboo-wave` draws the shader, this window the title on top). Audio comes from the default sink monitor via `parec`; the spectrum
//! is a plain windowed DFT over log-spaced bands, no extra dependency.

use std::{
    f32::consts::PI,
    io::Read,
    process::{Child, Command, Stdio},
    sync::Mutex,
    time::Duration,
};

use gpui::{
    canvas, div, linear_color_stop, linear_gradient, layer_shell::*, point, prelude::*, px, rgb, rgba, size, AnyWindowHandle, App, Bounds,
    Context, Entity, FocusHandle, FontWeight, Global, KeyDownEvent, MouseButton, PathBuilder, Window,
    WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions,
};

use crate::{home, sh, BAR_HEIGHT};

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

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0., 1.);
    t * t * (3. - 2. * t)
}

/// Spreads a level around its last ~3 s (mean ± 2 deviations -> 0..1), so any song at any volume
/// moves the visuals alike; fixed thresholds sat at the top for every real track. Measured on an
/// EDM and a ballad track, also 12 dB quieter: p10 0.15, p50 0.56, p90 0.76.
#[derive(Default)]
struct Norm {
    mean: f32,
    var: f32,
}

impl Norm {
    fn map(&mut self, x: f32) -> f32 {
        const K: f32 = 1. / 180.; // per frame at ~60 fps
        self.mean += (x - self.mean) * K;
        self.var += ((x - self.mean).powi(2) - self.var) * K;
        (0.5 + (x - self.mean) / (4. * self.var.sqrt().max(0.02))).clamp(0., 1.)
    }
}

#[derive(Default)]
struct Drive {
    bass: Norm,
    loud: Norm,
}

impl Drive {
    /// (bass, loudness, on) from the eased levels: the first two spread over 0..1 by `Norm`,
    /// `on` goes 0 -> 1 with any sound at all (silence reads exactly 0).
    fn update(&mut self, lv: &[f32; BANDS]) -> (f32, f32, f32) {
        let bass = lv[..8].iter().sum::<f32>() / 8.;
        let loud = lv.iter().sum::<f32>() / BANDS as f32;
        (self.bass.map(bass), self.loud.map(loud), smoothstep(0.02, 0.1, loud))
    }
}

/// Two crossing waves in the style of WayVes' logo: (top, bottom) offsets from the centre line at
/// `x` (0..1) for bass `s` (0..1) and phase `t`; flat in the middle 20 %, fading out towards both ends.
fn wayve(x: f32, s: f32, t: f32, amp: f32) -> (f32, f32) {
    let top = amp * (0.03 * (x * 500. * (1. + 0.5 * s) + 1000. * s + t)).sin();
    let bottom = 0.77 * amp * (0.03 * (x * 400. * (1. + 0.5 * s) + 400. * s - 12. + 2. * t)).sin();
    let e = (((x * 2. - 1.).abs() - 0.2) / 0.8).max(0.);
    let env = (-5. * e * e).exp();
    (top * env, bottom * env)
}

// ---------- now playing ----------
#[derive(Clone, Debug, Default, PartialEq)]
struct Track {
    artist: String,
    title: String,
}

/// `playerctl metadata --format '{{artist}}\t{{title}}'`
fn parse_track(line: &str) -> Option<Track> {
    let (artist, title) = line.split_once('\t')?;
    Some(Track { artist: artist.trim().into(), title: title.trim().into() })
}

// ---------- fiaboo-wave: the fullscreen shader, a separate process (GPUI has no custom shaders) ----------
static WAVE: Mutex<Option<Child>> = Mutex::new(None);

/// Start `fiaboo-wave` (next to this binary); the receiver fires once its first frame is up.
fn wave_start() -> Option<std::sync::mpsc::Receiver<()>> {
    use std::io::BufRead;
    let exe = std::env::current_exe().ok()?.with_file_name("fiaboo-wave");
    let mut child = Command::new(exe).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let out = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        if std::io::BufReader::new(out).read_line(&mut line).is_ok() && line.trim() == "ready" {
            let _ = tx.send(());
        }
    });
    *WAVE.lock().unwrap() = Some(child);
    Some(rx)
}

fn wave_send(bass: f32, loud: f32) {
    use std::io::Write;
    if let Some(stdin) = WAVE.lock().unwrap().as_mut().and_then(|c| c.stdin.as_mut()) {
        let _ = writeln!(stdin, "{bass:.3} {loud:.3}");
    }
}

fn wave_stop() {
    if let Some(mut c) = WAVE.lock().unwrap().take() {
        let _ = c.kill();
        let _ = c.wait();
    }
}

// ---------- windows ----------
#[derive(Default)]
struct VizState {
    mini: Option<Entity<Mini>>, // drawn inside the taskbar, behind its icons
    full: Option<AnyWindowHandle>,
    opening: bool, // waiting for fiaboo-wave's first frame
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

/// Close the fullscreen view from inside its own handlers: `handle.update` on the window being
/// updated fails silently, which left the view stuck on screen and opened a new one per toggle.
fn close_full_here(window: &mut Window, cx: &mut App) {
    state(cx).full = None;
    window.remove_window();
    close(None, cx);
}

fn close(handle: Option<AnyWindowHandle>, cx: &mut App) {
    if let Some(h) = handle {
        let _ = h.update(cx, |_, window, _| window.remove_window());
    }
    let s = state(cx);
    if s.full.is_none() {
        wave_stop();
    }
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

/// `fiaboo-shell viz`: show/hide the wave inside the taskbar.
pub fn toggle_mini(cx: &mut App) {
    if state(cx).mini.take().is_some() {
        let _ = std::fs::remove_file(mini_flag());
        cx.refresh_windows();
        return close(None, cx);
    }
    let _ = std::fs::create_dir_all(format!("{}/.cache/fiaboo-shell/viz", home()));
    let _ = std::fs::write(mini_flag(), "");
    capture_start();
    let m = cx.new(Mini::new);
    state(cx).mini = Some(m);
    cx.refresh_windows();
}

/// The taskbar wave while it is on, for the bar to draw behind its contents.
pub fn mini(cx: &App) -> Option<Entity<Mini>> {
    cx.try_global::<VizState>().and_then(|s| s.mini.clone())
}

/// `fiaboo-shell viz-full` (or double-click the strip): fullscreen studio; Esc leaves.
pub fn toggle_full(cx: &mut App) {
    if let Some(h) = state(cx).full.take() {
        return close(Some(h), cx);
    }
    if state(cx).opening {
        return;
    }
    state(cx).opening = true;
    capture_start();
    let ready = wave_start();
    cx.spawn(async move |cx| {
        // open the title layer only once the shader is on screen, so it maps above it
        let shader = match ready {
            Some(rx) => cx.background_executor().spawn(async move { rx.recv_timeout(Duration::from_secs(3)).is_ok() }).await,
            None => false,
        };
        let _ = cx.update(|cx| {
            if !shader {
                wave_stop(); // no GPU or no layer shell: the view falls back to dark glass
            }
            let h = layer_window(
                cx,
                "fiaboo-viz-full",
                Layer::Overlay,
                Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                0.,
                KeyboardInteractivity::Exclusive,
                move |w, cx| Full::new(w, cx, shader),
            );
            let s = state(cx);
            s.opening = false;
            s.full = h;
            if h.is_none() {
                close(None, cx);
            }
        });
    })
    .detach();
}

/// Run `frame` at ~60 fps while the view lives; it returns whether the view needs a redraw.
fn tick<V: 'static>(cx: &mut Context<V>, mut frame: impl FnMut(&mut V, u32) -> bool + 'static) {
    cx.spawn(async move |this, cx| {
        let mut n = 0u32;
        loop {
            cx.background_executor().timer(Duration::from_millis(16)).await;
            let alive = this
                .update(cx, |v, cx| {
                    if frame(v, n) {
                        cx.notify();
                    }
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

pub struct Mini {
    shown: [f32; BANDS],
    drive: Drive,
    bass: f32,  // eased bass that bends the waves
    alpha: f32, // eased loudness: the strip fades out in silence
    drawn: bool, // the last redraw showed something
    start: std::time::Instant,
}

impl Mini {
    fn new(cx: &mut Context<Self>) -> Self {
        tick(cx, |m: &mut Mini, _| m.step());
        Mini { shown: [0.; BANDS], drive: Drive::default(), bass: 0., alpha: 0., drawn: false, start: std::time::Instant::now() }
    }

    /// Follow the music; true when the bar must redraw (in silence it stays still: whole-bar redraws cost ~8 % CPU).
    fn step(&mut self) -> bool {
        ease(&mut self.shown, RISE, FALL);
        let (bass, _, on) = self.drive.update(&self.shown);
        // bass moves the phase a lot (up to ~30 rad), so it gets its own slow easing
        self.bass += (bass - self.bass) * 0.04;
        // at most 0.75: it sits behind the taskbar icons
        self.alpha += (0.75 * on - self.alpha) * 0.03;
        let visible = self.alpha >= 0.01;
        let redraw = visible || self.drawn; // one more frame to clear the last faint wave
        self.drawn = visible;
        redraw
    }
}

impl Render for Mini {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let (s, alpha, t) = (self.bass, self.alpha, self.start.elapsed().as_secs_f32() * 20.);
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
                        if alpha < 0.01 {
                            return;
                        }
                        const CYAN: u32 = 0x00fff2;
                        const WHITE: u32 = 0xffffff;
                        const MAGENTA: u32 = 0xf700ff;
                        const STEP: f32 = 4.;
                        let (x0, w) = (f32::from(b.origin.x), f32::from(b.size.width));
                        let h = f32::from(b.size.height);
                        let mid = f32::from(b.origin.y) + h / 2.;
                        let amp = (h - 12.) * 7. / 16.;
                        let n = (w / STEP) as usize + 1;
                        // (x, top y, bottom y) along the screen
                        let wave: Vec<(f32, f32, f32)> = (0..=n)
                            .map(|i| {
                                let x = (i as f32 * STEP).min(w);
                                let (a, b) = wayve(x / w, s, t, amp);
                                (x0 + x, mid - a, mid - b)
                            })
                            .collect();
                        let a = |v: f32| ((v * alpha) as u32).min(0xff);
                        // the blue body between the two waves
                        let mut pb = PathBuilder::fill();
                        let body: Vec<_> = wave.iter().map(|p| point(px(p.0), px(p.1))).chain(wave.iter().rev().map(|p| point(px(p.0), px(p.2)))).collect();
                        pb.add_polygon(&body, true);
                        if let Ok(p) = pb.build() {
                            window.paint_path(p, rgba(0x004cff00 | a(48.)));
                        }
                        // the lines: a ribbon `half` px thick each side, thinning over the last 100 px at the ends;
                        // wide faint ribbons under the core fake the glow. Two halves: cyan -> white -> magenta.
                        let taper = |x: f32| smoothstep(0., 100., x - x0) * smoothstep(0., 100., x0 + w - x);
                        let half_n = wave.len() / 2;
                        for (half, v) in [(7., 20.), (4., 48.), (1.5, 255.)] {
                            for top in [true, false] {
                                for (part, from, to) in [(&wave[..=half_n], CYAN, WHITE), (&wave[half_n..], WHITE, MAGENTA)] {
                                    let y = |p: &(f32, f32, f32)| if top { p.1 } else { p.2 };
                                    let pts: Vec<_> = part
                                        .iter()
                                        .map(|p| point(px(p.0), px(y(p) - half * taper(p.0))))
                                        .chain(part.iter().rev().map(|p| point(px(p.0), px(y(p) + half * taper(p.0)))))
                                        .collect();
                                    let mut pb = PathBuilder::fill();
                                    pb.add_polygon(&pts, true);
                                    if let Ok(p) = pb.build() {
                                        window.paint_path(
                                            p,
                                            linear_gradient(90., linear_color_stop(rgba((from << 8) | a(v)), 0.), linear_color_stop(rgba((to << 8) | a(v)), 1.)),
                                        );
                                    }
                                }
                            }
                        }
                    },
                )
                .size_full(),
            )
    }
}

struct Full {
    focus: FocusHandle,
    track: Track,
    shown: [f32; BANDS],
    drive: Drive,
    shader: bool, // fiaboo-wave runs below; otherwise plain dark glass
}

impl Full {
    fn new(window: &mut Window, cx: &mut Context<Self>, shader: bool) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        // the levels go to fiaboo-wave every frame; this layer only redraws when the track changes
        tick(cx, |f: &mut Full, n| {
            ease(&mut f.shown, RISE, FALL);
            let (bass, loud, on) = f.drive.update(&f.shown);
            wave_send(bass * on, loud * on);
            if n % 60 != 0 {
                return false;
            }
            let line = sh("playerctl metadata --format '{{artist}}\t{{title}}' 2>/dev/null");
            let t = parse_track(&line).unwrap_or_default();
            let changed = t != f.track;
            f.track = t;
            changed
        });
        Full { focus, track: Track::default(), shown: [0.; BANDS], drive: Drive::default(), shader }
    }
}

impl Render for Full {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (title, artist) = if self.track.title.is_empty() {
            ("Nothing playing".to_string(), "Play something to see it here".to_string())
        } else {
            (self.track.title.clone(), self.track.artist.clone())
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_end()
            .pb(px(BAR_HEIGHT / 2.))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|_, ev: &KeyDownEvent, window, cx| match ev.keystroke.key.as_str() {
                "escape" => close_full_here(window, cx),
                "space" => crate::run_bg("playerctl play-pause"),
                "left" => crate::run_bg("playerctl previous"),
                "right" => crate::run_bg("playerctl next"),
                _ => {}
            }))
            .on_mouse_down(MouseButton::Right, |_, window, cx| close_full_here(window, cx))
            // dark glass only without the shader: Hyprland's layer blur shows the desktop through it
            .when(!self.shader, |d| d.bg(rgba(0x0000008c)))
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
            .child(div().mt_1().text_size(px(17.)).text_color(rgba(0xffffffb0)).child(artist))
            .child(
                div()
                    .mt(px(48.))
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
    fn wayve_tapers_to_nothing_at_the_ends() {
        for s in [0., 0.5, 1.] {
            let (a, b) = wayve(0., s, 3., 26.);
            let (c, d) = wayve(1., s, 3., 26.);
            assert!([a, b, c, d].iter().all(|v| v.abs() < 0.2), "edges {a} {b} {c} {d}");
        }
        let peak = (0..=100).map(|i| wayve(i as f32 / 100., 0.3, 0., 26.).0.abs()).fold(0., f32::max);
        assert!(peak > 15., "middle peak {peak}");
    }
    #[test]
    fn norm_spreads_around_recent_level() {
        let mut n = Norm::default();
        for _ in 0..2000 {
            n.map(0.8);
        }
        assert!((n.map(0.8) - 0.5).abs() < 0.05, "steady level sits in the middle");
        assert!(n.map(0.9) > 0.9, "a jump above it reads high");
        assert!(n.map(0.6) < 0.1, "a drop below it reads low");
    }
    #[test]
    fn track_line() {
        let t = parse_track("Artist\tSong").unwrap();
        assert!(t.title == "Song" && t.artist == "Artist");
        assert_eq!(parse_track(""), None);
    }
}
