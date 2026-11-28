//! Desktop entries, icon lookup, pinned apps and launching.
use crate::commons::util::{data_dirs, home, read_capped};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};

// ---------- icon lookup ----------
#[derive(Clone)]
pub(crate) struct Desktop {
    pub(crate) name: String,
    pub(crate) stem: String, // lowercase .desktop file stem, the id pins and matching use
    pub(crate) file: String, // the same stem in its real case, what gtk-launch wants (com.system76.CosmicFiles)
    pub(crate) icon: String,
    pub(crate) wm_class: String, // lowercase StartupWMClass or ""
    pub(crate) settings: bool,   // Categories has Settings: a configuration tool, listed after the apps in Start
}

// User dir first: a same-named file there overrides (or hides) the system one, per XDG.
pub(crate) fn app_dirs() -> Vec<String> {
    data_dirs().into_iter().map(|d| format!("{d}/applications")).collect()
}

/// `desktop` is `$XDG_CURRENT_DESKTOP`: entries with OnlyShowIn/NotShowIn for other desktops are skipped.
pub(crate) fn scan_desktops(dirs: &[String], desktop: &str) -> Vec<Desktop> {
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
            let shown_in = |k| field(k).map(|v| v.split(';').any(|d| d == desktop));
            if shown_in("OnlyShowIn") == Some(false) || shown_in("NotShowIn") == Some(true) {
                continue;
            }
            let (Some(icon), Some(stem)) = (field("Icon"), path.file_stem().and_then(|s| s.to_str())) else {
                continue;
            };
            all.push(Desktop {
                name: field("Name").unwrap_or_else(|| stem.to_string()),
                stem: stem.to_lowercase(),
                file: stem.to_string(),
                icon,
                wm_class: field("StartupWMClass").unwrap_or_default().to_lowercase(),
                settings: field("Categories").is_some_and(|c| c.split(';').any(|c| c == "Settings")),
            });
        }
    }
    all
}

pub(crate) type DesktopIndex = HashMap<String, String>; // lowercase window class / desktop file stem -> Icon= value
pub(crate) static ICON_CACHE: OnceLock<Mutex<HashMap<String, Option<PathBuf>>>> = OnceLock::new();

