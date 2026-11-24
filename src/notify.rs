//! Notification daemon (org.freedesktop.Notifications): keeps a list the UI renders as
//! popups and a notification center. Replaces swaync while fiaboo-shell runs; if the shell
//! dies the name is released and D-Bus activation brings swaync back.
use std::{
    collections::HashMap,
    fs,
    hash::{Hash, Hasher},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        Mutex, OnceLock,
    },
    thread,
    time::{Duration, Instant},
};

use zbus::{
    blocking::Connection,
    fdo::RequestNameFlags,
    object_server::SignalEmitter,
    zvariant::OwnedValue,
};

const NAME: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";

#[derive(Clone, Debug, PartialEq)]
pub struct Notif {
    pub id: u32,
    pub app: String,
    pub summary: String,
    pub body: String,
    pub icon: String, // icon name or file path
    pub actions: Vec<(String, String)>, // (key, label)
    pub critical: bool,
    pub time: String,
    pub popup_until: Option<Instant>, // None = not shown as popup
}

static LIST: Mutex<Vec<Notif>> = Mutex::new(Vec::new()); // newest first
static NEXT_ID: AtomicU32 = AtomicU32::new(1);
static GEN: AtomicU64 = AtomicU64::new(0); // bumps on every change, the UI re-renders when it moves
static DND: AtomicBool = AtomicBool::new(false);
static SEEN: AtomicU32 = AtomicU32::new(0); // ids up to this one were on screen in the center
static CONN: OnceLock<Connection> = OnceLock::new();

fn changed() {
    GEN.fetch_add(1, Ordering::Relaxed);
}

pub fn generation() -> u64 {
    GEN.load(Ordering::Relaxed)
}

pub fn all() -> Vec<Notif> {
    LIST.lock().unwrap().clone()
}

/// How many arrived since the center was last opened (the bell count).
pub fn unseen() -> usize {
    let seen = SEEN.load(Ordering::Relaxed);
    LIST.lock().unwrap().iter().filter(|n| n.id > seen).count()
}

/// The center was opened: everything in it now counts as seen.
pub fn mark_seen() {
    SEEN.store(NEXT_ID.load(Ordering::Relaxed).saturating_sub(1), Ordering::Relaxed);
    changed();
}

pub fn popups() -> Vec<Notif> {
    let now = Instant::now();
    LIST.lock().unwrap().iter().filter(|n| n.popup_until.is_some_and(|t| t > now)).take(3).cloned().collect()
}

/// A panel covers the popups: keep each one up until at least 3 s after it can show again.
pub fn hold_popups() {
    hold(&mut LIST.lock().unwrap(), Instant::now() + Duration::from_secs(3));
}

fn hold(list: &mut [Notif], until: Instant) {
    for t in list.iter_mut().filter_map(|n| n.popup_until.as_mut()) {
        *t = (*t).max(until);
    }
}

/// Hide popups whose time ran out (they stay in the center).
pub fn expire_popups() {
    let now = Instant::now();
    let mut list = LIST.lock().unwrap();
    let mut any = false;
    for n in list.iter_mut() {
        if n.popup_until.is_some_and(|t| t <= now) {
            n.popup_until = None;
            any = true;
        }
    }
    if any {
        changed();
    }
}

pub fn dnd() -> bool {
    DND.load(Ordering::Relaxed)
}

pub fn toggle_dnd() {
    DND.fetch_xor(true, Ordering::Relaxed);
    if dnd() {
        for n in LIST.lock().unwrap().iter_mut() {
            n.popup_until = None;
        }
    }
    changed();
}

pub fn hide_popup(id: u32) {
    if let Some(n) = LIST.lock().unwrap().iter_mut().find(|n| n.id == id) {
        n.popup_until = None;
    }
    changed();
}

pub fn dismiss(id: u32) {
    dismiss_many(&[id]);
}

