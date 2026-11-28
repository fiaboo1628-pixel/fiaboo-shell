//! Start menu.
use crate::commons::theme::{
    A_ACTIVE, A_BORDER, A_HOVER, card_fill, card_shadow, NF, Palette, palette, PANEL_FILL,
};
use crate::commons::ui::{avatar, button, overlay_options, step_focus};
use crate::commons::util::spawn_bg;
use crate::panels::search::search_input;
use crate::panels::settings::toggle_settings;
use crate::services::apps::{apps, Desktop, find_icon_file, icon_for, launch_app, pinned, toggle_pin};
use crate::services::hypr::{focus_window, hidden_ws};
use crate::services::wm::hypr_json as hyprctl_json;
use gpui::{
    AnyWindowHandle, App, Context, div, FontWeight, Global, img, KeyDownEvent, MouseButton, ObjectFit, px,
    rgb, rgba, Window,
};
use gpui::prelude::*;
use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

// ---------- start menu ----------
#[derive(Clone)]
pub(crate) struct Entry {
    name: String,
    stem: String,
    icon: Option<PathBuf>,
    settings: bool,
}

/// One search result row.
pub(crate) enum Hit {
    Calc(String),
    Win { title: String, class: String, address: String },
    App(Entry, Option<String>), // the second value is the hidden .desktop stem when that is what matched
}

/// Enter or click on a row: do its action, then close Start.
fn activate(hit: &Hit, window: &mut Window, cx: &mut App) {
    match hit {
        Hit::Calc(v) => spawn_bg(Command::new("wl-copy").args(["--", v])),
        Hit::Win { address, .. } => focus_window(address),
        Hit::App(e, _) => launch_app(&e.stem),
    }
    close_start(window, cx);
}

pub(crate) struct StartState(pub(crate) Option<AnyWindowHandle>);
impl Global for StartState {}

/// (button, confirmation question, command)
pub(crate) const POWER: [(&str, &str, &str); 4] = [
    ("Sleep", "Sleep now?", "systemctl suspend"),
    ("Log out", "Log out now?", "hyprctl dispatch \"hl.dsp.exit()\""),
    ("Restart", "Restart now?", "systemctl reboot"),
    ("Shut down", "Shut down now?", "systemctl poweroff"),
];

pub(crate) fn entry_of(d: &Desktop) -> Entry {
    Entry { name: d.name.clone(), stem: d.stem.clone(), icon: find_icon_file(&d.icon), settings: d.settings }
}

pub(crate) fn toggle_start(cx: &mut App) {
    if let Some(handle) = cx.try_global::<StartState>().and_then(|s| s.0) {
        cx.set_global(StartState(None));
        if handle.update(cx, |_, window, _| window.remove_window()).is_ok() {
            return; // was open, now closed
        }
    }
    let opened = cx.open_window(
        overlay_options("fiaboo-start"),
        |window, cx| cx.new(|cx| StartMenu::new(window, cx)),
    );
    if let Ok(h) = opened {
        cx.set_global(StartState(Some(h.into())));
    }
}

pub(crate) struct StartMenu {
    pal: Palette,
    query: String, // mirrors `field`
    field: gpui::Entity<gpui_base::input::InputState>,
    all: Vec<Entry>,
    pins: Vec<Entry>,
    sel: usize, // highlighted result (arrow keys); with no query it walks the all-apps list
    scroll: gpui::ScrollHandle,
    last_input: std::time::Instant,
    power: bool,            // power row shown
    confirm: Option<usize>, // POWER action waiting for confirmation
    wins: Vec<serde_json::Value>, // Hyprland clients when Start opened
    focus_on: Option<StartStop>, // Tab focus on a tile or button; None = Enter and the arrows work the list
}

/// What Tab walks besides the list, in screen order: pinned tiles, the power row, the footer.
#[derive(Clone, Copy, PartialEq)]
enum StartStop {
    Pin(usize),
    PowerAct(usize),
    Confirm,
    Cancel,
    Settings,
    Lock,
    Power,
}

/// `cmd` in its own session, so it outlives Start (and the shell).
fn run_detached(cmd: &str) {
    spawn_bg(Command::new("sh").args(["-c", &format!("setsid {cmd} >/dev/null 2>&1 &")]));
}

