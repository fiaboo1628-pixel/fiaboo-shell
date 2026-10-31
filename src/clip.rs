//! Clipboard history (cliphist).

use crate::*;

// ---------- clipboard history (cliphist) ----------
pub(crate) struct ClipState(pub(crate) Option<AnyWindowHandle>);
impl Global for ClipState {}

#[derive(Clone)]
pub(crate) struct Clip {
    id: String,
    raw: String,     // the full cliphist line, needed by `cliphist delete`
    preview: String, // text, whitespace collapsed
    image: Option<PathBuf>,
}

/// Text row height: roomier since 2026-10-27 (owner: rows "only just fit the text", 39 -> 44), so 10 rows keep the panel height.
const CLIP_ROW: f32 = 44.;
const CLIP_ROWS: f32 = 10.;

/// API keys and tokens by their well-known prefixes (Anthropic/OpenAI sk-, GitHub ghp_/gho_, Slack xox?-, AWS AKIA).
fn looks_secret(text: &str) -> bool {
    text.split(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == '=' || c == ':').any(|w| {
        let long = w.len() >= 20;
        long && (w.starts_with("sk-") || w.starts_with("ghp_") || w.starts_with("gho_") || w.starts_with("xox") || w.starts_with("AKIA"))
    })
}

#[cfg(test)]
mod secret_tests {
    use super::looks_secret;

    #[test]
    fn keys_are_masked_plain_text_is_not() {
        assert!(looks_secret("ANTHROPIC_API_KEY=sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789"));
        assert!(looks_secret("ghp_0123456789abcdefghijklmnopqrstuvwxyzAB"));
        assert!(looks_secret("export AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE"));
        assert!(!looks_secret("sk-learn is a python library"));
        assert!(!looks_secret("Nothing to see here"));
    }
}

pub(crate) fn read_clips() -> Vec<Clip> {
    let cache = format!("{}/.cache/fiaboo-shell", home());
    let _ = fs::create_dir_all(&cache);
    let mut images = 0;
    let clips = sh("cliphist list")
        .lines()
        .take(80)
        .filter_map(|line| {
            let (id, rest) = line.split_once('\t')?;
            if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let is_img = rest.starts_with("[[ binary data") && (rest.contains("png") || rest.contains("jpg") || rest.contains("jpeg"));
            let image = if is_img && images < 8 {
                images += 1;
                let path = PathBuf::from(format!("{cache}/clip-{id}.img"));
                if !path.exists() {
                    // a failed decode must not leave an empty "image" that is reused forever
                    let tmp = path.with_extension("tmp");
                    let ok = Command::new("sh")
                        .args(["-c", &format!("cliphist decode {id} > '{}'", tmp.display())])
                        .status()
                        .is_ok_and(|s| s.success());
                    if ok && fs::metadata(&tmp).is_ok_and(|m| m.len() > 0) {
                        let _ = fs::rename(&tmp, &path);
                    } else {
                        let _ = fs::remove_file(&tmp);
                    }
                }
                path.exists().then_some(path)
            } else {
                None
            };
            let preview = if is_img {
                "Image".to_string()
            } else if looks_secret(rest) {
                "\u{2022}\u{2022}\u{2022}\u{2022} secret \u{b7} Enter copies".to_string() // never on screen (or on camera); copying still works
            } else {
                rest.split_whitespace().collect::<Vec<_>>().join(" ")
            };
            Some(Clip { id: id.to_string(), raw: line.to_string(), preview, image })
        })
        .collect::<Vec<Clip>>();
    // decoded images of entries that were deleted, wiped or are no longer shown
    if let Ok(rd) = fs::read_dir(&cache) {
        for e in rd.flatten() {
            let p = e.path();
            let ours = p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("clip-") && n.ends_with(".img"));
            if ours && !clips.iter().any(|c| c.image.as_deref() == Some(p.as_path())) {
                let _ = fs::remove_file(&p);
            }
        }
    }
    clips
}

