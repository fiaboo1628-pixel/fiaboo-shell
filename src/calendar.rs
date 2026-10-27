//! Calendar popup with weather and the lunar date.

use crate::*;
use gpui::{Animation, AnimationExt};

// ---------- calendar + weather ----------
#[derive(Clone)]
pub(crate) struct Day {
    label: String,
    code: i64,
    tmax: f64,
    tmin: f64,
}

#[derive(Clone)]
pub(crate) struct Weather {
    city: String,
    temp: f64,
    code: i64,
    is_day: bool,
    days: Vec<Day>,
}

/// Weather location, in this order:
///  1. ~/.config/fiaboo-shell/weather.json  {"city": "...", "lat": 10.82, "lon": 106.63}  (manual override)
///  2. IP geolocation (ipwho.is), cached in ~/.cache/fiaboo-shell/location.json for 6 hours
///  3. Ho Chi Minh City
pub(crate) fn weather_location() -> (String, f64, f64) {
    let read = |path: String| -> serde_json::Value {
        fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    };
    let pick = |v: &serde_json::Value| -> Option<(String, f64, f64)> {
        Some((v["city"].as_str()?.to_string(), v["lat"].as_f64()?, v["lon"].as_f64()?))
    };
    if let Some(loc) = pick(&read(format!("{}/.config/fiaboo-shell/weather.json", home()))) {
        return loc;
    }
    let cache = format!("{}/.cache/fiaboo-shell/location.json", home());
    let fresh = fs::metadata(&cache)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < Duration::from_secs(6 * 3600));
    if fresh {
        if let Some(loc) = pick(&read(cache.clone())) {
            return loc;
        }
    }
    let geo = Command::new("curl")
        .args(["-s", "-m", "8", "https://ipwho.is/?fields=success,city,latitude,longitude"])
        .output()
        .ok()
        .and_then(|o| serde_json::from_slice::<serde_json::Value>(&o.stdout).ok())
        .filter(|v| v["success"].as_bool() == Some(true));
    if let Some(v) = geo {
        if let (Some(city), Some(lat), Some(lon)) = (v["city"].as_str(), v["latitude"].as_f64(), v["longitude"].as_f64()) {
            let _ = fs::create_dir_all(format!("{}/.cache/fiaboo-shell", home()));
            let _ = fs::write(&cache, serde_json::json!({"city": city, "lat": lat, "lon": lon}).to_string());
            return (city.to_string(), lat, lon);
        }
    }
    // network down: reuse an old cached location if there is one
    if let Some(loc) = pick(&read(cache)) {
        return loc;
    }
    ("Ho Chi Minh City".to_string(), 10.8231, 106.6297)
}

pub(crate) fn weekday_short(date: &str) -> String {
    let p: Vec<i32> = date.split('-').filter_map(|x| x.parse().ok()).collect();
    if p.len() != 3 {
        return String::new();
    }
    let w = weekday(p[0], p[1] as u32, p[2] as u32);
    ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"][w as usize].to_string()
}

pub(crate) fn fetch_weather() -> Option<Weather> {
    let (city, lat, lon) = weather_location();
    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={lat}&longitude={lon}&current=temperature_2m,weather_code,is_day&daily=weather_code,temperature_2m_max,temperature_2m_min&timezone=auto&forecast_days=5"
    );
    let out = Command::new("curl").args(["-s", "-m", "10", &url]).output().ok()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let c = &v["current"];
    let d = &v["daily"];
    let days = (0..5)
        .filter_map(|i| {
            Some(Day {
                label: weekday_short(d["time"][i].as_str()?),
                code: d["weather_code"][i].as_i64()?,
                tmax: d["temperature_2m_max"][i].as_f64()?,
                tmin: d["temperature_2m_min"][i].as_f64()?,
            })
        })
        .collect();
    Some(Weather {
        city,
        temp: c["temperature_2m"].as_f64()?,
        code: c["weather_code"].as_i64()?,
        is_day: c["is_day"].as_i64().unwrap_or(1) == 1,
        days,
    })
}

