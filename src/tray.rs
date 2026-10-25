//! StatusNotifierItem host: polls the session bus for tray items (via the running
//! org.kde.StatusNotifierWatcher) and exposes a snapshot to the UI, plus dbusmenu helpers.
use std::{
    collections::HashMap,
    sync::Arc,
    fs,
    hash::{Hash, Hasher},
    path::PathBuf,
    sync::{Mutex, OnceLock},
    thread,
    time::Duration,
};

use zbus::{
    blocking::{proxy::Builder, Connection, Proxy},
    fdo::{RequestNameFlags, RequestNameReply},
    object_server::SignalEmitter,
    proxy::CacheProperties,
    zvariant::{OwnedValue, Value},
};

const HOST: &str = "org.kde.StatusNotifierHost-fiaboo";
const ITEM_IFACE: &str = "org.kde.StatusNotifierItem";
const HIDDEN_TRAY_ITEMS: &[&str] = &["nm_applet"];

#[derive(Clone, Debug, PartialEq)]
pub struct TrayItem {
    pub dest: String,
    pub path: String,
    pub title: String,
    pub icon: Option<PathBuf>,
    pub menu: Option<String>,
}

#[derive(Clone, Debug)]
pub struct MenuEntry {
    pub id: i32,
    pub label: String,
    pub enabled: bool,
    pub separator: bool,
}

static CONN: OnceLock<Connection> = OnceLock::new();
static ITEMS: Mutex<Vec<TrayItem>> = Mutex::new(Vec::new());

pub fn items() -> Vec<TrayItem> {
    ITEMS.lock().unwrap().clone()
}

fn conn() -> Option<&'static Connection> {
    if CONN.get().is_none() {
        let c = Connection::session().ok()?;
        let _ = CONN.set(c);
    }
    CONN.get()
}

fn proxy<'a>(conn: &Connection, dest: &'a str, path: &'a str, iface: &'a str) -> zbus::Result<Proxy<'a>> {
    Builder::new(conn)
        .destination(dest)?
        .path(path)?
        .interface(iface)?
        .cache_properties(CacheProperties::No)
        .build()
}

fn split_registration(reg: &str) -> (String, String) {
    match reg.find('/') {
        Some(i) => (reg[..i].to_string(), reg[i..].to_string()),
        None => (reg.to_string(), "/StatusNotifierItem".to_string()),
    }
}


// ---------- StatusNotifierWatcher (used when no other process owns the name) ----------
const WATCHER_NAME: &str = "org.kde.StatusNotifierWatcher";

#[derive(Clone, Default)]
pub struct Watcher {
    items: Arc<Mutex<Vec<String>>>,
    hosts: Arc<Mutex<Vec<String>>>,
}