impl StartMenu {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut all: Vec<Entry> = apps().0.iter().map(entry_of).collect();
        // apps A–Z first, then configuration tools (network editor, input method setup…) A–Z
        all.sort_by_key(|e| (e.settings, e.name.to_lowercase()));
        all.dedup_by(|a, b| a.stem == b.stem);
        let pins = pinned().iter().map(|(_, d)| entry_of(d)).collect();
        let field = search_input("Search apps and windows, or type a sum…", window, cx);
        // observe, not InputEvent::Change: the IME preedit emits no Change, and the list filters on it too
        cx.observe(&field, |m, field, cx| {
            let text = field.read(cx).value();
            if m.query != text.as_ref() {
                m.last_input = std::time::Instant::now(); // only on edits: the caret blink notifies too
                m.query = text.to_string();
                m.sel = 0;
                m.focus_on = None; // typing goes back to the results
                cx.notify();
            }
        })
        .detach();
        // open windows load off the UI thread; safety net: close after 90 s without a key press (the menu holds exclusive keyboard focus)
        cx.spawn_in(window, async move |this, cx| {
            let wins = cx.background_executor().spawn(async {
                let mut wins: Vec<serde_json::Value> = hyprctl_json("clients").as_array().cloned().unwrap_or_default();
                wins.retain(|c| {
                    let class = c["class"].as_str().unwrap_or("");
                    !class.is_empty() && class != "fiaboo-shell" && !hidden_ws(c["workspace"]["name"].as_str().unwrap_or(""))
                });
                wins.sort_by_key(|c| c["focusHistoryID"].as_i64().unwrap_or(i64::MAX));
                wins
            }).await;
            let _ = this.update(cx, |m, cx| { m.wins = wins; cx.notify() });
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let idle = this
                    .update(cx, |m, _| m.last_input.elapsed() > Duration::from_secs(90))
                    .unwrap_or(true);
                if idle {
                    let _ = this.update_in(cx, |_, window, cx| close_start(window, cx));
                    break;
                }
            }
        })
        .detach();
        StartMenu {
            pal: palette(),
            query: String::new(),
            field,
            all,
            pins,
            sel: 0,
            scroll: gpui::ScrollHandle::new(),
            last_input: std::time::Instant::now(),
            power: false,
            confirm: None,
            wins: vec![],
            focus_on: None,
        }
    }

    fn stops(&self) -> Vec<StartStop> {
        use StartStop::*;
        let mut v: Vec<StartStop> = if self.query.is_empty() { (0..self.pins.len()).map(Pin).collect() } else { vec![] };
        if self.power {
            match self.confirm {
                Some(_) => v.extend([Confirm, Cancel]),
                None => v.extend((0..POWER.len()).map(PowerAct)),
            }
        }
        v.extend([Settings, Lock, Power]);
        v
    }

    /// What a click on the tile or button, or Enter on its focus, does.
    fn run_stop(&mut self, s: StartStop, window: &mut Window, cx: &mut Context<Self>) {
        match s {
            StartStop::Pin(i) => {
                if let Some(e) = self.pins.get(i) {
                    launch_app(&e.stem);
                }
                return close_start(window, cx);
            }
            StartStop::Settings => {
                close_start(window, cx);
                return toggle_settings(cx);
            }
            StartStop::Lock => {
                run_detached("hyprlock");
                return close_start(window, cx);
            }
            StartStop::Confirm => {
                if let Some(i) = self.confirm {
                    run_detached(POWER[i].2);
                }
                return close_start(window, cx);
            }
            StartStop::Power => {
                self.power = !self.power;
                self.confirm = None;
            }
            StartStop::PowerAct(i) => {
                self.confirm = Some(i);
                self.focus_on = self.focus_on.map(|_| StartStop::Cancel); // the safe answer, if the keyboard got here
            }
            StartStop::Cancel => {
                self.focus_on = self.focus_on.and(self.confirm.map(StartStop::PowerAct));
                self.confirm = None;
            }
        }
        cx.notify();
    }

    /// The calculator first, then the first 40 ranked apps, then matching open windows.
    fn results(&self) -> Vec<Hit> {
        let q = self.query.to_lowercase();
        let mut hits: Vec<(u8, Entry)> =
            self.all.iter().filter_map(|e| search_rank(&e.name, &e.stem, &q).map(|r| (r, e.clone()))).collect();
        hits.sort_by_key(|(r, _)| *r); // stable: alphabetical within a rank
        let cap = if q.is_empty() { usize::MAX } else { 40 }; // with no query, Enter opens the highlighted All apps row
        let apps = hits.into_iter().take(cap).map(|(r, e)| { let why = (r == 3).then(|| format!("matches {}", e.stem)); Hit::App(e, why) });
        let wins = self.wins.iter().filter_map(|c| {
            let (title, class) = (c["title"].as_str().unwrap_or(""), c["class"].as_str().unwrap_or(""));
            let address = c["address"].as_str()?.into();
            win_matches(title, class, &q).then(|| Hit::Win { title: title.into(), class: class.into(), address })
        });
        calc(&self.query).map(Hit::Calc).into_iter().chain(apps).chain(wins).collect()
    }
}

