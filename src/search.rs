//! The search text of Start, Clipboard and Wallpaper. fcitx5/Bamboo deliver commits and preedit
//! only through a GPUI input handler, never as KeyDownEvent.key_char, so the panels type here.
//! Plain keys reach it too: GPUI hands an unhandled key's key_char to the input handler.
use crate::*;
use gpui::{
    Bounds, ElementId, ElementInputHandler, Entity, EntityInputHandler, GlobalElementId, LayoutId,
    Pixels, Rgba, ShapedLine, SharedString, Style, TextRun, UTF16Selection, UnderlineStyle,
};
use std::ops::Range;

pub(crate) struct SearchField {
    focus: FocusHandle, // the panel's own: its key handler keeps arrows, Enter, Esc and Backspace
    placeholder: SharedString,
    placeholder_color: Rgba,
    pub(crate) text: String,
    marked: Option<Range<usize>>, // IME preedit, bytes
    last: Option<(ShapedLine, Bounds<Pixels>)>,
}

impl SearchField {
    pub(crate) fn new(focus: FocusHandle, placeholder: &'static str, placeholder_color: u32) -> Self {
        SearchField { focus, placeholder: placeholder.into(), placeholder_color: rgb(placeholder_color), text: String::new(), marked: None, last: None }
    }

    pub(crate) fn backspace(&mut self, cx: &mut Context<Self>) {
        self.text.pop();
        self.marked = self.marked.take().map(|m| m.start..m.end.min(self.text.len())).filter(|m| m.start < m.end);
        cx.notify();
    }
}

/// Byte offset of a UTF-16 offset; past the end clamps to the end, inside a char rounds up.
fn utf8_at(s: &str, utf16: usize) -> usize {
    let mut n = 0;
    for (i, ch) in s.char_indices() {
        if n >= utf16 {
            return i;
        }
        n += ch.len_utf16();
    }
    s.len()
}

fn utf16_at(s: &str, utf8: usize) -> usize {
    s.char_indices().take_while(|(i, _)| *i < utf8).map(|(_, ch)| ch.len_utf16()).sum()
}

/// A UTF-16 range from the IME as a valid byte range of `s`, whatever the IME sent.
fn utf8_range(s: &str, r: &Range<usize>) -> Range<usize> {
    let (a, b) = (utf8_at(s, r.start), utf8_at(s, r.end));
    a.min(b)..a.max(b)
}

fn utf16_range(s: &str, r: &Range<usize>) -> Range<usize> {
    utf16_at(s, r.start)..utf16_at(s, r.end)
}

/// Replace `range` (UTF-16; None = the preedit, else the end) by `new`; `mark` makes it the preedit.
fn splice(text: &mut String, marked: &mut Option<Range<usize>>, range: Option<Range<usize>>, new: &str, mark: bool) {
    let r = range.map(|r| utf8_range(text, &r)).or(marked.clone()).unwrap_or(text.len()..text.len());
    text.replace_range(r.clone(), new);
    *marked = (mark && !new.is_empty()).then(|| r.start..r.start + new.len());
}

impl EntityInputHandler for SearchField {
    fn text_for_range(&mut self, range: Range<usize>, adjusted: &mut Option<Range<usize>>, _: &mut Window, _: &mut Context<Self>) -> Option<String> {
        let r = utf8_range(&self.text, &range);
        adjusted.replace(utf16_range(&self.text, &r));
        Some(self.text[r].to_string())
    }

    // the caret always sits at the end (no caret movement), so that is where the IME edits
    fn selected_text_range(&mut self, _: bool, _: &mut Window, _: &mut Context<Self>) -> Option<UTF16Selection> {
        let end = utf16_at(&self.text, self.text.len());
        Some(UTF16Selection { range: end..end, reversed: false })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked.as_ref().map(|m| utf16_range(&self.text, m))
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
        cx.notify();
    }

