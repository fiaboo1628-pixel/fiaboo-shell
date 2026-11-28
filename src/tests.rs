//! Tests for the items that used to live in main.rs.
use crate::commons::theme::{Palette, palette_from};
use crate::commons::ui::{scroll_steps, step_focus};
use crate::commons::util::{die_with_shell, read_capped};
use crate::panels::settings::{bt_state, BtInfo, net_row, nmcli_fields, valid_mac};
use crate::services::apps::{class_matches, Desktop, read_pins, scan_desktops};
use crate::services::hypr::{dock_icons, hidden_ws, layout_of, layout_step};
use crate::services::system::{count_updates, Media, mic_in_use, parse_im, parse_media};
use gpui::{point, px};
use std::fs;
use std::process::Command;

#[test]
fn helper_gets_sigterm_when_its_spawning_thread_ends() {
    use std::os::unix::process::ExitStatusExt;
    let mut child = std::thread::spawn(|| die_with_shell(Command::new("sleep").arg("30")).spawn().unwrap()).join().unwrap();
    assert_eq!(child.wait().unwrap().signal(), Some(libc::SIGTERM));
}

#[test]
fn layouts_cycle_both_ways() {
    assert_eq!(layout_step("dwindle", 1), "master");
    assert_eq!(layout_step("master", 1), "scrolling");
    assert_eq!(layout_step("scrolling", 1), "dwindle");
    assert_eq!(layout_step("dwindle", -1), "scrolling");
    assert_eq!(layout_step("monocle", 1), "master");
}

#[test]
fn getoption_reply_gives_the_layout() {
    let v = serde_json::from_str(r#"{"option":"general:layout","str":"dwindle","set":true}"#).unwrap();
    assert_eq!(layout_of(&v), "dwindle");
    assert_eq!(layout_of(&serde_json::Value::Null), "");
}

#[test]
fn focus_starts_hidden_then_wraps() {
    assert_eq!(step_focus(None, 0, false), None);
    assert_eq!(step_focus(None, 3, false), Some(0));
    assert_eq!(step_focus(None, 3, true), Some(2));
    assert_eq!(step_focus(Some(2), 3, false), Some(0));
    assert_eq!(step_focus(Some(0), 3, true), Some(2));
    assert_eq!(step_focus(Some(7), 3, false), Some(0), "a list that shrank");
}

#[test]
fn launch_stderr_keeps_the_head_and_drains_the_rest() {
    let data = vec![b'x'; 10_000];
    let mut r = &data[..];
    assert_eq!(read_capped(&mut r, 4096).len(), 4096);
    assert!(r.is_empty(), "read to EOF");
    assert_eq!(read_capped(&b"short"[..], 4096), b"short");
}

#[test]
fn network_rows_read_in_plain_words() {
    let f = |v: [&str; 4]| v.map(String::from).to_vec();
    assert_eq!(net_row(&f(["wlp3s0", "wifi", "connected", "Home"])), ("Wi-Fi".into(), "Connected · Home".into()));
    assert_eq!(net_row(&f(["tailscale0", "tun", "connected (externally)", "tailscale0"])), ("VPN".into(), "Connected".into()));
    assert_eq!(net_row(&f(["enp2s0", "ethernet", "unavailable", ""])), ("Ethernet".into(), "Cable unplugged".into()));
}

#[test]
fn palette_file_sets_what_it_parses_and_nothing_else() {
    let base = Palette { bg: 1, fg: 2, accent: 3, accent2: 4, dim: 5, err: 6 };
    let p = palette_from(
        "# a theme, as `theme` writes it\n\
         bg      = 0x0b1b2b\n\
         accent  = 0x2b5e8f\n\
         \n\
         dim     = 0x{{ light_foreground_strip }}\n\
         err     = not-a-colour\n\
         nonsense\n\
         unknown = 0xffffff\n",
        base,
    );
    assert_eq!(p.bg, 0x0b1b2b, "a good line applies");
    assert_eq!(p.accent, 0x2b5e8f);
    // An unrendered token, a bad value, a line with no '=', and a key nobody knows all
    // leave their field alone rather than taking the whole file down with them.
    assert_eq!(p.dim, 5, "an unsubstituted template token keeps the default");
    assert_eq!(p.err, 6, "an unparseable value keeps the default");
    assert_eq!((p.fg, p.accent2), (2, 4), "fields the file never mentions keep theirs");
}

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
    fs::write(sys.join("c.desktop"), "Name=C\nIcon=c\nOnlyShowIn=KDE;\n").unwrap();
    fs::write(sys.join("d.desktop"), "Name=D\nIcon=d\nNotShowIn=Hyprland;\n").unwrap();
    fs::write(sys.join("e.desktop"), "Name=E\nIcon=e\nOnlyShowIn=GNOME;Hyprland;\n").unwrap();
    let dirs = [user.to_string_lossy().into_owned(), sys.to_string_lossy().into_owned()];
    let names: Vec<_> = scan_desktops(&dirs, "Hyprland").into_iter().map(|d| d.name).collect();
    fs::remove_dir_all(&root).unwrap();
    assert_eq!(names, ["B2", "E"]);
}
#[test]
fn pins_match_their_windows() {
    let d = Desktop { name: "Files".into(), stem: "org.gnome.nautilus".into(), file: "org.gnome.Nautilus".into(), icon: "nautilus".into(), wm_class: "nautilus".into(), settings: false };
    assert!(class_matches("org.gnome.Nautilus", "org.gnome.nautilus", &d), "the stem, any case");
    assert!(class_matches("nautilus", "org.gnome.nautilus", &d), "StartupWMClass");
    assert!(class_matches("Nautilus-Window", "nautilus", &d), "a pin id inside the class");
    assert!(!class_matches("firefox", "org.gnome.nautilus", &d));
    assert!(hidden_ws("special:scratch") && !hidden_ws("special:minimized") && !hidden_ws("3"));
}

