//! Music visualizer: two crossing waves inside the taskbar, on by itself while a player plays.
//! Audio comes from the default sink monitor via `parec`; the spectrum is a plain windowed DFT over
//! log-spaced bands, no extra dependency.

use std::{
    f32::consts::PI,
    io::{BufRead, BufReader, Read},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    time::Duration,
};

use gpui::{canvas, div, linear_color_stop, linear_gradient, point, prelude::*, px, rgba, App, Context, Entity, Global, PathBuilder, Window};

use crate::{home, palette, Palette};

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
    // a parec that died or never started is retried every 2 s, not on every 16 ms frame
    static FAILED_AT: Mutex<Option<std::time::Instant>> = Mutex::new(None);
    let mut failed_at = FAILED_AT.lock().unwrap();
    if let Some(c) = cap.as_mut() {
        if !matches!(c.try_wait(), Ok(Some(_))) {
            return;
        }
        *cap = None; // exited by itself (sound server restarted): reaped, capture again
        *failed_at = Some(std::time::Instant::now());
    }
    if failed_at.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(2)) {
        return;
    }
    let child = Command::new("parec")
        .args(["-d", "@DEFAULT_MONITOR@", "--format=s16le", "--rate=44100", "--channels=1", "--latency-msec=20"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else {
        *failed_at = Some(std::time::Instant::now());
        return;
    };
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

// ---------- the wave in the taskbar ----------
/// Whether a player is playing, kept current by `playerctl --follow` (no polling).
static PLAYING: AtomicBool = AtomicBool::new(false);

fn start_player_watch() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        std::thread::spawn(|| loop {
            if let Ok(mut child) = Command::new("playerctl").args(["--follow", "status"]).stdout(Stdio::piped()).stderr(Stdio::null()).spawn() {
                if let Some(out) = child.stdout.take() {
                    for line in BufReader::new(out).lines().map_while(Result::ok) {
                        PLAYING.store(line.trim() == "Playing", Ordering::Relaxed);
                    }
                }
                let _ = child.wait();
            }
            PLAYING.store(false, Ordering::Relaxed);
            std::thread::sleep(Duration::from_secs(5)); // playerctl gone: retry
        });
    });
}

#[derive(Default)]
struct VizState {
    mini: Option<Entity<Mini>>, // drawn inside the taskbar, behind its icons
}
impl Global for VizState {}

fn state(cx: &mut App) -> &mut VizState {
    if !cx.has_global::<VizState>() {
        cx.set_global(VizState::default());
    }
    cx.global_mut::<VizState>()
}

/// Present when the wave was switched off, so it stays off after a shell restart or a new login.
fn off_flag() -> String {
    format!("{}/.cache/fiaboo-shell/viz/mini-off", home())
}

/// Turn the wave on at startup unless it was switched off.
pub fn restore(cx: &mut App) {
    if !std::path::Path::new(&off_flag()).exists() && state(cx).mini.is_none() {
        toggle_mini(cx);
    }
}

/// `fiaboo-shell viz`: switch the taskbar wave on/off (while on, it shows whenever music plays).
pub fn toggle_mini(cx: &mut App) {
    if state(cx).mini.take().is_some() {
        let _ = std::fs::create_dir_all(format!("{}/.cache/fiaboo-shell/viz", home()));
        let _ = std::fs::write(off_flag(), "");
        capture_stop();
        cx.refresh_windows();
        return;
    }
    let _ = std::fs::remove_file(off_flag());
    start_player_watch();
    let m = cx.new(Mini::new);
    state(cx).mini = Some(m);
    cx.refresh_windows();
}

/// The taskbar wave while it is on, for the bar to draw behind its contents.
pub fn mini(cx: &App) -> Option<Entity<Mini>> {
    cx.try_global::<VizState>().and_then(|s| s.mini.clone())
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
    pal: Palette, // the wave takes its colours from the wallpaper (matugen)
    shown: [f32; BANDS],
    drive: Drive,
    bass: f32,  // eased bass that bends the waves
    alpha: f32, // eased loudness: the strip fades out in silence
    drawn: bool, // the last redraw showed something
    start: std::time::Instant,
}

impl Mini {
    fn new(cx: &mut Context<Self>) -> Self {
        tick(cx, |m: &mut Mini, n| {
            if n % 120 == 0 {
                m.pal = palette(); // picks up a new wallpaper within ~2 s
            }
            m.step()
        });
        Mini { pal: palette(), shown: [0.; BANDS], drive: Drive::default(), bass: 0., alpha: 0., drawn: false, start: std::time::Instant::now() }
    }

    /// Follow the music; true when the bar must redraw (in silence it stays still: whole-bar redraws cost ~8 % CPU).
    fn step(&mut self) -> bool {
        // audio capture only while something plays (and until the wave has faded out)
        if PLAYING.load(Ordering::Relaxed) {
            capture_start();
        } else if self.alpha < 0.01 {
            capture_stop();
        }
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
        let (c_left, c_mid, c_right) = (self.pal.accent, self.pal.fg, self.pal.accent2);
        div()
            .size_full()
            .child(
                canvas(
                    |_, _, _| (),
                    move |b, _, window, _| {
                        if alpha < 0.01 {
                            return;
                        }
                        const STEP: f32 = 8.;
                        let (x0, w) = (f32::from(b.origin.x), f32::from(b.size.width));
                        let h = f32::from(b.size.height);
                        let mid = f32::from(b.origin.y) + h / 2.;
                        let amp = h * 0.18; // ~9 px each way on the 48 px bar
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
                            window.paint_path(p, rgba((c_left << 8) | a(48.)));
                        }
                        // the lines: a ribbon `half` px thick each side, thinning over the last 100 px at the ends;
                        // wide faint ribbons under the core fake the glow. Two halves: wallpaper primary -> foreground -> tertiary.
                        let taper = |x: f32| smoothstep(0., 100., x - x0) * smoothstep(0., 100., x0 + w - x);
                        let half_n = wave.len() / 2;
                        for (half, v) in [(4., 16.), (2.5, 40.), (1., 230.)] {
                            for top in [true, false] {
                                for (part, from, to) in [(&wave[..=half_n], c_left, c_mid), (&wave[half_n..], c_mid, c_right)] {
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
}
