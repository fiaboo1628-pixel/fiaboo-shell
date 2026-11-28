//! System state: volume, Wi-Fi, Bluetooth, media, and the bar indicators (input method, mic, updates).
use crate::commons::util::sh;
use crate::panels::quick::keep_awake;
use crate::panels::settings::{nmcli_fields, read_bt};
use crate::services::{notify, tray};
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

// ---------- system state (quick settings) ----------
#[derive(Clone, Default, PartialEq)]
pub(crate) struct Sys {
    pub(crate) wifi_avail: bool, // a Wi-Fi device NetworkManager can use; false = no radio or blocked (Settings says "Unavailable")
    pub(crate) wifi_on: bool,
    pub(crate) ssid: String,
    pub(crate) bt_on: bool,
    pub(crate) vol: f32,
    pub(crate) muted: bool,
    pub(crate) dnd: bool,
    pub(crate) night: bool,
    pub(crate) awake: bool, // the Keep awake idle inhibitor runs
    pub(crate) gamma: i32, // hyprsunset gamma %, the software brightness (the monitor has no DDC/CI); 100 = off
    pub(crate) media: Option<Media>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Media {
    pub(crate) playing: bool,
    pub(crate) artist: String,
    pub(crate) title: String,
}

/// One `playerctl metadata --format '{{status}}\t{{artist}}\t{{title}}'` line; empty when no player.
pub(crate) fn parse_media(line: &str) -> Option<Media> {
    let mut f = line.splitn(3, '\t');
    let (status, artist, title) = (f.next()?, f.next()?.trim(), f.next()?.trim());
    (!title.is_empty()).then(|| Media { playing: status == "Playing", artist: artist.into(), title: title.into() })
}

/// (volume 0..1+, muted) of the default sink.
pub(crate) fn read_volume() -> (f32, bool) {
    let line = sh("wpctl get-volume @DEFAULT_AUDIO_SINK@");
    (line.split_whitespace().nth(1).and_then(|v| v.parse().ok()).unwrap_or(0.0), line.contains("MUTED"))
}

pub(crate) fn wifi_usable() -> bool {
    sh("nmcli -t -f TYPE,STATE device").lines().any(|l| l.starts_with("wifi:") && !l.ends_with(":unavailable"))
}

/// What the bar shows of `Sys` besides volume (that comes via OSD_PENDING): (wifi_on, bt_on).
/// One nmcli call: NetworkManager reports the Wi-Fi device as unavailable while the radio is off.
pub(crate) fn read_bar_sys() -> (bool, bool) {
    (wifi_usable(), read_bt().on)
}

pub(crate) fn read_sys() -> Sys {
    let (vol, muted) = read_volume();
    let sunset = !sh("pidof hyprsunset").is_empty(); // pgrep reads every cmdline (22 ms), pidof only comm
    let wifi_avail = wifi_usable();
    Sys {
        wifi_avail,
        // two more nmcli round trips every 8 s, pointless without an adapter
        wifi_on: wifi_avail && sh("nmcli -t -f WIFI g") == "enabled",
        ssid: if wifi_avail { sh("nmcli -t -f active,ssid dev wifi list --rescan no").lines().map(nmcli_fields).find(|f| f[0] == "yes").and_then(|f| f.get(1).cloned()).unwrap_or_default() } else { String::new() },
        bt_on: read_bt().on,
        vol,
        muted,
        dnd: notify::dnd(),
        awake: keep_awake(),
        night: sunset && sh("hyprctl hyprsunset temperature").parse::<f32>().is_ok_and(|t| t < 6500.),
        gamma: if sunset { sh("hyprctl hyprsunset gamma").parse::<f32>().map_or(100, |g| g.round() as i32) } else { 100 },
        media: parse_media(&sh("timeout 2 playerctl metadata --format '{{status}}\t{{artist}}\t{{title}}' 2>/dev/null")),
    }
}

pub(crate) fn sys_glyphs(sys: &Sys) -> (&'static str, &'static str, &'static str) {
    (
        if sys.wifi_on { "\u{f05a9}" } else { "\u{f05aa}" },
        if sys.bt_on { "\u{f00af}" } else { "\u{f00b2}" },
        if sys.muted || sys.vol <= 0.0 { "\u{f0581}" } else { "\u{f057e}" },
    )
}

// ---------- bar indicators: input method, mic in use, pending updates ----------
#[derive(Clone, Default, PartialEq)]
pub(crate) struct Indicators {
    pub(crate) vi: Option<bool>, // fcitx5 in Bamboo; None = fcitx5 not running, hidden
    pub(crate) mic: bool,        // some app other than our own parec is recording
    pub(crate) updates: usize,
}
pub(crate) static INDICATORS: Mutex<Indicators> = Mutex::new(Indicators { vi: None, mic: false, updates: 0 });

/// `fcitx5-remote -n` / `CurrentInputMethod`: the current input method name, empty when fcitx5 is not running.
pub(crate) fn parse_im(name: &str) -> Option<bool> {
    let name = name.trim();
    (!name.is_empty()).then(|| name.starts_with("bamboo"))
}

/// `pactl -f json list source-outputs`: a live (uncorked) stream that is neither the music
/// line's `parec` nor this process. parec runs as binary `pacat`, so match its client name.
/// wf-recorder (fiaboo-record) records the sink monitor, not a mic.
pub(crate) fn mic_in_use(json: &str, own_pid: u32) -> bool {
    let v: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
    v.as_array().into_iter().flatten().any(|o| {
        let prop = |k: &str| o["properties"][k].as_str().unwrap_or("");
        !o["corked"].as_bool().unwrap_or(false)
            && prop("application.name") != "fiaboo-musicline"
            && prop("application.process.id") != own_pid.to_string()
            && prop("application.process.binary") != "wf-recorder"
    })
}

/// Run by the `pactl subscribe` loop when a recording stream comes, goes or (un)corks.
pub(crate) fn read_mic() {
    let mic = mic_in_use(&sh("pactl -f json list source-outputs 2>/dev/null"), std::process::id());
    INDICATORS.lock().unwrap().mic = mic;
}

/// `checkupdates` prints one line per pending package; nothing when offline, failing or missing.
pub(crate) fn count_updates(out: &str) -> usize {
    out.lines().filter(|l| !l.trim().is_empty()).count()
}

pub(crate) fn check_updates() {
    INDICATORS.lock().unwrap().updates = count_updates(&sh("checkupdates 2>/dev/null"));
}

/// The input method every 2 s, checkupdates every 30 min; the bar loop picks them up.
/// The mic comes from `start_volume_watch`.
pub(crate) fn start_indicators() {
    std::thread::spawn(|| loop {
        let vi = parse_im(&tray::current_im().unwrap_or_default());
        INDICATORS.lock().unwrap().vi = vi;
        std::thread::sleep(Duration::from_secs(2));
    });
    std::thread::spawn(|| loop {
        check_updates();
        std::thread::sleep(Duration::from_secs(30 * 60));
    });
}

/// Run `sudo pacman -Syu` in a kitty window, then recount once it is closed.
pub(crate) fn open_updates() {
    std::thread::spawn(|| {
        // own session and no pipes back to us: a shell restart must not take pacman down mid-transaction
        let ok = Command::new("setsid")
            .args(["kitty", "--class=fiaboo.updates", "--hold", "sudo", "pacman", "-Syu"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        if ok.is_ok() {
            check_updates();
        }
    });
}
