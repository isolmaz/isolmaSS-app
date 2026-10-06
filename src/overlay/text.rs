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
        }
    }

    pub fn redo(&mut self) {
        if let Some(next) = self.redo.pop() {
            self.undo.push((self.text.clone(), self.caret, self.anchor));
            (self.text, self.caret, self.anchor) = next;
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
