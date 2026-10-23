//! fiaboo-shell prototype: a GPUI layer-shell bar (bottom of the screen) with logo, workspaces,
//! running apps (icons, click to focus) and clock. Colours: fixed Catppuccin Mocha (`palette()`).
mod lunar;
mod notify;
mod tray;
mod viz;
mod wm;
mod quick;
mod traymenu;
mod calendar;
mod notif_ui;
mod clip;
mod shot;
mod wallpaper;
mod settings;
mod start;
mod taskview;
use quick::*;
use traymenu::*;
use calendar::*;
use notif_ui::*;
use clip::*;
use shot::*;
use wallpaper::*;
use settings::*;
use start::*;
use taskview::*;

use std::{
    collections::{HashMap, HashSet},
    fs,
    path::PathBuf,
    process::Command,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use gpui::{
    div, img, layer_shell::*, point, prelude::*, px, rgb, rgba, size, AnyWindowHandle, App, Bounds,
    Context, FocusHandle, FontWeight, Global, KeyDownEvent, MouseButton, ObjectFit, Window,
    WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions,
};
use gpui_platform::application;

const BAR_HEIGHT: f32 = 48.0;
/// Panel/bar background alpha: the glass plugin (scripts/glass.lua) sees through this fill.
const GLASS: u32 = 0xC8;
// alpha steps for palette colours used as fills and borders (never for text)
const A_SURFACE: u32 = 0x20; // resting tiles, chips
const A_HOVER: u32 = 0x30;
const A_ACTIVE: u32 = 0x48; // selected / pressed; keeps fg >= 4.5:1 even on an accent fill
const A_STRONG: u32 = 0x70; // borders, focus rings, the accent card corner

fn home() -> String {
    std::env::var("HOME").unwrap_or_default()
}

/// Write an RGBA image as PNG under a temp name, then rename: callers reuse any file that exists,
/// so a half-written one (crash, full disk) would otherwise stay a broken icon for good.
fn write_png(file: &std::path::Path, w: u32, h: u32, rgba: &[u8]) -> Option<()> {
    let mut buf = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut buf, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().ok()?;
        writer.write_image_data(rgba).ok()?;
        writer.finish().ok()?;
    }
    let tmp = file.with_extension("tmp");
    if fs::write(&tmp, &buf).is_err() {
        let _ = fs::remove_file(&tmp);
        return None;
    }
    fs::rename(&tmp, file).ok()
}

// ---------- palette ----------
#[derive(Clone)]
struct Palette {
    bg: u32,
    fg: u32,
    accent: u32,
    accent2: u32, // tertiary, for two-colour accents (the music line)
    dim: u32,
    err: u32,
}

// One fixed Catppuccin Mocha palette (owner, 2026-10-17); nothing follows the wallpaper.
// secondary text is subtext1, not subtext0: it sits on see-through glass
fn palette() -> Palette {
    Palette { bg: 0x1e1e2e, fg: 0xcdd6f4, accent: 0x89b4fa, accent2: 0xcba6f7, dim: 0xbac2de, err: 0xf38ba8 }
}

// ---------- icon lookup ----------
#[derive(Clone)]
struct Desktop {
    name: String,
    stem: String, // lowercase .desktop file stem (also what gtk-launch wants)
    icon: String,
    wm_class: String, // lowercase StartupWMClass or ""
    settings: bool,   // Categories has Settings: a configuration tool, listed after the apps in Start
}

// User dir first: a same-named file there overrides (or hides) the system one, per XDG.
fn app_dirs() -> [String; 2] {
    [format!("{}/.local/share/applications", home()), "/usr/share/applications".to_string()]
}