/// Remove notifications and tell the sending apps (reason 2 = dismissed by the user): one save,
/// and a NotificationClosed only for ids that were still there.
pub fn dismiss_many(ids: &[u32]) {
    let gone = remove_ids(&mut LIST.lock().unwrap(), ids);
    if gone.is_empty() {
        return;
    }
    changed();
    save();
    for id in gone {
        emit(|e| async move { Server::notification_closed(&e, id, 2).await });
    }
}

fn remove_ids(list: &mut Vec<Notif>, ids: &[u32]) -> Vec<u32> {
    let mut gone = vec![];
    list.retain(|n| !ids.contains(&n.id) || {
        gone.push(n.id);
        false
    });
    gone
}

/// Click on a notification: run its default action (if any), then dismiss it.
pub fn activate(id: u32) {
    let has_default = LIST.lock().unwrap().iter().any(|n| n.id == id && n.actions.iter().any(|(k, _)| k == "default"));
    if has_default {
        invoke(id, "default");
    } else {
        dismiss(id);
    }
}

/// Run one of the notification's actions (the app does the work), then dismiss it.
pub fn invoke(id: u32, key: &str) {
    let key = key.to_string();
    emit(|e| async move { Server::action_invoked(&e, id, &key).await });
    dismiss(id);
}

fn emit<F, Fut>(f: F)
where
    F: FnOnce(SignalEmitter<'static>) -> Fut,
    Fut: std::future::Future<Output = zbus::Result<()>>,
{
    let Some(conn) = CONN.get() else { return };
    if let Ok(e) = SignalEmitter::new(conn.inner(), PATH) {
        let _ = zbus::block_on(f(e.into_owned()));
    }
}

/// "dd/mm HH:MM" → "HH:MM" when it is today (entries saved before dates were added are plain "HH:MM").
pub fn short_time(t: &str, today: &str) -> String {
    t.strip_prefix(today).map(|r| r.trim_start().to_string()).unwrap_or_else(|| t.to_string())
}

/// Notification bodies may carry a small HTML subset: drop tags, decode the basic entities.
/// A '<' that does not start a tag (`a < b`, unescaped by the sender) is kept as text.
pub fn strip_markup(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    let mut tag = String::new(); // given back if it never closes (`if a<b then c`)
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '<' if !in_tag && chars.peek().is_some_and(|n| n.is_ascii_alphabetic() || *n == '/') => {
                in_tag = true;
                tag.clear();
                tag.push(c);
            }
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => tag.push(c),
        }
    }
    if in_tag {
        out.push_str(&tag);
    }
    out.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

/// At most `n` chars (on a char boundary): apps can send megabytes, the card shows two lines.
fn truncate(mut s: String, n: usize) -> String {
    if let Some((i, _)) = s.char_indices().nth(n) {
        s.truncate(i);
    }
    s
}

