//! Pixel preparation, shrinking, result mapping, command outcomes and the e-mail and phone
//! matching of `ocr.rs`, all with canned engine answers: no recognition language is needed.
use super::matching::sensitive_boxes;
use super::*;
use crate::i18n::{t, tf};
use std::sync::Mutex;

/// Characters are 6 px wide, lines 10 px tall, words 3 px apart.
fn line(texts: &[&str]) -> OcrLine {
    let mut left = 0.0;
    let mut words = Vec::new();
    for text in texts {
        let right = left + 6.0 * text.chars().count() as f32;
        words.push(OcrWord {
            text: (*text).to_string(),
            bounds: WordBox::new(left, 0.0, right, 10.0),
        });
        left = right + 3.0;
    }
    OcrLine {
        text: texts.join(" "),
        words,
    }
}

/// The boxes found in one line of `texts`.
fn found(texts: &[&str]) -> Vec<WordBox> {
    sensitive_boxes(&[line(texts)])
}

/// The box around words `first..=last` of the line `texts`.
fn around(texts: &[&str], first: usize, last: usize) -> WordBox {
    let words = line(texts).words;
    words[first].bounds.union(words[last].bounds)
}

struct Canned {
    limit: u32,
    result: Recognized,
    seen: Mutex<Vec<OcrImage>>,
}

impl Canned {
    fn new(limit: u32, result: Recognized) -> Self {
        Self {
            limit,
            result,
            seen: Mutex::new(Vec::new()),
        }
    }
}

impl Engine for Canned {
    fn max_dimension(&self) -> u32 {
        self.limit
    }

    fn recognize(&self, image: &OcrImage) -> Recognized {
        self.seen.lock().unwrap().push(image.clone());
        self.result.clone()
    }
}

// ---- e-mail addresses ---------------------------------------------------------------

#[test]
fn e_mail_addresses_are_found_whatever_surrounds_them() {
    for text in [
        "john@example.com",
        "john.doe+tag@mail.example.co.uk",
        "o_brien-x@ex-ample.org",
        "öğrenci@üniversite.edu.tr",
        "<john@example.com>,",
        "E-mail:john@example.com.",
        "john@example.com-",
        "-john@example.com",
    ] {
        assert_eq!(found(&[text]), vec![around(&[text], 0, 0)], "{text}");
    }
}

#[test]
fn text_that_only_looks_like_an_address_is_left_alone() {
    for text in [
        "john@example",
        "@handle",
        "john@.com",
        "a@b.c",
        "john@-x.com",
        "john@example..com",
        "john@example.c0m",
        "-@example.com",
    ] {
        assert!(found(&[text]).is_empty(), "{text}");
    }
}

#[test]
fn an_address_the_engine_split_at_the_at_sign_is_joined_again() {
    let words = ["write", "john", "@", "example.com"];
    assert_eq!(found(&words), vec![around(&words, 1, 3)]);
    assert!(found(&["Reach", "me", "@", "home"]).is_empty());
}

#[test]
fn digits_inside_an_address_are_not_read_as_a_phone_number() {
    assert_eq!(found(&["5551234567@example.com"]).len(), 1);
}

// ---- phone numbers ------------------------------------------------------------------

#[test]
fn phone_numbers_are_found_in_the_usual_notations() {
    // Each case: the words of a line and the index of the last word of the number.
    for (words, last) in [
        (vec!["0532-123-4567"], 0),
        (vec!["05321234567"], 0),
        (vec!["555-1234"], 0),
        (vec!["555.123.4567"], 0),
        (vec!["06.12.34.56.78"], 0),
        (vec!["Tel:05321234567"], 0),
        (vec!["+90", "532", "123", "45", "67"], 4),
        (vec!["0090", "532", "123", "45", "67"], 4),
        (vec!["(212)", "555-0199"], 1),
        (vec!["(212)555-0199"], 0),
        (vec!["+1", "(212)", "555-0199"], 2),
        (vec!["+49", "(0)", "30", "1234567"], 3),
        (vec!["1", "800", "555", "0199"], 3),
        (vec!["0532", "123", "45", "67", "(call", "me)"], 3),
    ] {
        assert_eq!(found(&words), vec![around(&words, 0, last)], "{words:?}");
    }
}

#[test]
fn two_numbers_in_a_line_are_two_matches() {
    let words = [
        "Tel", "0532", "123", "45", "67", "Fax", "0212", "555", "01", "99",
    ];
    assert_eq!(
        found(&words),
        vec![around(&words, 1, 4), around(&words, 6, 9)]
    );
    let mixed = ["mail", "john@x.com", "tel", "5551234567"];
    assert_eq!(
        found(&mixed),
        vec![around(&mixed, 1, 1), around(&mixed, 3, 3)]
    );
}

