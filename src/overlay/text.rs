use crate::capture::Rect;
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{DEFAULT_CHARSET, LOGFONTW};

/// The few `imm32` calls needed here; the `windows` crate exposes them only behind the
/// `Win32_UI_Input_Ime` feature.
mod ime {
    use std::ffi::c_void;
    use windows::Win32::Foundation::{HWND, POINT, RECT};
    use windows::Win32::Graphics::Gdi::LOGFONTW;

    pub const CFS_POINT: u32 = 0x0002;

    #[repr(C)]
    pub struct CompositionForm {
        pub style: u32,
        pub current_pos: POINT,
        pub area: RECT,
    }

    #[link(name = "imm32")]
    unsafe extern "system" {
        pub fn ImmGetContext(hwnd: HWND) -> *mut c_void;
        pub fn ImmReleaseContext(hwnd: HWND, himc: *mut c_void) -> i32;
        pub fn ImmSetCompositionWindow(himc: *mut c_void, form: *const CompositionForm) -> i32;
        pub fn ImmSetCompositionFontW(himc: *mut c_void, font: *const LOGFONTW) -> i32;
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CharClass {
    Space,
    Word,
    Other,
}

fn char_class(ch: char) -> CharClass {
    if ch.is_whitespace() {
        CharClass::Space
    } else if ch.is_alphanumeric() || ch == '_' {
        CharClass::Word
    } else {
        CharClass::Other
    }
}

/// Start of the word before `caret`: the spaces left of it are skipped, then one run of
/// word characters or of punctuation.
fn word_start(chars: &[char], caret: usize) -> usize {
    let mut at = caret.min(chars.len());
    while at > 0 && char_class(chars[at - 1]) == CharClass::Space {
        at -= 1;
    }
    if at > 0 {
        let class = char_class(chars[at - 1]);
        while at > 0 && char_class(chars[at - 1]) == class {
            at -= 1;
        }
    }
    at
}

/// Start of the word after `caret`: the rest of the current run is skipped, then the
/// spaces that follow it.
fn next_word_start(chars: &[char], caret: usize) -> usize {
    let mut at = caret.min(chars.len());
    if at < chars.len() && char_class(chars[at]) != CharClass::Space {
        let class = char_class(chars[at]);
        while at < chars.len() && char_class(chars[at]) == class {
            at += 1;
        }
    }
    while at < chars.len() && char_class(chars[at]) == CharClass::Space {
        at += 1;
    }
    at
}

/// Character boundary nearest to `x` (distance from the text origin) among `0..=count`.
/// `prefix_width(n)` is the width of the first `n` characters and never decreases with `n`.
fn nearest_boundary(count: usize, x: i32, prefix_width: impl Fn(usize) -> i32) -> usize {
    if x <= 0 {
        return 0;
    }
    // Smallest boundary whose prefix reaches `x`; `count` when the click is past the end.
    let (mut low, mut high) = (0, count);
    while low < high {
        let middle = (low + high) / 2;
        if prefix_width(middle) >= x {
            high = middle;
        } else {
            low = middle + 1;
        }
    }
    if low > 0 && x - prefix_width(low - 1) < prefix_width(low) - x {
        low - 1
    } else {
        low
    }
}

/// Air between the text and the frame drawn around the box being edited.
pub(super) fn text_box_margin(dpi: u32) -> i32 {
    (4 * dpi as i32 / 96).max(1)
}

#[derive(Debug, Clone)]
pub struct TextEditState {
    pub pos: (i32, i32),
    pub text: String,
    pub(super) original_text: String,
    pub caret: usize,
    pub color: [u8; 4],
    pub font_size: i32,
    pub editing_id: Option<usize>,
    pub caret_visible: bool,
    pub anchor: Option<usize>,
    pending_surrogate: Option<u16>,
    undo: Vec<(String, usize, Option<usize>)>,
    redo: Vec<(String, usize, Option<usize>)>,
}

impl TextEditState {
    pub fn new(
        pos: (i32, i32),
        text: String,
        color: [u8; 4],
        font_size: i32,
        editing_id: Option<usize>,
    ) -> Self {
        let caret = text.chars().count();
        let original_text = text.clone();
        Self {
            pos,
            text,
            original_text,
            caret,
            color,
            font_size,
            editing_id,
            caret_visible: true,
            anchor: None,
            pending_surrogate: None,
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    fn remember(&mut self) {
        if self.undo.len() == 32 {
            self.undo.remove(0);
        }
        self.undo.push((self.text.clone(), self.caret, self.anchor));
        self.redo.clear();
    }

    pub fn undo(&mut self) {
        if let Some(previous) = self.undo.pop() {
            self.redo.push((self.text.clone(), self.caret, self.anchor));
            (self.text, self.caret, self.anchor) = previous;
            self.caret_visible = true;
        }
    }

    pub fn redo(&mut self) {
        if let Some(next) = self.redo.pop() {
            self.undo.push((self.text.clone(), self.caret, self.anchor));
            (self.text, self.caret, self.anchor) = next;
            self.caret_visible = true;
        }
    }

    pub fn insert_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let selected_bytes = self.selection().map_or(0, |range| {
            self.byte_index(range.end) - self.byte_index(range.start)
        });
        let available = 16_384usize.saturating_sub(self.text.len() - selected_bytes);
        let mut end = text.len().min(available);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        if end == 0 {
            return;
        }
        self.remember();
        self.delete_selection();
        let at = self.byte_index(self.caret);
        self.text.insert_str(at, &text[..end]);
        self.caret += text[..end].chars().count();
        self.caret_visible = true;
    }

    pub fn insert_char(&mut self, ch: char) {
        if ch == '\t' {
            self.insert_text("    ");
        } else {
            self.insert_text(ch.encode_utf8(&mut [0; 4]));
        }
    }

    pub fn backspace(&mut self) -> bool {
        if self.selection().is_some() {
            self.remember();
            self.delete_selection();
            return true;
        }
        let caret = self.caret.min(self.text.chars().count());
        if caret > 0 {
            self.remember();
            self.text.remove(self.byte_index(caret - 1));
            self.caret = caret - 1;
            self.caret_visible = true;
            true
        } else {
            false
        }
    }

    pub fn delete(&mut self) -> bool {
        if self.selection().is_some() {
            self.remember();
            self.delete_selection();
            return true;
        }
        let caret = self.caret.min(self.text.chars().count());
        if caret < self.text.chars().count() {
            self.remember();
            self.text.remove(self.byte_index(caret));
            self.caret_visible = true;
            true
        } else {
            false
        }
    }

    pub fn move_left(&mut self) -> bool {
        if self.caret > 0 {
            self.caret -= 1;
            self.caret_visible = true;
            true
        } else {
            false
        }
    }

    pub fn move_right(&mut self) -> bool {
        if self.caret < self.text.chars().count() {
            self.caret += 1;
            self.caret_visible = true;
            true
        } else {
            false
        }
    }

    /// One character left or right. Without `extend`, a selection collapses to the edge
    /// in that direction instead of moving from the caret.
    pub fn step(&mut self, forward: bool, extend: bool) {
        if !extend && let Some(range) = self.selection() {
            self.move_to(if forward { range.end } else { range.start }, false);
        } else {
            let target = if forward {
                self.caret + 1
            } else {
                self.caret.saturating_sub(1)
            };
            self.move_to(target, extend);
        }
    }

    /// Jumps to the start of the previous or next word.
    pub fn move_word(&mut self, forward: bool, extend: bool) {
        let chars: Vec<char> = self.text.chars().collect();
        let caret = self.caret.min(chars.len());
        let target = if forward {
            next_word_start(&chars, caret)
        } else {
            word_start(&chars, caret)
        };
        self.move_to(target, extend);
    }

    /// Deletes the selection, else the word before or after the caret.
    pub fn delete_word(&mut self, forward: bool) -> bool {
        if self.delete_selected() {
            return true;
        }
        let chars: Vec<char> = self.text.chars().collect();
        let caret = self.caret.min(chars.len());
        let (start, end) = if forward {
            (caret, next_word_start(&chars, caret))
        } else {
            (word_start(&chars, caret), caret)
        };
        if start == end {
            return false;
        }
        self.remember();
        let (from, to) = (self.byte_index(start), self.byte_index(end));
        self.text.replace_range(from..to, "");
        self.caret = start;
        self.anchor = None;
        self.caret_visible = true;
        true
    }

    pub fn selected_text(&self) -> Option<String> {
        self.selection().map(|range| {
            self.text
                .chars()
                .skip(range.start)
                .take(range.end - range.start)
                .collect()
        })
    }

    /// Removes the selected characters as one undoable edit.
    pub fn delete_selected(&mut self) -> bool {
        if self.selection().is_none() {
            return false;
        }
        self.remember();
        self.delete_selection();
        self.caret_visible = true;
        true
    }

    fn byte_index(&self, character: usize) -> usize {
        self.text
            .char_indices()
            .nth(character)
            .map_or(self.text.len(), |(index, _)| index)
    }

    pub fn selection(&self) -> Option<std::ops::Range<usize>> {
        self.anchor
            .filter(|anchor| *anchor != self.caret)
            .map(|anchor| anchor.min(self.caret)..anchor.max(self.caret))
    }

    fn delete_selection(&mut self) -> bool {
        let Some(range) = self.selection() else {
            self.anchor = None;
            return false;
        };
        let start = self.byte_index(range.start);
        let end = self.byte_index(range.end);
        self.text.replace_range(start..end, "");
        self.caret = range.start;
        self.anchor = None;
        true
    }

    pub fn move_to(&mut self, position: usize, extend: bool) {
        if extend {
            self.anchor.get_or_insert(self.caret);
        } else {
            self.anchor = None;
        }
        self.caret = position.min(self.text.chars().count());
        self.caret_visible = true;
    }

    pub fn select_all(&mut self) {
        self.anchor = Some(0);
        self.caret = self.text.chars().count();
        self.caret_visible = true;
    }

    /// Width of the first `count` characters.
    fn prefix_width(&self, count: usize) -> i32 {
        if count == 0 {
            0
        } else {
            let end = self.byte_index(count);
            crate::drawing::measure_text(&self.text[..end], self.font_size).0
        }
    }

    /// Horizontal distance from the box origin to the caret.
    pub(super) fn caret_offset(&self) -> i32 {
        self.prefix_width(self.caret)
    }

    /// The text's extent, at least one em wide so an empty box is still a target.
    pub(super) fn bounds(&self) -> Rect {
        let (width, height) = crate::drawing::measure_text(&self.text, self.font_size);
        Rect::new(
            self.pos.0,
            self.pos.1,
            self.pos.0 + width.max(self.font_size),
            self.pos.1 + height,
        )
    }

    /// Whether `pt` is inside the box, including the margin its frame is drawn in.
    pub(super) fn contains(&self, pt: (i32, i32), dpi: u32) -> bool {
        let margin = text_box_margin(dpi);
        self.bounds().inflate(margin, margin).contains(pt.0, pt.1)
    }

    /// Character boundary nearest to the horizontal position `x`.
    pub(super) fn caret_at(&self, x: i32) -> usize {
        nearest_boundary(self.text.chars().count(), x - self.pos.0, |count| {
            self.prefix_width(count)
        })
    }

    /// Moves the IME composition window to the caret; otherwise Windows opens it, and
    /// the candidate list, at the origin of the virtual screen.
    pub(super) fn place_ime_window(&self, hwnd: HWND) {
        let form = ime::CompositionForm {
            style: ime::CFS_POINT,
            current_pos: POINT {
                x: self.pos.0 + self.caret_offset(),
                y: self.pos.1,
            },
            area: RECT::default(),
        };
        let mut font = LOGFONTW {
            lfHeight: self.font_size,
            lfWeight: 700,
            lfCharSet: DEFAULT_CHARSET,
            ..Default::default()
        };
        for (slot, unit) in font
            .lfFaceName
            .iter_mut()
            .zip(crate::theme::ui_face().encode_utf16().take(31))
        {
            *slot = unit;
        }
        unsafe {
            let context = ime::ImmGetContext(hwnd);
            if context.is_null() {
                return;
            }
            let _ = ime::ImmSetCompositionWindow(context, &form);
            let _ = ime::ImmSetCompositionFontW(context, &font);
            let _ = ime::ImmReleaseContext(hwnd, context);
        }
    }

    pub fn insert_utf16(&mut self, unit: u32) {
        if (0xd800..=0xdbff).contains(&unit) {
            self.pending_surrogate = Some(unit as u16);
            return;
        }
        let code = if (0xdc00..=0xdfff).contains(&unit) {
            let Some(high) = self.pending_surrogate.take() else {
                return;
            };
            0x10000 + (((high as u32 - 0xd800) << 10) | (unit - 0xdc00))
        } else {
            self.pending_surrogate = None;
            unit
        };
        if let Some(ch) = char::from_u32(code).filter(|ch| !ch.is_control() || *ch == '\t') {
            self.insert_char(ch);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(text: &str) -> TextEditState {
        TextEditState::new((0, 0), text.to_string(), [0, 0, 0, 255], 22, None)
    }

    fn chars(text: &str) -> Vec<char> {
        text.chars().collect()
    }

    #[test]
    fn word_start_skips_spaces_then_one_run() {
        let text = chars("foo  bar.baz");
        assert_eq!(word_start(&text, 12), 9);
        assert_eq!(word_start(&text, 9), 8);
        assert_eq!(word_start(&text, 8), 5);
        assert_eq!(word_start(&text, 5), 0);
        assert_eq!(word_start(&text, 0), 0);
    }

    #[test]
    fn next_word_start_finishes_the_run_and_its_spaces() {
        let text = chars("foo  bar.baz");
        assert_eq!(next_word_start(&text, 0), 5);
        assert_eq!(next_word_start(&text, 3), 5);
        assert_eq!(next_word_start(&text, 5), 8);
        assert_eq!(next_word_start(&text, 8), 9);
        assert_eq!(next_word_start(&text, 9), 12);
        assert_eq!(next_word_start(&text, 12), 12);
    }

    #[test]
    fn click_position_snaps_to_the_nearest_boundary() {
        // Ten pixels per character, five characters.
        let width = |count: usize| count as i32 * 10;
        assert_eq!(nearest_boundary(5, -3, width), 0);
        assert_eq!(nearest_boundary(5, 0, width), 0);
        assert_eq!(nearest_boundary(5, 4, width), 0);
        assert_eq!(nearest_boundary(5, 6, width), 1);
        assert_eq!(nearest_boundary(5, 14, width), 1);
        assert_eq!(nearest_boundary(5, 16, width), 2);
        assert_eq!(nearest_boundary(5, 50, width), 5);
        assert_eq!(nearest_boundary(5, 400, width), 5);
        assert_eq!(nearest_boundary(0, 25, width), 0);
    }

    #[test]
    fn click_position_follows_proportional_widths() {
        // "i" is narrow, "W" is wide.
        let advances = [4, 20, 4, 20];
        let width = |count: usize| advances[..count].iter().sum::<i32>();
        assert_eq!(nearest_boundary(4, 5, width), 1);
        assert_eq!(nearest_boundary(4, 13, width), 1);
        assert_eq!(nearest_boundary(4, 15, width), 2);
        assert_eq!(nearest_boundary(4, 27, width), 3);
    }

    #[test]
    fn arrows_collapse_a_selection_to_its_edge() {
        let mut state = edit("hello");
        state.anchor = Some(1);
        state.caret = 3;
        state.step(false, false);
        assert_eq!((state.caret, state.anchor), (1, None));

        state.anchor = Some(1);
        state.caret = 3;
        state.step(true, false);
        assert_eq!((state.caret, state.anchor), (3, None));
    }

    #[test]
    fn shift_arrows_extend_from_the_anchor() {
        let mut state = edit("hello");
        state.anchor = Some(1);
        state.caret = 3;
        state.step(true, true);
        assert_eq!((state.caret, state.anchor), (4, Some(1)));
        state.step(false, true);
        state.step(false, true);
        state.step(false, true);
        assert_eq!((state.caret, state.anchor), (1, Some(1)));
        assert_eq!(state.selection(), None);
    }

    #[test]
    fn plain_arrows_stop_at_the_ends() {
        let mut state = edit("ab");
        state.step(true, false);
        assert_eq!(state.caret, 2);
        state.move_to(0, false);
        state.step(false, false);
        assert_eq!(state.caret, 0);
    }

    #[test]
    fn word_jumps_move_and_extend() {
        let mut state = edit("one two three");
        state.move_word(false, false);
        assert_eq!(state.caret, 8);
        state.move_word(false, true);
        assert_eq!((state.caret, state.anchor), (4, Some(8)));
        state.move_word(true, false);
        assert_eq!((state.caret, state.anchor), (8, None));
        state.move_word(true, false);
        assert_eq!(state.caret, 13);
    }

    #[test]
    fn ctrl_backspace_deletes_the_previous_word_as_one_undo() {
        let mut state = edit("one two three");
        assert!(state.delete_word(false));
        assert_eq!((state.text.as_str(), state.caret), ("one two ", 8));
        assert!(state.delete_word(false));
        assert_eq!((state.text.as_str(), state.caret), ("one ", 4));
        state.undo();
        assert_eq!(state.text, "one two ");
        state.move_to(0, false);
        assert!(!state.delete_word(false));
    }

    #[test]
    fn ctrl_delete_removes_the_word_and_the_spaces_after_it() {
        let mut state = edit("one two three");
        state.move_to(4, false);
        assert!(state.delete_word(true));
        assert_eq!((state.text.as_str(), state.caret), ("one three", 4));
        state.move_to(9, false);
        assert!(!state.delete_word(true));
    }

    #[test]
    fn word_delete_removes_a_selection_first() {
        let mut state = edit("one two");
        state.anchor = Some(0);
        state.caret = 3;
        assert!(state.delete_word(false));
        assert_eq!(
            (state.text.as_str(), state.caret, state.anchor),
            (" two", 0, None)
        );
    }

    #[test]
    fn word_delete_handles_multibyte_characters() {
        let mut state = edit("héllo wörld");
        assert!(state.delete_word(false));
        assert_eq!(state.text, "héllo ");
        state.move_to(0, false);
        assert!(state.delete_word(true));
        assert_eq!(state.text, "");
    }

    #[test]
    fn cut_returns_the_selected_characters_and_removes_them() {
        let mut state = edit("a🙂bc");
        state.anchor = Some(1);
        state.caret = 3;
        assert_eq!(state.selected_text().as_deref(), Some("🙂b"));
        assert!(state.delete_selected());
        assert_eq!((state.text.as_str(), state.caret), ("ac", 1));
        assert_eq!(state.selected_text(), None);
        assert!(!state.delete_selected());
    }
}
