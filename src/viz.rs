//! Music line: while a player plays, a thin line on the taskbar's top edge grows out from the
//! middle with the loudness, a kick pushes it a little further, and its ends melt into the bar.
//! Audio comes from the default sink monitor via `parec`; only loudness and bass are measured.
//! The line lives in its own small layer surface on top of the bar, so the bar never
//! redraws for it, and the surface exists only while there is something to show.

use std::{
    io::{BufRead, BufReader, Read},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};

use gpui::{
    div, layer_shell::*, linear_color_stop, linear_gradient, point, prelude::*, px, relative, rgba, size, App, Bounds,
    Context, Global, Window, WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind, WindowOptions,
};

use crate::{home, palette, Palette};

const RATE: usize = 44100;
const FPS: u64 = 30;
const HOP: usize = RATE / FPS as usize; // one measurement per frame of audio
const STRIP: f32 = 8.; // surface height: the line on top, its glow spilling down into the bar

/// (loudness dB, bass dB) of the latest hop, written by the capture thread.
static LEVEL: Mutex<(f32, f32)> = Mutex::new((-120., -120.));
static CAPTURE: Mutex<Option<Child>> = Mutex::new(None);

/// RMS loudness and bass loudness (one-pole low-pass at ~150 Hz) of `samples`, in dBFS.
/// `lp` carries the filter state from one hop to the next.
fn analyze(samples: &[f32], lp: &mut f32) -> (f32, f32) {
    const A: f32 = 0.021; // 1 - exp(-2π·150/44100)
    let (mut all, mut low) = (0f32, 0f32);
    for &x in samples {
        *lp += A * (x - *lp);
        all += x * x;
        low += *lp * *lp;
    }
    let db = |e: f32| 10. * (e / samples.len().max(1) as f32).max(1e-12).log10();
    (db(all), db(low))
}