fn scan_desktops(dirs: &[String]) -> Vec<Desktop> {
    let mut all = vec![];
    let mut seen = HashSet::new();
    for dir in dirs {
        let Ok(rd) = fs::read_dir(dir) else { continue };
        for e in rd.flatten() {
            let path = e.path();
            if path.extension().is_none_or(|x| x != "desktop") || !seen.insert(e.file_name()) {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else { continue };
            let field = |k: &str| {
                text.lines()
                    .find_map(|l| l.strip_prefix(k).and_then(|v| v.strip_prefix('=')))
                    .map(str::trim)
                    .map(String::from)
            };
            if field("NoDisplay").as_deref() == Some("true") {
                continue;
            }
            let (Some(icon), Some(stem)) = (field("Icon"), path.file_stem().and_then(|s| s.to_str())) else {
                continue;
            };
            all.push(Desktop {
                name: field("Name").unwrap_or_else(|| stem.to_string()),
                stem: stem.to_lowercase(),
                icon,
                wm_class: field("StartupWMClass").unwrap_or_default().to_lowercase(),
                settings: field("Categories").is_some_and(|c| c.split(';').any(|c| c == "Settings")),
            });
        }
    }
    all
}

type DesktopIndex = HashMap<String, String>; // lowercase window class / desktop file stem -> Icon= value
static ICON_CACHE: OnceLock<Mutex<HashMap<String, Option<PathBuf>>>> = OnceLock::new();

/// Installed apps; rescanned when an applications dir changes (installs/removals rename files in it).
fn apps() -> Arc<(Vec<Desktop>, DesktopIndex)> {
    static CACHE: Mutex<Option<(Vec<Option<std::time::SystemTime>>, Arc<(Vec<Desktop>, DesktopIndex)>)>> =
        Mutex::new(None);
    let stamp: Vec<_> = app_dirs().iter().map(|d| fs::metadata(d).and_then(|m| m.modified()).ok()).collect();
    let mut cache = CACHE.lock().unwrap();
    if let Some((s, a)) = cache.as_ref() {
        if *s == stamp {
            return a.clone();
        }
    }
    let list = scan_desktops(&app_dirs());
    let mut map = HashMap::new();
    for d in &list {
        map.insert(d.stem.clone(), d.icon.clone());
        if !d.wm_class.is_empty() {
            map.insert(d.wm_class.clone(), d.icon.clone());
        }
    }
    let a = Arc::new((list, map));
    *cache = Some((stamp, a.clone()));
    if let Some(c) = ICON_CACHE.get() {
        c.lock().unwrap().clear(); // a new app may now resolve a class that had no icon
    }
    a
}

fn find_icon_file(name: &str) -> Option<PathBuf> {
    if name.starts_with('/') {
        return PathBuf::from(name).exists().then(|| PathBuf::from(name));
    }
    // ~16 stats per name, asked per notification card and Start entry on every render; an app rescan clears it
    let key = format!("icon-file:{name}");
    let cache = ICON_CACHE.get_or_init(Default::default);
    if let Some(hit) = cache.lock().unwrap().get(&key) {
        return hit.clone();
    }
    let found = find_icon_file_uncached(name);
    cache.lock().unwrap().insert(key, found.clone());
    found
}

fn find_icon_file_uncached(name: &str) -> Option<PathBuf> {
    // the app's own icon first (kora's restyled set only as a fallback); vector, then the PNG nearest
    // our 26-48 px display sizes: GPUI samples big bitmaps without mipmaps, so 256 px looks grainy
    let mut candidates = vec![format!("/usr/share/icons/hicolor/scalable/apps/{name}.svg")];
    for size in ["64x64", "128x128", "48x48", "256x256", "512x512"] {
        candidates.push(format!("/usr/share/icons/hicolor/{size}/apps/{name}.png"));
    }
    for size in ["256x256", "128x128", "64x64", "48x48", "512x512"] {
        candidates.push(format!("/usr/share/icons/hicolor/{size}/apps/{name}.svg"));
    }
    candidates.push(format!("{}/.local/share/icons/hicolor/256x256/apps/{name}.png", home()));
    candidates.push(format!("/usr/share/pixmaps/{name}.png"));
    candidates.push(format!("/usr/share/pixmaps/{name}.svg"));
    candidates.push(format!("{}/.local/share/icons/kora/apps/scalable/{name}.svg", home()));
    candidates.into_iter().map(PathBuf::from).find(|p| p.exists())
}

fn icon_for(class: &str) -> Option<PathBuf> {
    let apps = apps(); // before taking the cache lock: a rescan clears it
    let cache = ICON_CACHE.get_or_init(Default::default);
    if let Some(hit) = cache.lock().unwrap().get(class) {
        return hit.clone();
    }
    let lower = class.to_lowercase();
    let found = apps
        .1
        .get(&lower)
        .and_then(|icon| find_icon_file(icon))
        .or_else(|| find_icon_file(&lower));
    cache.lock().unwrap().insert(class.to_string(), found.clone());
    found
}

// ---------- hyprland state ----------
#[derive(Clone, Default)]
struct AppIcon {
    class: String,          // window class, or pinned id when not running
    address: String,        // window to focus ("" when not running)
    launch: Option<String>, // desktop file stem for gtk-launch
    icon: Option<PathBuf>,
    active: bool,
    running: bool,
}

/// Pinned ids from the Start menu (~/.local/state/start-pinned.json), resolved to desktop entries.
fn pinned_ids() -> Vec<String> {
    fs::read_to_string(format!("{}/.local/state/start-pinned.json", home()))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn pinned() -> Vec<(String, Desktop)> {
    let apps = apps();
    pinned_ids().into_iter()
        .filter_map(|id| {
            let id = id.to_lowercase();
            let d = apps
                .0
                .iter()
                .find(|d| d.stem == id)
                .or_else(|| apps.0.iter().find(|d| d.stem.contains(&id)))?;
            Some((id, d.clone()))
        })
        .collect()
}

/// Right-click in Start: pin the app if not pinned, otherwise unpin it.
fn toggle_pin(stem: &str) {
    let path = format!("{}/.local/state/start-pinned.json", home());
    // toggle in the raw list: pins whose .desktop is missing right now (uninstalled, renamed) must survive
    let mine: Vec<String> = pinned().into_iter().filter(|(_, d)| d.stem == stem).map(|(id, _)| id).collect();
    let mut ids: Vec<String> = pinned_ids().into_iter().filter(|id| !mine.contains(&id.to_lowercase())).collect();
    if mine.is_empty() {
        ids.push(stem.to_string());
    }
    let _ = fs::write(path, serde_json::to_string(&ids).unwrap_or_default());
}

fn class_matches(class: &str, id: &str, d: &Desktop) -> bool {
    let lc = class.to_lowercase();
    lc == d.stem || lc == id || (!d.wm_class.is_empty() && lc == d.wm_class) || lc.contains(id)
}

#[derive(Clone, Default)]
struct State {
    active_ws: i64,
    occupied: Vec<i64>,
    title: String,
    apps: Vec<AppIcon>,
    time: String,
}

use wm::hypr_json as hyprctl_json;

fn read_state() -> State {
    let active = hyprctl_json("activewindow");
    let active_addr = active["address"].as_str().unwrap_or("").to_string();
    let active_ws = hyprctl_json("activeworkspace")["id"].as_i64().unwrap_or(1);

    let mut occupied = vec![];
    if let Some(list) = hyprctl_json("workspaces").as_array() {
        for w in list {
            if w["windows"].as_i64().unwrap_or(0) > 0 {
                if let Some(id) = w["id"].as_i64().filter(|i| *i > 0) {
                    occupied.push(id);
                }
            }
        }
    }
    occupied.sort();

    // one icon per window class, keep the most recently focused window of each class
    let mut clients: Vec<serde_json::Value> =
        hyprctl_json("clients").as_array().cloned().unwrap_or_default();
    clients.sort_by_key(|c| c["focusHistoryID"].as_i64().unwrap_or(i64::MAX));
    let mut apps: Vec<AppIcon> = vec![];
    let pins = pinned();
    let mut used_class: Vec<String> = vec![];
    for (id, d) in &pins {
        let win = clients.iter().find(|c| {
            let class = c["class"].as_str().unwrap_or("");
            !class.is_empty()
                && !c["workspace"]["name"].as_str().unwrap_or("").starts_with("special")
                && !used_class.iter().any(|u| u == class) // the loose contains() match could let two pins claim one app
                && class_matches(class, id, d)
        });
        let (class, address) = match win {
            Some(c) => (
                c["class"].as_str().unwrap_or("").to_string(),
                c["address"].as_str().unwrap_or("").to_string(),
            ),
            None => (id.clone(), String::new()),
        };
        if win.is_some() {
            used_class.push(class.clone());
        }
        apps.push(AppIcon {
            icon: find_icon_file(&d.icon).or_else(|| icon_for(&class)),
            active: !address.is_empty() && address == active_addr,
            running: win.is_some(),
            launch: Some(d.stem.clone()),
            class,
            address,
        });
    }
    // other running apps (not pinned), one icon per class, ordered by class
    let mut extra: Vec<AppIcon> = vec![];
    for c in &clients {
        let class = c["class"].as_str().unwrap_or("");
        let ws = c["workspace"]["name"].as_str().unwrap_or("");
        if class.is_empty() || class == "fiaboo-shell" || ws.starts_with("special") {
            continue;
        }
        if used_class.iter().any(|u| u == class) || extra.iter().any(|a| a.class == class) {
            continue;
        }
        let address = c["address"].as_str().unwrap_or("").to_string();
        extra.push(AppIcon {
            class: class.to_string(),
            icon: icon_for(class),
            active: address == active_addr,
            running: true,
            launch: None,
            address,
        });
    }
    extra.sort_by(|a, b| a.class.cmp(&b.class));
    apps.extend(extra);

    State {
        active_ws,
        occupied,
        title: active["title"].as_str().unwrap_or("").to_string(),
        apps,
        time: Command::new("date")
            .arg("+%H:%M|%d/%m/%Y")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_default(),
    }
}

fn launch_app(stem: &str) {
    // Stems are stored lowercase, but gtk-launch needs the file's real case (com.system76.CosmicFiles).
    let real = app_dirs()
        .iter()
        .filter_map(|d| fs::read_dir(d).ok())
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.strip_suffix(".desktop").map(String::from))
        .find(|s| s.to_lowercase() == stem);
    let stem = real.as_deref().unwrap_or(stem);
    // args, not `sh -c`: Steam's .desktop names have spaces
    spawn_bg(
        Command::new("setsid")
            .args(["-f", "gtk-launch", stem])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    );
}

/// Start a command without waiting on it; a thread reaps it so it never lingers as a zombie.
fn spawn_bg(cmd: &mut Command) {
    if let Ok(mut child) = cmd.spawn() {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

/// Turn wheel events into whole steps (+1 = scroll down). Touchpads send many small pixel
/// deltas, so those are accumulated until they add up to one notch.
fn scroll_steps(delta: &gpui::ScrollDelta) -> i32 {
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

fn focus_window(address: &str) {
    wm::focus_window(address);
}


// ---------- system state (quick settings) ----------
#[derive(Clone, Default)]
struct Sys {
    wifi_on: bool,
    ssid: String,
    bt_on: bool,
    vol: f32,
    muted: bool,
    dnd: bool,
    night: bool,
    media: Option<Media>,
}

#[derive(Clone, Debug, PartialEq)]
struct Media {
    playing: bool,
    artist: String,
    title: String,
}

/// One `playerctl metadata --format '{{status}}\t{{artist}}\t{{title}}'` line; empty when no player.
fn parse_media(line: &str) -> Option<Media> {
    let mut f = line.splitn(3, '\t');
    let (status, artist, title) = (f.next()?, f.next()?.trim(), f.next()?.trim());
    (!title.is_empty()).then(|| Media { playing: status == "Playing", artist: artist.into(), title: title.into() })
}

fn sh(cmd: &str) -> String {
    Command::new("sh")
        .args(["-c", cmd])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn run_bg(cmd: &str) {
    spawn_bg(Command::new("sh").args(["-c", &format!("{cmd} >/dev/null 2>&1 &")]));
}

/// (volume 0..1+, muted) of the default sink.
fn read_volume() -> (f32, bool) {
    let line = sh("wpctl get-volume @DEFAULT_AUDIO_SINK@");
    (line.split_whitespace().nth(1).and_then(|v| v.parse().ok()).unwrap_or(0.0), line.contains("MUTED"))
}

fn read_sys() -> Sys {
    let (vol, muted) = read_volume();
    Sys {
        wifi_on: sh("nmcli -t -f WIFI g") == "enabled",
        ssid: sh("nmcli -t -f active,ssid dev wifi list --rescan no | grep '^yes' | head -1 | cut -d: -f2-"),
        bt_on: sh("timeout 2 bluetoothctl show | grep -c 'Powered: yes'") == "1",
        vol,
        muted,
        dnd: notify::dnd(),
        night: !sh("pgrep -x hyprsunset").is_empty(),
        media: parse_media(&sh("timeout 2 playerctl metadata --format '{{status}}\t{{artist}}\t{{title}}' 2>/dev/null")),
    }
}

const NF: &str = "JetBrainsMono Nerd Font";

fn sys_glyphs(sys: &Sys) -> (&'static str, &'static str, &'static str) {
    (
        if sys.wifi_on { "\u{f05a9}" } else { "\u{f05aa}" },
        if sys.bt_on { "\u{f00af}" } else { "\u{f00b2}" },
        if sys.muted || sys.vol <= 0.0 { "\u{f0581}" } else { "\u{f057e}" },
    )
}

// ---------- view ----------
struct Bar {
    pal: Palette,
    st: State,
    sys: Sys,
    tray: Vec<tray::TrayItem>,
    notif_gen: u64,
    notif_count: usize,
    recording: bool,
    panels_open: bool, // popups were hidden for an open panel
}

/// The compositor event stream (Hyprland .socket2) flags the bar to refresh when windows/workspaces change,
/// instead of polling `hyprctl` several times a second.
static HYPR_DIRTY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

// ---------- volume OSD ----------
static OSD_PENDING: Mutex<Option<(f32, bool)>> = Mutex::new(None);

/// Follow `pactl subscribe`: when the default sink's volume or mute state changes (media keys,
/// quick settings, apps), queue an OSD. Event-driven, no polling.
fn start_volume_watch() {
    use std::io::{BufRead, BufReader};
    std::thread::spawn(|| loop {
        let mut last = read_volume();
        if let Ok(mut child) = Command::new("pactl").arg("subscribe").stdout(std::process::Stdio::piped()).spawn() {
            if let Some(out) = child.stdout.take() {
                for line in BufReader::new(out).lines().map_while(Result::ok) {
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

struct OsdState(Option<(gpui::WindowHandle<Osd>, std::time::Instant)>);
impl Global for OsdState {}

struct Osd {
    pal: Palette,
    vol: f32,
    muted: bool,
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
            .bg(rgba((p.bg << 8) | GLASS))
            .border_1()
            .border_color(rgba((p.dim << 8) | A_STRONG))
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
fn show_osd(cx: &mut App, vol: f32, muted: bool) {
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

fn hide_osd_if_due(cx: &mut App) {
    if let Some((h, until)) = cx.try_global::<OsdState>().and_then(|s| s.0) {
        if std::time::Instant::now() >= until {
            let _ = h.update(cx, |_, window, _| window.remove_window());
            cx.set_global(OsdState(None));
        }
    }
}

fn start_hypr_events() {
    use std::sync::atomic::Ordering;
    std::thread::spawn(|| loop {
        wm::follow_events(|| HYPR_DIRTY.store(true, Ordering::Relaxed));
        std::thread::sleep(Duration::from_secs(2)); // compositor restarted or socket not up yet: reconnect
    });
}

impl Bar {
    fn new(cx: &mut Context<Self>) -> Self {
        if let Some(mut rx) = IPC_RX.lock().unwrap().take() {
            cx.spawn(async move |_, cx| {
                use futures::StreamExt;
                while let Some(cmd) = rx.next().await {
                    let _ = cx.update(|cx| match cmd.as_str() {
                        "start" => toggle_start(cx),
                        "taskview" => toggle_taskview(cx),
                        "clipboard" => toggle_clip(cx),
                        "shot" => toggle_shot(cx),
                        "wallpaper" => toggle_wp(cx),
                        "settings" => toggle_settings(cx),
                        "calendar" => toggle_calendar(cx),
                        "viz" => viz::toggle_mini(cx),
                        _ => {}
                    });
                }
            })
            .detach();
        }
        cx.spawn(async move |this, cx| {
            use std::sync::atomic::Ordering;
            let long_ago = std::time::Instant::now() - Duration::from_secs(60);
            let (mut last_full, mut last_sys) = (long_ago, long_ago);
            loop {
                // window/workspace state: only when Hyprland reports a change (plus a slow refresh for the clock and pins)
                let st = if HYPR_DIRTY.swap(false, Ordering::Relaxed) || last_full.elapsed() > Duration::from_secs(10) {
                    last_full = std::time::Instant::now();
                    Some(cx.background_executor().spawn(async { read_state() }).await)
                } else {
                    None
                };
                let sys = if last_sys.elapsed() > Duration::from_secs(8) {
                    last_sys = std::time::Instant::now();
                    Some(cx.background_executor().spawn(async { read_sys() }).await)
                } else {
                    None
                };
                let tray_now = tray::items();
                let rec_flag = format!("{}/fiaboo-recording", std::env::var("XDG_RUNTIME_DIR").unwrap_or_default());
                let alive = this.update(cx, |bar, cx| {
                    let mut changed = false;
                    if let Some(st) = st {
                        bar.st = st;
                        changed = true;
                    }
                    if let Some(sys) = sys {
                        bar.sys = sys;
                        changed = true;
                    }
                    if let Some((vol, muted)) = OSD_PENDING.lock().unwrap().take() {
                        show_osd(cx, vol, muted);
                        bar.sys.vol = vol;
                        bar.sys.muted = muted;
                        changed = true;
                    }
                    hide_osd_if_due(cx);
                    notify::expire_popups();
                    let g = notify::generation();
                    let panels = panel_open(cx);
                    if g != bar.notif_gen || panels != bar.panels_open {
                        bar.notif_gen = g;
                        bar.panels_open = panels;
                        bar.notif_count = notify::unseen();
                        sync_notif_popups(cx);
                        changed = true;
                    }
                    let rec = std::path::Path::new(&rec_flag).exists();
                    if rec != bar.recording {
                        bar.recording = rec;
                        changed = true;
                    }
                    if tray_now != bar.tray {
                        bar.tray = tray_now;
                        changed = true;
                    }
                    if changed {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
                cx.background_executor().timer(Duration::from_millis(150)).await;
            }
        })
        .detach();
        Bar { pal: palette(), st: State::default(), sys: Sys::default(), tray: tray::items(), notif_gen: u64::MAX, notif_count: 0, recording: false, panels_open: false }
    }
}

impl Render for Bar {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.pal.clone();
        let st = &self.st;

        let dots = (1..=5).map(|id| {
            let is_active = id == st.active_ws;
            let used = st.occupied.contains(&id);
            div()
                .id(("ws", id as usize))
                .cursor_pointer()
                .py(px(12.))
                .px(px(4.))
                .on_click(move |_, _, _| wm::focus_workspace(id))
                .child(div()
                .w(if is_active { px(22.) } else { px(8.) })
                .h(px(8.))
                .rounded_full()
                .bg(if is_active {
                    rgb(p.accent)
                } else if used {
                    rgb(p.dim)
                } else {
                    rgba((p.dim << 8) | A_ACTIVE)
                }))
        });

        let apps = st.apps.iter().enumerate().map(|(i, a)| {
            let address = a.address.clone();
            let launch = a.launch.clone();
            let running = a.running;
            let letter = a.class.chars().find(|c| c.is_alphabetic()).unwrap_or('?').to_uppercase().to_string();
            let icon = match &a.icon {
                Some(path) => div()
                    .size(px(26.))
                    .child(img(path.clone()).size(px(26.)).object_fit(ObjectFit::Contain))
                    .into_any_element(),
                None => div()
                    .size(px(26.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .bg(rgba((p.dim << 8) | A_ACTIVE))
                    .text_color(rgb(p.fg))
                    .child(letter)
                    .into_any_element(),
            };
            div()
                .id(("app", i))
                .cursor_pointer()
                .flex()
                .flex_col()
                .items_center()
                .gap_0p5()
                .px_2()
                .py_1()
                .rounded_md()
                .hover(|s| s.bg(rgba((p.dim << 8) | A_HOVER)))
                .on_mouse_down(MouseButton::Middle, {
                    let launch = launch.clone();
                    move |_, _, _| {
                        if let Some(stem) = &launch {
                            launch_app(stem)
                        }
                    }
                })
                .on_click(move |_, _, _| {
                    if running {
                        focus_window(&address)
                    } else if let Some(stem) = &launch {
                        launch_app(stem)
                    }
                })
                .child(icon)
                .child(
                    div()
                        .h(px(3.))
                        .w(if a.active { px(14.) } else { px(5.) })
                        .rounded_full()
                        .bg(if a.active {
                            rgb(p.accent)
                        } else if a.running {
                            rgba((p.dim << 8) | A_STRONG)
                        } else {
                            rgba(0x00000000)
                        }),
                )
        });

        let (clock_time, clock_date) = st.time.split_once('|').unwrap_or((st.time.as_str(), ""));
        div()
            .font_family(NF).font_weight(FontWeight::MEDIUM)
            .size_full()
            .relative()
            .flex()
            .flex_row()
            .items_center()
            .px_3()
            .bg(rgba((p.bg << 8) | GLASS))
            .border_t_1()
            .border_color(rgba((p.dim << 8) | A_STRONG))
            .text_color(rgb(p.fg))
            .text_size(px(14.))
            // left: workspaces
            .child(
                div()
                    .w(px(340.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .id("ws-dots")
                            .flex()
                            .flex_row()
                            .items_center()
                            .on_scroll_wheel({
                                let active = st.active_ws;
                                move |ev, _, _| {
                                    let n = scroll_steps(&ev.delta) as i64;
                                    let to = (active + n).clamp(1, 5);
                                    if n != 0 && to != active {
                                        wm::focus_workspace(to);
                                    }
                                }
                            })
                            .children(dots),
                    ),
            )
            // center (Win11): Start, Task View, apps
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_center()
                    .gap_1()
                    .child(
                        div()
                            .id("start-button")
                            .cursor_pointer()
                            .p_1()
                            .rounded_lg()
                            .hover(|s| s.bg(rgba((p.dim << 8) | A_HOVER)))
                            .on_click(|_, _, cx| toggle_start(cx))
                            .child(
                                img(PathBuf::from(format!("{}/Pictures/fiaboo/cauvang.jpg", home())))
                                    .size(px(30.))
                                    .rounded_full()
                                    .object_fit(ObjectFit::Cover),
                            ),
                    )
                    .child(
                        div()
                            .id("taskview-button")
                            .cursor_pointer()
                            .size(px(32.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_lg()
                            .font_family(NF).font_weight(FontWeight::MEDIUM)
                            .text_size(px(18.))
                            .hover(|s| s.bg(rgba((p.dim << 8) | A_HOVER)))
                            .on_click(|_, _, cx| toggle_taskview(cx))
                            .child("\u{f056e}"),
                    )
                    .children(apps),
            )
            // right: tray, status icons, clock, notifications
            .child(
                div()
                    .w(px(340.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_end()
                    .gap_2()
                    .child(
                div().flex().items_center().gap_1().children(self.tray.iter().enumerate().map(|(i, it)| {
                    let (left, right) = (it.clone(), it.clone());
                    div()
                        .id(("tray", i))
                        .cursor_pointer()
                        .size(px(36.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_md()
                        .hover(|s| s.bg(rgba((p.dim << 8) | A_HOVER)))
                        .on_mouse_down(MouseButton::Left, move |_, _, _| {
                            let it = left.clone();
                            std::thread::spawn(move || tray::activate(&it));
                        })
                        .on_mouse_down(MouseButton::Right, move |_, _, cx| open_tray_menu(cx, right.clone()))
                        .child(match &it.icon {
                            Some(path) => img(path.clone()).size(px(18.)).object_fit(ObjectFit::Contain).into_any_element(),
                            None => div()
                                .size(px(18.))
                                .rounded_full()
                                .bg(rgba((p.dim << 8) | A_STRONG))
                                .into_any_element(),
                        })
                })),
            )
                    .when(self.recording, |d| {
                        d.child(
                            div()
                                .id("rec")
                                .cursor_pointer()
                                .px_2()
                                .py_1()
                                .rounded_lg()
                                .font_family(NF).font_weight(FontWeight::MEDIUM)
                                .text_size(px(16.))
                                .text_color(rgb(p.err))
                                .hover(|s| s.bg(rgba((p.dim << 8) | A_HOVER)))
                                .on_click(|_, _, _| run_bg("~/.local/bin/fiaboo-record"))
                                .child("\u{f044a}"),
                        )
                    })
                    .child({
                let (g_wifi, g_bt, g_vol) = sys_glyphs(&self.sys);
                div()
                    .id("sys-button")
                    .on_scroll_wheel(|ev, _, _| {
                        let n = scroll_steps(&ev.delta);
                        if n != 0 {
                            let dir = if n > 0 { "-" } else { "+" };
                            run_bg(&format!("wpctl set-volume -l 1 @DEFAULT_AUDIO_SINK@ {}%{dir}", 5 * n.abs()));
                        }
                    })
                    .cursor_pointer()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_2()
                    .h(px(36.))
                    .rounded_lg()
                    .font_family(NF).font_weight(FontWeight::MEDIUM)
                    .text_size(px(16.))
                    .hover(|s| s.bg(rgba((p.dim << 8) | A_HOVER)))
                    .on_click(|_, _, cx| toggle_quick(cx))
                    .child(div().text_color(if self.sys.wifi_on { rgb(p.fg) } else { rgb(p.dim) }).child(g_wifi))
                    .child(div().text_color(if self.sys.bt_on { rgb(p.fg) } else { rgb(p.dim) }).child(g_bt))
                    .child(div().text_color(if self.sys.muted { rgb(p.dim) } else { rgb(p.fg) }).child(g_vol))
            })
                    .child(
                        div()
                            .id("clock")
                            .cursor_pointer()
                            .rounded_lg()
                            .h(px(36.))
                            .justify_center()
                            .hover(|s| s.bg(rgba((p.dim << 8) | A_HOVER)))
                            .on_click(|_, _, cx| toggle_calendar(cx))
                            .flex()
                            .flex_col()
                            .items_end()
                            .px_2()
                            // whole-pixel line heights (16+16 in a 36 px box): GPUI has no hinting, so a fractional
                            // baseline is what makes small text look soft
                            .child(div().text_size(px(14.)).line_height(px(16.)).font_weight(FontWeight::SEMIBOLD).child(clock_time.to_string()))
                            .child(div().text_size(px(13.)).line_height(px(16.)).text_color(rgb(p.dim)).child(clock_date.to_string())),
                    )
                    .child(
                        div()
                            .id("bell")
                            .cursor_pointer()
                            .font_family(NF).font_weight(FontWeight::MEDIUM)
                            .text_size(px(18.))
                            .px_2()
                            .h(px(36.))
                            .rounded_lg()
                            .hover(|s| s.bg(rgba((p.dim << 8) | A_HOVER)))
                            .on_click(|_, _, cx| toggle_notif_center(cx))
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(if notify::dnd() { "\u{f009b}" } else { "\u{f009c}" })
                            .when(self.notif_count > 0, |d| {
                                d.child(
                                    div()
                                        .text_size(px(12.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(rgb(p.accent))
                                        .child(self.notif_count.to_string()),
                                )
                            }),
                    ),
            )
    }
}

// ---------- IPC: `fiaboo-shell start|taskview` toggles a panel in the running shell ----------
// the socket thread pushes commands into a channel the UI awaits, so a key press opens the panel at once
static IPC_RX: Mutex<Option<futures::channel::mpsc::UnboundedReceiver<String>>> = Mutex::new(None);

fn ipc_path() -> String {
    format!("{}/fiaboo-shell.sock", std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into()))
}

fn start_ipc() {
    use std::io::Read;
    let path = ipc_path();
    // A live shell already answers here: removing its socket would leave it deaf to every keybind.
    if std::os::unix::net::UnixStream::connect(&path).is_ok() {
        eprintln!("fiaboo-shell is already running");
        std::process::exit(1);
    }
    let _ = fs::remove_file(&path);
    let Ok(listener) = std::os::unix::net::UnixListener::bind(&path) else { return };
    let (tx, rx) = futures::channel::mpsc::unbounded();
    *IPC_RX.lock().unwrap() = Some(rx);
    std::thread::spawn(move || {
        for mut conn in listener.incoming().flatten() {
            let mut cmd = String::new();
            let _ = conn.read_to_string(&mut cmd);
            let _ = tx.unbounded_send(cmd.trim().to_string());
        }
    });
}

/// Client side: send the command to the running shell, exit 1 if none is listening.
fn send_ipc(cmd: &str) -> ! {
    use std::io::Write;
    match std::os::unix::net::UnixStream::connect(ipc_path()) {
        Ok(mut s) => {
            let _ = s.write_all(cmd.as_bytes());
            std::process::exit(0)
        }
        Err(_) => std::process::exit(1),
    }
}

fn main() {
    if let Some(cmd) = std::env::args().nth(1) {
        send_ipc(&cmd);
    }
    start_ipc();
    notify::start();
    tray::start();
    start_hypr_events();
    start_volume_watch();
    // Explicit: losing the bar (its only window) must not quit the app before on_window_closed reopens it.
    application().with_quit_mode(gpui::QuitMode::Explicit).run(|cx: &mut App| {
        open_bar(cx);
        // Hyprland closes every layer surface when its monitor goes away (DP-1 re-plugs itself at login);
        // GPUI then just drops the window, so put the bar back once a monitor is there again.
        cx.on_window_closed(|cx, _| {
            if cx.windows().iter().any(|w| w.downcast::<Bar>().is_some()) {
                return;
            }
            cx.spawn(async move |cx| {
                cx.background_executor().timer(Duration::from_millis(500)).await;
                let _ = cx.update(|cx| {
                    if cx.windows().iter().all(|w| w.downcast::<Bar>().is_none()) {
                        open_bar(cx);
                    }
                });
            })
            .detach();
        })
        .detach();
        viz::restore(cx);
    });
}

fn open_bar(cx: &mut App) {
    cx.open_window(
        WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: point(px(0.), px(0.)),
                size: size(px(0.), px(BAR_HEIGHT)), // width 0 = compositor stretches it (anchored left+right)
            })),
            app_id: Some("fiaboo-shell".to_string()),
            window_background: WindowBackgroundAppearance::Transparent,
            kind: WindowKind::LayerShell(LayerShellOptions {
                namespace: "fiaboo-bar".to_string(),
                layer: Layer::Top,
                anchor: Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                exclusive_zone: Some(px(BAR_HEIGHT)), // full-width taskbar at the bottom, windows tile above it
                margin: None,
                keyboard_interactivity: KeyboardInteractivity::None,
                ..Default::default()
            }),
            ..Default::default()
        },
        |_, cx| cx.new(Bar::new),
    )
    .unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_rows_read_in_plain_words() {
        let f = |v: [&str; 4]| v.map(String::from).to_vec();
        assert_eq!(net_row(&f(["wlp3s0", "wifi", "connected", "Home"])), ("Wi-Fi".into(), "Connected · Home".into()));
        assert_eq!(net_row(&f(["tailscale0", "tun", "connected (externally)", "tailscale0"])), ("VPN".into(), "Connected".into()));
        assert_eq!(net_row(&f(["enp2s0", "ethernet", "unavailable", ""])), ("Ethernet".into(), "Cable unplugged".into()));
    }

    #[test]
    fn user_desktop_overrides_system() {
        let root = std::env::temp_dir().join(format!("fiaboo-desktops-{}", std::process::id()));
        let (user, sys) = (root.join("user"), root.join("sys"));
        fs::create_dir_all(&user).unwrap();
        fs::create_dir_all(&sys).unwrap();
        fs::write(sys.join("a.desktop"), "Name=A\nIcon=a\n").unwrap();
        fs::write(sys.join("b.desktop"), "Name=B\nIcon=b\n").unwrap();
        fs::write(user.join("a.desktop"), "Name=A\nIcon=a\nNoDisplay=true\n").unwrap();
        fs::write(user.join("b.desktop"), "Name=B2\nIcon=b\n").unwrap();
        let dirs = [user.to_string_lossy().into_owned(), sys.to_string_lossy().into_owned()];
        let names: Vec<_> = scan_desktops(&dirs).into_iter().map(|d| d.name).collect();
        fs::remove_dir_all(&root).unwrap();
        assert_eq!(names, ["B2"]);
    }
    #[test]
    fn media_line() {
        assert_eq!(
            parse_media("Playing\tArtist\tSong"),
            Some(Media { playing: true, artist: "Artist".into(), title: "Song".into() })
        );
        assert_eq!(parse_media("Paused\t\tVideo\twith tab").map(|m| (m.playing, m.title)), Some((false, "Video\twith tab".into())));
        assert_eq!(parse_media(""), None);
        assert_eq!(
            bt_device_lines("Device 4C:3C:8F:5D:76:9B JBL Go 4\nnoise\nDevice ;rm${IFS}-rf:x evil"),
            [("4C:3C:8F:5D:76:9B".to_string(), "JBL Go 4".to_string())]
        );
        let bt = BtInfo { on: true, paired: vec![("AA".into(), "JBL".into(), false)], ..Default::default() };
        assert_eq!((bt_state(&bt, "power"), bt_state(&bt, "AA"), bt_state(&bt, "BB")), (Some(true), Some(false), None));
        assert_eq!(nmcli_fields("wlan0:wifi:connected:My\\:Net"), ["wlan0", "wifi", "connected", "My:Net"]);
        assert_eq!(nmcli_fields("p2p:wifi-p2p:disconnected:"), ["p2p", "wifi-p2p", "disconnected", ""]);
        assert_eq!(parse_media("Stopped\t\t"), None);
    }
}
