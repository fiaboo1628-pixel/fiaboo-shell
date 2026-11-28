//! Small helpers: home and XDG data dirs, shelling out (sh, run_bg, spawn_bg), capped reads, PNG writing.
use std::fs;
use std::process::Command;

pub(crate) fn home() -> String {
    std::env::var("HOME").unwrap_or_default()
}

/// The XDG data directories, most specific first: $XDG_DATA_HOME then $XDG_DATA_DIRS.
///
/// Every icon and desktop-entry lookup below goes through this. The paths used to be
/// written out as /usr/share/..., which is true on Arch and false on NixOS -- there they
/// only resolved because the system configuration symlinks /usr/share into the store, a
/// workaround living in a different repository from the assumption that needed it. The
/// environment already names the right directories on both.
pub(crate) fn data_dirs() -> Vec<String> {
    let data_home = std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("{}/.local/share", home()));
    // The defaults are the spec's own, for a session that sets neither.
    let dirs = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_string());
    std::iter::once(data_home)
        .chain(dirs.split(':').filter(|s| !s.is_empty()).map(|s| s.trim_end_matches('/').to_string()))
        .collect()
}

/// Write an RGBA image as PNG under a temp name, then rename: callers reuse any file that exists,
/// so a half-written one (crash, full disk) would otherwise stay a broken icon for good.
pub(crate) fn write_png(file: &std::path::Path, w: u32, h: u32, rgba: &[u8]) -> Option<()> {
    let mut buf = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut buf, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().ok()?;
        writer.write_image_data(rgba).ok()?;
        writer.finish().ok()?;
    }
    let tmp = file.with_extension("tmp");
    if fs::write(&tmp, &buf).is_err() {
        let _ = fs::remove_file(&tmp);
        return None;
    }
    fs::rename(&tmp, file).ok()
}

/// The first `cap` bytes of `r`; the rest is read to EOF and dropped.
pub(crate) fn read_capped(mut r: impl std::io::Read, cap: u64) -> Vec<u8> {
    use std::io::Read;
    let mut head = vec![];
    let _ = r.by_ref().take(cap).read_to_end(&mut head);
    let _ = std::io::copy(&mut r, &mut std::io::sink());
    head
}

/// Start a command without waiting on it; a thread reaps it so it never lingers as a zombie.
pub(crate) fn spawn_bg(cmd: &mut Command) {
    if let Ok(mut child) = cmd.spawn() {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

/// For long-lived helpers (pactl subscribe, playerctl --follow, parec): the kernel sends them
/// SIGTERM when the shell dies, even by SIGKILL on an upgrade, so they don't pile up as orphans.
/// The signal follows the *thread* that spawns, so call this only from a thread that outlives the helper.
pub(crate) fn die_with_shell(cmd: &mut Command) -> &mut Command {
    use std::os::unix::process::CommandExt;
    let parent = std::process::id() as libc::pid_t;
    // SAFETY: the closure runs between fork and exec and calls only async-signal-safe libc functions.
    unsafe {
        cmd.pre_exec(move || {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            if libc::getppid() != parent {
                libc::_exit(0); // the shell died before prctl took effect
            }
            Ok(())
        })
    }
}

pub(crate) fn sh(cmd: &str) -> String {
    Command::new("sh")
        .args(["-c", cmd])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

pub(crate) fn run_bg(cmd: &str) {
    spawn_bg(Command::new("sh").args(["-c", &format!("{cmd} >/dev/null 2>&1 &")]));
}