pub(crate) static WEATHER: Mutex<Option<(std::time::Instant, Weather)>> = Mutex::new(None);

pub(crate) fn cached_weather() -> Option<Weather> {
    WEATHER.lock().unwrap().as_ref().map(|(_, w)| w.clone())
}

pub(crate) fn weather_stale() -> bool {
    WEATHER.lock().unwrap().as_ref().is_none_or(|(t, _)| t.elapsed() > Duration::from_secs(1800))
}

pub(crate) fn weather_glyph(code: i64, day: bool) -> &'static str {
    match code {
        0 => if day { "\u{f0599}" } else { "\u{f0594}" },
        1 | 2 => if day { "\u{f0595}" } else { "\u{f0f31}" },
        3 => "\u{f0590}",
        45 | 48 => "\u{f0591}",
        51..=57 | 61 | 63 | 80 | 81 => "\u{f0597}",
        65 | 66 | 67 | 82 => "\u{f0596}",
        71..=77 | 85 | 86 => "\u{f0598}",
        95..=99 => "\u{f0593}",
        _ => "\u{f0590}",
    }
}

pub(crate) fn weather_text(code: i64) -> &'static str {
    match code {
        0 => "Clear sky",
        1 => "Mostly clear",
        2 => "Partly cloudy",
        3 => "Overcast",
        45 | 48 => "Fog",
        51..=57 => "Drizzle",
        61 | 63 | 80 | 81 => "Rain",
        65..=67 | 82 => "Heavy rain",
        71..=77 | 85 | 86 => "Snow",
        95..=99 => "Thunderstorm",
        _ => "Unknown",
    }
}

/// 0 = Sunday
pub(crate) fn weekday(y: i32, m: u32, d: u32) -> u32 {
    const T: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let y = if m < 3 { y - 1 } else { y };
    ((y + y / 4 - y / 100 + y / 400 + T[(m - 1) as usize] + d as i32).rem_euclid(7)) as u32
}

pub(crate) fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 { 29 } else { 28 },
    }
}

pub(crate) fn month_name(m: u32) -> &'static str {
    ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"]
        [(m - 1) as usize]
}

/// "dd/mm", the date prefix notifications are stamped with
pub(crate) fn today_dm() -> String {
    let (_, m, d) = today_ymd();
    format!("{d:02}/{m:02}")
}

pub(crate) fn today_ymd() -> (i32, u32, u32) {
    // notification cards ask on every render: fork `date` at most every 20 s
    static LAST: Mutex<Option<(std::time::Instant, (i32, u32, u32))>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap();
    if let Some((at, ymd)) = *last {
        if at.elapsed() < Duration::from_secs(20) {
            return ymd;
        }
    }
    let t = sh("date +%Y-%m-%d");
    let p: Vec<i32> = t.split('-').filter_map(|x| x.parse().ok()).collect();
    let ymd = if p.len() == 3 { (p[0], p[1] as u32, p[2] as u32) } else { (2026, 1, 1) };
    *last = Some((std::time::Instant::now(), ymd));
    ymd
}

pub(crate) struct CalState(pub(crate) Option<AnyWindowHandle>);
impl Global for CalState {}

pub(crate) fn toggle_calendar(cx: &mut App) {
    if let Some(handle) = cx.try_global::<CalState>().and_then(|s| s.0) {
        cx.set_global(CalState(None));
        if handle.update(cx, |_, window, _| window.remove_window()).is_ok() {
            return;
        }
    }
    let opened = cx.open_window(
        WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: point(px(0.), px(0.)),
                size: size(px(0.), px(0.)),
            })),
            app_id: Some("fiaboo-calendar".to_string()),
            window_background: WindowBackgroundAppearance::Transparent,
            kind: WindowKind::LayerShell(LayerShellOptions {
                namespace: "fiaboo-calendar".to_string(),
                layer: Layer::Overlay,
                anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                exclusive_zone: Some(px(-1.)),
                keyboard_interactivity: KeyboardInteractivity::Exclusive,
                ..Default::default()
            }),
            ..Default::default()
        },
        |window, cx| cx.new(|cx| CalendarPopup::new(window, cx)),
    );
    if let Ok(h) = opened {
        cx.set_global(CalState(Some(h.into())));
    }
}