#[test]
fn dates_times_ranges_amounts_versions_and_other_figures_are_not_phone_numbers() {
    for words in [
        vec!["2024"],
        vec!["12:30"],
        vec!["12:30:45"],
        vec!["2024-01-15"],
        vec!["15.01.2024"],
        vec!["15", "01", "2024"],
        vec!["2024-01-15", "12:30"],
        vec!["12:30", "2024-01-15"],
        vec!["$1,299.00"],
        vec!["1.299.000"],
        vec!["12.345.678"],
        vec!["1", "234", "567"],
        vec!["$1234567"],
        vec!["1234567,89"],
        vec!["1234567%"],
        vec!["1234567.89"],
        vec!["10.0.19045.3803"],
        vec!["10.15.19045.1234"],
        vec!["1.2.3.4567"],
        vec!["192.168.100.200"],
        vec!["10.20.30.40"],
        vec!["3.14159265"],
        vec!["ABC1234567"],
        vec!["1", "2", "3", "4", "5", "6", "7", "8"],
        vec!["(12.04.2024)"],
        vec!["(212)"],
        vec!["123+4567890"],
        vec!["09.00-18.00"],
        vec!["9.00-18.00"],
        vec!["Open", "09:00-18:00"],
        vec!["Open", "09:00", "-", "18:00"],
        vec!["2020-2024"],
        vec!["\u{a9}", "2019-2024"],
        vec!["Since", "1998", "2024"],
        vec!["555--1234"],
        vec!["555", "-", "1234"],
        vec!["123-45"],
    ] {
        assert!(found(&words).is_empty(), "{words:?}");
    }
}

#[test]
fn numbers_in_parentheses_are_found_without_them() {
    // 7 digits with a dash is a legitimate local number, so it counts even in parentheses.
    for (words, first, last) in [
        (
            vec!["Ahmet", "Y\u{131}lmaz", "(0532", "123", "45", "67)"],
            2,
            5,
        ),
        (vec!["John", "(555-123-4567)"], 1, 1),
        (vec!["(555-1234)"], 0, 0),
        // The engine lost the closing parenthesis of the area code.
        (vec!["(555", "123-4567"], 0, 1),
    ] {
        assert_eq!(
            found(&words),
            vec![around(&words, first, last)],
            "{words:?}"
        );
    }
}

#[test]
fn a_rejected_run_does_not_hide_the_numbers_inside_it() {
    let dash = [
        "Tel:", "0532", "123", "45", "67", "-", "0533", "765", "43", "21",
    ];
    assert_eq!(found(&dash), vec![around(&dash, 1, 4), around(&dash, 6, 9)]);
    let date = ["15.01.2024", "0532", "123", "45", "67"];
    assert_eq!(found(&date), vec![around(&date, 1, 4)]);
    let side_by_side = ["0212", "345", "67", "89", "0532", "123", "45", "67"];
    assert_eq!(
        found(&side_by_side),
        vec![around(&side_by_side, 0, 3), around(&side_by_side, 4, 7)]
    );
}

#[test]
fn an_order_number_before_a_number_never_leaves_digits_uncovered() {
    let words = ["Order", "12345", "0532", "123", "45", "67"];
    // The digits cannot be told apart, so they are covered in two pieces.
    assert_eq!(
        found(&words),
        vec![around(&words, 1, 2), around(&words, 3, 5)]
    );
}

#[test]
fn words_far_apart_are_not_one_number() {
    let near = [
        OcrWord {
            text: "1234".into(),
            bounds: WordBox::new(0.0, 0.0, 24.0, 10.0),
        },
        OcrWord {
            text: "5678".into(),
            bounds: WordBox::new(30.0, 0.0, 54.0, 10.0),
        },
    ];
    let mut far = near.clone();
    far[1].bounds = WordBox::new(124.0, 0.0, 148.0, 10.0);
    let lines = |words: &[OcrWord]| {
        vec![OcrLine {
            text: "1234 5678".into(),
            words: words.to_vec(),
        }]
    };
    assert_eq!(
        sensitive_boxes(&lines(&near)),
        vec![WordBox::new(0.0, 0.0, 54.0, 10.0)]
    );
    assert!(sensitive_boxes(&lines(&far)).is_empty());
}