#[test]
fn pins_claim_their_own_windows() {
    let d = |stem: &str, wm: &str| (stem.to_string(), Desktop { name: stem.into(), stem: stem.into(), file: stem.into(), icon: stem.into(), wm_class: wm.into(), settings: false });
    let pins = [d("code", "code"), d("code-insiders", "code - insiders"), d("org.gnome.nautilus", "")];
    let w = |class: &str, addr: &str, focus: i64, ws: &str| serde_json::json!({"class": class, "address": addr, "focusHistoryID": focus, "workspace": {"name": ws}});
    let clients = vec![
        w("Code", "0xa", 2, "1"),
        w("code-insiders", "0xb", 0, "1"), // focused last, and "code" is inside its class
        w("Code", "0xa2", 1, "2"),
        w("kitty", "0xk", 3, "1"),
        w("firefox", "0xf", 4, "special:scratch"),
        w("fiaboo-shell", "0xs", 5, "1"),
        w("Alacritty", "0xz", 6, "special:minimized"),
    ];
    let got: Vec<_> = dock_icons(clients, &pins, "0xb")
        .into_iter()
        .map(|a| (a.class, a.address, a.running, a.active, a.launch))
        .collect();
    let row = |c: &str, a: &str, r, act, l: Option<&str>| (c.to_string(), a.to_string(), r, act, l.map(String::from));
    assert_eq!(got, [
        row("Code", "0xa2", true, false, Some("code")), // the most recently focused Code window
        row("code-insiders", "0xb", true, true, Some("code-insiders")),
        row("org.gnome.nautilus", "", false, false, Some("org.gnome.nautilus")),
        row("Alacritty", "0xz", true, false, None), // minimized stays on the taskbar, sorted by class
        row("kitty", "0xk", true, false, None),
    ]);
    let only_insiders = dock_icons(vec![w("code-insiders", "0xb", 0, "1")], &pins, "");
    assert_eq!(only_insiders.iter().map(|a| a.running).collect::<Vec<_>>(), [false, true, false], "the code pin leaves it alone");
}

#[test]
fn scroll_adds_up_touchpad_pixels() {
    use gpui::ScrollDelta::{Lines, Pixels};
    let px_ = |y: f32| Pixels(point(px(0.), px(y)));
    assert_eq!(scroll_steps(&Lines(point(0., 1.))), -1, "a wheel notch up");
    assert_eq!(scroll_steps(&Lines(point(1., 0.))), 0, "horizontal tilt");
    assert_eq!([15., 15., 15.].map(|y| scroll_steps(&px_(y))), [0, 0, -1], "45 px: one step, 5 px carried");
    assert_eq!(scroll_steps(&px_(-50.)), 1, "carry 5 - 50 = -45: one step down");
}