#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    async fn register_status_notifier_item(
        &self,
        service: &str,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        let sender = hdr.sender().map(|s| s.to_string()).unwrap_or_default();
        // items pass either a bus name or an object path (then the caller itself is the service)
        let entry = if service.starts_with('/') {
            format!("{sender}{service}")
        } else {
            format!("{service}/StatusNotifierItem")
        };
        {
            let mut items = self.items.lock().unwrap();
            if items.contains(&entry) {
                return Ok(());
            }
            items.push(entry.clone());
        }
        Self::status_notifier_item_registered(&emitter, &entry).await?;
        Ok(())
    }

    async fn register_status_notifier_host(
        &self,
        service: &str,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        let first = {
            let mut hosts = self.hosts.lock().unwrap();
            if hosts.iter().any(|h| h == service) {
                return Ok(());
            }
            hosts.push(service.to_string());
            hosts.len() == 1
        };
        if first {
            Self::status_notifier_host_registered(&emitter).await?;
        }
        Ok(())
    }

    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        self.items.lock().unwrap().clone()
    }

    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        !self.hosts.lock().unwrap().is_empty()
    }

    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }

    #[zbus(signal)]
    async fn status_notifier_item_registered(emitter: &SignalEmitter<'_>, service: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn status_notifier_item_unregistered(emitter: &SignalEmitter<'_>, service: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn status_notifier_host_registered(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

impl Watcher {
    /// Drop items and hosts whose owner left the bus, telling other hosts (waybar…) about gone items.
    fn prune(&self, conn: &Connection) {
        let Ok(bus) = zbus::blocking::fdo::DBusProxy::new(conn) else { return };
        let alive = |entry: &String| {
            let (dest, _) = split_registration(entry);
            let Ok(name) = zbus::names::BusName::try_from(dest.as_str()) else { return false };
            bus.name_has_owner(name).unwrap_or(false)
        };
        let mut gone = vec![];
        self.items.lock().unwrap().retain(|entry| alive(entry) || {
            gone.push(entry.clone());
            false
        });
        self.hosts.lock().unwrap().retain(alive);
        for entry in gone {
            let _ = conn.emit_signal(
                None::<()>,
                "/StatusNotifierWatcher",
                WATCHER_NAME,
                "StatusNotifierItemUnregistered",
                &(entry.as_str(),),
            );
        }
    }
}

/// Serve the watcher interface on `conn` and try to own the well-known name. Returns the watcher state
/// and whether this process owns the name (false = another watcher is running).
pub fn serve_watcher(conn: &Connection) -> (Watcher, bool) {
    let w = Watcher::default();
    let _ = conn.object_server().at("/StatusNotifierWatcher", w.clone());
    let owner = try_own_watcher(conn);
    (w, owner)
}

fn try_own_watcher(conn: &Connection) -> bool {
    matches!(
        conn.request_name_with_flags(WATCHER_NAME, RequestNameFlags::DoNotQueue.into()),
        Ok(RequestNameReply::PrimaryOwner) | Ok(RequestNameReply::AlreadyOwner)
    )
}

pub fn start() {
    thread::spawn(|| {
        let mut registered = false;
        let mut watcher: Option<(Watcher, bool)> = None;
        loop {
            if let Some(conn) = conn() {
                if watcher.is_none() {
                    watcher = Some(serve_watcher(conn));
                }
                if let Some((w, owner)) = watcher.as_mut() {
                    if !*owner {
                        *owner = try_own_watcher(conn); // take over if the previous watcher quit
                    }
                    if *owner {
                        w.prune(conn);
                    }
                }
                if !registered {
                    let _ = conn.request_name(HOST);
                    registered = true;
                }
                if let Ok(list) = poll(conn) {
                    drop_unused_icons(&list);
                    *ITEMS.lock().unwrap() = list;
                }
            }
            thread::sleep(Duration::from_millis(1500));
        }
    });
}

fn poll(conn: &Connection) -> zbus::Result<Vec<TrayItem>> {
    let watcher = proxy(
        conn,
        "org.kde.StatusNotifierWatcher",
        "/StatusNotifierWatcher",
        "org.kde.StatusNotifierWatcher",
    )?;
    let _ = watcher.call_method("RegisterStatusNotifierHost", &(HOST,));
    let regs: Vec<String> = watcher.get_property("RegisteredStatusNotifierItems")?;
    let mut out = vec![];
    for reg in regs {
        let (dest, path) = split_registration(&reg);
        // Wi-Fi is already in the status icons / Quick settings, so hide nm-applet's duplicate tray icon
        if HIDDEN_TRAY_ITEMS.iter().any(|h| path.contains(h)) {
            continue;
        }
        let Ok(p) = proxy(conn, &dest, &path, ITEM_IFACE) else { continue };
        let status: String = p.get_property("Status").unwrap_or_default();
        if status == "Passive" {
            continue;
        }
        let mut title: String = p.get_property("Title").unwrap_or_default();
        if title.is_empty() {
            title = p.get_property("Id").unwrap_or_default();
        }
        let menu: Option<String> = p
            .get_property::<zbus::zvariant::OwnedObjectPath>("Menu")
            .ok()
            .map(|o| o.as_str().to_string())
            .filter(|m| m != "/");
        let icon = icon_of(&p, &reg);
        out.push(TrayItem { icon, dest: dest.clone(), path: path.clone(), title, menu });
    }
    Ok(out)
}

// ---------- icons ----------
fn home() -> String {
    std::env::var("HOME").unwrap_or_default()
}

fn find_in_themes(name: &str) -> Option<PathBuf> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<PathBuf>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(hit) = cache.lock().unwrap().get(name) {
        return hit.clone();
    }
    let mut found = None;
    // Theme order is the preference and stays fixed; the prefix comes from the
    // environment rather than a written-out /usr/share (see data_dirs).
    let bases: Vec<String> = ["kora", "Adwaita", "breeze", "hicolor"]
        .iter()
        .flat_map(|theme| crate::data_dirs().into_iter().map(move |d| format!("{d}/icons/{theme}")))
        .collect();
    'outer: for base in bases {
        let Ok(l1) = fs::read_dir(&base) else { continue };
        for a in l1.flatten() {
            let Ok(l2) = fs::read_dir(a.path()) else { continue };
            for b in l2.flatten() {
                for ext in ["svg", "png"] {
                    let f = b.path().join(format!("{name}.{ext}"));
                    if f.exists() {
                        found = Some(f);
                        break 'outer;
                    }
                }
            }
        }
    }
    cache.lock().unwrap().insert(name.to_string(), found.clone());
    found
}