#[test]
fn every_line_is_matched_on_its_own() {
    let lines = [
        line(&["a@b.com"]),
        line(&["nothing"]),
        line(&["5551234567"]),
    ];
    assert_eq!(
        sensitive_boxes(&lines),
        vec![around(&["a@b.com"], 0, 0), around(&["5551234567"], 0, 0)]
    );
}

// ---- pixels -------------------------------------------------------------------------

/// A 4 x 3 frame whose pixel (x, y) is `[x * 10, y * 10, x + y, 7]`.
fn frame() -> Vec<u8> {
    (0..3)
        .flat_map(|y| (0..4).flat_map(move |x| [x as u8 * 10, y as u8 * 10, (x + y) as u8, 7]))
        .collect()
}

#[test]
fn a_selection_is_copied_out_of_the_frame_opaque() {
    let image = OcrImage::from_selection(&frame(), 4, 3, Rect::new(1, 1, 3, 3)).unwrap();
    assert_eq!((image.origin, image.width, image.height), ((1, 1), 2, 2));
    assert_eq!(image.area(), Rect::new(1, 1, 3, 3));
    assert_eq!(
        image.bgra,
        [
            10, 10, 2, 255, 20, 10, 3, 255, 10, 20, 3, 255, 20, 20, 4, 255
        ]
    );
}

#[test]
fn a_selection_beyond_the_frame_is_cut_to_it_or_refused() {
    let cut = OcrImage::from_selection(&frame(), 4, 3, Rect::new(2, 0, 10, 10)).unwrap();
    assert_eq!((cut.width, cut.height, cut.origin), (2, 3, (2, 0)));
    assert!(OcrImage::from_selection(&frame(), 4, 3, Rect::new(5, 5, 9, 9)).is_none());
    assert!(OcrImage::from_selection(&frame(), 0, 0, Rect::new(0, 0, 4, 3)).is_none());
    assert!(OcrImage::from_selection(&frame()[..20], 4, 3, Rect::new(0, 0, 4, 3)).is_none());
}

#[test]
fn images_are_shrunk_to_the_limit_keeping_their_proportions() {
    assert_eq!(fit(5000, 2500, 2600), (2600, 1300));
    assert_eq!(fit(2500, 5000, 2600), (1300, 2600));
    assert_eq!(fit(100, 100, 2600), (100, 100));
    assert_eq!(fit(2600, 10, 2600), (2600, 10));
    assert_eq!(fit(10, 5000, 100), (1, 100), "a side never vanishes");
    assert_eq!(fit(5, 5, 0), (1, 1));
}

fn gray(width: i32, height: i32, values: &[u8]) -> OcrImage {
    OcrImage {
        origin: (3, 4),
        width,
        height,
        bgra: values.iter().flat_map(|&v| [v, v, v, 255]).collect(),
    }
}

#[test]
fn an_image_within_the_limit_goes_to_the_engine_as_it_is() {
    let image = gray(2, 1, &[1, 2]);
    let lines = vec![line(&["word"])];
    let engine = Canned::new(2, Ok(lines.clone()));
    assert_eq!(recognize(&engine, &image), Ok(lines));
    assert_eq!(*engine.seen.lock().unwrap(), vec![image]);
}

#[test]
fn a_large_image_is_averaged_down_and_its_boxes_map_back() {
    // 5 x 1 shrunk to 2 x 1: the first pixel covers two sources, the second three.
    let image = gray(5, 1, &[0, 10, 20, 30, 40]);
    let found = vec![OcrLine {
        text: "w".into(),
        words: vec![OcrWord {
            text: "w".into(),
            bounds: WordBox::new(1.0, 0.0, 2.0, 1.0),
        }],
    }];
    let engine = Canned::new(2, Ok(found));
    let lines = recognize(&engine, &image).unwrap();
    let seen = engine.seen.lock().unwrap();
    assert_eq!((seen[0].width, seen[0].height), (2, 1));
    assert_eq!(seen[0].origin, (3, 4));
    assert_eq!(seen[0].bgra, [5, 5, 5, 255, 30, 30, 30, 255]);
    // x grows by 5 / 2, y by 1 / 1.
    assert_eq!(lines[0].words[0].bounds, WordBox::new(2.5, 0.0, 5.0, 1.0));
    assert_eq!(lines[0].text, "w");
}

#[test]
fn a_failure_of_the_engine_comes_through_a_shrunk_image_too() {
    let engine = Canned::new(1, Err(OcrFailure::NoLanguage));
    assert_eq!(
        recognize(&engine, &gray(4, 4, &[0; 16])),
        Err(OcrFailure::NoLanguage)
    );
}