    fn replace_text_in_range(&mut self, range: Option<Range<usize>>, new: &str, _: &mut Window, cx: &mut Context<Self>) {
        splice(&mut self.text, &mut self.marked, range, new, false);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(&mut self, range: Option<Range<usize>>, new: &str, _: Option<Range<usize>>, _: &mut Window, cx: &mut Context<Self>) {
        splice(&mut self.text, &mut self.marked, range, new, true);
        cx.notify();
    }

    fn bounds_for_range(&mut self, range: Range<usize>, bounds: Bounds<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<Bounds<Pixels>> {
        let (line, _) = self.last.as_ref()?;
        let r = utf8_range(&self.text, &range);
        Some(Bounds::from_corners(
            point(bounds.left() + line.x_for_index(r.start), bounds.top()),
            point(bounds.left() + line.x_for_index(r.end), bounds.bottom()),
        ))
    }

    fn character_index_for_point(&mut self, p: gpui::Point<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        let (line, bounds) = self.last.as_ref()?;
        let local = bounds.localize(&p)?;
        let i = line.index_for_x(local.x)?.min(self.text.len());
        Some(utf16_at(&self.text, i))
    }
}

/// The field's one line: the text plus the "▏" caret, or the placeholder; preedit underlined.
/// Sized to its text like the plain text div it replaces, so the panels look the same.
pub(crate) struct SearchLine(pub(crate) Entity<SearchField>);

impl Element for SearchLine {
    type RequestLayoutState = Option<ShapedLine>;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&gpui::InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, Self::RequestLayoutState) {
        let f = self.0.read(cx);
        let style = window.text_style();
        let (shown, color): (SharedString, gpui::Hsla) =
            if f.text.is_empty() { (f.placeholder.clone(), f.placeholder_color.into()) } else { (format!("{}\u{258f}", f.text).into(), style.color) };
        let run = TextRun { len: shown.len(), font: style.font(), color, background_color: None, underline: None, strikethrough: None };
        let runs = match f.marked.clone().filter(|_| !f.text.is_empty()) {
            Some(m) => vec![
                TextRun { len: m.start, ..run.clone() },
                TextRun { len: m.end - m.start, underline: Some(UnderlineStyle { color: Some(color), thickness: px(1.), wavy: false }), ..run.clone() },
                TextRun { len: shown.len() - m.end, ..run },
            ]
            .into_iter()
            .filter(|r| r.len > 0)
            .collect(),
            None => vec![run],
        };
        let line = window.text_system().shape_line(shown, style.font_size.to_pixels(window.rem_size()), &runs, None);
        let mut layout = Style::default();
        layout.size.width = line.width().into();
        layout.size.height = window.line_height().into();
        (window.request_layout(layout, [], cx), Some(line))
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&gpui::InspectorElementId>, _: Bounds<Pixels>, _: &mut Self::RequestLayoutState, _: &mut Window, _: &mut App) {}

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&gpui::InspectorElementId>, bounds: Bounds<Pixels>, line: &mut Self::RequestLayoutState, _: &mut (), window: &mut Window, cx: &mut App) {
        let focus = self.0.read(cx).focus.clone();
        window.handle_input(&focus, ElementInputHandler::new(bounds, self.0.clone()), cx);
        let Some(line) = line.take() else { return };
        let _ = line.paint(bounds.origin, window.line_height(), gpui::TextAlign::Left, None, window, cx);
        self.0.update(cx, |f, _| f.last = Some((line, bounds)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_offsets_follow_chars() {
        let s = "Việt😀"; // ệ: 3 bytes, 1 UTF-16 unit; 😀: 4 bytes, 2 units
        assert_eq!(utf8_at(s, 2), 2);
        assert_eq!(utf8_at(s, 3), 5);
        assert_eq!(utf8_at(s, 4), 6);
        assert_eq!(utf8_at(s, 5), s.len()); // inside the surrogate pair: rounds up to the char boundary
        assert_eq!(utf8_at(s, 99), s.len());
        assert_eq!(utf16_at(s, 5), 3);
        assert_eq!(utf16_at(s, s.len()), 6);
    }

    #[test]
    fn preedit_then_commit_vietnamese() {
        let (mut text, mut marked) = ("ab ".to_string(), None);
        splice(&mut text, &mut marked, None, "vie", true);
        assert_eq!((text.as_str(), marked.clone()), ("ab vie", Some(3..6)));
        splice(&mut text, &mut marked, None, "việ", true);
        assert_eq!((text.as_str(), marked.clone()), ("ab việ", Some(3..8)));
        splice(&mut text, &mut marked, None, "Việt", false);
        assert_eq!((text.as_str(), marked), ("ab Việt", None));
    }

    #[test]
    fn explicit_utf16_range_replaces_the_right_bytes() {
        let (mut text, mut marked) = ("Việt".to_string(), None);
        splice(&mut text, &mut marked, Some(2..3), "e", false);
        assert_eq!(text, "Viet");
        splice(&mut text, &mut marked, Some(4..4), " Nam", false);
        assert_eq!(text, "Viet Nam");
    }

    #[test]
    fn bad_ranges_from_the_ime_clamp() {
        let (mut text, mut marked) = ("Việt".to_string(), None);
        splice(&mut text, &mut marked, Some(50..90), "!", false);
        assert_eq!(text, "Việt!");
        splice(&mut text, &mut marked, Some(4..1), "", false); // reversed
        assert_eq!(text, "V!");
        splice(&mut text, &mut marked, None, "", true); // empty preedit leaves nothing marked
        assert_eq!((text.as_str(), marked), ("V!", None));
    }
}