/// Lower is better: typed name first, then a word in the name, then anywhere in the name, last the
/// hidden .desktop stem (so "fi" lists Files before Homelab, whose match is in the stem).
fn search_rank(name: &str, stem: &str, q: &str) -> Option<u8> {
    let name = name.to_lowercase();
    if name.starts_with(q) {
        Some(0)
    } else if name.split_whitespace().any(|w| w.starts_with(q)) {
        Some(1)
    } else if name.contains(q) {
        Some(2)
    } else if q.len() >= 3 && stem.contains(q) {
        Some(3) // hidden-stem matches only once the query is specific enough to explain
    } else {
        None
    }
}

fn win_matches(title: &str, class: &str, q: &str) -> bool {
    !q.is_empty() && (title.to_lowercase().contains(q) || class.to_lowercase().contains(q))
}

/// The value of an arithmetic query (+ - * / % ^, parentheses, decimals), formatted; None when the query is not
/// one, has no operator (a bare number is no sum), or divides by zero.
fn calc(q: &str) -> Option<String> {
    if q.len() > 256 {
        return None; // the parser recurses per '(' and '-': bound the depth so a paste can't overflow the stack
    }
    let s: Vec<u8> = q.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if !s.iter().any(|b| b"+-*/%^(".contains(b)) {
        return None;
    }
    let mut i = 0;
    let v = calc_sum(&s, &mut i)?;
    if i != s.len() || !v.is_finite() {
        return None;
    }
    let t = format!("{v:.10}");
    let t = t.trim_end_matches('0').trim_end_matches('.');
    Some(if t == "-0" { "0".into() } else { t.into() })
}

fn calc_sum(s: &[u8], i: &mut usize) -> Option<f64> {
    let mut v = calc_product(s, i)?;
    while let Some(&op @ (b'+' | b'-')) = s.get(*i) {
        *i += 1;
        let r = calc_product(s, i)?;
        v = if op == b'+' { v + r } else { v - r };
    }
    Some(v)
}

fn calc_product(s: &[u8], i: &mut usize) -> Option<f64> {
    let mut v = calc_unary(s, i)?;
    while let Some(&op @ (b'*' | b'/' | b'%')) = s.get(*i) {
        *i += 1;
        let r = calc_unary(s, i)?;
        if op != b'*' && r == 0. {
            return None;
        }
        v = match op { b'*' => v * r, b'/' => v / r, _ => v % r };
    }
    Some(v)
}

/// -2^2 = -4: the power binds tighter than the sign, and is right-associative (2^3^2 = 2^9).
fn calc_unary(s: &[u8], i: &mut usize) -> Option<f64> {
    match s.get(*i) {
        Some(b'-') => { *i += 1; calc_unary(s, i).map(|v| -v) }
        Some(b'+') => { *i += 1; calc_unary(s, i) }
        _ => {
            let base = calc_atom(s, i)?;
            if s.get(*i) == Some(&b'^') {
                *i += 1;
                return Some(base.powf(calc_unary(s, i)?));
            }
            Some(base)
        }
    }
}

fn calc_atom(s: &[u8], i: &mut usize) -> Option<f64> {
    if s.get(*i) == Some(&b'(') {
        *i += 1;
        let v = calc_sum(s, i)?;
        if s.get(*i) != Some(&b')') {
            return None;
        }
        *i += 1;
        return Some(v);
    }
    let start = *i;
    while s.get(*i).is_some_and(|b| b.is_ascii_digit() || *b == b'.') {
        *i += 1;
    }
    std::str::from_utf8(&s[start..*i]).ok()?.parse().ok() // "" and "1.2.3" fail here
}

#[cfg(test)]
mod tests {
    use super::{calc, search_rank, win_matches};

