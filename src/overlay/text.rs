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
        #[cfg(test)]
        pub fn ImmGetCompositionWindow(himc: *mut c_void, form: *mut CompositionForm) -> i32;
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

    // ---- editing --------------------------------------------------------------------

    #[test]
    fn the_frame_margin_scales_with_the_dpi_and_never_vanishes() {
        assert_eq!(text_box_margin(96), 4);
        assert_eq!(text_box_margin(192), 8);
        assert_eq!(text_box_margin(10), 1);
    }

    #[test]
    fn typing_inserts_at_the_caret_and_undo_redo_walk_the_edits() {
        let mut state = edit("ac");
        state.move_to(1, false);
        state.insert_text("b");
        assert_eq!((state.text.as_str(), state.caret), ("abc", 2));
        state.undo();
        assert_eq!((state.text.as_str(), state.caret), ("ac", 1));
        state.redo();
        assert_eq!((state.text.as_str(), state.caret), ("abc", 2));
        // Nothing left to redo or undo past the ends.
        state.redo();
        assert_eq!(state.text, "abc");
        state.undo();
        state.undo();
        assert_eq!(state.text, "ac");
        // A new edit forgets the undone ones.
        state.insert_text("x");
        state.redo();
        assert_eq!(state.text, "axc");
    }

    #[test]
    fn only_the_last_thirty_two_edits_can_be_undone() {
        let mut state = edit("");
        for _ in 0..40 {
            state.insert_text("x");
        }
        for _ in 0..50 {
            state.undo();
        }
        assert_eq!(state.text.len(), 8);
    }

    #[test]
    fn inserting_replaces_the_selection_and_ignores_empty_text() {
        let mut state = edit("hello");
        state.insert_text("");
        assert_eq!(state.text, "hello");
        assert!(state.undo.is_empty());
        state.anchor = Some(1);
        state.caret = 4;
        state.insert_text("ipp");
        assert_eq!(
            (state.text.as_str(), state.caret, state.anchor),
            ("hippo", 4, None)
        );
    }

    #[test]
    fn text_longer_than_the_limit_is_cut_at_a_character_boundary() {
        let mut state = edit(&"a".repeat(16_383));
        state.insert_text("é");
        assert_eq!(
            state.text.len(),
            16_383,
            "a two-byte character does not fit in one byte"
        );
        state.insert_text("bcd");
        assert_eq!(state.text.len(), 16_384);
        assert!(state.text.ends_with('b'));
        state.insert_text("more");
        assert_eq!(state.text.len(), 16_384);
    }

    #[test]
    fn a_tab_becomes_four_spaces_and_other_characters_are_inserted_as_they_are() {
        let mut state = edit("");
        state.insert_char('\t');
        state.insert_char('é');
        assert_eq!(state.text, "    é");
        assert_eq!(state.caret, 5);
    }

    #[test]
    fn backspace_removes_the_selection_or_the_character_before_the_caret() {
        let mut state = edit("héllo");
        assert!(state.backspace());
        assert_eq!((state.text.as_str(), state.caret), ("héll", 4));
        state.anchor = Some(1);
        state.caret = 3;
        assert!(state.backspace());
        assert_eq!(
            (state.text.as_str(), state.caret, state.anchor),
            ("hl", 1, None)
        );
        state.move_to(0, false);
        assert!(!state.backspace());
        assert_eq!(state.text, "hl");
    }

    #[test]
    fn delete_removes_the_selection_or_the_character_after_the_caret() {
        let mut state = edit("héllo");
        assert!(!state.delete());
        state.move_to(1, false);
        assert!(state.delete());
        assert_eq!((state.text.as_str(), state.caret), ("hllo", 1));
        state.anchor = Some(0);
        assert!(state.delete());
        assert_eq!(
            (state.text.as_str(), state.caret, state.anchor),
            ("llo", 0, None)
        );
    }

    #[test]
    fn the_caret_moves_one_character_and_stops_at_the_ends() {
        let mut state = edit("ab");
        assert!(!state.move_right());
        assert!(state.move_left());
        assert_eq!(state.caret, 1);
        assert!(state.move_left());
        assert!(!state.move_left());
        assert!(state.move_right());
        assert_eq!(state.caret, 1);
    }

    #[test]
    fn select_all_covers_the_whole_text() {
        let mut state = edit("héllo");
        state.select_all();
        assert_eq!(state.selection(), Some(0..5));
        assert_eq!(state.selected_text().as_deref(), Some("héllo"));
    }

    // ---- geometry -------------------------------------------------------------------

    #[test]
    fn the_caret_offset_is_the_width_of_the_text_before_it() {
        let mut state = edit("héllo");
        state.move_to(2, false);
        assert_eq!(
            state.caret_offset(),
            crate::drawing::measure_text("hé", 22).0
        );
        state.move_to(0, false);
        assert_eq!(state.caret_offset(), 0);
    }

    #[test]
    fn the_box_is_the_text_extent_and_at_least_one_em_wide() {
        let mut state = TextEditState::new((30, 40), "hello".to_string(), [0, 0, 0, 255], 22, None);
        let (width, height) = crate::drawing::measure_text("hello", 22);
        assert_eq!(
            state.bounds(),
            Rect::new(30, 40, 30 + width.max(22), 40 + height)
        );
        state.text.clear();
        let (_, empty_height) = crate::drawing::measure_text("", 22);
        assert_eq!(state.bounds(), Rect::new(30, 40, 52, 40 + empty_height));
    }

    #[test]
    fn the_box_includes_the_margin_its_frame_is_drawn_in() {
        let state = TextEditState::new((30, 40), "hello".to_string(), [0, 0, 0, 255], 22, None);
        let bounds = state.bounds();
        assert!(state.contains((30, 40), 96));
        assert!(state.contains((bounds.left - 4, bounds.top - 4), 96));
        assert!(!state.contains((bounds.left - 5, bounds.top), 96));
        assert!(state.contains((bounds.left - 8, bounds.top), 192));
        assert!(!state.contains((bounds.right + 20, bounds.top), 96));
    }

    #[test]
    fn a_click_picks_the_nearest_character_boundary() {
        let state = TextEditState::new((30, 40), "héllo".to_string(), [0, 0, 0, 255], 22, None);
        let boundary = 30 + crate::drawing::measure_text("hé", 22).0;
        assert_eq!(state.caret_at(boundary), 2);
        assert_eq!(state.caret_at(0), 0);
        assert_eq!(state.caret_at(10_000), 5);
    }

    // ---- characters from the keyboard -------------------------------------------------

    #[test]
    fn utf16_units_are_assembled_into_characters() {
        let mut state = edit("");
        state.insert_utf16('a' as u32);
        // A surrogate pair is one character.
        state.insert_utf16(0xd83d);
        state.insert_utf16(0xde42);
        assert_eq!(state.text, "a🙂");
        // A low surrogate on its own is dropped, as is a high one that is not completed.
        state.insert_utf16(0xde42);
        state.insert_utf16(0xd83d);
        state.insert_utf16('b' as u32);
        assert_eq!(state.text, "a🙂b");
        // Control characters are dropped, a tab becomes spaces.
        state.insert_utf16(0x08);
        state.insert_utf16(0x0d);
        state.insert_utf16('\t' as u32);
        assert_eq!(state.text, "a🙂b    ");
    }

    // ---- IME -------------------------------------------------------------------------

    #[test]
    fn the_ime_window_opens_at_the_caret() {
        use crate::overlay::test_support::TestWindow;
        let window = TestWindow::new();
        let mut state =
            TextEditState::new((120, 70), "héllo".to_string(), [0, 0, 0, 255], 22, None);
        state.move_to(2, false);
        state.place_ime_window(window.0);
        let mut form = ime::CompositionForm {
            style: 0,
            current_pos: POINT::default(),
            area: RECT::default(),
        };
        unsafe {
            let context = ime::ImmGetContext(window.0);
            assert!(!context.is_null(), "the window has an input context");
            assert_ne!(ime::ImmGetCompositionWindow(context, &mut form), 0);
            ime::ImmReleaseContext(window.0, context);
        }
        assert_eq!(form.style, ime::CFS_POINT);
        assert_eq!(
            (form.current_pos.x, form.current_pos.y),
            (120 + state.caret_offset(), 70)
        );
    }

    #[test]
    fn a_window_without_an_input_context_is_left_alone() {
        let state = edit("abc");
        state.place_ime_window(HWND::default());
    }
}
