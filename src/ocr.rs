//! Text recognition of the editor selection. The Windows OCR engine sits behind [`Engine`]
//! (`ocr/engine.rs`); everything around it lives here: reading the pixels, shrinking images
//! the engine cannot take, mapping its word boxes back, finding e-mail addresses and phone
//! numbers (`ocr/matching.rs`) and deciding what a finished recognition asks of the editor.
use crate::capture::Rect;

mod engine;
mod matching;
#[cfg(test)]
mod tests;

pub use engine::SystemEngine;

/// Margin, in pixels, a redaction keeps around the words it covers so no glyph edge peeks out.
const REDACTION_PADDING: i32 = 2;

/// A word's box in image pixels. The engine reports fractions of a pixel, and a shrunk image
/// maps back to fractions as well.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WordBox {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl WordBox {
    pub const fn new(left: f32, top: f32, right: f32, bottom: f32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    fn height(self) -> f32 {
        self.bottom - self.top
    }

    fn union(self, other: Self) -> Self {
        Self::new(
            self.left.min(other.left),
            self.top.min(other.top),
            self.right.max(other.right),
            self.bottom.max(other.bottom),
        )
    }

    fn scaled(self, x: f32, y: f32) -> Self {
        Self::new(self.left * x, self.top * y, self.right * x, self.bottom * y)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct OcrWord {
    pub text: String,
    pub bounds: WordBox,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OcrLine {
    pub text: String,
    pub words: Vec<OcrWord>,
}

impl OcrLine {
    fn scaled(self, x: f32, y: f32) -> Self {
        Self {
            text: self.text,
            words: self
                .words
                .into_iter()
                .map(|word| OcrWord {
                    text: word.text,
                    bounds: word.bounds.scaled(x, y),
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OcrFailure {
    /// Windows has no text recognition language for the user's profile.
    NoLanguage,
    Failed(String),
}

/// The lines an engine found, or why it found none.
pub type Recognized = Result<Vec<OcrLine>, OcrFailure>;

/// The pixels of a selection: top-down BGRA with every pixel opaque.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OcrImage {
    /// Top-left corner of the pixels in the capture.
    pub origin: (i32, i32),
    pub width: i32,
    pub height: i32,
    pub bgra: Vec<u8>,
}

impl OcrImage {
    /// Copies `selection` out of a top-down BGRA frame of `full_width` x `full_height`
    /// pixels. `None` when no part of the selection lies inside the frame or the frame is
    /// smaller than it claims.
    pub fn from_selection(
        frame: &[u8],
        full_width: i32,
        full_height: i32,
        selection: Rect,
    ) -> Option<Self> {
        let area = selection.clamp(full_width, full_height);
        if area.is_empty() {
            return None;
        }
        let stride = full_width as usize * 4;
        let row = area.width() as usize * 4;
        let mut bgra = Vec::with_capacity(row * area.height() as usize);
        for y in area.top..area.bottom {
            let start = y as usize * stride + area.left as usize * 4;
            bgra.extend_from_slice(frame.get(start..start + row)?);
        }
        for pixel in bgra.as_chunks_mut::<4>().0 {
            pixel[3] = 255;
        }
        Some(Self {
            origin: (area.left, area.top),
            width: area.width(),
            height: area.height(),
            bgra,
        })
    }

    /// The pixels' place in the capture.
    pub fn area(&self) -> Rect {
        Rect::new(
            self.origin.0,
            self.origin.1,
            self.origin.0 + self.width,
            self.origin.1 + self.height,
        )
    }

    /// The image averaged down to `width` x `height`: each pixel is the mean of the source
    /// pixels it covers.
    fn shrunk(&self, width: i32, height: i32) -> Self {
        let (source_width, source_height) = (self.width as usize, self.height as usize);
        let (width, height) = (width as usize, height as usize);
        let mut bgra = Vec::with_capacity(width * height * 4);
        for y in 0..height {
            let rows = covered(y, source_height, height);
            for x in 0..width {
                let columns = covered(x, source_width, width);
                let count = (rows.len() * columns.len()) as u32;
                let mut sum = [0u32; 4];
                for row in rows.clone() {
                    for column in columns.clone() {
                        let at = (row * source_width + column) * 4;
                        for (total, value) in sum.iter_mut().zip(&self.bgra[at..at + 4]) {
                            *total += u32::from(*value);
                        }
                    }
                }
                bgra.extend(sum.map(|total| ((total + count / 2) / count) as u8));
            }
        }
        Self {
            origin: self.origin,
            width: width as i32,
            height: height as i32,
            bgra,
        }
    }
}

/// The source positions, out of `source`, that destination position `index` of `target`
/// covers when an axis is shrunk; always at least one.
fn covered(index: usize, source: usize, target: usize) -> std::ops::Range<usize> {
    let start = index * source / target;
    start..((index + 1) * source / target).max(start + 1)
}

/// Size of a `width` x `height` image shrunk, keeping its proportions, until its longest
/// side is no more than `limit`.
fn fit(width: i32, height: i32, limit: u32) -> (i32, i32) {
    let limit = i64::from(limit.max(1));
    let longest = i64::from(width.max(height));
    if longest <= limit {
        return (width, height);
    }
    let shrink = |side: i32| (i64::from(side) * limit / longest).max(1) as i32;
    (shrink(width), shrink(height))
}

/// A text recognition engine.
pub trait Engine: Send + Sync {
    /// The longest side, in pixels, of an image the engine takes.
    fn max_dimension(&self) -> u32;

    /// The lines found in `image`, with word boxes in the pixels of that image.
    fn recognize(&self, image: &OcrImage) -> Recognized;
}

/// Recognizes the text in `image`. An image with a side beyond the engine's limit is shrunk
/// first; the word boxes always come back in the pixels of `image`.
pub fn recognize(engine: &dyn Engine, image: &OcrImage) -> Recognized {
    let (width, height) = fit(image.width, image.height, engine.max_dimension());
    if (width, height) == (image.width, image.height) {
        return engine.recognize(image);
    }
    let lines = engine.recognize(&image.shrunk(width, height))?;
    let x = image.width as f32 / width as f32;
    let y = image.height as f32 / height as f32;
    Ok(lines.into_iter().map(|line| line.scaled(x, y)).collect())
}

/// What the user asked the recognized text to be used for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    CopyText,
    RedactSensitive,
}

impl Intent {
    /// The command behind the key pressed together with Ctrl+Shift.
    pub fn for_key(vk: usize) -> Option<Self> {
        match vk {
            0x43 => Some(Self::CopyText),
            0x52 => Some(Self::RedactSensitive),
            _ => None,
        }
    }

    /// The command's name; it also titles the messages about its outcome.
    pub fn title(self) -> &'static str {
        match self {
            Self::CopyText => crate::i18n::t("Copy text"),
            Self::RedactSensitive => crate::i18n::t("Redact emails and phone numbers"),
        }
    }
}

/// What a finished recognition asks the editor to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Put `text` on the clipboard, then say `message`.
    Copy { text: String, message: &'static str },
    /// Cover each rectangle (capture pixels) with an opaque redaction, then say `message`.
    Redact { rects: Vec<Rect>, message: String },
    /// A brief word to the user, nothing else changes.
    Notice(String),
    /// Something the user has to act on or know about: an error dialog titled `title`.
    Error {
        title: &'static str,
        message: String,
    },
}

/// Decides what the outcome of recognizing the pixels of `area` (capture pixels) means for
/// the command `intent`.
pub fn resolve(intent: Intent, recognized: Recognized, area: Rect) -> Resolution {
    let lines = match recognized {
        Ok(lines) => lines,
        Err(OcrFailure::NoLanguage) => {
            // A notice vanishes after a moment, and without a language nothing works at all.
            return Resolution::Error {
                title: intent.title(),
                message: crate::i18n::t(
                    "Windows has no text recognition language installed. Add a language under Settings › Time & language › Language & region.",
                )
                .to_owned(),
            };
        }
        Err(OcrFailure::Failed(message)) => {
            return Resolution::Error {
                title: crate::i18n::t("Text recognition failed"),
                message,
            };
        }
    };
    match intent {
        Intent::CopyText => {
            let text = clipboard_text(&lines);
            if text.trim().is_empty() {
                Resolution::Notice(crate::i18n::t("No text found in the selection").to_owned())
            } else {
                Resolution::Copy {
                    text,
                    message: crate::i18n::t("Text copied"),
                }
            }
        }
        Intent::RedactSensitive => {
            let rects = redaction_rects(&lines, area);
            let message = match rects.len() {
                0 => {
                    return Resolution::Notice(
                        crate::i18n::t("No email addresses or phone numbers found").to_owned(),
                    );
                }
                1 => crate::i18n::t("Redacted 1 item").to_owned(),
                count => crate::i18n::tf("Redacted {} items", &[&count]),
            };
            Resolution::Redact { rects, message }
        }
    }
}

/// The recognized lines as clipboard text: one per line, CRLF between them.
fn clipboard_text(lines: &[OcrLine]) -> String {
    lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\r\n")
}

/// One rectangle in capture pixels for each e-mail address and phone number in `lines`,
/// which were recognized in the pixels of `area`: the match's word boxes, rounded outwards,
/// padded, and kept inside `area`.
fn redaction_rects(lines: &[OcrLine], area: Rect) -> Vec<Rect> {
    matching::sensitive_boxes(lines)
        .into_iter()
        .filter_map(|bounds| {
            let padded = Rect::new(
                area.left + bounds.left.floor() as i32 - REDACTION_PADDING,
                area.top + bounds.top.floor() as i32 - REDACTION_PADDING,
                area.left + bounds.right.ceil() as i32 + REDACTION_PADDING,
                area.top + bounds.bottom.ceil() as i32 + REDACTION_PADDING,
            );
            let inside = Rect::new(
                padded.left.max(area.left),
                padded.top.max(area.top),
                padded.right.min(area.right),
                padded.bottom.min(area.bottom),
            );
            (!inside.is_empty()).then_some(inside)
        })
        .collect()
}
