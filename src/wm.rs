//! Compositor backend: Hyprland's JSON queries, dispatchers and event socket.

use std::process::Command;

use serde_json::Value;

/// `hyprctl <arg> -j`, asked on Hyprland's request socket: no process per query.
pub fn hypr_json(arg: &str) -> Value {
    instance_dir().and_then(|d| query(&format!("{d}/.socket.sock"), arg)).unwrap_or_default()
}

fn query(socket: &str, arg: &str) -> Option<Value> {
    use std::io::{Read, Write};
    let mut s = std::os::unix::net::UnixStream::connect(socket).ok()?;
    // a hung compositor must not hang the bar loop for good
    s.set_read_timeout(Some(std::time::Duration::from_secs(5))).ok()?;
    s.write_all(format!("j/{arg}").as_bytes()).ok()?;
    let mut out = vec![];
    s.read_to_end(&mut out).ok()?;
    serde_json::from_slice(&out).ok()
}

/// `$XDG_RUNTIME_DIR/hypr/<instance signature>`, where Hyprland keeps its sockets.
fn instance_dir() -> Option<String> {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok().or_else(|| {
        // started outside Hyprland's environment: use the newest instance
        std::fs::read_dir(format!("{runtime}/hypr"))
            .ok()?
            .flatten()
            .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
    })?;
    Some(format!("{runtime}/hypr/{sig}"))
}

fn hypr_dispatch(arg: &str) {
    crate::spawn_bg(Command::new("hyprctl").args(["dispatch", arg]));
}

pub fn focus_workspace(n: i64) {
    hypr_dispatch(&format!("hl.dsp.focus({{ workspace = {n} }})"));
}

/// `address` is what `hypr_json("clients")` reported.
/// A minimized window (dropped on the taskbar, see keybindings/default.lua) comes back to the current workspace first.
pub fn focus_window(address: &str) {
    let w = format!("\"address:{address}\"");
    crate::spawn_bg(Command::new("hyprctl").args(["eval", &format!(
        "local w = hl.get_window({w}) \
         if w and w.workspace and w.workspace.name == \"special:minimized\" then \
           hl.dispatch(hl.dsp.window.move({{ workspace = hl.get_active_workspace().id, window = {w} }})) end \
         hl.dispatch(hl.dsp.focus({{ window = {w} }}))"
    )]));
}

pub fn close_window(address: &str) {
    hypr_dispatch(&format!("hl.dsp.window.close({{ window = \"address:{address}\" }})"));
}

/// Block on Hyprland's event socket, calling `changed` for events that alter workspaces or windows.
/// Returns when the stream ends (compositor restarted); the caller reconnects.
pub fn follow_events(changed: impl Fn()) {
    use std::io::{BufRead, BufReader};
    let Some(dir) = instance_dir() else { return };
    let Ok(stream) = std::os::unix::net::UnixStream::connect(format!("{dir}/.socket2.sock")) else { return };
    for line in BufReader::new(stream).lines().map_while(Result::ok) {
        let ev = line.split(">>").next().unwrap_or("");
        if matches!(
            ev,
            // not windowtitle: the bar shows classes, and Ghostty/Chromium retitle on every prompt or page
            "workspace" | "workspacev2" | "focusedmon" | "focusedmonv2" | "activewindow" | "activewindowv2"
                | "openwindow" | "closewindow" | "movewindow" | "movewindowv2"
                | "createworkspace" | "createworkspacev2" | "destroyworkspace"
                | "destroyworkspacev2" | "fullscreen" | "changefloatingmode" | "monitoradded"
                | "monitorremoved" | "configreloaded"
        ) {
            changed();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_sends_j_slash_arg_and_reads_to_eof() {
        use std::io::{Read, Write};
        let path = std::env::temp_dir().join(format!("fiaboo-hypr-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (mut c, _) = listener.accept().unwrap();
            let mut req = [0u8; 64];
            let n = c.read(&mut req).unwrap();
            assert_eq!(&req[..n], b"j/activeworkspace");
            // in two writes, then close: the reply only ends at EOF
            c.write_all(br#"{"id":"#).unwrap();
            c.write_all(b" 3}").unwrap();
        });
        let v = query(path.to_str().unwrap(), "activeworkspace");
        server.join().unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(v.unwrap()["id"].as_i64(), Some(3));
        assert!(query("/nonexistent/.socket.sock", "clients").is_none());
    }
}