pub(crate) fn close_calendar(window: &mut Window, cx: &mut App) {
    cx.set_global(CalState(None));
    window.remove_window();
}

pub(crate) struct CalendarPopup {
    pal: Palette,
    focus: FocusHandle,
    month_offset: i32,
    today: (i32, u32, u32),
    weather: Option<Weather>,
    weather_failed: bool, // the last fetch failed and nothing is cached: say so instead of loading forever
    retry: bool,          // "click to retry" pressed; the poll loop takes it
    last_input: std::time::Instant,
}

impl CalendarPopup {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let mut failed_at: Option<std::time::Instant> = None;
        cx.spawn_in(window, async move |this, cx| loop {
            // offline: retry once a minute, not on every 1.5 s tick
            let retry = this.update(cx, |c, _| std::mem::take(&mut c.retry)).unwrap_or(false);
            if weather_stale() && (retry || failed_at.is_none_or(|t| t.elapsed() > Duration::from_secs(60))) {
                match cx.background_executor().spawn(async { fetch_weather() }).await {
                    Some(w) => *WEATHER.lock().unwrap() = Some((std::time::Instant::now(), w)),
                    None => failed_at = Some(std::time::Instant::now()),
                }
            }
            let idle = this
                .update(cx, |c, cx| {
                    c.weather = cached_weather();
                    c.weather_failed = c.weather.is_none() && failed_at.is_some();
                    cx.notify();
                    c.last_input.elapsed() > Duration::from_secs(90)
                })
                .unwrap_or(true);
            if idle {
                let _ = this.update_in(cx, |_, window, cx| close_calendar(window, cx));
                break;
            }
            cx.background_executor().timer(Duration::from_millis(1500)).await;
        })
        .detach();
        CalendarPopup {
            pal: palette(),
            focus,
            month_offset: 0,
            today: today_ymd(),
            weather: cached_weather(),
            weather_failed: false,
            retry: false,
            last_input: std::time::Instant::now(),
        }
    }

    fn shown_month(&self) -> (i32, u32) {
        let total = self.today.0 * 12 + (self.today.1 as i32 - 1) + self.month_offset;
        (total.div_euclid(12), (total.rem_euclid(12) + 1) as u32)
    }
}

impl Render for CalendarPopup {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.pal.clone();
        let (year, month) = self.shown_month();

