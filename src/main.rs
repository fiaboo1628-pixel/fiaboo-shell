//! fiaboo-shell prototype: a GPUI layer-shell bar (top of the screen) with logo, workspaces,
//! running apps (icons, click to focus) and clock. Colours follow the matugen palette used by kitty.
mod lunar;
mod notify;
mod tray;
mod viz;

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
/// Panel/bar background alpha: low enough that Hyprland's layer blur shows through (frosted glass).
const GLASS: u32 = 0xC8;

fn home() -> String {
    std::env::var("HOME").unwrap_or_default()
}

// ---------- palette ----------
#[derive(Clone)]
struct Palette {
    bg: u32,
    fg: u32,
    accent: u32,
    accent2: u32, // tertiary, for two-colour accents (the taskbar wave)
    dim: u32,
}

fn palette() -> Palette {
    let conf = fs::read_to_string(format!("{}/.config/kitty/colors-matugen.conf", home()))
        .unwrap_or_default();
    let get = |key: &str, default: u32| {
        conf.lines()
            .find_map(|l| {
                let mut it = l.split_whitespace();
                (it.next()? == key).then(|| it.next()).flatten()
            })
            .and_then(|h| u32::from_str_radix(h.trim_start_matches('#'), 16).ok())
            .unwrap_or(default)
    };
    Palette {
        bg: get("background", 0x0f1417),
        fg: get("foreground", 0xdfe3e7),
        accent: get("color4", 0x8fcff3),
        accent2: get("color5", 0xd5bee5),
        dim: get("color6", 0xb5c9d7),
    }
}

// ---------- icon lookup ----------
#[derive(Clone)]
struct Desktop {
    name: String,
    stem: String, // lowercase .desktop file stem (also what gtk-launch wants)
    icon: String,
    wm_class: String, // lowercase StartupWMClass or ""
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
    let mut candidates = vec![
        format!("{}/.local/share/icons/kora/apps/scalable/{name}.svg", home()),
    ];
    for size in ["256x256", "128x128", "64x64", "48x48", "512x512"] {
        candidates.push(format!("/usr/share/icons/hicolor/{size}/apps/{name}.png"));
    }
    for size in ["256x256", "128x128", "64x64", "48x48", "512x512"] {
        candidates.push(format!("/usr/share/icons/hicolor/{size}/apps/{name}.svg"));
    }
    candidates.push(format!("/usr/share/icons/hicolor/scalable/apps/{name}.svg"));
    candidates.push(format!("{}/.local/share/icons/hicolor/256x256/apps/{name}.png", home()));
    candidates.push(format!("/usr/share/pixmaps/{name}.png"));
    candidates.push(format!("/usr/share/pixmaps/{name}.svg"));
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
fn pinned() -> Vec<(String, Desktop)> {
    let ids: Vec<String> = fs::read_to_string(format!("{}/.local/state/start-pinned.json", home()))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let apps = apps();
    ids.into_iter()
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
    let mut ids: Vec<String> = pinned()
        .into_iter()
        .map(|(id, d)| if d.stem == stem { String::new() } else { id })
        .filter(|id| !id.is_empty())
        .collect();
    if !pinned().iter().any(|(_, d)| d.stem == stem) {
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

fn hyprctl_json(arg: &str) -> serde_json::Value {
    Command::new("hyprctl")
        .args([arg, "-j"])
        .output()
        .ok()
        .and_then(|o| serde_json::from_slice(&o.stdout).ok())
        .unwrap_or_default()
}

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
    let _ = Command::new("sh")
        .args(["-c", &format!("setsid gtk-launch {stem} >/dev/null 2>&1 &")])
        .spawn();
}

fn hypr_dispatch(arg: &str) {
    let _ = Command::new("hyprctl").args(["dispatch", arg]).spawn();
}

/// Turn wheel events into whole steps (+1 = scroll down). Touchpads send many small pixel
/// deltas, so those are accumulated until they add up to one notch.
fn scroll_steps(delta: &gpui::ScrollDelta) -> i32 {
    static ACC: Mutex<f32> = Mutex::new(0.);
    let dy = match delta {
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
    let _ = Command::new("hyprctl")
        .args(["dispatch", &format!("hl.dsp.focus({{ window = \"address:{address}\" }})")])
        .spawn();
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
    let _ = Command::new("sh").args(["-c", &format!("{cmd} >/dev/null 2>&1 &")]).spawn();
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
        ssid: sh("nmcli -t -f active,ssid dev wifi | grep '^yes' | head -1 | cut -d: -f2-"),
        bt_on: sh("bluetoothctl show | grep -c 'Powered: yes'") == "1",
        vol,
        muted,
        dnd: notify::dnd(),
        night: !sh("pgrep -x hyprsunset").is_empty(),
        media: parse_media(&sh("playerctl metadata --format '{{status}}\t{{artist}}\t{{title}}' 2>/dev/null")),
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

// ---------- quick settings ----------
struct QuickState(Option<AnyWindowHandle>);
impl Global for QuickState {}

fn toggle_quick(cx: &mut App) {
    if let Some(handle) = cx.try_global::<QuickState>().and_then(|s| s.0) {
        cx.set_global(QuickState(None));
        if handle.update(cx, |_, window, _| window.remove_window()).is_ok() {
            return;
        }
    }
    let opened = cx.open_window(
        WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: point(px(0.), px(0.)),
                size: size(px(0.), px(0.)),
            })),
            app_id: Some("fiaboo-quick".to_string()),
            window_background: WindowBackgroundAppearance::Transparent,
            kind: WindowKind::LayerShell(LayerShellOptions {
                namespace: "fiaboo-quick".to_string(),
                layer: Layer::Overlay,
                anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                exclusive_zone: Some(px(-1.)),
                keyboard_interactivity: KeyboardInteractivity::Exclusive,
                ..Default::default()
            }),
            ..Default::default()
        },
        |window, cx| cx.new(|cx| Quick::new(window, cx)),
    );
    if let Ok(h) = opened {
        cx.set_global(QuickState(Some(h.into())));
    }
}

fn close_quick(window: &mut Window, cx: &mut App) {
    cx.set_global(QuickState(None));
    window.remove_window();
}

struct Quick {
    pal: Palette,
    sys: Sys,
    focus: FocusHandle,
    last_input: std::time::Instant,
    bt: BtUi,
    /// Showing the Bluetooth page (opened with the › on the Bluetooth tile)
    bt_view: bool,
}

impl HasBt for Quick {
    fn bt(&mut self) -> &mut BtUi {
        &mut self.bt
    }
    fn touch(&mut self) {
        self.last_input = std::time::Instant::now();
    }
}

impl Quick {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        cx.spawn_in(window, async move |this, cx| loop {
            let bt_view = this.update(cx, |q, _| q.bt_view).unwrap_or(false);
            let (sys, bt) = cx.background_executor().spawn(async move { (read_sys(), bt_view.then(read_bt)) }).await;
            let idle = this
                .update(cx, |q, cx| {
                    q.sys = sys;
                    if let Some(bt) = bt {
                        q.bt.refresh(bt);
                    }
                    cx.notify();
                    q.last_input.elapsed() > Duration::from_secs(60)
                })
                .unwrap_or(true);
            if idle {
                let _ = this.update_in(cx, |_, window, cx| close_quick(window, cx));
                break;
            }
            cx.background_executor().timer(Duration::from_millis(1200)).await;
        })
        .detach();
        Quick { pal: palette(), sys: Sys::default(), focus, last_input: std::time::Instant::now(), bt: BtUi::default(), bt_view: false }
    }
}

impl Render for Quick {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.pal.clone();
        let sys = self.sys.clone();
        let (g_wifi, g_bt, g_vol) = sys_glyphs(&sys);
        let bt_view = self.bt_view;
        let bt_page = bt_view.then(|| bt_rows(&self.bt, &p, cx));

        let tile = |id: &'static str, glyph: &'static str, title: &'static str, sub: String, on: bool, cmd: String| {
            div()
                .id(id)
                .cursor_pointer()
                .flex_1()
                .flex()
                .items_center()
                .gap_3()
                .px_3()
                .py_3()
                .rounded_xl()
                .bg(if on { rgba((p.accent << 8) | 0x38) } else { rgba((p.dim << 8) | 0x24) })
                .hover(|s| s.bg(rgba((p.accent << 8) | 0x4a)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.last_input = std::time::Instant::now();
                    if cmd == "@dnd" { notify::toggle_dnd() } else { run_bg(&cmd) }
                    cx.notify();
                }))
                .child(
                    div()
                        .font_family(NF)
                        .text_size(px(22.))
                        .text_color(if on { rgb(p.accent) } else { rgb(p.fg) })
                        .child(glyph),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .child(div().font_weight(FontWeight::SEMIBOLD).text_size(px(14.)).child(title))
                        .child(div().text_size(px(12.)).text_color(rgb(p.dim)).child(sub)),
                )
        };

        let wifi_cmd = format!("nmcli radio wifi {}", if sys.wifi_on { "off" } else { "on" });
        let bt_cmd = if sys.bt_on {
            "bluetoothctl power off".to_string()
        } else {
            "rfkill unblock bluetooth; bluetoothctl power on".to_string()
        };

        let level = (sys.vol * 100.0).round() as i32;
        let segments = (1..=20).map(|i| {
            let filled = !sys.muted && (i * 5) <= level + 2;
            div()
                .id(("vol", i as usize))
                .cursor_pointer()
                .flex_1()
                .h(px(22.))
                .flex()
                .items_center()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.last_input = std::time::Instant::now();
                    run_bg(&format!("wpctl set-volume @DEFAULT_AUDIO_SINK@ {}%", i * 5));
                    cx.notify();
                }))
                .child(
                    div()
                        .w_full()
                        .h(if filled { px(8.) } else { px(5.) })
                        .rounded_full()
                        .bg(if filled { rgb(p.accent) } else { rgba((p.dim << 8) | 0x50) }),
                )
        });

        let media_btn = |id: &'static str, glyph: &'static str, cmd: &'static str| {
            div()
                .id(id)
                .cursor_pointer()
                .size(px(34.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .font_family(NF)
                .text_size(px(18.))
                .hover(|s| s.bg(rgba((p.accent << 8) | 0x40)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.last_input = std::time::Instant::now();
                    run_bg(&format!("playerctl {cmd}"));
                    if let (Some(m), "play-pause") = (this.sys.media.as_mut(), cmd) {
                        m.playing = !m.playing; // show it now, the next poll confirms
                    }
                    cx.notify();
                }))
                .child(glyph)
        };
        let media = sys.media.as_ref().map(|m| {
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .py_2()
                .rounded_xl()
                .bg(rgba((p.dim << 8) | 0x1c))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .flex()
                        .flex_col()
                        .child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().font_weight(FontWeight::SEMIBOLD).text_size(px(14.)).child(m.title.clone()))
                        .child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(px(12.)).text_color(rgb(p.dim)).child(m.artist.clone())),
                )
                .child(media_btn("m-prev", "\u{f04ae}", "previous"))
                .child(media_btn("m-play", if m.playing { "\u{f03e4}" } else { "\u{f040a}" }, "play-pause"))
                .child(media_btn("m-next", "\u{f04ad}", "next"))
        });

        let footer_btn = |id: &'static str, label: &'static str, cmd: &'static str| {
            div()
                .id(id)
                .cursor_pointer()
                .flex_1()
                .flex()
                .justify_center()
                .py_2()
                .rounded_lg()
                .text_size(px(14.))
                .bg(rgba((p.dim << 8) | 0x28))
                .hover(|s| s.bg(rgba((p.accent << 8) | 0x40)))
                .on_click(move |_, window, cx| {
                    run_bg(&format!("setsid {cmd}"));
                    close_quick(window, cx);
                })
                .child(label)
        };

        div()
            .font_family(NF)
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                this.last_input = std::time::Instant::now();
                if ev.keystroke.key == "escape" {
                    if this.bt_view {
                        this.bt_view = false;
                        cx.notify();
                    } else {
                        close_quick(window, cx);
                    }
                }
            }))
            .flex()
            .flex_col()
            .justify_end()
            .items_end()
            .pr(px(10.0))
            .pb(px(56.))
            .on_mouse_down(MouseButton::Left, |_, window, cx| close_quick(window, cx)).on_mouse_down(MouseButton::Right, |_, window, cx| close_quick(window, cx))
            .child(
                div()
                    .w(px(380.0))
                    .h(px(if media.is_some() || bt_view { 400. } else { 330. }))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                    .flex()
                    .flex_col()
            .gap_3()
            .p_4()
            .rounded_2xl()
            .bg(rgba((p.bg << 8) | GLASS))
            .border_1()
            .border_color(rgba((p.dim << 8) | 0x70))
            .text_color(rgb(p.fg))
            .text_size(px(14.))
            .when_some(bt_page, |d, rows| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .id("q-bt-back")
                                .cursor_pointer()
                                .px_1()
                                .rounded_md()
                                .font_family(NF)
                                .text_size(px(18.))
                                .hover(|s| s.bg(rgba((p.accent << 8) | 0x40)))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.last_input = std::time::Instant::now();
                                    this.bt_view = false;
                                    cx.notify();
                                }))
                                .child("\u{f0141}"),
                        )
                        .child(div().font_weight(FontWeight::SEMIBOLD).text_size(px(16.)).child("Bluetooth")),
                )
                .child(div().id("q-bt-list").flex_1().overflow_y_scroll().flex().flex_col().gap_1().children(rows))
            })
            .when(!bt_view, |d| d
            .child(div().font_weight(FontWeight::SEMIBOLD).text_size(px(16.)).child("Quick settings"))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(tile(
                        "q-wifi",
                        g_wifi,
                        "Wi-Fi",
                        if !sys.wifi_on {
                            "Off".to_string()
                        } else if sys.ssid.is_empty() {
                            "Not connected".to_string()
                        } else {
                            sys.ssid.clone()
                        },
                        sys.wifi_on,
                        wifi_cmd,
                    ))
                    .child(
                        tile(
                            "q-bt",
                            g_bt,
                            "Bluetooth",
                            if sys.bt_on { "On" } else { "Off" }.to_string(),
                            sys.bt_on,
                            bt_cmd,
                        )
                        .child(
                            div()
                                .id("q-bt-more")
                                .ml_auto()
                                .px_1()
                                .rounded_md()
                                .font_family(NF)
                                .text_size(px(18.))
                                .text_color(rgb(p.dim))
                                .hover(|s| s.text_color(rgb(p.fg)).bg(rgba((p.accent << 8) | 0x40)))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation(); // don't also toggle the tile
                                    this.last_input = std::time::Instant::now();
                                    this.bt.refresh(read_bt());
                                    this.bt_view = true;
                                    cx.notify();
                                }))
                                .child("\u{f0142}"),
                        ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(tile(
                        "q-night",
                        "\u{f0594}",
                        "Night light",
                        if sys.night { "On" } else { "Off" }.to_string(),
                        sys.night,
                        "pkill -x hyprsunset || setsid hyprsunset".to_string(),
                    ))
                    .child(tile(
                        "q-dnd",
                        "\u{f009b}",
                        "Do not disturb",
                        if sys.dnd { "On" } else { "Off" }.to_string(),
                        sys.dnd,
                        "@dnd".to_string(),
                    )),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .rounded_xl()
                    .bg(rgba((p.dim << 8) | 0x1c))
                    .child(
                        div()
                            .id("q-mute")
                            .cursor_pointer()
                            .font_family(NF)
                            .text_size(px(22.))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.last_input = std::time::Instant::now();
                                run_bg("wpctl set-mute @DEFAULT_AUDIO_SINK@ toggle");
                                cx.notify();
                            }))
                            .child(g_vol),
                    )
                    .child(div().flex_1().flex().gap_0p5().children(segments))
                    .child(div().w(px(40.)).text_size(px(12.)).text_color(rgb(p.dim)).child(
                        if sys.muted { "Muted".to_string() } else { format!("{level}%") },
                    )),
            )
            .children(media)
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(footer_btn("q-ctrl", "Control menu", "~/.local/bin/ctrl-menu"))
                    .child(footer_btn("q-lock", "Lock", "hyprlock")),
            ))
            )
    }
}