/// Installed apps; rescanned when an applications dir changes (installs/removals rename files in it).
pub(crate) fn apps() -> Arc<(Vec<Desktop>, DesktopIndex)> {
    #[allow(clippy::type_complexity)]
    static CACHE: Mutex<Option<(Vec<(Option<PathBuf>, Option<std::time::SystemTime>)>, Arc<(Vec<Desktop>, DesktopIndex)>)>> =
        Mutex::new(None);
    // The resolved path, not only the mtime: on NixOS /usr/share/applications is a
    // symlink into the store, where every directory has mtime 1, so mtime alone never
    // changed and Start kept its first list until the shell restarted. The store path
    // carries the hash of the package set, so it changes on every install.
    let stamp: Vec<_> = app_dirs()
        .iter()
        .map(|d| (fs::canonicalize(d).ok(), fs::metadata(d).and_then(|m| m.modified()).ok()))
        .collect();
    let mut cache = CACHE.lock().unwrap();
    if let Some((s, a)) = cache.as_ref() {
        if *s == stamp {
            return a.clone();
        }
    }
    let list = scan_desktops(&app_dirs(), &std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default());
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

pub(crate) fn find_icon_file(name: &str) -> Option<PathBuf> {
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

pub(crate) fn find_icon_file_uncached(name: &str) -> Option<PathBuf> {
    // kora first, then the app's own icon. Measured over the 29 Icon= names the installed
    // .desktop files actually ask for: kora covers 26, hicolor 12, breeze 10, Adwaita 1.
    // Nine of them sat in both, and with hicolor first those nine -- chromium, kitty, mpv,
    // CosmicFiles, fcitx, the two gtk3 demos, nix-snowflake, nm-device-wireless -- drew
    // their vendor icon while the rest drew kora's, so the taskbar carried two visual
    // languages at once. This desktop already commits to one font and one palette; icons
    // were the last surface that did not. The three kora misses (btop, Emote, satty) ship
    // their own icon and still reach it through the rungs below.
    // Vector first, then the PNG nearest our 26-48 px display sizes: GPUI samples big
    // bitmaps without mipmaps, so 256 px looks grainy. Each rung is tried across every
    // data directory before moving to the next, so a user override still beats a system
    // icon of the same rank.
    let dirs = data_dirs();
    let mut candidates = vec![];
    let mut rung = |pat: &str| {
        for d in &dirs {
            candidates.push(format!("{d}/{}", pat.replace("{}", name)));
        }
    };
    rung("icons/kora/apps/scalable/{}.svg");
    rung("icons/hicolor/scalable/apps/{}.svg");
    for size in ["64x64", "128x128", "48x48", "256x256", "512x512"] {
        rung(&format!("icons/hicolor/{size}/apps/{{}}.png"));
    }
    for size in ["256x256", "128x128", "64x64", "48x48", "512x512"] {
        rung(&format!("icons/hicolor/{size}/apps/{{}}.svg"));
    }
    rung("pixmaps/{}.png");
    rung("pixmaps/{}.svg");
    candidates.into_iter().map(PathBuf::from).find(|p| p.exists())
}

pub(crate) fn icon_for(class: &str) -> Option<PathBuf> {
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

/// Pinned ids from the Start menu (~/.local/state/start-pinned.json), resolved to desktop entries.
pub(crate) fn pinned_ids() -> Vec<String> {
    read_pins(&pins_path()).unwrap_or_default()
}

pub(crate) fn pins_path() -> String {
    format!("{}/.local/state/start-pinned.json", home())
}

/// None when the file is there but unreadable as a list, so nobody overwrites it.
pub(crate) fn read_pins(path: &str) -> Option<Vec<String>> {
    match fs::read_to_string(path) {
        Ok(t) => serde_json::from_str(&t).ok(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(vec![]),
        Err(_) => None,
    }
}

pub(crate) fn pinned() -> Vec<(String, Desktop)> {
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
pub(crate) fn toggle_pin(stem: &str) {
    let path = pins_path();
    let Some(ids) = read_pins(&path) else {
        eprintln!("fiaboo-shell: {path} is not a list of pins, leaving it alone");
        return;
    };
    // toggle in the raw list: pins whose .desktop is missing right now (uninstalled, renamed) must survive
    let mine: Vec<String> = pinned().into_iter().filter(|(_, d)| d.stem == stem).map(|(id, _)| id).collect();
    let mut ids: Vec<String> = ids.into_iter().filter(|id| !mine.contains(&id.to_lowercase())).collect();
    if mine.is_empty() {
        ids.push(stem.to_string());
    }
    let tmp = format!("{path}.tmp");
    if fs::write(&tmp, serde_json::to_string(&ids).unwrap_or_default()).is_err() || fs::rename(&tmp, &path).is_err() {
        let _ = fs::remove_file(&tmp);
    }
}

pub(crate) fn class_is(class: &str, id: &str, d: &Desktop) -> bool {
    let lc = class.to_lowercase();
    lc == d.stem || lc == id || (!d.wm_class.is_empty() && lc == d.wm_class)
}

pub(crate) fn class_matches(class: &str, id: &str, d: &Desktop) -> bool {
    class_is(class, id, d) || class.to_lowercase().contains(id)
}

pub(crate) fn launch_app(stem: &str) {
    // the label is what Start calls it, for the failure toast; the stem is the file, not the app's name
    let (file, label) = apps().0.iter().find(|d| d.stem == stem).map_or_else(
        || (stem.to_string(), stem.to_string()),
        |d| (d.file.clone(), d.name.clone()),
    );
    // `setsid -f` was here with both streams on /dev/null. -f forks, so the status anything
    // could have waited on was the fork's and never gtk-launch's, and the reason went
    // nowhere: clicking an entry that cannot start did nothing at all, twice, silently.
    // Plain setsid execs (this child is not a process group leader), so the child *is*
    // gtk-launch and its exit code means something, while the app still gets its own
    // session and outlives a shell restart.
    // args, not `sh -c`: Steam's .desktop names have spaces
    let Ok(mut child) = Command::new("setsid")
        .args(["gtk-launch", &file])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
    else {
        return;
    };
    // the app inherits this pipe as its stderr, so EOF only comes when the app exits: reap
    // gtk-launch first, and keep draining (not closing: the app would get SIGPIPE) while it runs
    let reader = child.stderr.take().map(|e| std::thread::spawn(move || read_capped(e, 4096)));
    std::thread::spawn(move || {
        let Ok(status) = child.wait() else { return };
        if status.success() {
            return;
        }
        // failed: no app started, so the pipe closes right away
        let err = reader.and_then(|r| r.join().ok()).unwrap_or_default();
        let err = String::from_utf8_lossy(&err);
        let why = err.lines().find(|l| !l.trim().is_empty()).unwrap_or("gtk-launch gave no reason").trim();
        let _ = Command::new("notify-send")
            .args(["-u", "critical", "-a", "fiaboo-shell", &format!("{label} did not start"), why])
            .status();
    });
}
