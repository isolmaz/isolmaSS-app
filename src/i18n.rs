//! Interface language. Source strings are English and are translated through
//! one table per language; a missing entry falls back to the English text.
//! Adding a language means adding a table module and a [`Language`] variant.
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU8, Ordering};

mod tr;

/// The user's choice in Settings; `System` follows the Windows display language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LanguagePreference {
    #[default]
    System,
    #[serde(rename = "en")]
    English,
    #[serde(rename = "tr")]
    Turkish,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    English,
    Turkish,
}

// 0 = follow Windows, 1 = English, 2 = Turkish.
static PREFERENCE: AtomicU8 = AtomicU8::new(0);

pub fn set_preference(preference: LanguagePreference) {
    let value = match preference {
        LanguagePreference::System => 0,
        LanguagePreference::English => 1,
        LanguagePreference::Turkish => 2,
    };
    PREFERENCE.store(value, Ordering::Release);
}

pub fn current() -> Language {
    match PREFERENCE.load(Ordering::Acquire) {
        1 => Language::English,
        2 => Language::Turkish,
        _ => *SYSTEM,
    }
}

/// The BCP 47 code of the current language, for HTML `lang` attributes.
pub fn code() -> &'static str {
    match current() {
        Language::English => "en",
        Language::Turkish => "tr",
    }
}

/// Turkish when the Windows display language is Turkish, otherwise English.
static SYSTEM: LazyLock<Language> = LazyLock::new(|| {
    language_of(unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() })
});

/// The interface language for a Windows language identifier (LANGID): the primary
/// language is in the low ten bits, the sublanguage (region) above them.
fn language_of(id: u16) -> Language {
    const LANG_TURKISH: u16 = 0x1f;
    if id & 0x3ff == LANG_TURKISH {
        Language::Turkish
    } else {
        Language::English
    }
}

static TURKISH: LazyLock<HashMap<&'static str, &'static str>> =
    LazyLock::new(|| tr::TABLE.iter().copied().collect());

/// Translates an English interface string into the current language.
pub fn t(english: &'static str) -> &'static str {
    match current() {
        Language::English => english,
        Language::Turkish => TURKISH.get(english).copied().unwrap_or(english),
    }
}

static ENGLISH: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    tr::TABLE
        .iter()
        .map(|&(english, turkish)| (turkish, english))
        .collect()
});

/// The English source text of an interface string that was already translated, so
/// logs keep one language and stored texts can be translated again after a language
/// change. Text that is not a translation is returned unchanged.
pub fn english_of(text: &str) -> &str {
    ENGLISH.get(text).copied().unwrap_or(text)
}

/// Translates `template` and replaces each `{}` with the next argument.
pub fn tf(template: &'static str, args: &[&dyn std::fmt::Display]) -> String {
    let mut out = String::new();
    let mut args = args.iter();
    let mut parts = t(template).split("{}");
    if let Some(first) = parts.next() {
        out.push_str(first);
    }
    for part in parts {
        if let Some(arg) = args.next() {
            out.push_str(&arg.to_string());
        }
        out.push_str(part);
    }
    out
}

