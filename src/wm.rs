//! Compositor backend: Hyprland or niri, picked from the environment the shell was started in.
//!
//! The rest of the shell reads Hyprland-shaped JSON (`hyprctl -j clients`, `workspaces`, …). Under niri,
//! `hypr_json` builds the same shape from `niri msg -j`, so only actions and the event stream differ.
//! Workspace numbers under niri are the per-output indices (1, 2, …) that niri's own binds use.

use std::process::Command;

use serde_json::{json, Value};

pub fn is_niri() -> bool {
    std::env::var_os("NIRI_SOCKET").is_some()
}

fn run_json(cmd: &str, args: &[&str]) -> Value {
    Command::new(cmd)
        .args(args)
        .output()
        .ok()
        .and_then(|o| serde_json::from_slice(&o.stdout).ok())
        .unwrap_or_default()
}

fn niri(what: &str) -> Value {
    run_json("niri", &["msg", "-j", what])
}

/// `hyprctl <arg> -j`, or its niri equivalent in the same shape (only the fields the shell reads).
pub fn hypr_json(arg: &str) -> Value {
    if !is_niri() {
        return run_json("hyprctl", &[arg, "-j"]);
    }
    niri_as_hypr(arg, &niri("workspaces"), &niri("windows"), &niri("outputs"), &niri("focused-output"))
}

fn niri_as_hypr(arg: &str, workspaces: &Value, windows: &Value, outputs: &Value, focused_output: &Value) -> Value {
    let wss = workspaces.as_array().cloned().unwrap_or_default();
    let wins = windows.as_array().cloned().unwrap_or_default();
    // niri workspace id -> the index shown on the bar
    let idx_of = |ws_id: &Value| wss.iter().find(|w| w["id"] == *ws_id).map_or(0, |w| w["idx"].as_i64().unwrap_or(0));
    let focused_ws = wss.iter().find(|w| w["is_focused"].as_bool() == Some(true));
    let out_name = focused_output["name"].as_str().unwrap_or("");
    let out_xy = (
        focused_output["logical"]["x"].as_f64().unwrap_or(0.),
        focused_output["logical"]["y"].as_f64().unwrap_or(0.),
    );
    let client = |w: &Value, rank: usize| {
        // niri only reports positions of floating windows; tiled ones get none (no window snapping in shots)
        let pos = &w["layout"]["tile_pos_in_workspace_view"];
        let size = &w["layout"]["window_size"];
        json!({
            "address": w["id"].to_string(),
            "class": w["app_id"].as_str().unwrap_or(""),
            "title": w["title"].as_str().unwrap_or(""),
            "workspace": { "id": idx_of(&w["workspace_id"]), "name": "" },
            "focusHistoryID": rank,
            "at": if pos.is_array() { json!([pos[0].as_f64().unwrap_or(0.) + out_xy.0, pos[1].as_f64().unwrap_or(0.) + out_xy.1]) } else { json!([-100000, -100000]) },
            "size": if pos.is_array() { size.clone() } else { json!([0, 0]) },
            "mapped": true,
            "hidden": false,
            "stableId": "",
        })
    };
    match arg {
        "clients" => {
            let mut sorted = wins.clone();
            // most recently focused first, like Hyprland's focusHistoryID
            let ts = |w: &Value| {
                let t = &w["focus_timestamp"];
                (t["secs"].as_u64().unwrap_or(0), t["nanos"].as_u64().unwrap_or(0))
            };
            sorted.sort_by_key(|w| std::cmp::Reverse(ts(w)));
            Value::Array(sorted.iter().enumerate().map(|(i, w)| client(w, i)).collect())
        }
        "activewindow" => wins.iter().find(|w| w["is_focused"].as_bool() == Some(true)).map_or(json!({}), |w| client(w, 0)),
        "activeworkspace" => json!({ "id": focused_ws.map_or(1, |w| w["idx"].as_i64().unwrap_or(1)) }),
        "workspaces" => Value::Array(
            wss.iter()
                .filter(|w| w["output"].as_str() == Some(out_name))
                .map(|w| {
                    let n = wins.iter().filter(|x| x["workspace_id"] == w["id"]).count();
                    json!({ "id": w["idx"], "windows": n })
                })
                .collect(),
        ),
        "monitors" => Value::Array(
            outputs
                .as_object()
                .map(|o| o.values().cloned().collect::<Vec<_>>())
                .unwrap_or_default()
                .iter()
                .map(|o| {
                    let l = &o["logical"];
                    let scale = l["scale"].as_f64().unwrap_or(1.);
                    let mode = o["current_mode"].as_u64().and_then(|i| o["modes"].get(i as usize).cloned()).unwrap_or_default();
                    let name = o["name"].as_str().unwrap_or("");
                    json!({
                        "name": name,
                        "model": o["model"],
                        "x": l["x"], "y": l["y"],
                        // Hyprland reports physical pixels (the shell divides by scale); logical is already rotated
                        "width": (l["width"].as_f64().unwrap_or(0.) * scale).round(),
                        "height": (l["height"].as_f64().unwrap_or(0.) * scale).round(),
                        "scale": scale,
                        "transform": 0,
                        "refreshRate": mode["refresh_rate"].as_f64().unwrap_or(0.) / 1000.,
                        "focused": name == out_name,
                        "activeWorkspace": { "id": focused_ws.map_or(1, |w| w["idx"].as_i64().unwrap_or(1)) },
                    })
                })
                .collect(),
        ),
        _ => Value::Null,
    }
}