// ---------- tray context menu (dbusmenu) ----------
struct TrayMenuState(Option<AnyWindowHandle>);
impl Global for TrayMenuState {}

fn close_tray_menu_global(cx: &mut App) -> bool {
    if let Some(handle) = cx.try_global::<TrayMenuState>().and_then(|s| s.0) {
        cx.set_global(TrayMenuState(None));
        return handle.update(cx, |_, window, _| window.remove_window()).is_ok();
    }
    false
}

fn open_tray_menu(cx: &mut App, item: tray::TrayItem) {
    let was_open = close_tray_menu_global(cx);
    if was_open {
        return;
    }
    cx.spawn(async move |cx| {
        let it = item.clone();
        let entries = cx.background_executor().spawn(async move { tray::fetch_menu(&it) }).await;
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

fn show_tray_menu(cx: &mut App, item: tray::TrayItem, entries: Vec<tray::MenuEntry>) {
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

struct TrayMenu {
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
                return div().h(px(9.)).flex().items_center().child(div().w_full().h(px(1.)).bg(rgba((p.dim << 8) | 0x40))).into_any_element();
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
                .when(e.enabled, |d| d.cursor_pointer().hover(|s| s.bg(rgba((p.accent << 8) | 0x30))))
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
        div()
            .font_family(NF)
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
            .border_color(rgba((p.dim << 8) | 0x70))
            .text_size(px(14.))
            .children(rows)
            )
    }
}


// ---------- calendar + weather ----------
#[derive(Clone)]
struct Day {
    label: String,
    code: i64,
    tmax: f64,
    tmin: f64,
    rain: i64,
}

#[derive(Clone)]
struct Weather {
    city: String,
    temp: f64,
    feels: f64,
    humidity: i64,
    wind: f64,
    code: i64,
    is_day: bool,
    days: Vec<Day>,
}

/// Weather location, in this order:
///  1. ~/.config/fiaboo-shell/weather.json  {"city": "...", "lat": 10.82, "lon": 106.63}  (manual override)
///  2. IP geolocation (ipwho.is), cached in ~/.cache/fiaboo-shell/location.json for 6 hours
///  3. Ho Chi Minh City
fn weather_location() -> (String, f64, f64) {
    let read = |path: String| -> serde_json::Value {
        fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    };
    let pick = |v: &serde_json::Value| -> Option<(String, f64, f64)> {
        Some((v["city"].as_str()?.to_string(), v["lat"].as_f64()?, v["lon"].as_f64()?))
    };
    if let Some(loc) = pick(&read(format!("{}/.config/fiaboo-shell/weather.json", home()))) {
        return loc;
    }
    let cache = format!("{}/.cache/fiaboo-shell/location.json", home());
    let fresh = fs::metadata(&cache)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < Duration::from_secs(6 * 3600));
    if fresh {
        if let Some(loc) = pick(&read(cache.clone())) {
            return loc;
        }
    }
    let geo = Command::new("curl")
        .args(["-s", "-m", "8", "https://ipwho.is/?fields=success,city,latitude,longitude"])
        .output()
        .ok()
        .and_then(|o| serde_json::from_slice::<serde_json::Value>(&o.stdout).ok())
        .filter(|v| v["success"].as_bool() == Some(true));
    if let Some(v) = geo {
        if let (Some(city), Some(lat), Some(lon)) = (v["city"].as_str(), v["latitude"].as_f64(), v["longitude"].as_f64()) {
            let _ = fs::create_dir_all(format!("{}/.cache/fiaboo-shell", home()));
            let _ = fs::write(&cache, serde_json::json!({"city": city, "lat": lat, "lon": lon}).to_string());
            return (city.to_string(), lat, lon);
        }
    }
    // network down: reuse an old cached location if there is one
    if let Some(loc) = pick(&read(cache)) {
        return loc;
    }
    ("Ho Chi Minh City".to_string(), 10.8231, 106.6297)
}

fn weekday_short(date: &str) -> String {
    let p: Vec<i32> = date.split('-').filter_map(|x| x.parse().ok()).collect();
    if p.len() != 3 {
        return String::new();
    }
    let w = weekday(p[0], p[1] as u32, p[2] as u32);
    ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"][w as usize].to_string()
}

fn fetch_weather() -> Option<Weather> {
    let (city, lat, lon) = weather_location();
    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={lat}&longitude={lon}&current=temperature_2m,apparent_temperature,relative_humidity_2m,weather_code,wind_speed_10m,is_day&daily=weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max&timezone=auto&forecast_days=5"
    );
    let out = Command::new("curl").args(["-s", "-m", "10", &url]).output().ok()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let c = &v["current"];
    let d = &v["daily"];
    let days = (0..5)
        .filter_map(|i| {
            Some(Day {
                label: weekday_short(d["time"][i].as_str()?),
                code: d["weather_code"][i].as_i64()?,
                tmax: d["temperature_2m_max"][i].as_f64()?,
                tmin: d["temperature_2m_min"][i].as_f64()?,
                rain: d["precipitation_probability_max"][i].as_i64().unwrap_or(0),
            })
        })
        .collect();
    Some(Weather {
        city,
        temp: c["temperature_2m"].as_f64()?,
        feels: c["apparent_temperature"].as_f64().unwrap_or(0.0),
        humidity: c["relative_humidity_2m"].as_i64().unwrap_or(0),
        wind: c["wind_speed_10m"].as_f64().unwrap_or(0.0),
        code: c["weather_code"].as_i64()?,
        is_day: c["is_day"].as_i64().unwrap_or(1) == 1,
        days,
    })
}

static WEATHER: Mutex<Option<(std::time::Instant, Weather)>> = Mutex::new(None);

fn cached_weather() -> Option<Weather> {
    WEATHER.lock().unwrap().as_ref().map(|(_, w)| w.clone())
}

fn weather_stale() -> bool {
    WEATHER.lock().unwrap().as_ref().is_none_or(|(t, _)| t.elapsed() > Duration::from_secs(1800))
}

fn weather_glyph(code: i64, day: bool) -> &'static str {
    match code {
        0 => if day { "\u{f0599}" } else { "\u{f0594}" },
        1 | 2 => if day { "\u{f0595}" } else { "\u{f0f31}" },
        3 => "\u{f0590}",
        45 | 48 => "\u{f0591}",
        51..=57 | 61 | 63 | 80 | 81 => "\u{f0597}",
        65 | 66 | 67 | 82 => "\u{f0596}",
        71..=77 | 85 | 86 => "\u{f0598}",
        95..=99 => "\u{f0593}",
        _ => "\u{f0590}",
    }
}

fn weather_text(code: i64) -> &'static str {
    match code {
        0 => "Clear sky",
        1 => "Mostly clear",
        2 => "Partly cloudy",
        3 => "Overcast",
        45 | 48 => "Fog",
        51..=57 => "Drizzle",
        61 | 63 | 80 | 81 => "Rain",
        65..=67 | 82 => "Heavy rain",
        71..=77 | 85 | 86 => "Snow",
        95..=99 => "Thunderstorm",
        _ => "Unknown",
    }
}

/// 0 = Sunday
fn weekday(y: i32, m: u32, d: u32) -> u32 {
    const T: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let y = if m < 3 { y - 1 } else { y };
    ((y + y / 4 - y / 100 + y / 400 + T[(m - 1) as usize] + d as i32).rem_euclid(7)) as u32
}

fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 { 29 } else { 28 },
    }
}

fn month_name(m: u32) -> &'static str {
    ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"]
        [(m - 1) as usize]
}

/// "dd/mm", the date prefix notifications are stamped with
fn today_dm() -> String {
    let (_, m, d) = today_ymd();
    format!("{d:02}/{m:02}")
}

fn today_ymd() -> (i32, u32, u32) {
    let t = sh("date +%Y-%m-%d");
    let p: Vec<i32> = t.split('-').filter_map(|x| x.parse().ok()).collect();
    if p.len() == 3 { (p[0], p[1] as u32, p[2] as u32) } else { (2026, 1, 1) }
}

struct CalState(Option<AnyWindowHandle>);
impl Global for CalState {}

fn toggle_calendar(cx: &mut App) {
    if let Some(handle) = cx.try_global::<CalState>().and_then(|s| s.0) {
        cx.set_global(CalState(None));
        if handle.update(cx, |_, window, _| window.remove_window()).is_ok() {
            return;
        }
    }
    let opened = cx.open_window(
        WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: point(px(0.), px(0.)),
                size: size(px(0.), px(0.)),
            })),
            app_id: Some("fiaboo-calendar".to_string()),
            window_background: WindowBackgroundAppearance::Transparent,
            kind: WindowKind::LayerShell(LayerShellOptions {
                namespace: "fiaboo-calendar".to_string(),
                layer: Layer::Overlay,
                anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                exclusive_zone: Some(px(-1.)),
                keyboard_interactivity: KeyboardInteractivity::Exclusive,
                ..Default::default()
            }),
            ..Default::default()
        },
        |window, cx| cx.new(|cx| CalendarPopup::new(window, cx)),
    );
    if let Ok(h) = opened {
        cx.set_global(CalState(Some(h.into())));
    }
}

fn close_calendar(window: &mut Window, cx: &mut App) {
    cx.set_global(CalState(None));
    window.remove_window();
}

struct CalendarPopup {
    pal: Palette,
    focus: FocusHandle,
    month_offset: i32,
    today: (i32, u32, u32),
    weather: Option<Weather>,
    last_input: std::time::Instant,
}

impl CalendarPopup {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        cx.spawn_in(window, async move |this, cx| loop {
            if weather_stale() {
                if let Some(w) = cx.background_executor().spawn(async { fetch_weather() }).await {
                    *WEATHER.lock().unwrap() = Some((std::time::Instant::now(), w));
                }
            }
            let idle = this
                .update(cx, |c, cx| {
                    c.weather = cached_weather();
                    cx.notify();
                    c.last_input.elapsed() > Duration::from_secs(90)
                })
                .unwrap_or(true);
            if idle {
                let _ = this.update_in(cx, |_, window, cx| close_calendar(window, cx));
                break;
            }
            cx.background_executor().timer(Duration::from_millis(1500)).await;
        })
        .detach();
        CalendarPopup {
            pal: palette(),
            focus,
            month_offset: 0,
            today: today_ymd(),
            weather: cached_weather(),
            last_input: std::time::Instant::now(),
        }
    }

    fn shown_month(&self) -> (i32, u32) {
        let total = self.today.0 * 12 + (self.today.1 as i32 - 1) + self.month_offset;
        (total.div_euclid(12), (total.rem_euclid(12) + 1) as u32)
    }
}

impl Render for CalendarPopup {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.pal.clone();
        let (year, month) = self.shown_month();

