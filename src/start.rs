//! Start menu.

use crate::*;

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
        crate::overlay_options("fiaboo-start"),
        |window, cx| cx.new(|cx| StartMenu::new(window, cx)),
    );
    if let Ok(h) = opened {
        cx.set_global(StartState(Some(h.into())));
    }
}

pub(crate) struct StartMenu {
    pal: Palette,
    query: String, // mirrors `field`
    field: gpui::Entity<SearchField>,
    focus: FocusHandle,
    all: Vec<Entry>,
    pins: Vec<Entry>,
    sel: usize, // highlighted result (arrow keys); with no query it walks the all-apps list
    scroll: gpui::ScrollHandle,
    last_input: std::time::Instant,
    power: bool,            // power row shown
    confirm: Option<usize>, // POWER action waiting for confirmation
    wins: Vec<serde_json::Value>, // Hyprland clients when Start opened
}

impl StartMenu {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let mut all: Vec<Entry> = apps().0.iter().map(entry_of).collect();
        // apps A–Z first, then configuration tools (network editor, input method setup…) A–Z
        all.sort_by_key(|e| (e.settings, e.name.to_lowercase()));
        all.dedup_by(|a, b| a.stem == b.stem);
        let pins = pinned().iter().map(|(_, d)| entry_of(d)).collect();
        let field = cx.new(|_| SearchField::new(focus.clone(), "Search apps and windows, or type a sum…", palette().dim));
        cx.observe(&field, |m, field, cx| {
            m.last_input = std::time::Instant::now();
            if m.query != field.read(cx).text {
                m.query = field.read(cx).text.clone();
                m.sel = 0;
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
            focus,
            all,
            pins,
            sel: 0,
            scroll: gpui::ScrollHandle::new(),
            last_input: std::time::Instant::now(),
            power: false,
            confirm: None,
            wins: vec![],
        }
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
            .child(div().text_color(rgb(p.fg)).child(SearchLine(self.field.clone())));

        let section = |title: &'static str| {
            div().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).text_color(rgb(p.dim)).child(title)
        };

        let row = |hit: Hit, i: usize, first: bool| {
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
                .cursor_pointer()
                .flex()
                .items_center()
                .gap_3()
                .px_3()
                .h(px(ROW_H))
                .flex_none() // the list scrolls; without this the rows shrink to fit it
                .rounded_lg()
                .when(first, |d| d.bg(card_fill(p.accent, true)).shadow(card_shadow(&p, false)))
                .hover(|s| s.bg(rgba((p.dim << 8) | A_HOVER)))
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
                let stem = e.stem.clone();
                let pin_stem = e.stem.clone();
                div()
                    .id(("pin", i))
                    .on_mouse_down(MouseButton::Right, cx.listener(move |this, _, _, cx| {
                        toggle_pin(&pin_stem);
                        this.pins = pinned().iter().map(|(_, d)| entry_of(d)).collect();
                        cx.notify();
                    }))
                    .cursor_pointer()
                    .w(px(92.))
                    .h(px(TILE_H))
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_1()
                    .rounded_lg()
                    .bg(card_fill(if i % 2 == 0 { p.accent } else { p.accent2 }, false))
                    .shadow(card_shadow(&p, false))
                    .hover(|s| s.bg(card_fill(if i % 2 == 0 { p.accent } else { p.accent2 }, true)).shadow(card_shadow(&p, true)))
                    .on_click(move |_, window, cx| {
                        launch_app(&stem);
                        close_start(window, cx);
                    })
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
                .child(div().flex().flex_row().flex_wrap().justify_center().gap_2().children(tiles))
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

        let footer_btn = |id: &'static str, label: &'static str, cmd: &'static str, tone: u32| {
            button(id, label, tone, &p).on_click(move |_, window, cx| {
                spawn_bg(Command::new("sh").args(["-c", &format!("setsid {cmd} >/dev/null 2>&1 &")]));
                close_start(window, cx);
            })
        };

        let small_btn = |id: &'static str, label: &'static str| button(id, label, p.accent, &p);
        let power_row = match self.confirm {
            Some(i) => {
                let (label, question, cmd) = POWER[i];
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).child(question))
                    .child(footer_btn("p-ok", label, cmd, p.err))
                    .child(small_btn("p-cancel", "Cancel").on_click(cx.listener(|this, _, _, cx| {
                        this.confirm = None;
                        cx.notify();
                    })))
            }
            None => div().flex().justify_end().gap_3().children(POWER.iter().enumerate().map(|(i, &(label, _, _))| {
                small_btn(label, label).on_click(cx.listener(move |this, _, _, cx| {
                    this.confirm = Some(i);
                    cx.notify();
                }))
            })),
        };

        let footer = div()
            .flex()
            .items_center()
            .gap_3()
            .pt_2()
            .child(
                img(PathBuf::from(format!("{}/Pictures/fiaboo/cauvang.jpg", home())))
                    .size(px(32.))
                    .rounded_full()
                    .object_fit(ObjectFit::Cover),
            )
            .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).child("fiaboo"))
            .child(small_btn("f-settings", "Settings").on_click(|_, window, cx| {
                close_start(window, cx);
                toggle_settings(cx);
            }))
            .child(footer_btn("f-lock", "Lock", "hyprlock", p.accent))
            .child(small_btn("f-power", "\u{23fb}").font_family(NF).font_weight(FontWeight::MEDIUM).on_click(cx.listener(|this, _, _, cx| {
                this.power = !this.power;
                this.confirm = None;
                cx.notify();
            })));

        div()
            .font_family(NF).font_weight(FontWeight::MEDIUM)
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                this.last_input = std::time::Instant::now();
                let k = &ev.keystroke;
                match k.key.as_str() {
                    "escape" => close_start(window, cx),
                    "backspace" => this.field.update(cx, |f, cx| f.backspace(cx)),
                    "down" | "up" => {
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
                    _ => {} // typed text arrives through `field`
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
                        "Type: search · Enter: open · Right-click: pin or unpin · Esc: close"
                    }),
            )
            .child(footer)
            )
    }
}