struct Server;

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Server {
    fn get_capabilities(&self) -> Vec<&str> {
        vec!["body", "body-markup", "actions", "persistence"]
    }

    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> u32 {
        let critical = hints.get("urgency").and_then(|v| u8::try_from(v).ok()) == Some(2);
        // spec priority: image-data, image-path, app_icon, icon_data
        let path_hint = hints
            .get("image-path")
            .or_else(|| hints.get("image_path"))
            .and_then(|v| <&str>::try_from(v).ok().map(str::to_string))
            .filter(|s| !s.is_empty());
        let icon = pixel_hint(&hints, &["image-data", "image_data"])
            .or(path_hint)
            .or_else(|| Some(app_icon).filter(|s| !s.is_empty()))
            .or_else(|| pixel_hint(&hints, &["icon_data"]))
            .unwrap_or_default();
        let icon = icon.strip_prefix("file://").unwrap_or(&icon).to_string();
        // a replaces_id we no longer hold (cleared, or from before a restart) gets a fresh id instead
        let held = replaces_id > 0 && LIST.lock().unwrap().iter().any(|x| x.id == replaces_id);
        let id = if held { replaces_id } else { NEXT_ID.fetch_add(1, Ordering::Relaxed) };
        let show = !dnd() || critical;
        let popup_until = match expire_timeout {
            _ if !show => None,
            0 => Some(Instant::now() + Duration::from_secs(3600)), // "never expire": keep until clicked
            t if t > 0 => Some(Instant::now() + Duration::from_millis(t as u64)),
            _ if critical => Some(Instant::now() + Duration::from_secs(3600)),
            _ => Some(Instant::now() + Duration::from_secs(6)),
        };
        let n = Notif {
            id,
            app: app_name,
            summary: truncate(summary, 200), // spec: plain text, never markup
            body: truncate(strip_markup(&body), 2000),
            icon,
            actions: actions.chunks(2).filter(|c| c.len() == 2).take(8).map(|c| (c[0].clone(), c[1].clone())).collect(),
            critical,
            time: chrono_now(),
            popup_until,
        };
        let mut list = LIST.lock().unwrap();
        list.retain(|x| x.id != id);
        list.insert(0, n);
        let keep = list.len().min(100);
        let dropped: Vec<u32> = list.drain(keep..).map(|x| x.id).collect();
        drop(list);
        changed();
        save();
        // apps waiting on the oldest ones (notify-send --wait) must hear they are gone: 1 = expired
        for old in dropped {
            emit(|e| async move { Server::notification_closed(&e, old, 1).await });
        }
        id
    }

    async fn close_notification(&self, id: u32, #[zbus(signal_emitter)] e: SignalEmitter<'_>) -> zbus::fdo::Result<()> {
        let removed = {
            let mut list = LIST.lock().unwrap();
            let before = list.len();
            list.retain(|n| n.id != id);
            list.len() < before
        };
        if !removed {
            // spec: an id we don't have gets an empty error reply, not a NotificationClosed signal
            return Err(zbus::fdo::Error::Failed(String::new()));
        }
        changed();
        save();
        Self::notification_closed(&e, id, 3).await?;
        Ok(())
    }

    fn get_server_information(&self) -> (String, String, String, String) {
        ("fiaboo-shell".into(), "fiaboo".into(), env!("CARGO_PKG_VERSION").into(), "1.2".into())
    }

    #[zbus(signal)]
    async fn notification_closed(e: &SignalEmitter<'_>, id: u32, reason: u32) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn action_invoked(e: &SignalEmitter<'_>, id: u32, action_key: &str) -> zbus::Result<()>;
}