        // ---- weather card ----
        let card = match &self.weather {
            Some(w) => div()
                .flex()
                .flex_col()
                .gap_3()
                .p_4()
                .rounded_2xl()
                .bg(gpui::linear_gradient(
                    135.,
                    gpui::linear_color_stop(rgba((p.accent << 8) | 0x70), 0.),
                    gpui::linear_color_stop(rgba((p.dim << 8) | 0x20), 1.),
                ))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .child(div().text_size(px(14.)).text_color(rgb(p.fg)).child(w.city.clone()))
                                .child(
                                    div()
                                        .flex()
                                        .items_start()
                                        .child(
                                            div()
                                                .text_size(px(60.))
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .child(format!("{:.0}", w.temp)),
                                        )
                                        .child(div().text_size(px(22.)).pt_2().child("\u{b0}C")),
                                )
                                .child(div().text_size(px(16.)).child(weather_text(w.code))),
                        )
                        .child(
                            div()
                                .font_family(NF)
                                .text_size(px(78.))
                                .text_color(rgb(p.accent))
                                .child(weather_glyph(w.code, w.is_day)),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .gap_4()
                        .text_size(px(12.))
                        .text_color(rgb(p.fg))
                        .child(format!("Feels like {:.0}\u{b0}", w.feels))
                        .child(format!("Humidity {}%", w.humidity))
                        .child(format!("Wind {:.0} km/h", w.wind)),
                )
                .into_any_element(),
            None => div()
                .h(px(150.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_2xl()
                .bg(rgba((p.dim << 8) | 0x20))
                .text_color(rgb(p.dim))
                .child("Loading weather\u{2026}")
                .into_any_element(),
        };

        // ---- 5-day forecast ----
        let forecast = div().flex().gap_2().children(
            self.weather.iter().flat_map(|w| w.days.iter().cloned()).enumerate().map(|(i, d)| {
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_1()
                    .py_2()
                    .rounded_xl()
                    .bg(if i == 0 { rgba((p.accent << 8) | 0x30) } else { rgba((p.dim << 8) | 0x1c) })
                    .child(div().text_size(px(12.)).text_color(rgb(p.dim)).child(if i == 0 { "Today".to_string() } else { d.label.clone() }))
                    .child(div().font_family(NF).text_size(px(24.)).text_color(rgb(p.accent)).child(weather_glyph(d.code, true)))
                    .child(div().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).child(format!("{:.0}\u{b0}", d.tmax)))
                    .child(div().text_size(px(12.)).text_color(rgb(p.dim)).child(format!("{:.0}\u{b0}", d.tmin)))
                    .child(div().text_size(px(12.)).text_color(rgb(p.accent)).child(format!("{}%", d.rain)))
            }),
        );

        // ---- calendar ----
        let first = (weekday(year, month, 1) + 6) % 7; // Monday first
        let dim_cur = days_in_month(year, month);
        let (py, pm) = if month == 1 { (year - 1, 12) } else { (year, month - 1) };
        let dim_prev = days_in_month(py, pm);
        let nav = |id: &'static str, label: &'static str, delta: i32| {
            div()
                .id(id)
                .cursor_pointer()
                .size(px(30.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_lg()
                .text_size(px(18.))
                .hover(|s| s.bg(rgba((p.accent << 8) | 0x35)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.last_input = std::time::Instant::now();
                    this.month_offset += delta;
                    cx.notify();
                }))
                .child(label)
        };
        let weekdays = div().flex().children(["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"].iter().map(|d| {
            div().flex_1().flex().justify_center().text_size(px(12.)).text_color(rgb(p.dim)).child(*d)
        }));
        let today = self.today;
        let p2 = p.clone();
        let weeks = (0..6).map(|r| {
            let p = p2.clone();
            div().flex().children((0..7).map(move |c| {
                let idx = r * 7 + c;
                let (day, current) = if idx < first {
                    (dim_prev - (first - idx - 1), false)
                } else if idx - first < dim_cur {
                    (idx - first + 1, true)
                } else {
                    (idx - first - dim_cur + 1, false)
                };
                let is_today = current && (year, month, day) == today;
                let weekend = c >= 5;
                let lunar_txt = current.then(|| {
                    let (ld, lm, _) = lunar::solar_to_lunar(day as i32, month as i32, year);
                    if ld == 1 { format!("{ld}/{lm}") } else { ld.to_string() }
                });
                div().flex_1().h(px(40.)).flex().items_center().justify_center().child(
                    div()
                        .size(px(38.))
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .text_size(px(14.))
                        .when(is_today, |d| d.bg(rgb(p.accent)).text_color(rgb(p.bg)).font_weight(FontWeight::SEMIBOLD))
                        .when(!is_today, |d| {
                            d.text_color(if !current {
                                rgba((p.dim << 8) | 0x60)
                            } else if weekend {
                                rgb(p.accent)
                            } else {
                                rgb(p.fg)
                            })
                        })
                        .child(day.to_string())
                        .children(lunar_txt.map(|t| {
                            div().text_size(px(12.)).line_height(px(12.)).opacity(0.7).child(t)
                        })),
                )
            }))
        });

        let calendar = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .pb_1()
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap_2()
                            .child(div().text_size(px(17.)).font_weight(FontWeight::SEMIBOLD).child(month_name(month)))
                            .child(div().text_size(px(14.)).text_color(rgb(p.dim)).child(year.to_string())),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(nav("c-prev", "\u{2039}", -1))
                            .child(
                                div()
                                    .id("c-today")
                                    .cursor_pointer()
                                    .px_2()
                                    .py_1()
                                    .rounded_lg()
                                    .text_size(px(12.))
                                    .hover(|s| s.bg(rgba((p.accent << 8) | 0x35)))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.last_input = std::time::Instant::now();
                                        this.month_offset = 0;
                                        cx.notify();
                                    }))
                                    .child("Today"),
                            )
                            .child(nav("c-next", "\u{203a}", 1)),
                    ),
            )
            .child(weekdays)
            .children(weeks);

        div()
            .font_family(NF)
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                this.last_input = std::time::Instant::now();
                match ev.keystroke.key.as_str() {
                    "escape" => close_calendar(window, cx),
                    "left" => {
                        this.month_offset -= 1;
                        cx.notify();
                    }
                    "right" => {
                        this.month_offset += 1;
                        cx.notify();
                    }
                    _ => {}
                }
            }))
            .flex()
            .flex_col()
            .justify_end()
            .items_end()
            .pr(px(10.0))
            .pb(px(56.))
            .on_mouse_down(MouseButton::Left, |_, window, cx| close_calendar(window, cx)).on_mouse_down(MouseButton::Right, |_, window, cx| close_calendar(window, cx))
            .child(
                div()
                    .w(px(400.0))
                    .h(px(724.))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                    .flex()
                    .flex_col()
            .gap_3()
            .p_4()
            .rounded_2xl()
            .bg(rgba((p.bg << 8) | GLASS))
            .border_1()
            .border_color(rgba((p.dim << 8) | 0x70))
            .text_color(rgb(p.fg))
            .text_size(px(14.))
            .child(card)
            .child(forecast)
            .child(calendar)
            )
    }
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

static HYPR_DIRTY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Listen to Hyprland's event socket (.socket2) and flag the bar to refresh when windows/workspaces change,
/// instead of polling `hyprctl` several times a second.
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
            .font_family(NF)
            .size_full()
            .flex()
            .items_center()
            .gap_3()
            .px_4()
            .rounded_2xl()
            .bg(rgba((p.bg << 8) | GLASS))
            .border_1()
            .border_color(rgba((p.dim << 8) | 0x70))
            .text_color(rgb(p.fg))
            .child(div().font_family(NF).text_size(px(20.)).w(px(24.)).child(glyph))
            .child(
                div()
                    .flex_1()
                    .h(px(6.))
                    .rounded_full()
                    .bg(rgba((p.dim << 8) | 0x50))
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
    use std::io::{BufRead, BufReader};
    use std::sync::atomic::Ordering;
    std::thread::spawn(|| loop {
        let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
        let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok().or_else(|| {
            // started outside Hyprland's environment: use the newest instance
            fs::read_dir(format!("{runtime}/hypr"))
                .ok()?
                .flatten()
                .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
                .map(|e| e.file_name().to_string_lossy().to_string())
        });
        if let Some(sig) = sig {
            if let Ok(stream) = std::os::unix::net::UnixStream::connect(format!("{runtime}/hypr/{sig}/.socket2.sock")) {
                for line in BufReader::new(stream).lines().map_while(Result::ok) {
                    let ev = line.split(">>").next().unwrap_or("");
                    if matches!(
                        ev,
                        "workspace" | "workspacev2" | "focusedmon" | "focusedmonv2" | "activewindow" | "activewindowv2"
                            | "openwindow" | "closewindow" | "movewindow" | "movewindowv2" | "windowtitle"
                            | "windowtitlev2" | "createworkspace" | "createworkspacev2" | "destroyworkspace"
                            | "destroyworkspacev2" | "fullscreen" | "changefloatingmode" | "monitoradded"
                            | "monitorremoved" | "configreloaded"
                    ) {
                        HYPR_DIRTY.store(true, Ordering::Relaxed);
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_secs(2)); // Hyprland restarted or socket not up yet: reconnect
    });
}

impl Bar {
    fn new(cx: &mut Context<Self>) -> Self {
        start_hypr_events();
        start_volume_watch();
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
                let _ = this.update(cx, |bar, cx| {
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
                cx.background_executor().timer(Duration::from_millis(150)).await;
            }
        })
        .detach();
        Bar { pal: palette(), st: State::default(), sys: Sys::default(), tray: tray::items(), notif_gen: u64::MAX, notif_count: 0, recording: false, panels_open: false }
    }
}

impl Render for Bar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.pal.clone();
        let st = &self.st;
        let wave = viz::mini(cx);

        let dots = (1..=5).map(|id| {
            let is_active = id == st.active_ws;
            let used = st.occupied.contains(&id);
            div()
                .id(("ws", id as usize))
                .cursor_pointer()
                .py_2()
                .px(px(3.))
                .on_click(move |_, _, _| hypr_dispatch(&format!("hl.dsp.focus({{ workspace = {id} }})")))
                .child(div()
                .w(if is_active { px(22.) } else { px(8.) })
                .h(px(8.))
                .rounded_full()
                .bg(if is_active {
                    rgb(p.accent)
                } else if used {
                    rgb(p.dim)
                } else {
                    rgba((p.dim << 8) | 0x40)
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
                    .bg(rgba((p.dim << 8) | 0x55))
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
                .hover(|s| s.bg(rgba((p.dim << 8) | 0x30)))
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
                            rgba((p.dim << 8) | 0x80)
                        } else {
                            rgba(0x00000000)
                        }),
                )
        });

        let (clock_time, clock_date) = st.time.split_once('|').unwrap_or((st.time.as_str(), ""));
        div()
            .font_family(NF)
            .size_full()
            .relative()
            .flex()
            .flex_row()
            .items_center()
            .px_3()
            .bg(rgba((p.bg << 8) | GLASS))
            .border_t_1()
            .border_color(rgba(0xffffff2e))
            .text_color(rgb(p.fg))
            .text_size(px(14.))
            // music wave behind everything (first child paints first)
            .children(wave.map(|w| div().absolute().top_0().left_0().size_full().child(w)))
            // left: start button + workspaces
            .child(
                div()
                    .w(px(340.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .id("start-button")
                            .cursor_pointer()
                            .p_1()
                            .rounded_lg()
                            .hover(|s| s.bg(rgba((p.dim << 8) | 0x30)))
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
                            .font_family(NF)
                            .text_size(px(18.))
                            .hover(|s| s.bg(rgba((p.dim << 8) | 0x30)))
                            .on_click(|_, _, cx| toggle_taskview(cx))
                            .child("\u{f056e}"),
                    )
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
                                        hypr_dispatch(&format!("hl.dsp.focus({{ workspace = {to} }})"));
                                    }
                                }
                            })
                            .children(dots),
                    ),
            )
            // center: apps
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_center()
                    .gap_1()
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
                        .size(px(28.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_md()
                        .hover(|s| s.bg(rgba((p.dim << 8) | 0x30)))
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
                                .bg(rgba((p.dim << 8) | 0x80))
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
                                .font_family(NF)
                                .text_size(px(16.))
                                .text_color(rgb(0xf85149))
                                .hover(|s| s.bg(rgba((p.dim << 8) | 0x30)))
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
                    .px_3()
                    .py_1()
                    .rounded_lg()
                    .font_family(NF)
                    .text_size(px(16.))
                    .hover(|s| s.bg(rgba((p.dim << 8) | 0x30)))
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
                            .py_1()
                            .hover(|s| s.bg(rgba((p.dim << 8) | 0x30)))
                            .on_click(|_, _, cx| toggle_calendar(cx))
                            .flex()
                            .flex_col()
                            .items_end()
                            .px_2()
                            .child(div().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).child(clock_time.to_string()))
                            .child(div().text_size(px(12.)).text_color(rgb(p.dim)).child(clock_date.to_string())),
                    )
                    .child(
                        div()
                            .id("bell")
                            .cursor_pointer()
                            .font_family(NF)
                            .text_size(px(17.))
                            .px_2()
                            .py_1()
                            .rounded_lg()
                            .hover(|s| s.bg(rgba((p.dim << 8) | 0x30)))
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

// ---------- notifications: popups + center ----------
const CARD_H: f32 = 100.;

fn notif_card(n: &notify::Notif, p: &Palette, in_center: bool, id: usize, today: &str) -> gpui::AnyElement {
    let (nid, nid2, nid3) = (n.id, n.id, n.id);
    let icon = find_icon_file(&n.icon).or_else(|| icon_for(&n.app.to_lowercase()));
    div()
        .id(("notif", id))
        .cursor_pointer()
        .w_full()
        .h(px(CARD_H))
        .flex_none()
        .flex()
        .gap_3()
        .p_3()
        .rounded_xl()
        .bg(rgba((p.bg << 8) | if in_center { 0x00 } else { GLASS }))
        .when(in_center, |d| d.bg(rgba((p.dim << 8) | 0x22)))
        .border_1()
        .border_color(if n.critical { rgba((0xf85149 << 8) | 0xcc) } else { rgba((p.dim << 8) | 0x70) })
        .hover(|s| s.bg(rgba((p.accent << 8) | 0x30)))
        .text_color(rgb(p.fg))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_mouse_down(MouseButton::Right, move |_, _, cx| {
            cx.stop_propagation();
            notify::dismiss(nid2);
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
                .rounded_lg()
                .bg(rgba((p.dim << 8) | 0x44))
                .font_family(NF)
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
                .child(div().max_h(px(34.)).line_height(px(17.)).overflow_hidden().text_size(px(12.)).text_color(rgb(p.fg)).opacity(0.85).child(n.body.clone())),
        )
        .child(
            div()
                .id(("notif-x", id))
                .flex_none()
                .size(px(22.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_md()
                .text_color(rgb(p.dim))
                .hover(|s| s.bg(rgba((p.dim << 8) | 0x40)).text_color(rgb(p.fg)))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    if in_center { notify::dismiss(nid3) } else { notify::hide_popup(nid3) }
                })
                .child("\u{2715}"),
        )
        .into_any_element()
}

struct NotifPopupState(Option<(gpui::WindowHandle<NotifPopup>, usize)>);
impl Global for NotifPopupState {}

struct NotifPopup {
    pal: Palette,
    list: Vec<notify::Notif>,
}

impl Render for NotifPopup {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.pal.clone();
        let today = today_dm();
        div()
            .font_family(NF)
            .size_full()
            .flex()
            .flex_col()
            .gap_2()
            .text_size(px(14.))
            .children(self.list.iter().enumerate().map(|(i, n)| notif_card(n, &p, false, i, &today)))
    }
}

/// Open, resize (by reopening) or close the popup window so it matches notify::popups().
/// Any shell panel or the screenshot overlay is open.
fn panel_open(cx: &App) -> bool {
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

fn sync_notif_popups(cx: &mut App) {
    // popups would cover the open panel, or end up in a screenshot; they come back when it closes
    let list = if panel_open(cx) { Vec::new() } else { notify::popups() };
    let cur = cx.try_global::<NotifPopupState>().and_then(|s| s.0);
    if let Some((h, n)) = cur {
        if n == list.len() && h.update(cx, |v, _, cx| { v.list = list.clone(); cx.notify(); }).is_ok() {
            return;
        }
        let _ = h.update(cx, |_, window, _| window.remove_window());
        cx.set_global(NotifPopupState(None));
    }
    if list.is_empty() {
        return;
    }
    let n = list.len();
    let h = n as f32 * (CARD_H + 8.);
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
    if let Ok(h) = opened {
        cx.set_global(NotifPopupState(Some((h, n))));
    }
}

struct NotifCenterState(Option<AnyWindowHandle>);
impl Global for NotifCenterState {}

fn toggle_notif_center(cx: &mut App) {
    if let Some(handle) = cx.try_global::<NotifCenterState>().and_then(|s| s.0) {
        cx.set_global(NotifCenterState(None));
        if handle.update(cx, |_, window, _| window.remove_window()).is_ok() {
            return;
        }
    }
    notify::mark_seen();
    let opened = cx.open_window(
        WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(Bounds { origin: point(px(0.), px(0.)), size: size(px(0.), px(0.)) })),
            app_id: Some("fiaboo-notifcenter".to_string()),
            window_background: WindowBackgroundAppearance::Transparent,
            kind: WindowKind::LayerShell(LayerShellOptions {
                namespace: "fiaboo-notifcenter".to_string(),
                layer: Layer::Overlay,
                anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                exclusive_zone: Some(px(-1.)),
                keyboard_interactivity: KeyboardInteractivity::Exclusive,
                ..Default::default()
            }),
            ..Default::default()
        },
        |window, cx| cx.new(|cx| NotifCenter::new(window, cx)),
    );
    if let Ok(h) = opened {
        cx.set_global(NotifCenterState(Some(h.into())));
    }
}