fn icon_of(p: &Proxy, key: &str) -> Option<PathBuf> {
    let name: String = p.get_property("IconName").unwrap_or_default();
    if !name.is_empty() {
        if name.starts_with('/') && PathBuf::from(&name).exists() {
            return Some(PathBuf::from(name));
        }
        let theme_path: String = p.get_property("IconThemePath").unwrap_or_default();
        if !theme_path.is_empty() {
            for ext in ["svg", "png"] {
                let f = PathBuf::from(&theme_path).join(format!("{name}.{ext}"));
                if f.exists() {
                    return Some(f);
                }
            }
        }
        if let Some(f) = find_in_themes(&name) {
            return Some(f);
        }
    }
    let pixmaps: Vec<(i32, i32, Vec<u8>)> = p.get_property("IconPixmap").unwrap_or_default();
    let (w, h, data) = pixmaps.into_iter().max_by_key(|(w, h, _)| w * h)?;
    if w <= 0 || h <= 0 || data.len() != (w * h * 4) as usize {
        return None;
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut hasher);
    data.hash(&mut hasher);
    let dir = PathBuf::from(format!("{}/.cache/fiaboo-shell", home()));
    fs::create_dir_all(&dir).ok()?;
    let file = dir.join(format!("tray-{:x}.png", hasher.finish()));
    if !file.exists() {
        // StatusNotifierItem pixmaps are ARGB32 (network byte order); PNG wants RGBA
        let mut rgba = Vec::with_capacity(data.len());
        for px in data.chunks_exact(4) {
            rgba.extend_from_slice(&[px[1], px[2], px[3], px[0]]);
        }
        crate::write_png(&file, w as u32, h as u32, &rgba)?;
    }
    Some(file)
}

/// Pixmap icons get a new file per distinct image (badge counters, animations): keep only the current ones.
fn drop_unused_icons(items: &[TrayItem]) {
    let Ok(rd) = fs::read_dir(format!("{}/.cache/fiaboo-shell", home())) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let ours = p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("tray-") && n.ends_with(".png"));
        if ours && !items.iter().any(|i| i.icon.as_deref() == Some(p.as_path())) {
            let _ = fs::remove_file(&p);
        }
    }
}

// ---------- actions ----------
pub fn activate(item: &TrayItem) {
    if let Some(conn) = conn() {
        if let Ok(p) = proxy(conn, &item.dest, &item.path, ITEM_IFACE) {
            let _ = p.call_method("Activate", &(0i32, 0i32));
        }
    }
}

pub fn context_menu(item: &TrayItem) {
    if let Some(conn) = conn() {
        if let Ok(p) = proxy(conn, &item.dest, &item.path, ITEM_IFACE) {
            let _ = p.call_method("ContextMenu", &(0i32, 0i32));
        }
    }
}