pub(crate) fn clip_copy(id: &str) {
    if id.bytes().all(|b| b.is_ascii_digit()) {
        run_bg(&format!("cliphist decode {id} | wl-copy"));
    }
}

pub(crate) fn clip_delete(raw: &str) {
    use std::io::Write;
    if let Ok(mut child) = Command::new("cliphist").arg("delete").stdin(std::process::Stdio::piped()).spawn() {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = writeln!(stdin, "{raw}");
        }
        let _ = child.wait();
    }
}

pub(crate) fn toggle_clip(cx: &mut App) {
    if let Some(handle) = cx.try_global::<ClipState>().and_then(|s| s.0) {
        cx.set_global(ClipState(None));
        if handle.update(cx, |_, window, _| window.remove_window()).is_ok() {
            return;
        }
    }
    let opened = cx.open_window(
        WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(Bounds { origin: point(px(0.), px(0.)), size: size(px(0.), px(0.)) })),
            app_id: Some("fiaboo-clip".to_string()),
            window_background: WindowBackgroundAppearance::Transparent,
            kind: WindowKind::LayerShell(LayerShellOptions {
                namespace: "fiaboo-clip".to_string(),
                layer: Layer::Overlay,
                anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                exclusive_zone: Some(px(-1.)),
                keyboard_interactivity: KeyboardInteractivity::Exclusive,
                ..Default::default()
            }),
            ..Default::default()
        },
        |window, cx| cx.new(|cx| ClipPanel::new(window, cx)),
    );
    if let Ok(h) = opened {
        cx.set_global(ClipState(Some(h.into())));
    }
}

pub(crate) fn close_clip(window: &mut Window, cx: &mut App) {
    cx.set_global(ClipState(None));
    window.remove_window();
}

pub(crate) struct ClipPanel {
    pal: Palette,
    focus: FocusHandle,
    clips: Vec<Clip>,
    query: String,
    sel: usize,
    last_input: std::time::Instant,
    confirm_wipe: bool, // "Clear all" asks once before deleting the whole history
    loaded: bool,       // cliphist answered (until then an empty list is not "Nothing here")
    hidden: std::collections::HashSet<String>, // deleted, still undoable
    pending: Option<(Vec<Clip>, bool, u64)>,   // (entries, whole history?, timer token)
    undo_token: u64,
}

const UNDO_SECS: u64 = 5;