fn close_notif_center(window: &mut Window, cx: &mut App) {
    cx.set_global(NotifCenterState(None));
    window.remove_window();
}

struct NotifCenter {
    pal: Palette,
    focus: FocusHandle,
    gen: u64,
    confirm_clear: bool, // "Clear all" asks once before deleting every notification
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
        NotifCenter { pal: palette(), focus, gen: notify::generation(), confirm_clear: false }
    }
}

impl Render for NotifCenter {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.pal.clone();
        let list = notify::all();
        let today = today_dm();
        let dnd = notify::dnd();
        let pill = |id: &'static str, label: String, on: bool| {
            div()
                .id(id)
                .cursor_pointer()
                .px_3()
                .py_1()
                .rounded_lg()
                .text_size(px(12.))
                .bg(if on { rgba((p.accent << 8) | 0x40) } else { rgba((p.dim << 8) | 0x28) })
                .hover(|s| s.bg(rgba((p.accent << 8) | 0x50)))
                .child(label)
        };
        div()
            .font_family(NF)
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|_, ev: &KeyDownEvent, window, cx| {
                if ev.keystroke.key == "escape" {
                    close_notif_center(window, cx);
                }
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
                    .max_h(px(620.))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                    .flex()
                    .flex_col()
                    .gap_3()
                    .p_4()
                    .rounded_2xl()
                    .bg(rgba((p.bg << 8) | GLASS))
                    .border_1()
                    .border_color(rgba((p.dim << 8) | 0x70))
                    .text_color(rgb(p.fg))
                    .text_size(px(14.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).text_size(px(16.)).child("Notifications"))
                            .child(pill("nc-dnd", if dnd { "\u{f009b} DND".into() } else { "\u{f009c} DND".into() }, dnd).on_click(
                                cx.listener(|_, _, _, cx| {
                                    notify::toggle_dnd();
                                    cx.notify();
                                }),
                            ))
                            .children(if self.confirm_clear {
                                let n = notify::all().len();
                                vec![
                                    pill("nc-keep", "Keep".into(), false).on_click(cx.listener(|this, _, _, cx| {
                                        this.confirm_clear = false;
                                        cx.notify();
                                    })),
                                    pill("nc-clear-yes", if n == 1 { "Delete 1".into() } else { format!("Delete all {n}") }, false)
                                        .bg(rgba((0xf85149 << 8) | 0xcc))
                                        .text_color(rgb(0xffffff))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            notify::clear_all();
                                            this.confirm_clear = false;
                                            cx.notify();
                                        })),
                                ]
                            } else {
                                vec![pill("nc-clear", "Clear all".into(), false).on_click(cx.listener(|this, _, _, cx| {
                                    this.confirm_clear = true;
                                    cx.notify();
                                }))]
                            }),
                    )
                    .child(
                        div()
                            .id("nc-list")
                            .flex_1()
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .children(list.iter().enumerate().map(|(i, n)| notif_card(n, &p, true, i, &today)))
                            .when(list.is_empty(), |d| {
                                d.child(div().py_6().flex().justify_center().text_color(rgb(p.dim)).child("No notifications"))
                            }),
                    ),
            )
    }
}

// ---------- clipboard history (cliphist) ----------
struct ClipState(Option<AnyWindowHandle>);
impl Global for ClipState {}

#[derive(Clone)]
struct Clip {
    id: String,
    raw: String,     // the full cliphist line, needed by `cliphist delete`
    preview: String, // text, whitespace collapsed
    image: Option<PathBuf>,
}

fn read_clips() -> Vec<Clip> {
    let cache = format!("{}/.cache/fiaboo-shell", home());
    let _ = fs::create_dir_all(&cache);
    let mut images = 0;
    sh("cliphist list")
        .lines()
        .take(80)
        .filter_map(|line| {
            let (id, rest) = line.split_once('\t')?;
            if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let is_img = rest.starts_with("[[ binary data") && (rest.contains("png") || rest.contains("jpg") || rest.contains("jpeg"));
            let image = if is_img && images < 8 {
                images += 1;
                let path = PathBuf::from(format!("{cache}/clip-{id}.img"));
                if !path.exists() {
                    let _ = sh(&format!("cliphist decode {id} > '{}'", path.display()));
                }
                Some(path)
            } else {
                None
            };
            let preview = if is_img { "Image".to_string() } else { rest.split_whitespace().collect::<Vec<_>>().join(" ") };
            Some(Clip { id: id.to_string(), raw: line.to_string(), preview, image })
        })
        .collect()
}

fn clip_copy(id: &str) {
    if id.bytes().all(|b| b.is_ascii_digit()) {
        run_bg(&format!("cliphist decode {id} | wl-copy"));
    }
}

fn clip_delete(raw: &str) {
    use std::io::Write;
    if let Ok(mut child) = Command::new("cliphist").arg("delete").stdin(std::process::Stdio::piped()).spawn() {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = writeln!(stdin, "{raw}");
        }
        let _ = child.wait();
    }
}

fn toggle_clip(cx: &mut App) {
    if let Some(handle) = cx.try_global::<ClipState>().and_then(|s| s.0) {
        cx.set_global(ClipState(None));
        if handle.update(cx, |_, window, _| window.remove_window()).is_ok() {
            return;
        }
    }
    let opened = cx.open_window(
        WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(Bounds { origin: point(px(0.), px(0.)), size: size(px(0.), px(0.)) })),
            app_id: Some("fiaboo-clip".to_string()),
            window_background: WindowBackgroundAppearance::Transparent,
            kind: WindowKind::LayerShell(LayerShellOptions {
                namespace: "fiaboo-clip".to_string(),
                layer: Layer::Overlay,
                anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                exclusive_zone: Some(px(-1.)),
                keyboard_interactivity: KeyboardInteractivity::Exclusive,
                ..Default::default()
            }),
            ..Default::default()
        },
        |window, cx| cx.new(|cx| ClipPanel::new(window, cx)),
    );
    if let Ok(h) = opened {
        cx.set_global(ClipState(Some(h.into())));
    }
}

fn close_clip(window: &mut Window, cx: &mut App) {
    cx.set_global(ClipState(None));
    window.remove_window();
}

struct ClipPanel {
    pal: Palette,
    focus: FocusHandle,
    clips: Vec<Clip>,
    query: String,
    sel: usize,
    last_input: std::time::Instant,
    confirm_wipe: bool, // "Clear all" asks once before deleting the whole history
}

impl ClipPanel {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        // exclusive keyboard focus: close after 90 s without a key press
        cx.spawn_in(window, async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let idle = this.update(cx, |c, _| c.last_input.elapsed() > Duration::from_secs(90)).unwrap_or(true);
            if idle {
                let _ = this.update_in(cx, |_, window, cx| close_clip(window, cx));
                break;
            }
        })
        .detach();
        ClipPanel { pal: palette(), focus, clips: read_clips(), query: String::new(), sel: 0, last_input: std::time::Instant::now(), confirm_wipe: false }
    }

    fn visible(&self) -> Vec<Clip> {
        let q = self.query.to_lowercase();
        self.clips.iter().filter(|c| c.preview.to_lowercase().contains(&q)).take(40).cloned().collect()
    }
}

impl Render for ClipPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.pal.clone();
        let small_btn = |id: &'static str, label: String| {
            div()
                .id(id)
                .cursor_pointer()
                .px_3()
                .py_2()
                .rounded_lg()
                .text_size(px(12.))
                .bg(rgba((p.dim << 8) | 0x28))
                .hover(|s| s.bg(rgba((p.accent << 8) | 0x50)))
                .child(label)
        };
        let list = self.visible();
        let sel = self.sel.min(list.len().saturating_sub(1));
        let rows = list.iter().enumerate().map(|(i, c)| {
            let (copy_id, del_raw) = (c.id.clone(), c.raw.clone());
            let id = c.id.clone();
            div()
                .id(("clip", i))
                .cursor_pointer()
                .flex_none()
                .flex()
                .items_center()
                .gap_3()
                .px_3()
                .py_2()
                .rounded_lg()
                .when(i == sel, |d| d.bg(rgba((p.accent << 8) | 0x30)))
                .hover(|s| s.bg(rgba((p.dim << 8) | 0x30)))
                .on_mouse_down(MouseButton::Right, cx.listener(move |this, _, _, cx| {
                    clip_delete(&del_raw);
                    this.clips.retain(|c| c.id != id);
                    cx.notify();
                }))
                .on_click(move |_, window, cx| {
                    clip_copy(&copy_id);
                    close_clip(window, cx);
                })
                .child(match &c.image {
                    Some(path) => img(path.clone()).h(px(64.)).max_w(px(300.)).object_fit(ObjectFit::Contain).into_any_element(),
                    None => div()
                        .flex_1()
                        .min_w(px(0.))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(c.preview.clone())
                        .into_any_element(),
                })
        });
        div()
            .font_family(NF)
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                this.last_input = std::time::Instant::now();
                let k = &ev.keystroke;
                let n = this.visible().len();
                match k.key.as_str() {
                    "escape" => close_clip(window, cx),
                    "backspace" => {
                        this.query.pop();
                        this.sel = 0;
                        cx.notify();
                    }
                    "down" if n > 0 => {
                        this.sel = (this.sel + 1) % n;
                        cx.notify();
                    }
                    "up" if n > 0 => {
                        this.sel = (this.sel + n - 1) % n;
                        cx.notify();
                    }
                    "enter" => {
                        if let Some(c) = this.visible().get(this.sel) {
                            clip_copy(&c.id);
                        }
                        close_clip(window, cx);
                    }
                    "delete" => {
                        if let Some(c) = this.visible().get(this.sel).cloned() {
                            clip_delete(&c.raw);
                            this.clips.retain(|x| x.id != c.id);
                            cx.notify();
                        }
                    }
                    _ => {
                        if !k.modifiers.control && !k.modifiers.alt && !k.modifiers.platform {
                            if let Some(ch) = &k.key_char {
                                this.query.push_str(ch);
                                this.sel = 0;
                                cx.notify();
                            }
                        }
                    }
                }
            }))
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(MouseButton::Left, |_, window, cx| close_clip(window, cx))
            .on_mouse_down(MouseButton::Right, |_, window, cx| close_clip(window, cx))
            .child(
                div()
                    .w(px(560.))
                    .h(px(560.))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                    .flex()
                    .flex_col()
                    .gap_3()
                    .p_4()
                    .rounded_2xl()
                    .bg(rgba((p.bg << 8) | GLASS))
                    .border_1()
                    .border_color(rgba((p.dim << 8) | 0x70))
                    .text_color(rgb(p.fg))
                    .text_size(px(14.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .h(px(40.))
                                    .flex()
                                    .items_center()
                                    .px_4()
                                    .rounded_xl()
                                    .bg(rgba((p.dim << 8) | 0x28))
                                    .gap_2()
                                    .child(div().text_color(rgb(p.dim)).child("\u{f002}"))
                                    .child(if self.query.is_empty() {
                                        div().text_color(rgb(p.dim)).child("Search clipboard history…")
                                    } else {
                                        div().child(format!("{}\u{258f}", self.query))
                                    }),
                            )
                            .children(if self.confirm_wipe {
                                let n = self.clips.len();
                                vec![
                                    small_btn("clip-keep", "Keep".into()).on_click(cx.listener(|this, _, _, cx| {
                                        this.confirm_wipe = false;
                                        cx.notify();
                                    })),
                                    small_btn("clip-wipe-yes", if n == 1 { "Delete 1 item".to_string() } else { format!("Delete {n} items") })
                                        .bg(rgba((0xf85149 << 8) | 0xcc))
                                        .text_color(rgb(0xffffff))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            run_bg("cliphist wipe");
                                            this.clips.clear();
                                            this.confirm_wipe = false;
                                            cx.notify();
                                        })),
                                ]
                            } else {
                                vec![small_btn("clip-wipe", "Clear all".into()).on_click(cx.listener(|this, _, _, cx| {
                                    this.confirm_wipe = true;
                                    cx.notify();
                                }))]
                            }),
                    )
                    .child(
                        div()
                            .id("clip-list")
                            .flex_1()
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .children(rows)
                            .when(list.is_empty(), |d| {
                                d.child(div().py_6().flex().justify_center().text_color(rgb(p.dim)).child("Nothing here"))
                            }),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(p.dim))
                            .child("Enter: copy   ·   Right-click / Del: remove   ·   Esc: close"),
                    ),
            )
    }
}