// ---------- dbusmenu ----------
fn unwrap_variant<'a>(v: &'a Value<'a>) -> &'a Value<'a> {
    match v {
        Value::Value(inner) => unwrap_variant(inner),
        other => other,
    }
}

fn parse_entry(v: &Value) -> Option<MenuEntry> {
    let Value::Structure(st) = unwrap_variant(v) else { return None };
    let f = st.fields();
    let Value::I32(id) = unwrap_variant(f.first()?) else { return None };
    let Value::Dict(props) = unwrap_variant(f.get(1)?) else { return None };
    let mut label = String::new();
    let (mut enabled, mut visible, mut separator) = (true, true, false);
    for (k, val) in props.iter() {
        let Value::Str(key) = unwrap_variant(k) else { continue };
        match (key.as_str(), unwrap_variant(val)) {
            ("label", Value::Str(s)) => label = s.to_string(),
            ("enabled", Value::Bool(b)) => enabled = *b,
            ("visible", Value::Bool(b)) => visible = *b,
            ("type", Value::Str(s)) => separator = s.as_str() == "separator",
            _ => {}
        }
    }
    if !visible {
        return None;
    }
    // dbusmenu mnemonics: "_File" -> "File", "__" -> "_"
    let label = label.replace("__", "\u{0}").replace('_', "").replace('\u{0}', "_");
    Some(MenuEntry { id: *id, label, enabled, separator })
}

pub fn fetch_menu(item: &TrayItem) -> Vec<MenuEntry> {
    let (Some(conn), Some(menu)) = (conn(), item.menu.as_deref()) else { return vec![] };
    let Ok(p) = proxy(conn, &item.dest, menu, "com.canonical.dbusmenu") else { return vec![] };
    let _ = p.call_method("AboutToShow", &(0i32,));
    let reply: zbus::Result<(u32, (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>))> =
        p.call("GetLayout", &(0i32, 1i32, Vec::<String>::new()));
    let Ok((_, (_, _, children))) = reply else { return vec![] };
    children.iter().filter_map(|c| parse_entry(c)).collect()
}

pub fn menu_click(item: &TrayItem, id: i32) {
    let (Some(conn), Some(menu)) = (conn(), item.menu.as_deref()) else { return };
    if let Ok(p) = proxy(conn, &item.dest, menu, "com.canonical.dbusmenu") {
        let _ = p.call_method("Event", &(id, "clicked", Value::from(0i32), 0u32));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run with a private bus: `dbus-run-session -- cargo test`
    #[test]
    fn watcher_registers_items_and_hosts() {
        let conn = Connection::session().expect("session bus (run under dbus-run-session)");
        let (_w, owner) = serve_watcher(&conn);
        assert!(owner, "must own the watcher name on a fresh bus");

        let client = Connection::session().unwrap();
        let p = proxy(&client, WATCHER_NAME, "/StatusNotifierWatcher", WATCHER_NAME).unwrap();
        p.call_method("RegisterStatusNotifierItem", &("/org/ayatana/NotificationItem/foo",)).unwrap();
        p.call_method("RegisterStatusNotifierItem", &("org.example.App",)).unwrap();
        p.call_method("RegisterStatusNotifierItem", &("org.example.App",)).unwrap(); // duplicate ignored
        p.call_method("RegisterStatusNotifierHost", &(HOST,)).unwrap();

        let items: Vec<String> = p.get_property("RegisteredStatusNotifierItems").unwrap();
        let me = client.unique_name().unwrap().to_string();
        assert_eq!(
            items,
            vec![format!("{me}/org/ayatana/NotificationItem/foo"), "org.example.App/StatusNotifierItem".to_string()]
        );
        let host: bool = p.get_property("IsStatusNotifierHostRegistered").unwrap();
        assert!(host);

        // a second process must not steal the name while we own it
        let other = Connection::session().unwrap();
        assert!(!try_own_watcher(&other));
        assert_eq!(split_registration(&items[1]), ("org.example.App".to_string(), "/StatusNotifierItem".to_string()));
    }
}