fn capture_start() {
    let mut cap = CAPTURE.lock().unwrap();
    // a parec that died or never started is retried every 2 s, not on every frame
    static FAILED_AT: Mutex<Option<Instant>> = Mutex::new(None);
    let mut failed_at = FAILED_AT.lock().unwrap();
    if let Some(c) = cap.as_mut() {
        if !matches!(c.try_wait(), Ok(Some(_))) {
            return;
        }
        *cap = None; // exited by itself (sound server restarted): reaped, capture again
        *failed_at = Some(Instant::now());
    }
    if failed_at.is_some_and(|t| t.elapsed() < Duration::from_secs(2)) {
        return;
    }
    let child = Command::new("parec")
        .args(["-d", "@DEFAULT_MONITOR@", "--format=s16le", "--rate=44100", "--channels=1", "--latency-msec=30"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else {
        *failed_at = Some(Instant::now());
        return;
    };
    let mut out = child.stdout.take().unwrap();
    *cap = Some(child);
    std::thread::spawn(move || {
        let mut buf = vec![0u8; HOP * 2];
        let mut samples = vec![0f32; HOP];
        let mut lp = 0.;
        while out.read_exact(&mut buf).is_ok() {
            for (s, c) in samples.iter_mut().zip(buf.chunks_exact(2)) {
                *s = i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.;
            }
            *LEVEL.lock().unwrap() = analyze(&samples, &mut lp);
        }
        *LEVEL.lock().unwrap() = (-120., -120.); // parec gone (killed or sound server restarted)
    });
}

fn capture_stop() {
    if let Some(mut c) = CAPTURE.lock().unwrap().take() {
        let _ = c.kill();
        let _ = c.wait();
    }
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0., 1.);
    t * t * (3. - 2. * t)
}

/// Spreads a level around its last ~3 s (mean ± 2 deviations -> 0..1), so any song at any volume
/// moves the line alike; fixed thresholds sat at the top for every real track.
#[derive(Default)]
struct Norm {
    mean: f32,
    var: f32,
}

impl Norm {
    fn map(&mut self, x: f32) -> f32 {
        const K: f32 = 1. / 90.; // per frame at 30 fps: ~3 s memory
        self.mean += (x - self.mean) * K;
        self.var += ((x - self.mean).powi(2) - self.var) * K;
        (0.5 + (x - self.mean) / (4. * self.var.sqrt().max(0.02))).clamp(0., 1.)
    }
}

/// Eased values the line is drawn from, advanced once per frame.
#[derive(Default)]
struct Motion {
    loud: Norm,
    bass: Norm,
    level: f32, // 0..1: how far the line reaches out
    kick: f32,  // 0..1: short push on a bass hit
    alpha: f32, // 0..1: fades in with sound, out in silence
}

impl Motion {
    fn step(&mut self, (db, bass_db): (f32, f32)) {
        let on = smoothstep(-58., -46., db); // silence (or a paused player) reads exactly 0
        let loud = self.loud.map(((db + 60.) / 60.).clamp(0., 1.));
        let bass = self.bass.map(((bass_db + 60.) / 60.).clamp(0., 1.));
        // soft and slow, like the rest of the shell: quick enough to follow the music, never twitchy
        let target = on * (0.3 + 0.7 * loud);
        self.level += (target - self.level) * if target > self.level { 0.22 } else { 0.07 };
        let hit = on * ((bass - 0.6) / 0.4).clamp(0., 1.);
        self.kick += (hit - self.kick) * if hit > self.kick { 0.55 } else { 0.12 };
        self.alpha += (on - self.alpha) * 0.06;
    }
}

// ---------- player state ----------
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

// ---------- on/off ----------
#[derive(Default)]
struct VizState {
    on: bool,
    run: u64, // bumped on every switch, so a stale controller loop ends itself
}
impl Global for VizState {}

/// Present when the line was switched off, so it stays off after a shell restart or a new login.
fn off_flag() -> String {
    format!("{}/.cache/fiaboo-shell/viz/mini-off", home())
}

/// Turn the line on at startup unless it was switched off.
pub fn restore(cx: &mut App) {
    if !std::path::Path::new(&off_flag()).exists() && !cx.try_global::<VizState>().is_some_and(|s| s.on) {
        toggle_mini(cx);
    }
}

/// `fiaboo-shell viz`: switch the music line on/off (while on, it shows whenever music plays).
pub fn toggle_mini(cx: &mut App) {
    let on = !cx.try_global::<VizState>().is_some_and(|s| s.on);
    let run = cx.try_global::<VizState>().map_or(0, |s| s.run) + 1;
    cx.set_global(VizState { on, run });
    if !on {
        let _ = std::fs::create_dir_all(format!("{}/.cache/fiaboo-shell/viz", home()));
        let _ = std::fs::write(off_flag(), "");
        return; // the controller sees the new run, fades the line out, closes it and stops the capture
    }
    let _ = std::fs::remove_file(off_flag());
    start_player_watch();
    cx.spawn(async move |cx| {
        let mut motion = Motion::default();
        let mut win: Option<WindowHandle<Line>> = None;
        let mut frame = 0u64;
        loop {
            let current = cx.update(|cx| cx.try_global::<VizState>().is_some_and(|s| s.on && s.run == run));
            let playing = current && PLAYING.load(Ordering::Relaxed);
            if playing {
                capture_start();
            }
            motion.step(if playing { *LEVEL.lock().unwrap() } else { (-120., -120.) });
            let visible = motion.alpha > 0.01;
            if visible || playing {
                if win.is_none() {
                    win = cx.update(open_line);
                }
                if let Some(h) = win {
                    let pal = (frame % (FPS * 2) == 0).then(palette); // a new wallpaper shows within ~2 s
                    let (level, kick, alpha) = (motion.level, motion.kick, motion.alpha);
                    let _ = h.update(cx, |l, _, cx| {
                        if let Some(p) = pal {
                            l.pal = p;
                        }
                        (l.level, l.kick, l.alpha) = (level, kick, alpha);
                        cx.notify();
                    });
                }
            } else {
                // nothing to show: no surface, no capture, and a slow idle tick
                capture_stop();
                if let Some(h) = win.take() {
                    let _ = h.update(cx, |_, window, _| window.remove_window());
                }
            }
            // switched off: keep looping until the line has faded and the branch above closed it
            if !current && !visible {
                break;
            }
            let wait = if visible || playing { 1000 / FPS } else { 250 };
            cx.background_executor().timer(Duration::from_millis(wait)).await;
            frame += 1;
        }
    })
    .detach();
}

/// A full-width strip laid over the taskbar's top edge (layer surfaces of the same layer stack in
/// creation order, so it sits above the bar). It covers only the bar's empty top 8 px, never a window.
fn open_line(cx: &mut App) -> Option<WindowHandle<Line>> {
    cx.open_window(
        WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(Bounds { origin: point(px(0.), px(0.)), size: size(px(0.), px(STRIP)) })),
            app_id: Some("fiaboo-musicline".to_string()),
            window_background: WindowBackgroundAppearance::Transparent,
            kind: WindowKind::LayerShell(LayerShellOptions {
                namespace: "fiaboo-musicline".to_string(),
                layer: Layer::Top,
                anchor: Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                exclusive_zone: Some(px(-1.)), // ignore the bar's zone: lie on top of it
                margin: Some((px(0.), px(0.), px(crate::BAR_HEIGHT - STRIP), px(0.))),
                keyboard_interactivity: KeyboardInteractivity::None,
                ..Default::default()
            }),
            ..Default::default()
        },
        |_, cx| cx.new(|_| Line { pal: palette(), level: 0., kick: 0., alpha: 0. }),
    )
    .ok()
}