// ---------- screenshot: drag = region, click = window under the pointer, F = whole screen ----------
struct ShotState(Option<AnyWindowHandle>);
impl Global for ShotState {}

#[derive(Clone)]
struct WinRect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    title: String,
}

/// (x, y, w, h) of the focused monitor, and the windows visible on it (front-most first).
fn shot_geometry() -> ((i32, i32, i32, i32), Vec<WinRect>) {
    let mons = hyprctl_json("monitors");
    let mon = mons
        .as_array()
        .and_then(|m| m.iter().find(|m| m["focused"].as_bool() == Some(true)).or_else(|| m.first()))
        .cloned()
        .unwrap_or_default();
    let g = |k: &str| mon[k].as_i64().unwrap_or(0) as i32;
    let ws = mon["activeWorkspace"]["id"].as_i64().unwrap_or(1);
    let (mx, my) = (g("x"), g("y"));
    let mut clients: Vec<serde_json::Value> = hyprctl_json("clients").as_array().cloned().unwrap_or_default();
    clients.retain(|c| {
        c["workspace"]["id"].as_i64() == Some(ws)
            && c["mapped"].as_bool() != Some(false)
            && c["hidden"].as_bool() != Some(true)
            && !c["class"].as_str().unwrap_or("").starts_with("fiaboo")
    });
    clients.sort_by_key(|c| c["focusHistoryID"].as_i64().unwrap_or(i64::MAX));
    let wins = clients
        .iter()
        .map(|c| WinRect {
            x: c["at"][0].as_f64().unwrap_or(0.) as f32 - mx as f32,
            y: c["at"][1].as_f64().unwrap_or(0.) as f32 - my as f32,
            w: c["size"][0].as_f64().unwrap_or(0.) as f32,
            h: c["size"][1].as_f64().unwrap_or(0.) as f32,
            title: c["title"].as_str().unwrap_or("").to_string(),
        })
        .collect();
    ((mx, my, g("width"), g("height")), wins)
}

fn toggle_shot(cx: &mut App) {
    if let Some(handle) = cx.try_global::<ShotState>().and_then(|s| s.0) {
        cx.set_global(ShotState(None));
        if handle.update(cx, |_, window, _| window.remove_window()).is_ok() {
            return;
        }
    }
    let opened = cx.open_window(
        WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(Bounds { origin: point(px(0.), px(0.)), size: size(px(0.), px(0.)) })),
            app_id: Some("fiaboo-shot".to_string()),
            window_background: WindowBackgroundAppearance::Transparent,
            kind: WindowKind::LayerShell(LayerShellOptions {
                namespace: "fiaboo-shot".to_string(),
                layer: Layer::Overlay,
                anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                exclusive_zone: Some(px(-1.)),
                keyboard_interactivity: KeyboardInteractivity::Exclusive,
                ..Default::default()
            }),
            ..Default::default()
        },
        |window, cx| cx.new(|cx| ShotOverlay::new(window, cx)),
    );
    if let Ok(h) = opened {
        cx.set_global(ShotState(Some(h.into())));
    }
}

fn close_shot(window: &mut Window, cx: &mut App) {
    cx.set_global(ShotState(None));
    window.remove_window();
}

/// Close the overlay, wait for the compositor to remove it, then grab `rect` (global coords) with grim.
fn take_shot(window: &mut Window, cx: &mut App, rect: Option<(i32, i32, i32, i32)>) {
    close_shot(window, cx);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(350));
        let dir = format!("{}/Pictures/shots", home());
        let _ = fs::create_dir_all(&dir);
        let file = format!("{dir}/shot-{}.png", sh("date +%Y%m%d-%H%M%S"));
        let geo = rect.map(|(x, y, w, h)| format!("-g '{x},{y} {w}x{h}'")).unwrap_or_default();
        let script = format!(
            "grim {geo} '{file}' && wl-copy < '{file}' && \
             case \"$(notify-send -a Screenshot -i '{file}' -A default=Open -A edit=Edit -t 6000 'Screenshot copied' '{file}')\" in \
             default) xdg-open '{file}' ;; \
             edit) satty --filename '{file}' --output-filename '{file}' --copy-command wl-copy --early-exit ;; esac"
        );
        let _ = Command::new("setsid").args(["sh", "-c", &script]).spawn();
    });
}

struct ShotOverlay {
    pal: Palette,
    focus: FocusHandle,
    mon: (i32, i32, i32, i32),
    wins: Vec<WinRect>,
    start: Option<gpui::Point<gpui::Pixels>>,
    cur: gpui::Point<gpui::Pixels>,
    last_input: std::time::Instant,
}

impl ShotOverlay {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        cx.spawn_in(window, async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let idle = this.update(cx, |o, _| o.last_input.elapsed() > Duration::from_secs(60)).unwrap_or(true);
            if idle {
                let _ = this.update_in(cx, |_, window, cx| close_shot(window, cx));
                break;
            }
        })
        .detach();
        let (mon, wins) = shot_geometry();
        ShotOverlay { pal: palette(), focus, mon, wins, start: None, cur: point(px(0.), px(0.)), last_input: std::time::Instant::now() }
    }

    fn hovered(&self) -> Option<&WinRect> {
        let (x, y) = (f32::from(self.cur.x), f32::from(self.cur.y));
        self.wins.iter().find(|w| x >= w.x && x < w.x + w.w && y >= w.y && y < w.y + w.h)
    }

    /// Selection rectangle in window coords while dragging.
    fn drag_rect(&self) -> Option<(f32, f32, f32, f32)> {
        let s = self.start?;
        let (sx, sy, cx_, cy) = (f32::from(s.x), f32::from(s.y), f32::from(self.cur.x), f32::from(self.cur.y));
        Some((sx.min(cx_), sy.min(cy), (sx - cx_).abs(), (sy - cy).abs()))
    }
}

impl Render for ShotOverlay {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.pal.clone();
        let dim = rgba(0x00000066);
        // the rectangle that stays bright: the drag, otherwise the hovered window
        let drag = self.drag_rect().filter(|(_, _, w, h)| *w > 4. || *h > 4.);
        let hole = drag.or_else(|| self.hovered().map(|w| (w.x, w.y, w.w, w.h)));
        let (mw, mh) = (self.mon.2 as f32, self.mon.3 as f32);
        let label = match (&drag, self.hovered()) {
            (Some((_, _, w, h)), _) => format!("{} × {}", *w as i32, *h as i32),
            (None, Some(win)) => format!("{}  ({} × {})", win.title, win.w as i32, win.h as i32),
            _ => "Drag to select a region · click a window · F for the whole screen".to_string(),
        };
        let mut root = div().font_family(NF)
            .size_full()
            .relative()
            .cursor_crosshair()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                this.last_input = std::time::Instant::now();
                match ev.keystroke.key.as_str() {
                    "escape" => close_shot(window, cx),
                    "f" | "enter" => take_shot(window, cx, Some(this.mon)),
                    _ => {}
                }
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, ev: &gpui::MouseDownEvent, _, cx| {
                this.start = Some(ev.position);
                this.cur = ev.position;
                cx.notify();
            }))
            .on_mouse_move(cx.listener(|this, ev: &gpui::MouseMoveEvent, _, cx| {
                this.last_input = std::time::Instant::now();
                this.cur = ev.position;
                cx.notify();
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, ev: &gpui::MouseUpEvent, window, cx| {
                this.cur = ev.position;
                let (mx, my, _, _) = this.mon;
                let global = |x: f32, y: f32, w: f32, h: f32| Some((mx + x as i32, my + y as i32, w.max(1.) as i32, h.max(1.) as i32));
                let rect = match this.drag_rect() {
                    Some((x, y, w, h)) if w > 4. || h > 4. => global(x, y, w, h),
                    _ => this.hovered().and_then(|w| global(w.x, w.y, w.w, w.h)),
                };
                this.start = None;
                match rect {
                    Some(r) => take_shot(window, cx, Some(r)),
                    None => cx.notify(),
                }
            }))
            .on_mouse_down(MouseButton::Right, |_, window, cx| close_shot(window, cx));
        match hole {
            None => root = root.child(div().absolute().top_0().left_0().size_full().bg(dim)),
            Some((x, y, w, h)) => {
                let (x, y) = (x.max(0.), y.max(0.));
                let (w, h) = (w.min(mw - x), h.min(mh - y));
                root = root
                    .child(div().absolute().left_0().top_0().w(px(mw)).h(px(y)).bg(dim))
                    .child(div().absolute().left_0().top(px(y + h)).w(px(mw)).h(px((mh - y - h).max(0.))).bg(dim))
                    .child(div().absolute().left_0().top(px(y)).w(px(x)).h(px(h)).bg(dim))
                    .child(div().absolute().left(px(x + w)).top(px(y)).w(px((mw - x - w).max(0.))).h(px(h)).bg(dim))
                    .child(
                        div()
                            .absolute()
                            .left(px(x))
                            .top(px(y))
                            .w(px(w))
                            .h(px(h))
                            .border_2()
                            .border_color(rgb(p.accent)),
                    );
            }
        }
        root.child(
            div()
                .absolute()
                .bottom(px(70.))
                .left_0()
                .w_full()
                .flex()
                .justify_center()
                .child(
                    div()
                        .px_4()
                        .py_2()
                        .rounded_full()
                        .bg(rgba((p.bg << 8) | GLASS))
                        .border_1()
                        .border_color(rgba((p.dim << 8) | 0x50))
                        .text_color(rgb(p.fg))
                        .text_size(px(14.))
                        .child(label),
                ),
        )
    }
}

// ---------- wallpaper picker ----------
struct WpState(Option<AnyWindowHandle>);
impl Global for WpState {}

const WP_COLS: usize = 4;

fn wp_dir() -> String {
    format!("{}/Pictures/wallpapers", home())
}

fn wp_list() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(wp_dir())
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    v.retain(|p| {
        p.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| matches!(e.to_lowercase().as_str(), "jpg" | "jpeg" | "png" | "webp"))
    });
    v.sort();
    v
}

/// Small cached copy of a wallpaper for the grid (full-size images are far too heavy to draw 100 of).
fn wp_thumb(src: &std::path::Path) -> PathBuf {
    let name = src.file_stem().and_then(|s| s.to_str()).unwrap_or("wp");
    PathBuf::from(format!("{}/.cache/fiaboo-shell/wp/{name}.jpg", home()))
}

fn wp_make_thumbs(list: Vec<PathBuf>) {
    let _ = fs::create_dir_all(format!("{}/.cache/fiaboo-shell/wp", home()));
    for src in list {
        let t = wp_thumb(&src);
        if !t.exists() {
            let _ = Command::new("magick")
                .arg(format!("{}[0]", src.display()))
                .args(["-thumbnail", "400x225^", "-gravity", "center", "-extent", "400x225", "-quality", "82"])
                .arg(&t)
                .status();
        }
    }
}

fn wp_current() -> String {
    fs::read_to_string(format!("{}/.cache/fiaboo-shell/wallpaper/current", home())).unwrap_or_default().trim().to_string()
}

fn wp_apply(path: &std::path::Path) {
    run_bg(&format!("setsid ~/.local/bin/fiaboo-wallpaper '{}'", path.display()));
}