#[test]
fn unreadable_pins_are_not_overwritten() {
    let f = std::env::temp_dir().join(format!("fiaboo-pins-{}", std::process::id()));
    let path = f.to_string_lossy().into_owned();
    assert_eq!(read_pins(&path), Some(vec![]), "no file yet");
    fs::write(&f, "[\"firefox\"").unwrap();
    assert_eq!(read_pins(&path), None, "cut-off file");
    fs::write(&f, "[\"firefox\"]").unwrap();
    assert_eq!(read_pins(&path), Some(vec!["firefox".to_string()]));
    fs::remove_file(&f).unwrap();
}

#[test]
fn media_line() {
    assert_eq!(
        parse_media("Playing\tArtist\tSong"),
        Some(Media { playing: true, artist: "Artist".into(), title: "Song".into() })
    );
    assert_eq!(parse_media("Paused\t\tVideo\twith tab").map(|m| (m.playing, m.title)), Some((false, "Video\twith tab".into())));
    assert_eq!(parse_media(""), None);
    assert!(valid_mac("4C:3C:8F:5D:76:9B"));
    assert!(!valid_mac(";rm${IFS}-rf:x")); // an address this shape would reach a shell command
    let bt = BtInfo { on: true, paired: vec![("AA".into(), "JBL".into(), false)], ..Default::default() };
    assert_eq!((bt_state(&bt, "power"), bt_state(&bt, "AA"), bt_state(&bt, "BB")), (Some(true), Some(false), None));
    assert_eq!(nmcli_fields("wlan0:wifi:connected:My\\:Net"), ["wlan0", "wifi", "connected", "My:Net"]);
    assert_eq!(nmcli_fields("p2p:wifi-p2p:disconnected:"), ["p2p", "wifi-p2p", "disconnected", ""]);
    assert_eq!(parse_media("Stopped\t\t"), None);
}

#[test]
fn input_method_label() {
    assert_eq!(parse_im("bamboo\n"), Some(true));
    assert_eq!(parse_im("keyboard-us\n"), Some(false));
    assert_eq!(parse_im(""), None, "fcitx5 not running: hidden");
}

#[test]
fn mic_ignores_our_own_parec() {
    let ours = r#"[
        {"index":71,"driver":"PipeWire","owner_module":4294967295,"client":70,"source":58,"corked":false,
         "properties":{"application.name":"fiaboo-musicline","application.process.id":"4242","application.process.binary":"pacat","media.name":"record"}},
        {"index":80,"driver":"PipeWire","owner_module":4294967295,"client":79,"source":58,"corked":false,
         "properties":{"application.name":"fiaboo-shell","application.process.id":"1000","application.process.binary":"fiaboo-shell"}}
    ]"#;
    assert!(!mic_in_use(ours, 1000));
    let discord = ours.replacen("[", r#"[{"index":90,"corked":false,"properties":{"application.process.id":"5555","application.process.binary":"Discord"}},"#, 1);
    assert!(mic_in_use(&discord, 1000));
    assert!(!mic_in_use(&ours.replace("fiaboo-musicline", "wf-recorder").replace("pacat", "wf-recorder"), 1000), "screen recording");
    assert!(!mic_in_use(&discord.replace(r#""corked":false,"properties":{"application.process.id":"5555""#, r#""corked":true,"properties":{"application.process.id":"5555""#), 1000), "paused stream");
    assert!(!mic_in_use("[]", 1000) && !mic_in_use("", 1000) && !mic_in_use("Unknown option", 1000));
}

#[test]
fn update_count() {
    assert_eq!(count_updates("linux 6.17.8.arch1-1 -> 6.17.9.arch1-1\nmesa 1:25.2.6-1 -> 1:25.2.7-1\n"), 2);
    assert_eq!(count_updates(""), 0, "none, offline, or pacman-contrib missing");
}
