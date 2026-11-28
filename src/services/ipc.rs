//! IPC: `fiaboo-shell <panel>` toggles a panel in the running shell over $XDG_RUNTIME_DIR/fiaboo-shell.sock.
use std::fs;
use std::sync::Mutex;

// ---------- IPC: `fiaboo-shell start|taskview` toggles a panel in the running shell ----------
// the socket thread pushes commands into a channel the UI awaits, so a key press opens the panel at once
pub(crate) static IPC_RX: Mutex<Option<futures::channel::mpsc::UnboundedReceiver<String>>> = Mutex::new(None);

pub(crate) fn ipc_path() -> String {
    format!("{}/fiaboo-shell.sock", std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into()))
}

pub(crate) fn start_ipc() {
    use std::io::Read;
    let path = ipc_path();
    // A live shell already answers here: removing its socket would leave it deaf to every keybind.
    if std::os::unix::net::UnixStream::connect(&path).is_ok() {
        eprintln!("fiaboo-shell is already running");
        std::process::exit(3); // fiaboo-session: not a start failure, stop supervising
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
pub(crate) fn send_ipc(cmd: &str) -> ! {
    use std::io::Write;
    match std::os::unix::net::UnixStream::connect(ipc_path()) {
        Ok(mut s) => {
            let _ = s.write_all(cmd.as_bytes());
            std::process::exit(0)
        }
        Err(_) => std::process::exit(1),
    }
}