fn toggle_wp(cx: &mut App) {
    if let Some(handle) = cx.try_global::<WpState>().and_then(|s| s.0) {
        cx.set_global(WpState(None));
        if handle.update(cx, |_, window, _| window.remove_window()).is_ok() {
            return;
        }
    }
    let opened = cx.open_window(
        WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(Bounds { origin: point(px(0.), px(0.)), size: size(px(0.), px(0.)) })),
            app_id: Some("fiaboo-wallpaper".to_string()),
            window_background: WindowBackgroundAppearance::Transparent,
            kind: WindowKind::LayerShell(LayerShellOptions {
                namespace: "fiaboo-wallpaper".to_string(),
                layer: Layer::Overlay,
                anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                exclusive_zone: Some(px(-1.)),
                keyboard_interactivity: KeyboardInteractivity::Exclusive,
                ..Default::default()
            }),
            ..Default::default()
        },
        |window, cx| cx.new(|cx| WpPicker::new(window, cx)),
    );
    if let Ok(h) = opened {
        cx.set_global(WpState(Some(h.into())));
    }
}

fn close_wp(window: &mut Window, cx: &mut App) {
    cx.set_global(WpState(None));
    window.remove_window();
}

struct WpPicker {
    pal: Palette,
    focus: FocusHandle,
    list: Vec<PathBuf>,
    current: String,
    sel: usize,
    scroll: gpui::ScrollHandle,
    last_input: std::time::Instant,
}

impl WpPicker {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let list = wp_list();
        let current = wp_current();
        let sel = list.iter().position(|p| p.to_string_lossy() == current).unwrap_or(0);
        let todo = list.clone();
        std::thread::spawn(move || wp_make_thumbs(todo));
        // thumbnails appear as the background thread produces them; also the idle safety net
        cx.spawn_in(window, async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_millis(700)).await;
            let keep = this
                .update_in(cx, |p, window, cx| {
                    if p.last_input.elapsed() > Duration::from_secs(120) {
                        close_wp(window, cx);
                        return false;
                    }
                    if p.list.iter().any(|s| !wp_thumb(s).exists()) {
                        cx.notify();
                    }
                    true
                })
                .unwrap_or(false);
            if !keep {
                break;
            }
        })
        .detach();
        let scroll = gpui::ScrollHandle::new();
        WpPicker { pal: palette(), focus, list, current, sel, scroll, last_input: std::time::Instant::now() }
    }
}

impl Render for WpPicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.pal.clone();
        let sel = self.sel;
        let cells = self.list.iter().enumerate().map(|(i, path)| {
            let thumb = wp_thumb(path);
            let target = path.clone();
            let is_cur = path.to_string_lossy() == self.current;
            div()
                .id(("wp", i))
                .cursor_pointer()
                .w(px(190.))
                .h(px(107.))
                .flex_none()
                .rounded_lg()
                .overflow_hidden()
                .bg(rgba((p.dim << 8) | 0x30))
                .border_2()
                .border_color(if i == sel {
                    rgb(p.accent)
                } else if is_cur {
                    rgba((p.accent << 8) | 0x70)
                } else {
                    rgba(0x00000000)
                })
                .on_click(move |_, window, cx| {
                    wp_apply(&target);
                    close_wp(window, cx);
                })
                .when(thumb.exists(), |d| d.child(img(thumb).size_full().object_fit(ObjectFit::Cover)))
        });
        div()
            .font_family(NF)
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                this.last_input = std::time::Instant::now();
                let n = this.list.len();
                let mut go = |this: &mut Self, to: usize, cx: &mut Context<Self>| {
                    this.sel = to;
                    this.scroll.scroll_to_item(to);
                    cx.notify();
                };
                match ev.keystroke.key.as_str() {
                    "escape" => close_wp(window, cx),
                    "right" if n > 0 => go(this, (this.sel + 1) % n, cx),
                    "left" if n > 0 => go(this, (this.sel + n - 1) % n, cx),
                    "down" if n > 0 => go(this, (this.sel + WP_COLS).min(n - 1), cx),
                    "up" if n > 0 => go(this, this.sel.saturating_sub(WP_COLS), cx),
                    "r" => {
                        run_bg("setsid ~/.local/bin/fiaboo-wallpaper --random");
                        close_wp(window, cx);
                    }
                    "enter" => {
                        if let Some(path) = this.list.get(this.sel) {
                            wp_apply(path);
                        }
                        close_wp(window, cx);
                    }
                    _ => {}
                }
            }))
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(MouseButton::Left, |_, window, cx| close_wp(window, cx))
            .on_mouse_down(MouseButton::Right, |_, window, cx| close_wp(window, cx))
            .child(
                div()
                    .w(px(840.))
                    .h(px(640.))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                    .flex()
                    .flex_col()
                    .gap_3()
                    .p_4()
                    .rounded_2xl()
                    .bg(rgba((p.bg << 8) | GLASS))
                    .border_1()
                    .border_color(rgba((p.dim << 8) | 0x70))
                    .text_color(rgb(p.fg))
                    .text_size(px(14.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).text_size(px(16.)).child("Wallpaper"))
                            .child(
                                div()
                                    .id("wp-random")
                                    .cursor_pointer()
                                    .px_3()
                                    .py_1()
                                    .rounded_lg()
                                    .text_size(px(12.))
                                    .bg(rgba((p.dim << 8) | 0x28))
                                    .hover(|s| s.bg(rgba((p.accent << 8) | 0x50)))
                                    .on_click(|_, window, cx| {
                                        run_bg("setsid ~/.local/bin/fiaboo-wallpaper --random");
                                        close_wp(window, cx);
                                    })
                                    .child("Shuffle"),
                            ),
                    )
                    .child(
                        div()
                            .id("wp-grid")
                            .flex_1()
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll)
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .content_start()
                            .gap_2()
                            .children(cells),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(p.dim))
                            .child(format!("{} images   ·   Arrows + Enter or click: apply   ·   R: shuffle   ·   Esc: close", self.list.len())),
                    ),
            )
    }
}

// ---------- settings ----------
struct SettingsState(Option<AnyWindowHandle>);
impl Global for SettingsState {}

#[derive(Clone, Default)]
struct SettingsInfo {
    net: Vec<(String, String)>,          // (device · type, "state · connection · ip")
    sinks: Vec<(String, String, bool)>,  // (name, description, default)
    displays: Vec<String>,
    about: Vec<(&'static str, String)>,
}

/// `bluetoothctl devices …` lines: "Device <MAC> <name>" -> (mac, name)
fn bt_device_lines(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let mut it = l.strip_prefix("Device ")?.splitn(2, ' ');
            let mac = it.next()?.to_string();
            // the MAC goes into shell commands, so accept only AA:BB:CC:DD:EE:FF
            let valid = mac.len() == 17 && mac.char_indices().all(|(i, c)| if i % 3 == 2 { c == ':' } else { c.is_ascii_hexdigit() });
            valid.then(|| (mac.clone(), it.next().unwrap_or(&mac).to_string()))
        })
        .collect()
}

/// Split one `nmcli -t` line: fields are ':'-separated, literal ':' is escaped as "\:".
fn nmcli_fields(line: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.last_mut().unwrap().extend(chars.next()),
            ':' => out.push(String::new()),
            _ => out.last_mut().unwrap().push(c),
        }
    }
    out
}

fn read_settings() -> SettingsInfo {
    let net = sh("nmcli -t -f DEVICE,TYPE,STATE,CONNECTION device")
        .lines()
        .map(nmcli_fields)
        .filter(|f| f.len() == 4 && f[1] != "loopback" && f[1] != "wifi-p2p")
        .map(|f| {
            let ip = sh(&format!("nmcli -g IP4.ADDRESS device show '{}' | head -1", f[0]));
            let detail = [f[2].as_str(), f[3].as_str(), ip.as_str()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>();
            (format!("{} · {}", f[0], f[1]), detail.join(" · "))
        })
        .collect();
    let default_sink = sh("pactl get-default-sink");
    let sinks = serde_json::from_str::<serde_json::Value>(&sh("pactl -f json list sinks"))
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|s| {
            let name = s["name"].as_str()?.to_string();
            let desc = s["description"].as_str().unwrap_or(&name).to_string();
            let is_default = name == default_sink;
            Some((name, desc, is_default))
        })
        .collect();
    let displays = hyprctl_json("monitors")
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|m| {
            format!(
                "{} · {}x{} @ {:.0} Hz · scale {}",
                m["name"].as_str().unwrap_or("?"),
                m["width"],
                m["height"],
                m["refreshRate"].as_f64().unwrap_or(0.0),
                m["scale"]
            )
        })
        .collect();
    let about = vec![
        ("Host", sh("cat /etc/hostname")),
        ("Kernel", sh("uname -r")),
        ("Uptime", sh("uptime -p").trim_start_matches("up ").to_string()),
        ("Memory", sh("free -h | awk '/^Mem/ {print $3 \" / \" $2}'")),
        ("Disk /", sh("df -h / | awk 'NR==2 {print $3 \" / \" $2}'")),
    ];
    SettingsInfo { net, sinks, displays, about }
}

// ---------- bluetooth list (Settings and the quick settings › page) ----------
#[derive(Clone, Default)]
struct BtInfo {
    on: bool,
    paired: Vec<(String, String, bool)>, // (mac, name, connected)
    found: Vec<(String, String)>,        // (mac, name) known to bluez but not paired, named only
}

fn read_bt() -> BtInfo {
    let connected = bt_device_lines(&sh("bluetoothctl devices Connected"));
    let paired: Vec<_> = bt_device_lines(&sh("bluetoothctl devices Paired"))
        .into_iter()
        .map(|(mac, name)| {
            let on = connected.iter().any(|c| c.0 == mac);
            (mac, name, on)
        })
        .collect();
    let found = bt_device_lines(&sh("bluetoothctl devices"))
        .into_iter()
        .filter(|(mac, name)| name.replace('-', ":") != *mac && !paired.iter().any(|p| p.0 == *mac))
        .collect();
    BtInfo { on: sh("bluetoothctl show | grep -c 'Powered: yes'") == "1", paired, found }
}

/// Current on/off of a Bluetooth row: "power" or a device MAC (on = connected).
fn bt_state(info: &BtInfo, key: &str) -> Option<bool> {
    if key == "power" {
        return Some(info.on);
    }
    info.paired.iter().find(|d| d.0 == key).map(|d| d.2)
}

const BT_SCAN_SECS: u64 = 20;

#[derive(Default)]
struct BtUi {
    info: BtInfo,
    /// Row clicked and not settled yet: (row key, target state, since)
    busy: Option<(String, bool, std::time::Instant)>,
    /// End of the current scan; Some once a scan was started in this panel (then found devices show).
    scan_until: Option<std::time::Instant>,
}

impl BtUi {
    fn refresh(&mut self, info: BtInfo) {
        self.info = info;
        if let Some((key, want, since)) = &self.busy {
            // pair + trust + connect can take a while
            if bt_state(&self.info, key) == Some(*want) || since.elapsed() > Duration::from_secs(30) {
                self.busy = None;
            }
        }
    }
}

/// A panel that shows `bt_rows`.
trait HasBt: 'static {
    fn bt(&mut self) -> &mut BtUi;
    fn touch(&mut self);
}

/// Power, paired devices (click: connect/disconnect), scan, found devices (click: pair + connect).
fn bt_rows<T: HasBt>(ui: &BtUi, p: &Palette, cx: &mut Context<T>) -> Vec<gpui::AnyElement> {
    let (accent, dim) = (p.accent, p.dim);
    let busy = ui.busy.as_ref().map(|b| b.0.clone());
    let row = |id: gpui::ElementId, key: String, label: String, glyph: &'static str, on: bool, want: bool, cmd: String| {
        let label = if busy.as_deref() == Some(key.as_str()) { format!("{label} · working…") } else { label };
        div()
            .id(id)
            .cursor_pointer()
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .py_2p5()
            .rounded_lg()
            .bg(if on { rgba((accent << 8) | 0x38) } else { rgba((dim << 8) | 0x1c) })
            .hover(|s| s.bg(rgba((accent << 8) | 0x4a)))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.touch();
                this.bt().busy = Some((key.clone(), want, std::time::Instant::now()));
                run_bg(&cmd); // the next refresh shows the new state and clears busy
                cx.notify();
            }))
            .child(div().font_family(NF).text_color(if on { rgb(accent) } else { rgb(dim) }).child(glyph))
            .child(div().flex_1().min_w(px(0.)).overflow_hidden().whitespace_nowrap().text_ellipsis().child(label))
            .into_any_element()
    };
    let info = &ui.info;
    let power_cmd = if info.on { "bluetoothctl power off" } else { "rfkill unblock bluetooth; bluetoothctl power on" };
    let glyph = |on: bool| if on { "\u{f00af}" } else { "\u{f00b2}" };
    let mut rows = vec![row("bt-power".into(), "power".into(), (if info.on { "On" } else { "Off" }).into(), glyph(info.on), info.on, !info.on, power_cmd.into())];
    if !info.on {
        return rows;
    }
    rows.extend(info.paired.iter().enumerate().map(|(i, (mac, name, on))| {
        let cmd = format!("bluetoothctl {} {mac}", if *on { "disconnect" } else { "connect" });
        row(("bt-dev", i).into(), mac.clone(), format!("{name} · {}", if *on { "connected" } else { "paired" }), glyph(*on), *on, !*on, cmd)
    }));
    let scanning = ui.scan_until.is_some_and(|t| t > std::time::Instant::now());
    rows.push(
        div()
            .id("bt-scan")
            .cursor_pointer()
            .px_3()
            .py_2p5()
            .rounded_lg()
            .text_color(rgb(if scanning { dim } else { p.fg }))
            .bg(rgba((dim << 8) | 0x28))
            .hover(|s| s.bg(rgba((accent << 8) | 0x40)))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.touch();
                if !scanning {
                    this.bt().scan_until = Some(std::time::Instant::now() + Duration::from_secs(BT_SCAN_SECS));
                    run_bg(&format!("bluetoothctl --timeout {BT_SCAN_SECS} scan on"));
                }
                cx.notify();
            }))
            .child(if scanning { "Scanning for new devices…" } else { "Scan for new devices" })
            .into_any_element(),
    );
    if ui.scan_until.is_some() {
        rows.extend(info.found.iter().enumerate().map(|(i, (mac, name))| {
            // shortcut: devices that ask for a PIN or passkey are not handled, pair those with `bluetoothctl` in a terminal
            // pairable only while pairing, so nearby devices can't pair on their own afterwards
            let cmd = format!("bluetoothctl pairable on; bluetoothctl --timeout 25 pair {mac} && bluetoothctl trust {mac}; bluetoothctl pairable off; bluetoothctl connect {mac}");
            row(("bt-new", i).into(), mac.clone(), format!("{name} · pair"), "\u{f00b1}", false, true, cmd)
        }));
    }
    rows
}