fn chrono_now() -> String {
    std::process::Command::new("date")
        .arg("+%d/%m %H:%M")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn store_path() -> String {
    format!("{}/.local/state/fiaboo-notifs.json", crate::home())
}

pub(crate) fn cache_dir() -> PathBuf {
    PathBuf::from(format!("{}/.cache/fiaboo-shell", crate::home()))
}

/// Keep the list across shell restarts (actions are dropped: the sender may be gone).
fn save() {
    // D-Bus and UI threads both save: one at a time, or their writes to the same .tmp interleave
    static SAVING: Mutex<()> = Mutex::new(());
    let _one = SAVING.lock().unwrap();
    let v: Vec<serde_json::Value> = LIST
        .lock()
        .unwrap()
        .iter()
        .map(|n| {
            serde_json::json!({"id": n.id, "app": n.app, "summary": n.summary, "body": n.body,
                "icon": n.icon, "critical": n.critical, "time": n.time})
        })
        .collect();
    let path = store_path();
    let tmp = format!("{path}.tmp");
    if fs::write(&tmp, serde_json::Value::from(v).to_string()).is_ok() {
        let _ = fs::rename(tmp, path);
    }
}

fn load() {
    // Apps keep ids across our restarts, so never hand out an old one again (ids were max+1, i.e. 1
    // after "Clear all"). shortcut: seeded from the clock, so a restart right after >1 notification/s
    // could still reuse one; persist NEXT_ID if that ever matters.
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_secs() as u32);
    NEXT_ID.store(now, Ordering::Relaxed);
    let Ok(text) = fs::read_to_string(store_path()) else { return };
    let Ok(serde_json::Value::Array(items)) = serde_json::from_str(&text) else { return };
    let s = |v: &serde_json::Value, k: &str| v[k].as_str().unwrap_or_default().to_string();
    let list: Vec<Notif> = items
        .iter()
        .filter_map(|v| {
            Some(Notif {
                id: u32::try_from(v["id"].as_u64()?).ok()?,
                app: s(v, "app"),
                summary: s(v, "summary"),
                body: s(v, "body"),
                icon: s(v, "icon"),
                actions: vec![],
                critical: v["critical"].as_bool().unwrap_or(false),
                time: s(v, "time"),
                popup_until: None,
            })
        })
        .collect();
    let max = list.iter().map(|n| n.id).max().unwrap_or(0);
    NEXT_ID.fetch_max(max + 1, Ordering::Relaxed);
    SEEN.store(max, Ordering::Relaxed); // what was stored before a restart is not news
    // drop cached notification images nothing refers to any more
    if let Ok(rd) = fs::read_dir(cache_dir()) {
        for e in rd.flatten() {
            let path = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with("notif-") && !list.iter().any(|n| PathBuf::from(&n.icon) == path) {
                let _ = fs::remove_file(path);
            }
        }
    }
    *LIST.lock().unwrap() = list;
    changed();
}

/// Raw pixel hint (iiibiiay: width, height, rowstride, has_alpha, bits, channels, data) -> PNG path.
fn pixel_hint(hints: &HashMap<String, OwnedValue>, keys: &[&str]) -> Option<String> {
    let v = keys.iter().find_map(|k| hints.get(*k))?;
    let (w, h, stride, alpha, bits, ch, data) =
        <(i32, i32, i32, bool, i32, i32, Vec<u8>)>::try_from(v.try_clone().ok()?).ok()?;
    let rgba = to_rgba(w, h, stride, alpha, bits, ch, &data)?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    data.hash(&mut hasher);
    let dir = cache_dir();
    fs::create_dir_all(&dir).ok()?;
    let file = dir.join(format!("notif-{:x}.png", hasher.finish()));
    if !file.exists() {
        crate::write_png(&file, w as u32, h as u32, &rgba)?;
    }
    Some(file.to_string_lossy().to_string())
}

fn to_rgba(w: i32, h: i32, stride: i32, alpha: bool, bits: i32, ch: i32, data: &[u8]) -> Option<Vec<u8>> {
    let (w, h, stride, ch) = (usize::try_from(w).ok()?, usize::try_from(h).ok()?, usize::try_from(stride).ok()?, usize::try_from(ch).ok()?);
    if w == 0 || h == 0 || w > 512 || h > 512 || bits != 8 || ch != if alpha { 4 } else { 3 } || stride < w * ch {
        return None;
    }
    if data.len() < stride * (h - 1) + w * ch {
        return None;
    }
    let mut out = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for px in data[y * stride..y * stride + w * ch].chunks_exact(ch) {
            out.extend_from_slice(&[px[0], px[1], px[2], if alpha { px[3] } else { 255 }]);
        }
    }
    Some(out)
}

