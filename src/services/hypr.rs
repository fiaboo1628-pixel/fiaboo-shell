//! Hyprland state for the bar: workspaces, dock icons, layouts, focus, and the socket2 event watcher.
use crate::services::apps::{class_is, class_matches, Desktop, find_icon_file, icon_for, pinned};
use crate::services::wm;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

// ---------- hyprland state ----------
#[derive(Clone, Default, PartialEq)]
pub(crate) struct AppIcon {
    pub(crate) name: String,           // tooltip: the pin's app name, else the window class
    pub(crate) class: String,          // window class, or pinned id when not running
    pub(crate) address: String,        // window to focus ("" when not running)
    pub(crate) launch: Option<String>, // desktop file stem for gtk-launch
    pub(crate) icon: Option<PathBuf>,
    pub(crate) active: bool,
    pub(crate) running: bool,
}

/// Special workspaces stay off the taskbar, except minimized windows (click restores them).
pub(crate) fn hidden_ws(ws: &str) -> bool {
    ws.starts_with("special") && ws != "special:minimized"
}

#[derive(Clone, Default, PartialEq)]
pub(crate) struct State {
    pub(crate) active_ws: i64,
    pub(crate) occupied: Vec<i64>,
    pub(crate) apps: Vec<AppIcon>,
    pub(crate) time: String,
    pub(crate) layout: String, // Hyprland's general:layout
}

use wm::hypr_json as hyprctl_json;

pub(crate) const LAYOUTS: [&str; 3] = ["dwindle", "master", "scrolling"];

/// The layout `by` steps from `cur` in LAYOUTS, wrapping; an unknown `cur` counts as dwindle.
pub(crate) fn layout_step(cur: &str, by: i32) -> &'static str {
    let i = LAYOUTS.iter().position(|l| *l == cur).unwrap_or(0) as i32;
    LAYOUTS[(i + by).rem_euclid(LAYOUTS.len() as i32) as usize]
}

/// The layout name in a `getoption general:layout` reply.
pub(crate) fn layout_of(v: &serde_json::Value) -> String {
    v["str"].as_str().unwrap_or("").to_string()
}

pub(crate) fn current_layout() -> String {
    layout_of(&hyprctl_json("getoption general:layout"))
}

/// `arg`: a layout name, `next` or `prev`; anything else is ignored.
pub(crate) fn set_layout(arg: &str) {
    let arg = arg.to_string();
    std::thread::spawn(move || {
        let to = match arg.as_str() {
            "next" => layout_step(&current_layout(), 1),
            "prev" => layout_step(&current_layout(), -1),
            a => match LAYOUTS.iter().find(|l| **l == a) {
                Some(l) => l,
                None => return,
            },
        };
        let lua = format!("hl.config({{ general = {{ layout = \"{to}\" }} }})");
        let _ = Command::new("hyprctl").args(["eval", &lua]).stdout(std::process::Stdio::null()).status();
        // Hyprland sends no event for a config change: re-read only once the eval has landed
        HYPR_DIRTY.store(true, std::sync::atomic::Ordering::Relaxed);
    });
}

/// Taskbar icons: pins first (each claims a window of its app), then other running apps;
/// one icon per window class, keeping the most recently focused window of each class.
pub(crate) fn dock_icons(mut clients: Vec<serde_json::Value>, pins: &[(String, Desktop)], active_addr: &str) -> Vec<AppIcon> {
    clients.sort_by_key(|c| c["focusHistoryID"].as_i64().unwrap_or(i64::MAX));
    let mut apps: Vec<AppIcon> = vec![];
    let mut used_class: Vec<String> = vec![];
    for (id, d) in pins {
        let cls = |c: &serde_json::Value| c["class"].as_str().unwrap_or("").to_string();
        let free = |c: &&serde_json::Value| {
            !cls(c).is_empty()
                && !hidden_ws(c["workspace"]["name"].as_str().unwrap_or(""))
                && !used_class.contains(&cls(c)) // the loose contains() match could let two pins claim one app
        };
        // exact first, and a loose match never takes another pin's own app ("code" inside code-insiders)
        let win = clients.iter().filter(free).find(|c| class_is(&cls(c), id, d)).or_else(|| {
            clients.iter().filter(free).find(|c| {
                class_matches(&cls(c), id, d) && !pins.iter().any(|(i, o)| class_is(&cls(c), i, o))
            })
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
            name: d.name.clone(),
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
        if class.is_empty() || class == "fiaboo-shell" || hidden_ws(ws) {
            continue;
        }
        if used_class.iter().any(|u| u == class) || extra.iter().any(|a| a.class == class) {
            continue;
        }
        let address = c["address"].as_str().unwrap_or("").to_string();
        extra.push(AppIcon {
            name: class.to_string(),
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
    apps
}

pub(crate) fn read_state() -> State {
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

    let apps = dock_icons(hyprctl_json("clients").as_array().cloned().unwrap_or_default(), &pinned(), &active_addr);

    State {
        active_ws,
        occupied,
        apps,
        time: Command::new("date")
            .arg("+%H:%M|%d/%m/%Y")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_default(),
        layout: current_layout(),
    }
}

pub(crate) fn focus_window(address: &str) {
    wm::focus_window(address);
}


/// The compositor event stream (Hyprland .socket2) flags the bar to refresh when windows/workspaces change,
/// instead of polling `hyprctl` several times a second.
pub(crate) static HYPR_DIRTY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
pub(crate) fn start_hypr_events() {
    use std::sync::atomic::Ordering;
    std::thread::spawn(|| loop {
        wm::follow_events(|| HYPR_DIRTY.store(true, Ordering::Relaxed));
        std::thread::sleep(Duration::from_secs(2)); // compositor restarted or socket not up yet: reconnect
    });
}