fn toggle_settings(cx: &mut App) {
    if let Some(handle) = cx.try_global::<SettingsState>().and_then(|s| s.0) {
        cx.set_global(SettingsState(None));
        if handle.update(cx, |_, window, _| window.remove_window()).is_ok() {
            return;
        }
    }
    let opened = cx.open_window(
        WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(Bounds { origin: point(px(0.), px(0.)), size: size(px(0.), px(0.)) })),
            app_id: Some("fiaboo-settings".to_string()),
            window_background: WindowBackgroundAppearance::Transparent,
            kind: WindowKind::LayerShell(LayerShellOptions {
                namespace: "fiaboo-settings".to_string(),
                layer: Layer::Overlay,
                anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                exclusive_zone: Some(px(-1.)),
                keyboard_interactivity: KeyboardInteractivity::Exclusive,
                ..Default::default()
            }),
            ..Default::default()
        },
        |window, cx| cx.new(|cx| SettingsPanel::new(window, cx)),
    );
    if let Ok(h) = opened {
        cx.set_global(SettingsState(Some(h.into())));
    }
}

fn close_settings(window: &mut Window, cx: &mut App) {
    cx.set_global(SettingsState(None));
    window.remove_window();
}

struct SettingsPanel {
    pal: Palette,
    focus: FocusHandle,
    info: SettingsInfo,
    last_input: std::time::Instant,
    bt: BtUi,
}

impl HasBt for SettingsPanel {
    fn bt(&mut self) -> &mut BtUi {
        &mut self.bt
    }
    fn touch(&mut self) {
        self.last_input = std::time::Instant::now();
    }
}

impl SettingsPanel {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        // refresh every few seconds; also the idle safety net (the panel holds exclusive keyboard focus)
        cx.spawn_in(window, async move |this, cx| loop {
            let (info, bt) = cx.background_executor().spawn(async { (read_settings(), read_bt()) }).await;
            let idle = this
                .update(cx, |s, cx| {
                    s.info = info;
                    s.bt.refresh(bt);
                    cx.notify();
                    s.last_input.elapsed() > Duration::from_secs(120)
                })
                .unwrap_or(true);
            if idle {
                let _ = this.update_in(cx, |_, window, cx| close_settings(window, cx));
                break;
            }
            cx.background_executor().timer(Duration::from_secs(3)).await;
        })
        .detach();
        SettingsPanel { pal: palette(), focus, info: SettingsInfo::default(), last_input: std::time::Instant::now(), bt: BtUi::default() }
    }
}

impl Render for SettingsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.pal.clone();
        let info = self.info.clone();
        let section = |title: &'static str| {
            div().pt_2().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).text_color(rgb(p.dim)).child(title)
        };
        let row = |label: String, value: String| {
            div()
                .flex()
                .gap_3()
                .px_3()
                .py_2p5()
                .rounded_lg()
                .bg(rgba((p.dim << 8) | 0x1c))
                .child(div().w(px(170.)).flex_none().overflow_hidden().whitespace_nowrap().text_ellipsis().child(label))
                .child(div().flex_1().min_w(px(0.)).overflow_hidden().whitespace_nowrap().text_ellipsis().text_color(rgb(p.dim)).child(value))
        };
        let sinks = info.sinks.iter().enumerate().map(|(i, (name, desc, is_default))| {
            let name = name.clone();
            div()
                .id(("sink", i))
                .cursor_pointer()
                .flex()
                .items_center()
                .gap_3()
                .px_3()
                .py_2p5()
                .rounded_lg()
                .bg(if *is_default { rgba((p.accent << 8) | 0x38) } else { rgba((p.dim << 8) | 0x1c) })
                .hover(|s| s.bg(rgba((p.accent << 8) | 0x4a)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.last_input = std::time::Instant::now();
                    run_bg(&format!("pactl set-default-sink '{name}'"));
                    for s in this.info.sinks.iter_mut() {
                        s.2 = s.0 == name; // show it now, the next refresh confirms
                    }
                    cx.notify();
                }))
                .child(div().font_family(NF).text_color(if *is_default { rgb(p.accent) } else { rgb(p.dim) }).child(if *is_default { "\u{f0133}" } else { "\u{f0130}" }))
                .child(div().flex_1().min_w(px(0.)).overflow_hidden().whitespace_nowrap().text_ellipsis().child(desc.clone()))
        });
        let btn = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .cursor_pointer()
                .px_3()
                .py_2p5()
                .rounded_lg()
                .text_size(px(14.))
                .bg(rgba((p.dim << 8) | 0x28))
                .hover(|s| s.bg(rgba((p.accent << 8) | 0x40)))
                .child(label)
        };
        div()
            .font_family(NF)
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                this.last_input = std::time::Instant::now();
                if ev.keystroke.key == "escape" {
                    close_settings(window, cx);
                }
            }))
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(MouseButton::Left, |_, window, cx| close_settings(window, cx))
            .on_mouse_down(MouseButton::Right, |_, window, cx| close_settings(window, cx))
            .child(
                div()
                    .w(px(640.))
                    .h(px(700.))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_4()
                    .rounded_2xl()
                    .bg(rgba((p.bg << 8) | GLASS))
                    .border_1()
                    .border_color(rgba((p.dim << 8) | 0x70))
                    .text_color(rgb(p.fg))
                    .text_size(px(14.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).text_size(px(16.)).child("Settings"))
                            .child(btn("s-wp", "Change wallpaper").on_click(|_, window, cx| {
                                close_settings(window, cx);
                                toggle_wp(cx);
                            }))
                            .child(btn("s-theme", "Switch theme").on_click(|_, _, _| run_bg("setsid ~/.local/bin/fiaboo-wallpaper --toggle-theme"))),
                    )
                    .child(
                        div()
                            .id("settings-body")
                            .flex_1()
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(section("Network (read only)"))
                            .children(info.net.iter().map(|(a, b)| row(a.clone(), b.clone())))
                            .child(section("Sound output"))
                            .children(sinks)
                            .child(section("Bluetooth"))
                            .children(bt_rows(&self.bt, &p, cx))
                            .child(section("Displays"))
                            .children(info.displays.iter().map(|d| row(d.split(" · ").next().unwrap_or("").to_string(), d.split_once(" · ").map(|x| x.1).unwrap_or("").to_string())))
                            .child(section("About"))
                            .children(info.about.iter().map(|(a, b)| row(a.to_string(), b.clone()))),
                    )

            )
    }
}

// ---------- start menu ----------
#[derive(Clone)]
struct Entry {
    name: String,
    stem: String,
    icon: Option<PathBuf>,
}

struct StartState(Option<AnyWindowHandle>);
impl Global for StartState {}

/// (button, confirmation question, command)
const POWER: [(&str, &str, &str); 3] = [
    ("Log out", "Log out now?", "hyprctl dispatch 'hl.dsp.exit()'"),
    ("Restart", "Restart now?", "systemctl reboot"),
    ("Shut down", "Shut down now?", "systemctl poweroff"),
];

fn entry_of(d: &Desktop) -> Entry {
    Entry { name: d.name.clone(), stem: d.stem.clone(), icon: find_icon_file(&d.icon) }
}

fn toggle_start(cx: &mut App) {
    if let Some(handle) = cx.try_global::<StartState>().and_then(|s| s.0) {
        cx.set_global(StartState(None));
        if handle.update(cx, |_, window, _| window.remove_window()).is_ok() {
            return; // was open, now closed
        }
    }
    let opened = cx.open_window(
        WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: point(px(0.), px(0.)),
                size: size(px(0.), px(0.)),
            })),
            app_id: Some("fiaboo-start".to_string()),
            window_background: WindowBackgroundAppearance::Transparent,
            kind: WindowKind::LayerShell(LayerShellOptions {
                namespace: "fiaboo-start".to_string(),
                layer: Layer::Overlay,
                anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                exclusive_zone: Some(px(-1.)),
                keyboard_interactivity: KeyboardInteractivity::Exclusive,
                ..Default::default()
            }),
            ..Default::default()
        },
        |window, cx| cx.new(|cx| StartMenu::new(window, cx)),
    );
    if let Ok(h) = opened {
        cx.set_global(StartState(Some(h.into())));
    }
}

struct StartMenu {
    pal: Palette,
    query: String,
    focus: FocusHandle,
    all: Vec<Entry>,
    pins: Vec<Entry>,
    sel: usize, // highlighted result (arrow keys); with no query it walks the all-apps list
    scroll: gpui::ScrollHandle,
    last_input: std::time::Instant,
    power: bool,            // power row shown
    confirm: Option<usize>, // POWER action waiting for confirmation
}

impl StartMenu {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let mut all: Vec<Entry> = apps().0.iter().map(entry_of).collect();
        all.sort_by_key(|e| e.name.to_lowercase());
        all.dedup_by(|a, b| a.stem == b.stem);
        let pins = pinned().iter().map(|(_, d)| entry_of(d)).collect();
        // safety net: close after 90 s without a key press (the menu holds exclusive keyboard focus)
        cx.spawn_in(window, async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let idle = this
                .update(cx, |m, _| m.last_input.elapsed() > Duration::from_secs(90))
                .unwrap_or(true);
            if idle {
                let _ = this.update_in(cx, |_, window, cx| close_start(window, cx));
                break;
            }
        })
        .detach();
        StartMenu {
            pal: palette(),
            query: String::new(),
            focus,
            all,
            pins,
            sel: 0,
            scroll: gpui::ScrollHandle::new(),
            last_input: std::time::Instant::now(),
            power: false,
            confirm: None,
        }
    }

    fn results(&self) -> Vec<Entry> {
        let q = self.query.to_lowercase();
        self.all
            .iter()
            .filter(|e| e.name.to_lowercase().contains(&q) || e.stem.contains(&q))
            .cloned()
            .collect()
    }
}

fn close_start(window: &mut Window, cx: &mut App) {
    cx.set_global(StartState(None));
    window.remove_window();
}

fn tile_icon(e: &Entry, size_px: f32, p: &Palette) -> gpui::AnyElement {
    match &e.icon {
        Some(path) => div()
            .size(px(size_px))
            .child(img(path.clone()).size(px(size_px)).object_fit(ObjectFit::Contain))
            .into_any_element(),
        None => div()
            .size(px(size_px))
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .bg(rgba((p.dim << 8) | 0x55))
            .child(e.name.chars().next().unwrap_or('?').to_uppercase().to_string())
            .into_any_element(),
    }
}

impl Render for StartMenu {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.pal.clone();
        let searching = !self.query.is_empty();

        let search = div()
            .h(px(44.))
            .flex()
            .items_center()
            .px_4()
            .rounded_xl()
            .bg(rgba((p.dim << 8) | 0x28))
            .text_size(px(16.))
            .gap_2()
            .child(div().text_color(rgb(p.dim)).child("\u{f002}"))
            .child(if searching {
                div().text_color(rgb(p.fg)).child(format!("{}\u{258f}", self.query))
            } else {
                div().text_color(rgb(p.dim)).child("Search apps…")
            });