/// Serve the interface and own the name (now, or as soon as swaync releases it).
pub fn start() {
    load();
    thread::spawn(|| {
        let Ok(conn) = Connection::session() else { return };
        if conn.object_server().at(PATH, Server).is_err() {
            return;
        }
        let conn = CONN.get_or_init(|| conn);
        // queue for the name: the bus hands it over the moment swaync exits, leaving no gap in
        // which a notification could D-Bus-activate swaync again
        let _ = conn.request_name_with_flags(NAME, RequestNameFlags::AllowReplacement.into());
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bell_counts_only_unseen() {
        let base = NEXT_ID.load(Ordering::Relaxed);
        SEEN.store(base.saturating_sub(1), Ordering::Relaxed);
        let before = unseen();
        for _ in 0..2 {
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            LIST.lock().unwrap().insert(0, Notif {
                id,
                app: "t".into(),
                summary: "s".into(),
                body: String::new(),
                icon: String::new(),
                actions: vec![],
                critical: false,
                time: String::new(),
                popup_until: None,
            });
        }
        assert_eq!(unseen(), before + 2);
        mark_seen();
        assert_eq!(unseen(), 0);
        LIST.lock().unwrap().retain(|n| n.id < base);
    }
    #[test]
    fn markup_is_stripped() {
        assert_eq!(strip_markup("<b>Hi</b> &amp; <i>bye</i> &lt;3"), "Hi & bye <3");
        assert_eq!(strip_markup("plain"), "plain");
        assert_eq!(strip_markup("1 < 2 and 3 > 2"), "1 < 2 and 3 > 2", "a bare '<' is not a tag");
        assert_eq!(strip_markup("<a href=\"x\">link</a> <br/>end"), "link end");
        assert_eq!(strip_markup("if a<b then c"), "if a<b then c", "an unclosed tag is text");
        assert_eq!(strip_markup("<b>x</b> y<z"), "x y<z");
    }

    fn notif(id: u32, popup_until: Option<Instant>) -> Notif {
        Notif { id, app: String::new(), summary: String::new(), body: String::new(), icon: String::new(), actions: vec![], critical: false, time: String::new(), popup_until }
    }

    #[test]
    fn only_removed_ids_are_reported() {
        let mut list = vec![notif(1, None), notif(2, None), notif(3, None)];
        assert_eq!(remove_ids(&mut list, &[3, 1, 9]), vec![1, 3]);
        assert_eq!(list.iter().map(|n| n.id).collect::<Vec<_>>(), vec![2]);
        assert!(remove_ids(&mut list, &[1]).is_empty(), "a second dismiss closes nothing");
    }

    #[test]
    fn held_popups_outlive_the_panel() {
        let now = Instant::now();
        let (soon, late) = (now + Duration::from_secs(3), now + Duration::from_secs(60));
        let mut list = vec![notif(1, Some(now)), notif(2, Some(late)), notif(3, None)];
        hold(&mut list, soon);
        assert_eq!(list.iter().map(|n| n.popup_until).collect::<Vec<_>>(), vec![Some(soon), Some(late), None]);
    }

    #[test]
    fn text_is_truncated_on_char_boundary() {
        assert_eq!(truncate("héllo".into(), 2), "hé");
        assert_eq!(truncate("hi".into(), 200), "hi");
    }

    #[test]
    fn time_is_shortened_today_only() {
        assert_eq!(short_time("04/10 18:30", "04/10"), "18:30");
        assert_eq!(short_time("03/10 18:30", "04/10"), "03/10 18:30");
        assert_eq!(short_time("18:30", "04/10"), "18:30");
    }

    #[test]
    fn pixels_to_rgba() {
        // 2x1 RGB with a padded row stride of 8
        let data = [1, 2, 3, 4, 5, 6, 0, 0];
        assert_eq!(to_rgba(2, 1, 8, false, 8, 3, &data), Some(vec![1, 2, 3, 255, 4, 5, 6, 255]));
        assert_eq!(to_rgba(1, 1, 4, true, 8, 4, &[9, 8, 7, 6]), Some(vec![9, 8, 7, 6]));
        assert_eq!(to_rgba(2, 2, 6, false, 8, 3, &data), None); // too short
        assert_eq!(to_rgba(1, 1, 4, true, 16, 4, &[0; 8]), None); // 16-bit unsupported
    }
}