// ---- what a result means ------------------------------------------------------------

const AREA: Rect = Rect::new(100, 100, 500, 400);

fn word_at(text: &str, left: f32, top: f32, right: f32, bottom: f32) -> Vec<OcrLine> {
    vec![OcrLine {
        text: text.into(),
        words: vec![OcrWord {
            text: text.into(),
            bounds: WordBox::new(left, top, right, bottom),
        }],
    }]
}

#[test]
fn the_shortcut_keys_name_the_commands() {
    assert_eq!(Intent::for_key(0x43), Some(Intent::CopyText));
    assert_eq!(Intent::for_key(0x52), Some(Intent::RedactSensitive));
    assert_eq!(Intent::for_key(0x56), None);
    assert_eq!(Intent::CopyText.title(), t("Copy text"));
    assert_eq!(
        Intent::RedactSensitive.title(),
        t("Redact emails and phone numbers")
    );
}

#[test]
fn recognized_lines_are_copied_one_per_line_with_crlf() {
    let lines = vec![line(&["Hello"]), line(&["big", "world"])];
    assert_eq!(
        resolve(Intent::CopyText, Ok(lines), AREA),
        Resolution::Copy {
            text: "Hello\r\nbig world".into(),
            message: t("Text copied"),
        }
    );
}

#[test]
fn a_selection_without_text_says_so() {
    for lines in [vec![], vec![line(&[" "])]] {
        assert_eq!(
            resolve(Intent::CopyText, Ok(lines), AREA),
            Resolution::Notice(t("No text found in the selection").into())
        );
    }
}

#[test]
fn a_missing_language_is_an_error_dialog_for_either_command() {
    for intent in [Intent::CopyText, Intent::RedactSensitive] {
        assert_eq!(
            resolve(intent, Err(OcrFailure::NoLanguage), AREA),
            Resolution::Error {
                title: intent.title(),
                message: t(
                    "Windows has no text recognition language installed. Add a language under Settings › Time & language › Language & region."
                )
                .into(),
            }
        );
    }
}

#[test]
fn an_engine_failure_is_an_error() {
    assert_eq!(
        resolve(
            Intent::RedactSensitive,
            Err(OcrFailure::Failed("boom".into())),
            AREA
        ),
        Resolution::Error {
            title: t("Text recognition failed"),
            message: "boom".into(),
        }
    );
}

#[test]
fn redactions_cover_the_matched_words_padded_and_in_capture_pixels() {
    // Rounded outwards to 10..111 x 20..34, moved by the selection origin, 2 px of padding.
    let lines = word_at("john@example.com", 10.2, 20.7, 110.1, 34.0);
    assert_eq!(
        resolve(Intent::RedactSensitive, Ok(lines), AREA),
        Resolution::Redact {
            rects: vec![Rect::new(108, 118, 213, 136)],
            message: t("Redacted 1 item").into(),
        }
    );
}

#[test]
fn redactions_stay_inside_the_selection_and_empty_ones_are_dropped() {
    // The first is pushed in by the selection's corner, the second lies beyond its right
    // edge and vanishes once clipped, the third is cut off on the right.
    let mut lines = word_at("john@example.com", 0.0, 0.0, 50.0, 12.0);
    lines.extend(word_at("a@b.com", 450.0, 0.0, 460.0, 12.0));
    lines.extend(word_at("c@d.com", 380.0, 5.0, 420.0, 12.0));
    assert_eq!(
        resolve(Intent::RedactSensitive, Ok(lines), AREA),
        Resolution::Redact {
            rects: vec![Rect::new(100, 100, 152, 114), Rect::new(478, 103, 500, 114)],
            message: tf("Redacted {} items", &[&2]),
        }
    );
}

#[test]
fn nothing_to_redact_says_so() {
    let lines = vec![line(&["Just", "words", "2024"])];
    assert_eq!(
        resolve(Intent::RedactSensitive, Ok(lines), AREA),
        Resolution::Notice(t("No email addresses or phone numbers found").into())
    );
}

#[test]
fn a_span_split_by_a_separator_no_phone_uses_is_not_one_number() {
    let parse = |text: &str| {
        let chars: Vec<char> = text.chars().collect();
        super::matching::Phone::parse(&chars).is_some()
    };
    assert!(parse("0532 123 4567"));
    assert!(!parse("0532 - 123 4567"));
    assert!(!parse("0532--123 4567"));
}