    #[test]
    fn calc_precedence_and_parentheses() {
        assert_eq!(calc("1+2*3").as_deref(), Some("7"));
        assert_eq!(calc("(1+2)*3").as_deref(), Some("9"));
        assert_eq!(calc("2^3^2").as_deref(), Some("512"));
        assert_eq!(calc("-2^2").as_deref(), Some("-4"));
        assert_eq!(calc("10 % 4 - 1.5").as_deref(), Some("0.5"));
        assert_eq!(calc("0.1+0.2").as_deref(), Some("0.3"));
        assert_eq!(calc("1/3").as_deref(), Some("0.3333333333"));
        assert_eq!(calc("2*(3+(4-1))/4").as_deref(), Some("3"));
    }

    #[test]
    fn calc_rejects_division_by_zero_and_garbage() {
        assert_eq!(calc("1/0"), None);
        assert_eq!(calc("5%(2-2)"), None);
        for q in ["", "42", "firefox", "1+", "(1+2", "1+2)", "1..2+1", "2**3", "1+a", "()"] {
            assert_eq!(calc(q), None, "{q}");
        }
        assert_eq!(calc(&"(".repeat(100_000)), None);
        assert_eq!(calc(&"-".repeat(100_000)), None);
    }

    #[test]
    fn windows_match_title_or_class() {
        assert!(win_matches("Pull request · GitHub", "firefox", "github"));
        assert!(win_matches("~/src", "com.mitchellh.ghostty", "ghost"));
        assert!(!win_matches("~/src", "com.mitchellh.ghostty", "zed"));
        assert!(!win_matches("~/src", "kitty", ""));
    }

    #[test]
    fn typed_name_outranks_hidden_stem() {
        assert_eq!(search_rank("Files", "org.gnome.nautilus", "fi"), Some(0));
        assert_eq!(search_rank("COSMIC Files", "com.system76.cosmicfiles", "fi"), Some(1));
        assert_eq!(search_rank("Easy Effects", "easyeffects", "ff"), Some(2));
        assert_eq!(search_rank("Homelab", "firefox-homelab", "fi"), None);
        assert_eq!(search_rank("Homelab", "firefox-homelab", "fire"), Some(3));
        assert_eq!(search_rank("Zed", "dev.zed.zed", "fi"), None);
    }
}

pub(crate) fn close_start(window: &mut Window, cx: &mut App) {
    cx.set_global(StartState(None));
    window.remove_window();
}

pub(crate) fn tile_icon(e: &Entry, size_px: f32, p: &Palette) -> gpui::AnyElement {
    match &e.icon {
        Some(path) => div()
            .size(px(size_px))
            .child(img(path.clone()).size(px(size_px)).object_fit(ObjectFit::Contain))
            .into_any_element(),
        None => div()
            .size(px(size_px))
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .bg(rgba((p.dim << 8) | A_ACTIVE))
            .child(e.name.chars().next().unwrap_or('?').to_uppercase().to_string())
            .into_any_element(),
    }
}

/// All apps row (8 + 26 icon + 8); the list is a whole number of rows so no row is cut at the bottom.
const ROW_H: f32 = 42.;
const LIST_ROWS: f32 = 7.;
const TILE_H: f32 = 76.;
/// Pinned header 16 + tiles + All apps header 24, three 8 px gaps, then the list.
const BODY_H: f32 = 16. + TILE_H + 24. + 3. * 8. + ROW_H * LIST_ROWS;

impl Render for StartMenu {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.pal.clone();
        let searching = !self.query.is_empty();
        let (focus_on, fg) = (self.focus_on, p.fg);
        let ring = move |s: StartStop| if focus_on == Some(s) { rgb(fg) } else { rgba(0x00000000) };

        let search = div()
            .h(px(44.))
            .flex()
            .items_center()
            .px_4()
            .rounded_xl()
            .bg(card_fill(p.accent, false))
            .text_size(px(16.))
            .gap_2()
            .child(div().text_color(rgb(p.dim)).child("\u{f002}"))
            .child(div().flex_1().min_w_0().text_color(rgb(p.fg)).child(gpui_base::Input::new(&self.field)));

