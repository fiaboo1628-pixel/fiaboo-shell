//! Compositor backend: Hyprland's JSON queries, dispatchers and event socket.

use std::process::Command;

use serde_json::Value;

/// `hyprctl <arg> -j`.
pub fn hypr_json(arg: &str) -> Value {
    Command::new("hyprctl")
        .args([arg, "-j"])
        .output()
        .ok()
        .and_then(|o| serde_json::from_slice(&o.stdout).ok())
        .unwrap_or_default()
}

fn hypr_dispatch(arg: &str) {
    crate::spawn_bg(Command::new("hyprctl").args(["dispatch", arg]));
}

pub fn focus_workspace(n: i64) {
    hypr_dispatch(&format!("hl.dsp.focus({{ workspace = {n} }})"));
}

/// `address` is what `hypr_json("clients")` reported.
pub fn focus_window(address: &str) {
    hypr_dispatch(&format!("hl.dsp.focus({{ window = \"address:{address}\" }})"));
}

pub fn close_window(address: &str) {
    hypr_dispatch(&format!("hl.dsp.window.close({{ window = \"address:{address}\" }})"));
}

/// Block on Hyprland's event socket, calling `changed` for events that alter workspaces or windows.
/// Returns when the stream ends (compositor restarted); the caller reconnects.
pub fn follow_events(changed: impl Fn()) {
    use std::io::{BufRead, BufReader};
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok().or_else(|| {
        // started outside Hyprland's environment: use the newest instance
        std::fs::read_dir(format!("{runtime}/hypr"))
            .ok()?
            .flatten()
            .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
    });
    let Some(sig) = sig else { return };
    let Ok(stream) = std::os::unix::net::UnixStream::connect(format!("{runtime}/hypr/{sig}/.socket2.sock")) else { return };
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
            changed();
        }
    }
}