        // ---- weather card ----
        let card = match &self.weather {
            Some(w) => div()
                .flex()
                .flex_col()
                .gap_3()
                .p_4()
                .rounded_2xl()
                .bg(gpui::linear_gradient(
                    135.,
                    gpui::linear_color_stop(rgba((p.accent << 8) | A_HOVER), 0.),
                    gpui::linear_color_stop(rgba((p.dim << 8) | A_SURFACE), 1.),
                ))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .child(div().text_size(px(14.)).text_color(rgb(p.fg)).child(w.city.clone()))
                                .child(
                                    div()
                                        .flex()
                                        .items_start()
                                        .child(
                                            div()
                                                .text_size(px(60.))
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .child(format!("{:.0}", w.temp)),
                                        )
                                        .child(div().text_size(px(22.)).pt_2().child("\u{b0}C")),
                                )
                                .child(div().text_size(px(16.)).child(weather_text(w.code))),
                        )
                        .child(
                            div()
                                .font_family(NF).font_weight(FontWeight::MEDIUM)
                                .text_size(px(78.))
                                .pr_4() // Nerd Font weather glyphs draw wider than their advance and were clipped
                                .text_color(rgb(p.accent))
                                .child(weather_glyph(w.code, w.is_day)),
                        ),
                )
                .into_any_element(),
            None if self.weather_failed => div()
                .id("w-retry")
                .cursor_pointer()
                .h(px(150.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_2xl()
                .bg(rgba((p.dim << 8) | A_SURFACE))
                .hover(|s| s.bg(rgba((p.accent << 8) | A_HOVER)))
                .text_color(rgb(p.dim))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.last_input = std::time::Instant::now();
                    this.retry = true;
                    this.weather_failed = false; // shows "Loading…" until the poll answers
                    cx.notify();
                }))
                .child("Weather unavailable \u{b7} click to retry")
                .into_any_element(),
            None => div()
                .h(px(150.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_2xl()
                .bg(rgba((p.dim << 8) | A_SURFACE))
                .text_color(rgb(p.dim))
                .child("Loading weather\u{2026}")
                .into_any_element(),
        };

        // ---- 5-day forecast ----
        // one roomy row per day (was five 72 px tiles of three 12 px lines each: "toàn chữ")
        let forecast = div().flex().flex_col().gap_1().children(
            self.weather.iter().flat_map(|w| w.days.iter().cloned()).enumerate().map(|(i, d)| {
                div()
                    .h(px(44.))
                    .flex()
                    .items_center()
                    .gap_4()
                    .px_4()
                    .rounded_xl()
                    .when(i == 0, |r| r.bg(rgba((p.accent << 8) | A_HOVER)))
                    .child(div().w(px(56.)).text_color(if i == 0 { rgb(p.fg) } else { rgb(p.dim) }).child(if i == 0 { "Today".to_string() } else { d.label.clone() }))
                    .child(div().flex_1().font_family(NF).font_weight(FontWeight::MEDIUM).text_size(px(22.)).text_color(rgb(p.accent)).child(weather_glyph(d.code, true)))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(format!("{:.0}\u{b0}", d.tmax)))
                    .child(div().w(px(36.)).text_color(rgb(p.dim)).flex().justify_end().child(format!("{:.0}\u{b0}", d.tmin)))
            }),
        );

        // ---- calendar ----
        let first = (weekday(year, month, 1) + 6) % 7; // Monday first
        let dim_cur = days_in_month(year, month);
        let (py, pm) = if month == 1 { (year - 1, 12) } else { (year, month - 1) };
        let dim_prev = days_in_month(py, pm);
        let nav = |id: &'static str, label: &'static str, delta: i32| {
            div()
                .id(id)
                .cursor_pointer()
                .size(px(36.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_md()
                .text_size(px(18.))
                .hover(|s| s.bg(rgba((p.accent << 8) | A_HOVER)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.last_input = std::time::Instant::now();
                    this.month_offset += delta;
                    cx.notify();
                }))
                .child(label)
        };
        let weekdays = div().flex().children(["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"].iter().map(|d| {
            div().flex_1().flex().justify_center().text_size(px(12.)).text_color(rgb(p.dim)).child(*d)
        }));
        let today = self.today;
        let p2 = p.clone();
        let weeks = (0..6).map(|r| {
            let p = p2.clone();
            // a row of only next-month days is left blank; it stays so the panel height (and ‹ ›) don't move
            let spare = r * 7 >= first + dim_cur;
            div().h(px(52.)).flex().when(!spare, |d| d.children((0..7).map(move |c| {
                let idx = r * 7 + c;
                let (day, current) = if idx < first {
                    (dim_prev - (first - idx - 1), false)
                } else if idx - first < dim_cur {
                    (idx - first + 1, true)
                } else {
                    (idx - first - dim_cur + 1, false)
                };
                let is_today = current && (year, month, day) == today;
                let weekend = c >= 5;
                // lunar date only where it matters -- the 1st (as d/m), the 15th, today -- not under
                // every day: 42 extra numbers made the panel "nhiều chữ quá" (owner, 2026-10-27)
                let lunar_txt = current.then(|| lunar::solar_to_lunar(day as i32, month as i32, year)).and_then(|(ld, lm, _)| {
                    if ld == 1 { Some(format!("{ld}/{lm}")) } else if ld == 15 || is_today { Some(ld.to_string()) } else { None }
                });
                div().flex_1().h(px(52.)).flex().items_center().justify_center().child(
                    div()
                        .size(px(46.))
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .text_size(px(14.))
                        .when(is_today, |d| d.bg(rgb(p.accent)).text_color(rgb(p.bg)).font_weight(FontWeight::SEMIBOLD))
                        .when(!is_today, |d| {
                            d.text_color(if !current {
                                rgba((p.dim << 8) | A_STRONG)
                            } else if weekend {
                                rgb(p.accent)
                            } else {
                                rgb(p.fg)
                            })
                        })
                        .child(day.to_string())
                        .children(lunar_txt.map(|t| {
                            div().text_size(px(12.)).line_height(px(12.)).when(!is_today, |d| d.text_color(rgb(p.dim))).child(t)
                        })),
                )
            })))
        });

        let calendar = div()
            .flex_1()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .pb_1()
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap_2()
                            .child(div().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).child(month_name(month)))
                            .child(div().text_size(px(14.)).text_color(rgb(p.dim)).child(year.to_string())),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(nav("c-prev", "\u{2039}", -1))
                            .child(
                                div()
                                    .id("c-today")
                                    .cursor_pointer()
                                    .px_2()
                                    .py_1()
                                    .rounded_lg()
                                    .text_size(px(12.))
                                    .hover(|s| s.bg(rgba((p.accent << 8) | A_HOVER)))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.last_input = std::time::Instant::now();
                                        this.month_offset = 0;
                                        cx.notify();
                                    }))
                                    .child("Today"),
                            )
                            .child(nav("c-next", "\u{203a}", 1)),
                    ),
            )
            .child(weekdays)
            .children(weeks);

        div()
            .font_family(NF).font_weight(FontWeight::MEDIUM)
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                this.last_input = std::time::Instant::now();
                match ev.keystroke.key.as_str() {
                    "escape" => close_calendar(window, cx),
                    "left" => {
                        this.month_offset -= 1;
                        cx.notify();
                    }
                    "right" => {
                        this.month_offset += 1;
                        cx.notify();
                    }
                    _ => {}
                }
            }))
            .flex()
            .flex_col()
            .justify_start() // drops from the top edge (owner, 2026-10-27); the slide is Hyprland's layer animation
            .items_center()
            .pt(px(12.))
            .on_mouse_down(MouseButton::Left, |_, window, cx| close_calendar(window, cx)).on_mouse_down(MouseButton::Right, |_, window, cx| close_calendar(window, cx))
            .child(
                div()
                    .w(px(820.0))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                    .flex()
                    .flex_row()
            .gap_6()
            .p_6()
            .rounded_2xl()
            .bg(rgba((p.bg << 8) | GLASS))
            .border_1()
            .border_color(rgba((p.fg << 8) | A_BORDER))
            .text_color(rgb(p.fg))
            .text_size(px(14.))
            .child(calendar)
            .child(div().w(px(320.)).flex().flex_col().gap_4().child(card).child(forecast))
            // drops in from the top edge (owner, 2026-10-27). Done here, not with a Hyprland
            // "slide top" layer rule: on this full-screen glass layer that left the panel
            // invisible for seconds after opening. Position only, no fade: the glass keys on
            // the panel's exact fill colour, which an opacity ramp would change.
            .with_animation(
                "cal-drop",
                Animation::new(std::time::Duration::from_millis(400)).with_easing(gpui::ease_out_quint()),
                |panel, t| panel.mt(px(-40. * (1. - t))),
            )
            )
    }
}