impl ClipPanel {
    /// Hide `clips` now; cliphist forgets them after UNDO_SECS unless `undo` runs first.
    fn defer_delete(&mut self, clips: Vec<Clip>, wipe: bool, cx: &mut Context<Self>) {
        self.commit_pending();
        self.hidden.extend(clips.iter().map(|c| c.id.clone()));
        self.undo_token += 1;
        let token = self.undo_token;
        self.pending = Some((clips, wipe, token));
        self.sel = self.sel.min(self.visible().len().saturating_sub(1));
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(UNDO_SECS)).await;
            let _ = this.update(cx, |t, cx| {
                if t.pending.as_ref().is_some_and(|p| p.2 == token) {
                    t.commit_pending();
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn commit_pending(&mut self) {
        if let Some((clips, wipe, _)) = self.pending.take() {
            for c in &clips {
                self.hidden.remove(&c.id);
            }
            if wipe {
                run_bg("cliphist wipe");
                self.clips.clear();
            } else {
                let ids: Vec<String> = clips.iter().map(|c| c.id.clone()).collect();
                self.clips.retain(|c| !ids.contains(&c.id));
                std::thread::spawn(move || clips.iter().for_each(|c| clip_delete(&c.raw)));
            }
        }
    }

    fn undo(&mut self) {
        if let Some((clips, _, _)) = self.pending.take() {
            for c in clips {
                self.hidden.remove(&c.id);
            }
        }
    }
}

// closing the panel (Esc, Enter, idle) must not bring back what the owner deleted
impl Drop for ClipPanel {
    fn drop(&mut self) {
        self.commit_pending();
    }
}

impl ClipPanel {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        // exclusive keyboard focus: close after 90 s without a key press
        cx.spawn_in(window, async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let idle = this.update(cx, |c, _| c.last_input.elapsed() > Duration::from_secs(90)).unwrap_or(true);
            if idle {
                let _ = this.update_in(cx, |_, window, cx| close_clip(window, cx));
                break;
            }
        })
        .detach();
        // cliphist + image decodes take a while: load off the UI thread
        cx.spawn(async move |this, cx| {
            let clips = cx.background_executor().spawn(async { read_clips() }).await;
            let _ = this.update(cx, |c, cx| {
                c.clips = clips;
                c.loaded = true;
                cx.notify();
            });
        })
        .detach();
        ClipPanel {
            pal: palette(),
            focus,
            clips: Vec::new(),
            query: String::new(),
            sel: 0,
            last_input: std::time::Instant::now(),
            confirm_wipe: false,
            loaded: false,
            hidden: Default::default(),
            pending: None,
            undo_token: 0,
        }
    }

    fn visible(&self) -> Vec<Clip> {
        let q = self.query.to_lowercase();
        self.clips.iter().filter(|c| !self.hidden.contains(&c.id) && c.preview.to_lowercase().contains(&q)).take(40).cloned().collect()
    }
}

impl Render for ClipPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.pal.clone();
        let small_btn = |id: &'static str, label: String| {
            div()
                .id(id)
                .cursor_pointer()
                .px_4()
                .h(px(44.))
                .flex()
                .items_center()
                .rounded_lg()
                .text_size(px(12.))
                .bg(card_fill(p.accent, false))
                .shadow(card_shadow(&p, false))
                .hover(|s| s.bg(rgba((p.accent << 8) | A_ACTIVE)).shadow(card_shadow(&p, true)))
                .child(label)
        };
        let list = self.visible();
        let sel = self.sel.min(list.len().saturating_sub(1));
        let rows = list.iter().enumerate().map(|(i, c)| {
            let (copy_id, copy_id2) = (c.id.clone(), c.id.clone());
            div()
                .id(("clip", i))
                .cursor_pointer()
                .flex_none()
                .h(px(if c.image.is_some() { 2. * CLIP_ROW } else { CLIP_ROW })) // image rows take two rows, so the list stays on the grid
                .flex()
                .items_center()
                .gap_3()
                .px_4()
                .rounded_lg()
                .when(i == sel, |d| d.bg(card_fill(p.accent, true)).shadow(card_shadow(&p, false)))
                .hover(|s| s.bg(rgba((p.dim << 8) | A_HOVER)))
                .on_mouse_down(MouseButton::Right, cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.last_input = std::time::Instant::now();
                    clip_copy(&copy_id2); // copy and stay: pick several in a row
                    cx.notify();
                }))
                .on_click(move |_, window, cx| {
                    clip_copy(&copy_id);
                    close_clip(window, cx);
                })
                .child(match &c.image {
                    Some(path) => img(path.clone()).h(px(2. * CLIP_ROW - 16.)).max_w(px(300.)).object_fit(ObjectFit::Contain).into_any_element(),
                    None => div()
                        .flex_1()
                        .min_w(px(0.))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(c.preview.clone())
                        .into_any_element(),
                })
        });
        div()
            .font_family(NF).font_weight(FontWeight::MEDIUM)
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                this.last_input = std::time::Instant::now();
                let k = &ev.keystroke;
                let n = this.visible().len();
                match k.key.as_str() {
                    "escape" => close_clip(window, cx),
                    "backspace" => {
                        this.query.pop();
                        this.sel = 0;
                        cx.notify();
                    }
                    "down" if n > 0 => {
                        this.sel = (this.sel + 1) % n;
                        cx.notify();
                    }
                    "up" if n > 0 => {
                        this.sel = (this.sel + n - 1) % n;
                        cx.notify();
                    }
                    "enter" => {
                        if let Some(c) = this.visible().get(this.sel) {
                            clip_copy(&c.id);
                        }
                        close_clip(window, cx);
                    }
                    "delete" => {
                        if let Some(c) = this.visible().get(this.sel).cloned() {
                            this.defer_delete(vec![c], false, cx);
                        }
                    }
                    "z" if k.modifiers.control => {
                        this.undo();
                        cx.notify();
                    }
                    _ => {
                        if !k.modifiers.control && !k.modifiers.alt && !k.modifiers.platform {
                            if let Some(ch) = &k.key_char {
                                this.query.push_str(ch);
                                this.sel = 0;
                                cx.notify();
                            }
                        }
                    }
                }
            }))
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(MouseButton::Left, |_, window, cx| close_clip(window, cx))
            .on_mouse_down(MouseButton::Right, |_, window, cx| close_clip(window, cx))
            .child(
                div()
                    .w(px(600.))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                    .flex()
                    .flex_col()
                    .gap_4()
                    .p_6()
                    .rounded_2xl()
                    .bg(rgba((p.bg << 8) | PANEL_FILL))
                    .border_1()
                    .border_color(rgba((p.fg << 8) | A_BORDER))
                    .text_color(rgb(p.fg))
                    .text_size(px(14.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .h(px(44.))
                                    .flex()
                                    .items_center()
                                    .px_4()
                                    .rounded_xl()
                                    .bg(card_fill(p.accent, false))
                                    .gap_2()
                                    .child(div().text_color(rgb(p.dim)).child("\u{f002}"))
                                    .child(if self.query.is_empty() {
                                        div().text_color(rgb(p.dim)).child("Search clipboard history…")
                                    } else {
                                        div().child(format!("{}\u{258f}", self.query))
                                    }),
                            )
                            .children(if self.confirm_wipe {
                                let n = self.clips.len();
                                vec![
                                    small_btn("clip-keep", "Keep".into()).on_click(cx.listener(|this, _, _, cx| {
                                        this.confirm_wipe = false;
                                        cx.notify();
                                    })),
                                    small_btn("clip-wipe-yes", if n == 1 { "Delete 1 item".to_string() } else { format!("Delete {n} items") })
                                        .bg(rgba((p.err << 8) | 0xcc))
                                        .text_color(rgb(p.bg)) // on light error red: dark text (white was ~1.7:1)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.confirm_wipe = false;
                                            let all = this.clips.clone();
                                            this.defer_delete(all, true, cx);
                                        })),
                                ]
                            } else {
                                vec![small_btn("clip-wipe", "Clear all".into()).on_click(cx.listener(|this, _, _, cx| {
                                    this.confirm_wipe = true;
                                    cx.notify();
                                }))]
                            }),
                    )
                    .child(
                        div()
                            .id("clip-list")
                            .h(px(CLIP_ROWS * CLIP_ROW)) // whole rows: the fixed 560 px panel cut the 12th
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .children(rows)
                            .when(list.is_empty() && self.loaded, |d| {
                                d.child(div().py_6().flex().justify_center().text_color(rgb(p.dim)).child("Nothing here"))
                            }),
                    )
                    // a deletion waits UNDO_SECS here before cliphist forgets it
                    .when_some(self.pending.as_ref().map(|p| p.0.len()), |d, n| {
                        d.child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_size(px(12.))
                                .text_color(rgb(p.dim))
                                .child(div().flex_1().child(if n == 1 { "Deleted 1 item".to_string() } else { format!("Deleted {n} items") }))
                                .child(small_btn("clip-undo", "Undo".into()).bg(rgba((p.accent << 8) | A_ACTIVE)).on_click(cx.listener(|this, _, _, cx| {
                                    this.undo();
                                    cx.notify();
                                }))),
                        )
                    })
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(p.dim))
                            .child("Enter: copy · Right-click: copy and stay · Del: remove · Esc: close"),
                    ),
            )
    }
}