fn action(args: &[&str]) {
    let mut cmd = Command::new("niri");
    cmd.args(["msg", "action"]).args(args);
    crate::spawn_bg(&mut cmd);
}

fn hypr_dispatch(arg: &str) {
    crate::spawn_bg(Command::new("hyprctl").args(["dispatch", arg]));
}

pub fn focus_workspace(n: i64) {
    if is_niri() {
        action(&["focus-workspace", &n.to_string()]);
    } else {
        hypr_dispatch(&format!("hl.dsp.focus({{ workspace = {n} }})"));
    }
}

/// `address` is what `hypr_json("clients")` reported (a niri window id under niri).
pub fn focus_window(address: &str) {
    if is_niri() {
        action(&["focus-window", "--id", address]);
    } else {
        hypr_dispatch(&format!("hl.dsp.focus({{ window = \"address:{address}\" }})"));
    }
}

pub fn close_window(address: &str) {
    if is_niri() {
        action(&["close-window", "--id", address]);
    } else {
        hypr_dispatch(&format!("hl.dsp.window.close({{ window = \"address:{address}\" }})"));
    }
}

/// Block on the compositor's event stream, calling `changed` for events that alter workspaces or windows.
/// Returns when the stream ends (compositor restarted); the caller reconnects.
pub fn follow_events(changed: impl Fn()) {
    use std::io::{BufRead, BufReader};
    if is_niri() {
        let Ok(mut child) = Command::new("niri")
            .args(["msg", "-j", "event-stream"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
        else {
            return;
        };
        if let Some(out) = child.stdout.take() {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                // every event is a one-key object; only these change what the bar shows
                if ["Workspace", "Window"].iter().any(|k| line.starts_with(&format!("{{\"{k}"))) {
                    changed();
                }
            }
        }
        let _ = child.kill();
        let _ = child.wait();
        return;
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn niri_json_in_hyprland_shape() {
        let wss = json!([
            {"id": 7, "idx": 1, "output": "DP-1", "is_focused": false},
            {"id": 9, "idx": 2, "output": "DP-1", "is_focused": true},
            {"id": 11, "idx": 3, "output": "DP-1", "is_focused": false}
        ]);
        let wins = json!([
            {"id": 3, "app_id": "kitty", "title": "a", "workspace_id": 7, "is_focused": false,
             "layout": {"tile_pos_in_workspace_view": null, "window_size": [800, 600]},
             "focus_timestamp": {"secs": 10, "nanos": 0}},
            {"id": 5, "app_id": "chromium", "title": "b", "workspace_id": 9, "is_focused": true,
             "layout": {"tile_pos_in_workspace_view": [40.0, 30.0], "window_size": [640, 480]},
             "focus_timestamp": {"secs": 20, "nanos": 0}}
        ]);
        let outs = json!({"DP-1": {"name": "DP-1", "current_mode": 0, "modes": [{"refresh_rate": 60000}],
            "logical": {"x": 0, "y": 0, "width": 1280, "height": 720, "scale": 1.5}}});
        let fo = json!({"name": "DP-1", "logical": {"x": 0, "y": 0}});
        let q = |a: &str| niri_as_hypr(a, &wss, &wins, &outs, &fo);

        assert_eq!(q("activeworkspace")["id"], 2);
        assert_eq!(q("activewindow")["address"], "5");
        let ws = q("workspaces");
        assert_eq!(ws[0]["windows"], 1);
        assert_eq!(ws[2]["windows"], 0);
        let c = q("clients");
        assert_eq!(c[0]["class"], "chromium"); // most recently focused first
        assert_eq!(c[0]["focusHistoryID"], 0);
        assert_eq!(c[0]["at"], json!([40.0, 30.0]));
        assert_eq!(c[1]["workspace"]["id"], 1);
        assert_eq!(c[1]["size"], json!([0, 0])); // tiled: no position known
        let m = &q("monitors")[0];
        assert_eq!(m["width"], 1920.0); // physical, like Hyprland
        assert_eq!(m["refreshRate"], 60.0);
        assert_eq!(m["focused"], true);
    }
}
