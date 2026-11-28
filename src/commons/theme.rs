//! Palette (read from palette.conf, Catppuccin Mocha fallback), surface alphas, card fill and shadow, the one font.
use crate::commons::util::home;
use gpui::{point, px, rgba};
use std::fs;
use std::sync::OnceLock;

/// Every surface (panels, the bar, toasts, OSD): 60 % fill over the hyprliquid glass, which adds
/// its own dark tint (rgba 0x11111b 35 %, scripts/hypr-custom.lua); together they keep text AA.
/// Without the plugin the same fill sits on Hyprland's blur. (80 % hid the glass.)
pub(crate) const PANEL_FILL: u32 = 0x99;
// alpha steps for palette colours used as fills and borders (never for text)
pub(crate) const A_SURFACE: u32 = 0x20; // resting tiles, chips
pub(crate) const A_HOVER: u32 = 0x30;
pub(crate) const A_ACTIVE: u32 = 0x48; // selected / pressed; keeps fg >= 4.5:1 even on an accent fill
pub(crate) const A_STRONG: u32 = 0x70; // focus rings, the accent card corner
pub(crate) const A_BORDER: u32 = 0x90; // panel borders: fg at this alpha stays >= 3:1 over the fill whatever is behind it

/// A card tinted with `c`: brighter at the top-left, fading towards the bottom-right.
pub(crate) fn card_fill(c: u32, strong: bool) -> gpui::Background {
    let (hi, lo) = if strong { (0x80, 0x38) } else { (0x40, 0x12) };
    gpui::linear_gradient(155., gpui::linear_color_stop(rgba((c << 8) | hi), 0.), gpui::linear_color_stop(rgba((c << 8) | lo), 1.))
}

/// `a` to `b` at `t` (0..1), per channel.
pub(crate) fn mix(a: u32, b: u32, t: f32) -> u32 {
    let ch = |s: u32| {
        let (x, y) = (((a >> s) & 0xff) as f32, ((b >> s) & 0xff) as f32);
        ((x + (y - x) * t).round() as u32) << s
    };
    ch(16) | ch(8) | ch(0)
}

/// A raised card: lit top edge, dark bottom edge, a faint inner bloom and a soft drop shadow,
/// so the rows of a panel read as thick slabs resting on it. `lifted` = hovered: longer shadow.
pub(crate) fn card_shadow(p: &Palette, lifted: bool) -> Vec<gpui::BoxShadow> {
    let lit = |a: u32| -> gpui::Hsla { rgba((p.fg << 8) | a).into() };
    let dark = |a: f32| gpui::hsla(0., 0., 0., a);
    let sh = |color, x: f32, y: f32, blur: f32, spread: f32, inset| gpui::BoxShadow {
        color, offset: point(px(x), px(y)), blur_radius: px(blur), spread_radius: px(spread), inset,
    };
    vec![
        sh(lit(0x55), 0., 1., 0., 0., true),
        sh(dark(0.35), 0., -1., 0., 0., true),
        sh(lit(0x1A), 0., 16., 28., -18., true),
        sh(dark(0.25), 0., 2., 2., 0., false),
        if lifted { sh(dark(0.55), 0., 14., 28., -8., false) } else { sh(dark(0.45), 0., 8., 18., -8., false) },
    ]
}

// ---------- palette ----------
#[derive(Clone)]
pub(crate) struct Palette {
    pub(crate) bg: u32,
    pub(crate) fg: u32,
    pub(crate) accent: u32,
    pub(crate) accent2: u32, // tertiary, for two-colour accents (the music line)
    pub(crate) dim: u32,
    pub(crate) err: u32,
}

/// Catppuccin Mocha unless a theme says otherwise; nothing follows the wallpaper.
/// Secondary text is subtext1, not subtext0: it sits on a see-through 80 % fill.
///
/// `theme` (~/.local/bin/theme) writes palette.conf from the same colours it gives the terminal, btop
/// and the window borders, so the whole desktop moves together. The values below
/// are what that file holds for the catppuccin theme, and what every field falls back to:
/// a missing file, or one line of it being unreadable, costs that one colour and nothing
/// else. Read once -- palette() is called from seventeen places and each panel keeps a
/// copy, so a theme change needs the shell restarted, which `theme` does.
pub(crate) fn palette() -> Palette {
    static CACHE: OnceLock<Palette> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            let p =
                Palette { bg: 0x1e1e2e, fg: 0xcdd6f4, accent: 0x89b4fa, accent2: 0xcba6f7, dim: 0xbac2de, err: 0xf38ba8 };
            match fs::read_to_string(format!("{}/.config/fiaboo-shell/palette.conf", home())) {
                Ok(text) => palette_from(&text, p),
                Err(_) => p,
            }
        })
        .clone()
}

/// Overlay `key = 0xrrggbb` lines onto `p`. Comments, blanks and anything that does not
/// parse are skipped, so one bad line costs one colour.
pub(crate) fn palette_from(text: &str, mut p: Palette) -> Palette {
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else { continue };
        let Ok(n) = u32::from_str_radix(value.trim().trim_start_matches("0x"), 16) else { continue };
        match key.trim() {
            "bg" => p.bg = n,
            "fg" => p.fg = n,
            "accent" => p.accent = n,
            "accent2" => p.accent2 = n,
            "dim" => p.dim = n,
            "err" => p.err = n,
            _ => {}
        }
    }
    p
}

pub(crate) const NF: &str = "JetBrainsMono Nerd Font";