        let section = |title: &'static str| {
            div().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).text_color(rgb(p.dim)).child(title)
        };

        let row = |e: &Entry, i: usize, first: bool| {
            let stem = e.stem.clone();
            let pin_stem = e.stem.clone();
            div()
                .id(("row", i))
                .on_mouse_down(MouseButton::Right, cx.listener(move |this, _, _, cx| {
                    toggle_pin(&pin_stem);
                    this.pins = pinned().iter().map(|(_, d)| entry_of(d)).collect();
                    cx.notify();
                }))
                .cursor_pointer()
                .flex()
                .items_center()
                .gap_3()
                .px_3()
                .py_1p5()
                .rounded_lg()
                .when(first, |d| d.bg(rgba((p.accent << 8) | 0x30)))
                .hover(|s| s.bg(rgba((p.dim << 8) | 0x30)))
                .on_click(move |_, window, cx| {
                    launch_app(&stem);
                    close_start(window, cx);
                })
                .child(tile_icon(e, 26., &p))
                .child(div().child(e.name.clone()))
        };

        let body = if searching {
            let list: Vec<_> =
                self.results().iter().take(40).enumerate().map(|(i, e)| row(e, i, i == self.sel)).collect();
            div()
                .id("results")
                .flex_1()
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .flex()
                .flex_col()
                .children(list)
                .into_any_element()
        } else {
            let tiles = self.pins.iter().enumerate().map(|(i, e)| {
                let stem = e.stem.clone();
                let pin_stem = e.stem.clone();
                div()
                    .id(("pin", i))
                    .on_mouse_down(MouseButton::Right, cx.listener(move |this, _, _, cx| {
                        toggle_pin(&pin_stem);
                        this.pins = pinned().iter().map(|(_, d)| entry_of(d)).collect();
                        cx.notify();
                    }))
                    .cursor_pointer()
                    .w(px(92.))
                    .py_2()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_1()
                    .rounded_lg()
                    .hover(|s| s.bg(rgba((p.dim << 8) | 0x30)))
                    .on_click(move |_, window, cx| {
                        launch_app(&stem);
                        close_start(window, cx);
                    })
                    .child(tile_icon(e, 38., &p))
                    .child(
                        div()
                            .max_w(px(86.))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(px(12.))
                            .child(e.name.clone()),
                    )
            });
            let all: Vec<_> = self.all.iter().enumerate().map(|(i, e)| row(e, 1000 + i, i == self.sel)).collect();
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap_2()
                .overflow_hidden()
                .child(section("Pinned"))
                .child(div().flex().flex_row().flex_wrap().children(tiles))
                .child(section("All apps"))
                .child(
                    div().id("all").flex_1().overflow_y_scroll().track_scroll(&self.scroll).flex().flex_col().children(all),
                )
                .into_any_element()
        };

        let footer_btn = |id: &'static str, label: &'static str, cmd: &'static str| {
            div()
                .id(id)
                .cursor_pointer()
                .px_3()
                .py_1p5()
                .rounded_lg()
                .text_size(px(14.))
                .bg(rgba((p.dim << 8) | 0x28))
                .hover(|s| s.bg(rgba((p.accent << 8) | 0x40)))
                .on_click(move |_, window, cx| {
                    let _ = Command::new("sh").args(["-c", &format!("setsid {cmd} >/dev/null 2>&1 &")]).spawn();
                    close_start(window, cx);
                })
                .child(label)
        };

        let small_btn = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .cursor_pointer()
                .px_3()
                .py_1p5()
                .rounded_lg()
                .text_size(px(14.))
                .bg(rgba((p.dim << 8) | 0x28))
                .hover(|s| s.bg(rgba((p.accent << 8) | 0x40)))
                .child(label)
        };
        let power_row = match self.confirm {
            Some(i) => {
                let (label, question, cmd) = POWER[i];
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).child(question))
                    .child(footer_btn("p-ok", label, cmd).bg(rgba((0xf85149 << 8) | 0x60)))
                    .child(small_btn("p-cancel", "Cancel").on_click(cx.listener(|this, _, _, cx| {
                        this.confirm = None;
                        cx.notify();
                    })))
            }
            None => div().flex().justify_end().gap_3().children(POWER.iter().enumerate().map(|(i, &(label, _, _))| {
                small_btn(label, label).on_click(cx.listener(move |this, _, _, cx| {
                    this.confirm = Some(i);
                    cx.notify();
                }))
            })),
        };

        let footer = div()
            .flex()
            .items_center()
            .gap_3()
            .pt_2()
            .child(
                img(PathBuf::from(format!("{}/Pictures/fiaboo/cauvang.jpg", home())))
                    .size(px(32.))
                    .rounded_full()
                    .object_fit(ObjectFit::Cover),
            )
            .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).child("fiaboo"))
            .child(small_btn("f-settings", "Settings").on_click(|_, window, cx| {
                close_start(window, cx);
                toggle_settings(cx);
            }))
            .child(footer_btn("f-menu", "Control menu", "~/.local/bin/ctrl-menu"))
            .child(footer_btn("f-lock", "Lock", "hyprlock"))
            .child(small_btn("f-power", "\u{23fb}").font_family(NF).on_click(cx.listener(|this, _, _, cx| {
                this.power = !this.power;
                this.confirm = None;
                cx.notify();
            })));

        div()
            .font_family(NF)
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                this.last_input = std::time::Instant::now();
                let k = &ev.keystroke;
                match k.key.as_str() {
                    "escape" => close_start(window, cx),
                    "backspace" => {
                        this.query.pop();
                        this.sel = 0;
                        cx.notify();
                    }
                    "down" | "up" => {
                        let n = this.results().len().min(40);
                        if n > 0 {
                            this.sel = if k.key == "down" { (this.sel + 1) % n } else { (this.sel + n - 1) % n };
                            this.scroll.scroll_to_item(this.sel);
                            cx.notify();
                        }
                    }
                    "enter" => {
                        if let Some(e) = this.results().get(this.sel) {
                            launch_app(&e.stem);
                            close_start(window, cx);
                        }
                    }
                    _ => {
                        if !k.modifiers.control && !k.modifiers.alt && !k.modifiers.platform {
                            if let Some(ch) = &k.key_char {
                                this.query.push_str(ch);
                                this.sel = 0;
                                cx.notify();
                            }
                        }
                    }
                }
            }))
            .flex()
            .flex_col()
            .justify_end()
            .items_start()
            .pl(px(10.0))
            .pb(px(56.))
            .on_mouse_down(MouseButton::Left, |_, window, cx| close_start(window, cx)).on_mouse_down(MouseButton::Right, |_, window, cx| close_start(window, cx))
            .child(
                div()
                    .w(px(640.0))
                    .h(px(620.))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                    .flex()
                    .flex_col()
            .gap_3()
            .p_4()
            .rounded_2xl()
            .bg(rgba((p.bg << 8) | GLASS))
            .border_1()
            .border_color(rgba((p.dim << 8) | 0x70))
            .text_color(rgb(p.fg))
            .text_size(px(14.))
            .child(search)
            .child(body)
            .when(self.power, |d| d.child(power_row))
            .child(footer)
            )
    }
}

// ---------- Task View ----------
struct TaskViewState(Option<AnyWindowHandle>);
impl Global for TaskViewState {}

struct TvWin {
    address: String,
    title: String,
    class: String,
    ws: i64,
    icon: Option<PathBuf>,
    stable_id: String,
    thumb: PathBuf, // filled in by a background `grim -T` (Hyprland's stableId is the ext-foreign-toplevel identifier)
}

/// Capture every window (hidden workspaces too) into small JPEGs. Each open uses fresh file
/// names so GPUI's image cache never shows an old frame; the previous set is deleted.
fn tv_capture(wins: Vec<(String, PathBuf)>) {
    let dir = PathBuf::from(format!("{}/.cache/fiaboo-shell", home()));
    let keep: Vec<PathBuf> = wins.iter().map(|w| w.1.clone()).collect();
    if let Ok(rd) = fs::read_dir(&dir) {
        for e in rd.flatten() {
            if e.file_name().to_string_lossy().starts_with("tv-") && !keep.contains(&e.path()) {
                let _ = fs::remove_file(e.path());
            }
        }
    }
    let jobs: Vec<_> = wins
        .into_iter()
        .map(|(id, file)| {
            std::thread::spawn(move || {
                let tmp = file.with_extension("tmp");
                let ok = Command::new("grim")
                    .args(["-T", &id, "-s", "0.3", "-t", "jpeg", "-q", "75"])
                    .arg(&tmp)
                    .status()
                    .is_ok_and(|s| s.success());
                if ok {
                    let _ = fs::rename(&tmp, &file); // appears whole, never half-written
                }
            })
        })
        .collect();
    for j in jobs {
        let _ = j.join();
    }
}

fn tv_windows() -> Vec<TvWin> {
    let mut v: Vec<TvWin> = hyprctl_json("clients")
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|c| {
            let class = c["class"].as_str()?.to_string();
            let title = c["title"].as_str().unwrap_or("").to_string();
            let ws = c["workspace"]["id"].as_i64()?;
            if class.is_empty() || ws < 1 || class == "fiaboo-shell" {
                return None;
            }
            let stable_id = c["stableId"].as_str().unwrap_or("0").to_string();
            Some(TvWin {
                address: c["address"].as_str()?.to_string(),
                icon: icon_for(&class),
                thumb: PathBuf::from(format!(
                    "{}/.cache/fiaboo-shell/tv-{}-{}.jpg",
                    home(),
                    stable_id,
                    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
                )),
                title,
                class,
                ws,
                stable_id,
            })
        })
        .collect();
    v.sort_by_key(|w| w.ws);
    v
}

fn toggle_taskview(cx: &mut App) {
    if let Some(handle) = cx.try_global::<TaskViewState>().and_then(|s| s.0) {
        cx.set_global(TaskViewState(None));
        if handle.update(cx, |_, window, _| window.remove_window()).is_ok() {
            return;
        }
    }
    let opened = cx.open_window(
        WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(Bounds { origin: point(px(0.), px(0.)), size: size(px(0.), px(0.)) })),
            app_id: Some("fiaboo-taskview".to_string()),
            window_background: WindowBackgroundAppearance::Transparent,
            kind: WindowKind::LayerShell(LayerShellOptions {
                namespace: "fiaboo-taskview".to_string(),
                layer: Layer::Overlay,
                anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                exclusive_zone: Some(px(-1.)),
                keyboard_interactivity: KeyboardInteractivity::Exclusive,
                ..Default::default()
            }),
            ..Default::default()
        },
        |window, cx| cx.new(|cx| TaskView::new(window, cx)),
    );
    if let Ok(h) = opened {
        cx.set_global(TaskViewState(Some(h.into())));
    }
}

fn close_taskview(window: &mut Window, cx: &mut App) {
    cx.set_global(TaskViewState(None));
    window.remove_window();
}

struct TaskView {
    pal: Palette,
    focus: FocusHandle,
    wins: Vec<TvWin>,
    sel: usize,
}

impl TaskView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let wins = tv_windows();
        let jobs: Vec<(String, PathBuf)> = wins.iter().map(|w| (w.stable_id.clone(), w.thumb.clone())).collect();
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = done.clone();
        std::thread::spawn(move || {
            tv_capture(jobs);
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        });
        // repaint while thumbnails land
        cx.spawn_in(window, async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_millis(120)).await;
            let finished = done.load(std::sync::atomic::Ordering::Relaxed);
            if this.update(cx, |_, cx| cx.notify()).is_err() || finished {
                break;
            }
        })
        .detach();
        TaskView { pal: palette(), focus, wins, sel: 0 }
    }
}

impl Render for TaskView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.pal.clone();
        let empty = self.wins.is_empty();
        let sel = self.sel;
        let cards = self.wins.iter().enumerate().map(|(i, w)| {
            let (focus_addr, close_addr) = (w.address.clone(), w.address.clone());
            div()
                .id(("tv", i))
                .cursor_pointer()
                .w(px(300.))
                .h(px(236.))
                .p_3()
                .flex()
                .flex_col()
                .gap_2()
                .rounded_xl()
                .bg(rgba((p.dim << 8) | 0x26))
                .border_1()
                .border_color(rgba((p.dim << 8) | 0x70))
                .when(i == sel, |d| d.border_2().border_color(rgb(p.accent)))
                .hover(|s| s.bg(rgba((p.accent << 8) | 0x38)))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_mouse_down(MouseButton::Right, move |_, window, cx| {
                    cx.stop_propagation();
                    let _ = Command::new("hyprctl")
                        .args(["dispatch", &format!("hl.dsp.window.close({{ window = \"address:{close_addr}\" }})")])
                        .spawn();
                    close_taskview(window, cx);
                })
                .on_click(move |_, window, cx| {
                    focus_window(&focus_addr);
                    close_taskview(window, cx);
                })
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(match &w.icon {
                            Some(path) => img(path.clone()).size(px(28.)).object_fit(ObjectFit::Contain).into_any_element(),
                            None => div().size(px(28.)).into_any_element(),
                        })
                        .child(div().flex_1().text_size(px(12.)).text_color(rgb(p.dim)).child(w.class.clone()))
                        .child(div().text_size(px(12.)).text_color(rgb(p.accent)).child(format!("WS {}", w.ws))),
                )
                .child(
                    div()
                        .flex_1()
                        .rounded_lg()
                        .overflow_hidden()
                        .bg(rgba((p.bg << 8) | 0x80))
                        .when(w.thumb.exists(), |d| d.child(img(w.thumb.clone()).size_full().object_fit(ObjectFit::Contain))),
                )
                .child(
                    div()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_size(px(14.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(w.title.clone()),
                )
        });
        div()
            .font_family(NF)
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                let n = this.wins.len();
                match ev.keystroke.key.as_str() {
                    "escape" => close_taskview(window, cx),
                    "right" | "down" | "tab" if n > 0 => {
                        this.sel = (this.sel + 1) % n;
                        cx.notify();
                    }
                    "left" | "up" if n > 0 => {
                        this.sel = (this.sel + n - 1) % n;
                        cx.notify();
                    }
                    "enter" => {
                        if let Some(w) = this.wins.get(this.sel) {
                            focus_window(&w.address);
                        }
                        close_taskview(window, cx);
                    }
                    _ => {}
                }
            }))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .bg(rgba((p.bg << 8) | 0xB0))
            .text_color(rgb(p.fg))
            .on_mouse_down(MouseButton::Left, |_, window, cx| close_taskview(window, cx))
            .on_mouse_down(MouseButton::Right, |_, window, cx| close_taskview(window, cx))
            .child(
                div()
                    .max_w(px(1600.))
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .justify_center()
                    .gap_4()
                    .children(cards)
                    .when(empty, |d| d.child(div().text_color(rgb(p.dim)).child("No open windows"))),
            )
            .child(
                div().pt_4().text_size(px(12.)).text_color(rgb(p.dim)).child("Click: switch   ·   Right-click: close   ·   Esc: dismiss"),
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
    application().run(|cx: &mut App| {
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
        viz::restore(cx);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
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