pub struct Line {
    pal: Palette,
    level: f32,
    kick: f32,
    alpha: f32,
}

impl Render for Line {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        // the wallpaper's second colour in the middle, its main colour towards the sides, then
        // nothing: the ends melt into the bar; a faint glow spills down from the middle
        let (mid, side) = (self.pal.accent2, self.pal.accent);
        let c = |rgb: u32, v: f32| rgba((rgb << 8) | ((v * self.alpha).clamp(0., 1.) * 255.) as u32);
        let reach = (0.12 + 0.76 * self.level + 0.08 * self.kick).min(0.98);
        let thick = 2. + 1.5 * self.kick;
        let seg = |from: gpui::Rgba, to: gpui::Rgba| {
            div().flex_1().h_full().bg(linear_gradient(90., linear_color_stop(from, 0.), linear_color_stop(to, 1.)))
        };
        div().size_full().flex().justify_center().child(
            div()
                .w(relative(reach))
                .h_full()
                .flex()
                .flex_col()
                .child(
                    div()
                        .h(px(thick))
                        .flex()
                        .child(seg(c(side, 0.), c(side, 0.9)))
                        .child(seg(c(side, 0.9), c(mid, 1.)))
                        .child(seg(c(mid, 1.), c(side, 0.9)))
                        .child(seg(c(side, 0.9), c(side, 0.))),
                )
                .child(
                    div().flex_1().flex().justify_center().child(
                        div()
                            .w(relative(0.6))
                            .h_full()
                            .rounded_full()
                            .bg(linear_gradient(180., linear_color_stop(c(mid, 0.28 + 0.2 * self.kick), 0.), linear_color_stop(c(mid, 0.), 1.))),
                    ),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(f: f32, amp: f32) -> Vec<f32> {
        (0..RATE).map(|i| (2. * std::f32::consts::PI * f * i as f32 / RATE as f32).sin() * amp).collect()
    }

    #[test]
    fn analyze_measures_loudness_and_bass() {
        let mut lp = 0.;
        let (db, bass) = analyze(&tone(60., 0.5), &mut lp);
        assert!((db - -9.).abs() < 0.5, "0.5 sine is about -9 dBFS, got {db}");
        assert!(db - bass < 1.5, "a 60 Hz tone is all bass: {db} vs {bass}");
        let mut lp = 0.;
        let (db, bass) = analyze(&tone(5000., 0.5), &mut lp);
        assert!(db - bass > 25., "a 5 kHz tone has no bass: {db} vs {bass}");
        let mut lp = 0.;
        assert!(analyze(&vec![0.; HOP], &mut lp).0 < -100., "silence");
    }

    #[test]
    fn line_grows_with_music_and_fades_in_silence() {
        let mut m = Motion::default();
        for i in 0..300 {
            // a loud song with a kick every 15 frames (120 bpm at 30 fps)
            m.step((-14. + if i % 15 == 0 { 3. } else { 0. }, if i % 15 == 0 { -10. } else { -24. }));
        }
        assert!(m.alpha > 0.95 && m.level > 0.3, "playing: alpha {} level {}", m.alpha, m.level);
        for _ in 0..120 {
            m.step((-120., -120.));
        }
        assert!(m.alpha < 0.01 && m.level < 0.01 && m.kick < 0.01, "silent: alpha {} level {}", m.alpha, m.level);
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
