//! Notification daemon (org.freedesktop.Notifications): keeps a list the UI renders as
//! popups and a notification center. Replaces swaync while fiaboo-shell runs; if the shell
//! dies the name is released and D-Bus activation brings swaync back.
use std::{
    collections::HashMap,
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

pub fn popups() -> Vec<Notif> {
    let now = Instant::now();
    LIST.lock().unwrap().iter().filter(|n| n.popup_until.is_some_and(|t| t > now)).take(3).cloned().collect()
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

/// Remove a notification and tell the sending app (reason 2 = dismissed by the user).
pub fn dismiss(id: u32) {
    LIST.lock().unwrap().retain(|n| n.id != id);
    changed();
    emit(|e| async move { Server::notification_closed(&e, id, 2).await });
}

pub fn clear_all() {
    let ids: Vec<u32> = LIST.lock().unwrap().drain(..).map(|n| n.id).collect();
    changed();
    for id in ids {
        emit(|e| async move { Server::notification_closed(&e, id, 2).await });
    }
}

/// Click on a notification: run its default action (if any), then dismiss it.
pub fn activate(id: u32) {
    let has_default = LIST.lock().unwrap().iter().any(|n| n.id == id && n.actions.iter().any(|(k, _)| k == "default"));
    if has_default {
        emit(|e| async move { Server::action_invoked(&e, id, "default").await });
    }
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

/// Notification bodies may carry a small HTML subset: drop tags, decode the basic entities.
pub fn strip_markup(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
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
        // ponytail: raw "image-data" pixel hints are ignored, only icon names / file paths are shown
        let icon = hints
            .get("image-path")
            .or_else(|| hints.get("image_path"))
            .and_then(|v| <&str>::try_from(v).ok().map(str::to_string))
            .filter(|s| !s.is_empty())
            .unwrap_or(app_icon);
        let icon = icon.strip_prefix("file://").unwrap_or(&icon).to_string();
        let id = if replaces_id > 0 { replaces_id } else { NEXT_ID.fetch_add(1, Ordering::Relaxed) };
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
            summary: strip_markup(&summary),
            body: strip_markup(&body),
            icon,
            actions: actions.chunks(2).filter(|c| c.len() == 2).map(|c| (c[0].clone(), c[1].clone())).collect(),
            critical,
            time: chrono_now(),
            popup_until,
        };
        let mut list = LIST.lock().unwrap();
        list.retain(|x| x.id != id);
        list.insert(0, n);
        list.truncate(100);
        drop(list);
        changed();
        id
    }

    async fn close_notification(&self, id: u32, #[zbus(signal_emitter)] e: SignalEmitter<'_>) -> zbus::fdo::Result<()> {
        LIST.lock().unwrap().retain(|n| n.id != id);
        changed();
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
        .arg("+%H:%M")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// Serve the interface and own the name (now, or as soon as swaync releases it).
pub fn start() {
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
    fn markup_is_stripped() {
        assert_eq!(strip_markup("<b>Hi</b> &amp; <i>bye</i> &lt;3"), "Hi & bye <3");
        assert_eq!(strip_markup("plain"), "plain");
    }
}
