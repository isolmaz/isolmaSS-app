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
    const LANG_TURKISH: u16 = 0x1f;
    let id = unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() };
    if id & 0x3ff == LANG_TURKISH {
        Language::Turkish
    } else {
        Language::English
    }
});

static TURKISH: LazyLock<HashMap<&'static str, &'static str>> =
    LazyLock::new(|| tr::TABLE.iter().copied().collect());

/// Translates an English interface string into the current language.
pub fn t(english: &'static str) -> &'static str {
    match current() {
        Language::English => english,
        Language::Turkish => TURKISH.get(english).copied().unwrap_or(english),
    }
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
        let _lock = TEST_LANGUAGE_LOCK.lock().unwrap();
        set_preference(LanguagePreference::English);
        assert_eq!(tf("{} of {}", &[&1, &"two"]), "1 of two");
        set_preference(LanguagePreference::System);
    }
}