        let section = |title: &'static str| {
            div().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).text_color(rgb(p.dim)).child(title)
        };

        let pinned_stems: HashSet<String> = self.pins.iter().map(|e| e.stem.clone()).collect();
        let row = |hit: Hit, i: usize, first: bool| {
            let is_pinned = matches!(&hit, Hit::App(e, _) if pinned_stems.contains(&e.stem));
            let (icon, name, why, pin_stem) = match &hit {
                Hit::App(e, why) => (tile_icon(e, 26., &p), e.name.clone(), why.clone(), Some(e.stem.clone())),
                Hit::Win { title, class, .. } => {
                    let e = Entry { name: class.clone(), stem: String::new(), icon: icon_for(class), settings: false };
                    (tile_icon(&e, 26., &p), title.clone(), Some(format!("Open window · {class}")), None)
                }
                Hit::Calc(v) => (
                    div().size(px(26.)).flex().items_center().justify_center().text_color(rgb(p.accent)).child("\u{f1ec}").into_any_element(),
                    format!("= {v}"),
                    Some("Enter: copy to clipboard".to_string()),
                    None,
                ),
            };
            div()
                .id(("row", i))
                .when_some(pin_stem, |d, pin_stem| d.on_mouse_down(MouseButton::Right, cx.listener(move |this, _, _, cx| {
                    toggle_pin(&pin_stem);
                    this.pins = pinned().iter().map(|(_, d)| entry_of(d)).collect();
                    cx.notify();
                })))
                .flex()
                .items_center()
                .gap_3()
                .px_3()
                .h(px(ROW_H))
                .flex_none() // the list scrolls; without this the rows shrink to fit it
                .rounded_lg()
                .when(first, |d| d.bg(card_fill(p.accent, true)).shadow(card_shadow(&p, false)))
                .when(!first, |d| d.hover(|s| s.bg(rgba((p.dim << 8) | A_HOVER)))) // the selected card stays itself under the pointer
                .on_click(move |_, window, cx| activate(&hit, window, cx))
                .child(icon)
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(div().line_height(px(18.)).overflow_hidden().whitespace_nowrap().text_ellipsis().child(name))
                        .when_some(why, |d, why| d.child(div().text_size(px(12.)).line_height(px(14.)).text_color(rgb(p.dim)).child(why))),
                )
                // pinned: the state right-click toggles, visible in the row
                .when(is_pinned, |d| d.child(div().flex_none().font_family(NF).text_color(rgb(p.dim)).child("\u{f0403}")))
        };

        let body = if searching {
            let list: Vec<_> =
                self.results().into_iter().enumerate().map(|(i, hit)| row(hit, i, i == self.sel)).collect();
            let none = list.is_empty();
            let q = self.query.clone();
            div()
                .id("results")
                .h(px(BODY_H))
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .flex()
                .flex_col()
                .children(list)
                // the one failure a search can have, said in words (an empty column read as broken)
                .when(none, |d| d.child(div().py_6().flex().justify_center().text_color(rgb(p.dim)).child(format!("Nothing matches \u{201c}{q}\u{201d}"))))
                .into_any_element()
        } else {
            let tiles = self.pins.iter().enumerate().map(|(i, e)| {
                let pin_stem = e.stem.clone();
                div()
                    .id(("pin", i))
                    .on_mouse_down(MouseButton::Right, cx.listener(move |this, _, _, cx| {
                        toggle_pin(&pin_stem);
                        this.pins = pinned().iter().map(|(_, d)| entry_of(d)).collect();
                        cx.notify();
                    }))
                    .w(px(92.))
                    .h(px(TILE_H))
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_1()
                    .border_2()
                    .border_color(ring(StartStop::Pin(i)))
                    .rounded_xl()
                    .bg(card_fill(if i % 2 == 0 { p.accent } else { p.accent2 }, false))
                    .shadow(card_shadow(&p, false))
                    .hover(|s| s.bg(card_fill(if i % 2 == 0 { p.accent } else { p.accent2 }, true)).shadow(card_shadow(&p, true)))
                    .on_click(cx.listener(move |this, _, window, cx| this.run_stop(StartStop::Pin(i), window, cx)))
                    .child(tile_icon(e, 38., &p))
                    .child(
                        div()
                            .max_w(px(86.))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(px(12.))
                            .child(e.name.clone()),
                    )
            });
            let all: Vec<_> = self.all.iter().enumerate().map(|(i, e)| row(Hit::App(e.clone(), None), 1000 + i, i == self.sel)).collect();
            div()
                .h(px(BODY_H))
                .flex()
                .flex_col()
                .gap_2()
                .overflow_hidden()
                .child(section("Pinned"))
                .child(div().flex().flex_row().flex_wrap().justify_center().gap_2().children(tiles).when(self.pins.is_empty(), |d| {
                    // the tile row's height stays reserved, so say what goes there
                    d.h(px(TILE_H)).items_center().text_size(px(12.)).text_color(rgb(p.dim)).child("Right-click an app to pin it")
                }))
                .child(section("All apps").pt_2())
                .child(
                    div()
                        .id("all")
                        .h(px(ROW_H * LIST_ROWS))
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                        .flex()
                        .flex_col()
                        .children(all),
                )
                .into_any_element()
        };

        let stop_btn = |id: &'static str, label: &'static str, tone: u32, s: StartStop| {
            button(id, label, tone, &p).border_color(ring(s)).on_click(cx.listener(move |this, _, window, cx| this.run_stop(s, window, cx)))
        };
        let power_row = match self.confirm {
            Some(i) => {
                let (label, question, _) = POWER[i];
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).child(question))
                    .child(stop_btn("p-ok", label, p.err, StartStop::Confirm))
                    .child(stop_btn("p-cancel", "Cancel", p.accent, StartStop::Cancel))
            }
            None => div().flex().justify_end().gap_3().children(POWER.iter().enumerate().map(|(i, &(label, _, _))| {
                stop_btn(label, label, p.accent, StartStop::PowerAct(i))
            })),
        };

        let footer = div()
            .flex()
            .items_center()
            .gap_3()
            .pt_2()
            .child(avatar(32., &p))
            .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).child("fiaboo"))
            .child(stop_btn("f-settings", "Settings\u{2026}", p.accent, StartStop::Settings))
            .child(stop_btn("f-lock", "Lock", p.accent, StartStop::Lock))
            .child(stop_btn("f-power", "\u{23fb}", p.accent, StartStop::Power).font_family(NF).font_weight(FontWeight::MEDIUM));

        div()
            .font_family(NF).font_weight(FontWeight::MEDIUM)
            .size_full()
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                this.last_input = std::time::Instant::now();
                let k = &ev.keystroke;
                match k.key.as_str() {
                    // Esc closes the topmost layer: the power confirmation, then the power row, then Start
                    "escape" if this.confirm.is_some() => {
                        this.confirm = None;
                        cx.notify();
                    }
                    "escape" if this.power => {
                        this.power = false;
                        cx.notify();
                    }
                    "escape" => close_start(window, cx),
                    "tab" => {
                        let stops = this.stops();
                        let cur = this.focus_on.and_then(|f| stops.iter().position(|s| *s == f));
                        this.focus_on = step_focus(cur, stops.len(), k.modifiers.shift).map(|i| stops[i]);
                        cx.notify();
                    }
                    "enter" if this.focus_on.is_some_and(|f| this.stops().contains(&f)) => {
                        let f = this.focus_on.unwrap();
                        this.run_stop(f, window, cx);
                    }
                    "down" | "up" => {
                        this.focus_on = None; // the arrows are the list's
                        // with no query the arrows walk the whole All apps list
                        let n = if this.query.is_empty() { this.all.len() } else { this.results().len() };
                        if n > 0 {
                            this.sel = if k.key == "down" { (this.sel + 1) % n } else { (this.sel + n - 1) % n };
                            this.scroll.scroll_to_item(this.sel);
                            cx.notify();
                        }
                    }
                    "enter" => {
                        if let Some(hit) = this.results().get(this.sel) {
                            activate(hit, window, cx);
                        }
                    }
                    _ => {} // typed text and the editing keys go to `field`
                }
            }))
            .flex()
            .flex_col()
            .justify_end()
            .items_center() // Win11: opens centered above the Start button
            .pb(px(56.))
            .on_mouse_down(MouseButton::Left, |_, window, cx| close_start(window, cx)).on_mouse_down(MouseButton::Right, |_, window, cx| close_start(window, cx))
            .child(
                div()
                    .w(px(640.0))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                    .flex()
                    .flex_col()
            .gap_3()
            .p_4()
            .rounded_2xl()
            .bg(rgba((p.bg << 8) | PANEL_FILL))
            .border_1()
            .border_color(rgba((p.fg << 8) | A_BORDER))
            .text_color(rgb(p.fg))
            .text_size(px(14.))
            .child(search)
            .child(body)
            .when(self.power, |d| d.child(power_row))
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(rgb(p.dim))
                    .child(if searching {
                        "Enter: open · Backspace: edit · Esc: close"
                    } else {
                        "Type: search · Enter: open · Tab: buttons · Right-click: pin or unpin · Esc: close"
                    }),
            )
            .child(footer)
            )
    }
}