/// Held by tests that change the language, since tests run in parallel.
#[cfg(test)]
pub static TEST_LANGUAGE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::test_env::lock;

    /// The string literal starting at `from` (just after its opening quote), unescaped.
    fn literal(text: &str, from: usize) -> String {
        let mut value = String::new();
        let mut chars = text[from..].chars();
        while let Some(ch) = chars.next() {
            match ch {
                '\\' => match chars.next() {
                    Some('n') => value.push('\n'),
                    Some('r') => value.push('\r'),
                    Some(other) => value.push(other),
                    None => break,
                },
                '"' => break,
                other => value.push(other),
            }
        }
        value
    }

    /// Every `t("…")` and `tf("…")` literal in the sources outside this module.
    fn source_strings() -> Vec<String> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut paths = vec![root];
        let mut found = Vec::new();
        while let Some(path) = paths.pop() {
            if path.is_dir() {
                paths.extend(std::fs::read_dir(&path).unwrap().map(|e| e.unwrap().path()));
                continue;
            }
            let name = path.to_string_lossy().replace('\\', "/");
            if !name.ends_with(".rs") || name.ends_with("src/i18n.rs") || name.contains("src/i18n/")
            {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let bytes = text.as_bytes();
            for needle in ["t(", "tf("] {
                let mut index = 0;
                while let Some(offset) = text[index..].find(needle) {
                    let start = index + offset;
                    index = start + needle.len();
                    let boundary = start == 0
                        || !(bytes[start - 1].is_ascii_alphanumeric()
                            || bytes[start - 1] == b'_'
                            || bytes[start - 1] == b'.');
                    let rest = text[index..].trim_start();
                    if boundary && rest.starts_with('"') {
                        found.push(literal(rest, 1));
                    }
                }
            }
        }
        found.sort();
        found.dedup();
        found
    }

    #[test]
    fn turkish_covers_every_interface_string() {
        let used = source_strings();
        assert!(used.len() > 100, "found only {} strings", used.len());
        let missing: Vec<_> = used
            .iter()
            .filter(|s| !TURKISH.contains_key(s.as_str()))
            .collect();
        assert!(missing.is_empty(), "missing Turkish entries: {missing:#?}");
        let unused: Vec<_> = tr::TABLE
            .iter()
            .filter(|(english, _)| !used.iter().any(|s| s == english))
            .collect();
        assert!(unused.is_empty(), "unused Turkish entries: {unused:#?}");
        assert_eq!(TURKISH.len(), tr::TABLE.len(), "duplicate Turkish keys");
        for (english, turkish) in tr::TABLE {
            assert_eq!(
                english.matches("{}").count(),
                turkish.matches("{}").count(),
                "placeholder count differs for {english:?}"
            );
        }
    }

    #[test]
    fn placeholders_fill_in_order() {
        let _lock = lock(&TEST_LANGUAGE_LOCK);
        set_preference(LanguagePreference::English);
        assert_eq!(tf("{} of {}", &[&1, &"two"]), "1 of two");
        set_preference(LanguagePreference::System);
    }

    #[test]
    fn literal_unescapes_up_to_the_closing_quote() {
        assert_eq!(
            literal(r#"a\"b\\c\nd\re" tail"#, 0),
            "a\"b\\c\nd\re",
            "escaped quote and backslash stay in the text, the bare quote ends it"
        );
        assert_eq!(
            literal("xyz\"", 1),
            "yz",
            "reading starts at the given offset"
        );
        assert_eq!(
            literal("open\\", 0),
            "open",
            "a backslash at the very end has nothing to escape"
        );
    }

    #[test]
    fn windows_language_ids_select_turkish_only_for_turkish() {
        assert_eq!(language_of(0x041f), Language::Turkish, "tr-TR");
        assert_eq!(language_of(0x001f), Language::Turkish, "neutral Turkish");
        assert_eq!(language_of(0x0409), Language::English, "en-US");
        assert_eq!(language_of(0x0407), Language::English, "de-DE");
        assert_eq!(
            language_of(0x041e),
            Language::English,
            "th-TH differs in one bit"
        );
        assert_eq!(language_of(0), Language::English);
    }

    #[test]
    fn preference_decides_between_windows_language_and_a_fixed_language() {
        let _lock = lock(&TEST_LANGUAGE_LOCK);
        let windows_language =
            language_of(unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() });
        set_preference(LanguagePreference::System);
        assert_eq!(current(), windows_language);
        set_preference(LanguagePreference::English);
        assert_eq!(current(), Language::English);
        assert_eq!(code(), "en");
        set_preference(LanguagePreference::Turkish);
        assert_eq!(current(), Language::Turkish);
        assert_eq!(code(), "tr");
        set_preference(LanguagePreference::System);
    }

    #[test]
    fn log_labels_stay_english_in_every_language() {
        let _lock = lock(&TEST_LANGUAGE_LOCK);
        let unique: Vec<&'static str> = tr::TABLE
            .iter()
            .filter(|(_, turkish)| tr::TABLE.iter().filter(|(_, t)| t == turkish).count() == 1)
            .map(|(english, _)| *english)
            .collect();
        assert!(unique.len() > 100);
        for preference in [
            LanguagePreference::English,
            LanguagePreference::Turkish,
            LanguagePreference::English,
        ] {
            set_preference(preference);
            for &english in &unique {
                assert_eq!(english_of(t(english)), english, "{preference:?}");
            }
        }
        set_preference(LanguagePreference::Turkish);
        assert_eq!(english_of("Worker update"), "Worker update");
        assert_eq!(english_of(t("Worker update")), "Worker update");
        assert_eq!(english_of("settings recovery"), "settings recovery");
        set_preference(LanguagePreference::System);
    }

    #[test]
    fn turkish_translates_known_strings_and_keeps_unknown_ones() {
        let _lock = lock(&TEST_LANGUAGE_LOCK);
        let (english, turkish) = tr::TABLE[0];
        assert_ne!(english, turkish, "the table must translate its first entry");
        set_preference(LanguagePreference::Turkish);
        assert_eq!(t(english), turkish);
        assert_eq!(t("Not an interface string"), "Not an interface string");
        set_preference(LanguagePreference::English);
        assert_eq!(t(english), english);
        set_preference(LanguagePreference::System);
    }
}
