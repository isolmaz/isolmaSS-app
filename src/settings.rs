use crate::annotation::ToolKind;
use crate::hotkey::HotkeyConfig;
use crate::i18n::LanguagePreference;
use crate::save::default_save_directory;
use crate::theme::ThemePreference;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use windows::Win32::Storage::FileSystem::{
    MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
};
use windows::core::PCWSTR;

pub const PRESET_COLORS: [[u8; 4]; 8] = [
    [49, 49, 224, 255],   // Red (#E03131)
    [7, 103, 247, 255],   // Orange (#F76707)
    [25, 196, 252, 255],  // Yellow (#FCC419)
    [68, 158, 47, 255],   // Green (#2F9E44)
    [194, 113, 25, 255],  // Blue (#1971C2)
    [181, 54, 156, 255],  // Purple (#9C36B5)
    [255, 255, 255, 255], // White (#FFFFFF)
    [41, 37, 33, 255],    // Black (#212529)
];

pub const PRESET_THICKNESSES: [i32; 3] = [2, 4, 8];

fn default_true() -> bool {
    true
}

fn default_jpeg_quality() -> u8 {
    90
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SaveFormat {
    #[default]
    Png,
    Jpeg,
}

impl SaveFormat {
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
        }
    }
}

/// Persistent application settings model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub hotkey: HotkeyConfig,
    pub save_directory: PathBuf,
    pub default_color: [u8; 4],
    pub last_custom_color: [u8; 4],
    pub default_thickness: i32,
    #[serde(default = "default_true")]
    pub enable_window_snap: bool,
    #[serde(default = "default_true")]
    pub close_after_action: bool,
    #[serde(default)]
    pub start_with_windows: bool,
    #[serde(default = "default_true")]
    pub notify_after_save: bool,
    #[serde(default)]
    pub capture_delay_ms: u32,
    #[serde(default)]
    pub save_format: SaveFormat,
    #[serde(default = "default_jpeg_quality")]
    pub jpeg_quality: u8,
    #[serde(default = "default_true")]
    pub check_updates_automatically: bool,
    #[serde(default)]
    pub skipped_update_version: Option<String>,
    #[serde(default)]
    pub cloud_url: Option<String>,
    #[serde(default)]
    pub theme_preference: ThemePreference,
    #[serde(default)]
    pub language: LanguagePreference,
    pub last_tool: ToolKind,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            hotkey: HotkeyConfig::default(),
            save_directory: default_save_directory(),
            default_color: PRESET_COLORS[0], // Default vibrant red
            last_custom_color: PRESET_COLORS[5],
            default_thickness: 4,
            enable_window_snap: true,
            close_after_action: true,
            start_with_windows: false,
            notify_after_save: true,
            capture_delay_ms: 0,
            save_format: SaveFormat::Png,
            jpeg_quality: default_jpeg_quality(),
            check_updates_automatically: true,
            skipped_update_version: None,
            cloud_url: None,
            theme_preference: ThemePreference::System,
            language: LanguagePreference::System,
            last_tool: ToolKind::default(),
        }
    }
}

impl Settings {
    /// Canonical path to settings file: `%APPDATA%\isolmaSS\settings.json`.
    pub fn config_path() -> Option<PathBuf> {
        crate::hotkey::settings_path()
    }

    /// Loads settings, returning a user-visible warning when recovery was required.
    ///
    /// A missing file yields defaults. Invalid content is preserved once under
    /// the cross-process settings lock and then reset to defaults. Every other
    /// read error (sharing violation, permissions, AV lock) is propagated
    /// unchanged and never causes a write over the existing file.
    pub fn load_with_warning() -> std::io::Result<(Self, Option<String>)> {
        let Some(path) = Self::config_path() else {
            return Ok((
                Self::default(),
                Some(
                    crate::i18n::t(
                        "Settings could not be loaded because %APPDATA% is unavailable.",
                    )
                    .to_string(),
                ),
            ));
        };

        match Self::load_from_path(&path) {
            Ok(settings) => Ok((settings, None)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok((Self::default(), None))
            }
            Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
                let _lock = crate::instance::SettingsLock::acquire()?;
                // Single fixed backup name: repeated loads can never accumulate
                // more than one preserved copy.
                let backup = path.with_extension("corrupt.json");
                std::fs::copy(&path, &backup).map_err(|copy_error| {
                    std::io::Error::new(
                        copy_error.kind(),
                        format!(
                            "settings.json is invalid ({error}) and could not be preserved before recovery: {copy_error}"
                        ),
                    )
                })?;
                let defaults = Self::default();
                defaults.save_to_path(&path).map_err(|save_error| {
                    std::io::Error::new(
                        save_error.kind(),
                        format!(
                            "settings.json is invalid but was preserved at {}; resetting it failed: {save_error}",
                            backup.display()
                        ),
                    )
                })?;
                Ok((
                    defaults,
                    Some(crate::i18n::tf(
                        "Settings were invalid and have been reset. The original was preserved at {}.",
                        &[&backup.display()],
                    )),
                ))
            }
            Err(error) => Err(error),
        }
    }

    /// Loads settings from disk, recording any recovery warning to diagnostics.
    pub fn load() -> std::io::Result<Self> {
        let (settings, warning) = Self::load_with_warning()?;
        if let Some(warning) = warning {
            crate::diagnostics::record("settings recovery", &warning);
        }
        Ok(settings)
    }

    /// Saves settings through a same-directory temporary file and an atomic replacement.
    pub fn save_to_path(&self, path: &Path) -> std::io::Result<()> {
        self.validate()?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "settings path has no parent directory",
                )
            })?;
        std::fs::create_dir_all(parent)?;

        let json = serde_json::to_vec_pretty(self)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let temp_path = parent.join(format!(".settings-{}-{unique}.tmp", std::process::id()));

        let result = (|| -> std::io::Result<()> {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp_path)?;
            file.write_all(&json)?;
            file.sync_all()?;
            drop(file);

            use std::os::windows::ffi::OsStrExt;
            let source: Vec<u16> = temp_path.as_os_str().encode_wide().chain(Some(0)).collect();
            let destination: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            unsafe {
                MoveFileExW(
                    PCWSTR(source.as_ptr()),
                    PCWSTR(destination.as_ptr()),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
            }
            .map_err(std::io::Error::other)
        })();

        if result.is_err() {
            let _ = std::fs::remove_file(&temp_path);
        }
        result
    }

    /// Loads settings from an arbitrary path on disk.
    pub fn load_from_path(path: &Path) -> std::io::Result<Self> {
        let mut content = String::new();
        std::fs::File::open(path)?
            .take(65_537)
            .read_to_string(&mut content)?;
        if content.len() > 65_536 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "settings.json exceeds 64 KiB",
            ));
        }
        let settings = serde_json::from_str::<Settings>(&content)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        settings.validate()?;
        Ok(settings)
    }

    pub fn validate(&self) -> std::io::Result<()> {
        let problem = if !(1..=64).contains(&self.default_thickness) {
            Some("default_thickness must be between 1 and 64 pixels")
        } else if self.capture_delay_ms > 5_000 {
            Some("capture_delay_ms must be between 0 and 5000")
        } else if !(1..=100).contains(&self.jpeg_quality) {
            Some("jpeg_quality must be between 1 and 100")
        } else if !self.save_directory.is_absolute()
            || self.save_directory.to_string_lossy().contains('\0')
        {
            Some("save_directory must be an absolute Windows path without NUL characters")
        } else if !self.hotkey.is_valid() {
            Some("hotkey description, key and modifiers must describe the same supported shortcut")
        } else if self.skipped_update_version.as_ref().is_some_and(|version| {
            version.len() > 32
                || !version
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || byte == b'.')
        }) {
            Some("skipped_update_version must be a dotted numeric version")
        } else if self
            .cloud_url
            .as_ref()
            .is_some_and(|url| !crate::cloudflare_setup::valid_cloud_origin(url))
        {
            Some("cloud_url must be a plain HTTPS origin without a path, credentials, or fragment")
        } else {
            None
        };
        match problem {
            Some(message) => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                message,
            )),
            None => Ok(()),
        }
    }

    /// Merge only editor preferences into the latest configuration, once per session.
    pub fn save_editor_preferences(&self) -> std::io::Result<()> {
        let _lock = crate::instance::SettingsLock::acquire()?;
        let path =
            Self::config_path().ok_or_else(|| std::io::Error::other("APPDATA is unavailable"))?;
        let mut latest = match Self::load_from_path(&path) {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(error) => return Err(error),
        };
        latest.default_color = self.default_color;
        latest.last_custom_color = self.last_custom_color;
        latest.default_thickness = self.default_thickness;
        latest.last_tool = self.last_tool;
        latest.save_to_path(&path)
    }

    /// Update dismissal merges into the latest on-disk preferences under the settings lock.
    pub fn skip_update_version(version: &str) -> std::io::Result<()> {
        let _lock = crate::instance::SettingsLock::acquire()?;
        let path =
            Self::config_path().ok_or_else(|| std::io::Error::other("APPDATA is unavailable"))?;
        let mut latest = match Self::load_from_path(&path) {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(error) => return Err(error),
        };
        latest.skipped_update_version = Some(version.to_owned());
        latest.save_to_path(&path)
    }

    fn save_to_config_path(&self, path: Option<&std::path::Path>) -> std::io::Result<()> {
        let path = path.ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "cannot save settings: %APPDATA% is unavailable",
            )
        })?;
        self.save_to_path(path)
    }

    /// Saves settings to disk as formatted JSON.
    pub fn save(&self) -> std::io::Result<()> {
        let _lock = crate::instance::SettingsLock::acquire()?;
        // Existence check lives only on this user-initiated path: validate()
        // runs on every overlay open and save_directory may be a UNC location.
        // An inaccessible folder is skipped, never treated as an error here.
        if let Ok(metadata) = std::fs::metadata(&self.save_directory)
            && metadata.is_file()
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "save_directory exists and is not a directory",
            ));
        }
        let path = Self::config_path();
        self.save_to_config_path(path.as_deref())
    }
}

pub use crate::settings_window::show_settings_dialog;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn older_settings_keep_saved_values_and_fill_new_preferences() {
        let legacy_json = serde_json::json!({
            "hotkey": HotkeyConfig::from_str("Ctrl+Alt+K").expect("supported shortcut"),
            "save_directory": r"C:\Capture Archive",
            "default_color": [23, 45, 67, 255],
            "default_thickness": 11,
        });
        let loaded: Settings = serde_json::from_value(legacy_json).expect("legacy settings");
        assert_eq!(loaded.hotkey.description, "Ctrl+Alt+K");
        assert_eq!(loaded.save_directory, PathBuf::from(r"C:\Capture Archive"));
        assert_eq!(loaded.default_color, [23, 45, 67, 255]);
        assert_eq!(loaded.default_thickness, 11);
        assert_eq!(loaded.theme_preference, ThemePreference::System);
        assert_eq!(loaded.skipped_update_version, None);
        assert!(loaded.enable_window_snap && loaded.close_after_action);
        loaded.validate().expect("migrated settings remain usable");
    }

    #[test]
    fn cloud_connection_requires_exact_secure_origin() {
        let mut settings = Settings {
            cloud_url: Some("https://screens.example.workers.dev".into()),
            ..Settings::default()
        };
        settings.validate().expect("secure Worker origin");
        for origin in [
            "http://screens.example.workers.dev",
            "https://screens.example.workers.dev/path",
            "https://screens.example.workers.dev/?token=secret",
            "https://user@screens.example.workers.dev",
            "https://screens.example.workers.dev:443",
            "https://screens.example.workers.dev#fragment",
        ] {
            settings.cloud_url = Some(origin.into());
            assert!(settings.validate().is_err(), "invalid origin: {origin}");
        }
    }

    #[test]
    fn test_settings_save_to_path_roundtrip_and_error() {
        let mut defaults = Settings::default();
        defaults.last_custom_color = [17, 83, 209, 255];
        defaults.default_color = defaults.last_custom_color;
        let unique_suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock should be after the Unix epoch")
            .as_nanos();
        let temp_root = std::env::temp_dir().join(format!(
            "isolmass_unit_test_settings_{}_{}",
            std::process::id(),
            unique_suffix
        ));
        let temp_file = temp_root.join("roundtrip").join("settings.json");

        defaults
            .save_to_path(&temp_file)
            .expect("save_to_path should create the parent directory");
        let loaded = Settings::load_from_path(&temp_file).expect("load_from_path should succeed");
        assert_eq!(loaded, defaults);

        let blocker = temp_root.join("parent_blocker");
        std::fs::write(&blocker, b"not a directory").expect("create parent blocker");
        let blocked_parent = blocker.join("missing");
        let expected_error = std::fs::create_dir_all(&blocked_parent)
            .expect_err("create_dir_all should fail below a file");
        let save_error = defaults
            .save_to_path(&blocked_parent.join("settings.json"))
            .expect_err("save_to_path should propagate the create_dir_all error");
        assert_eq!(save_error.kind(), expected_error.kind());
        assert_eq!(save_error.raw_os_error(), expected_error.raw_os_error());

        let missing_path_error = defaults
            .save_to_config_path(None)
            .expect_err("a missing config path should prevent saving");
        assert_eq!(missing_path_error.kind(), std::io::ErrorKind::NotFound);
        assert_eq!(
            missing_path_error.to_string(),
            "cannot save settings: %APPDATA% is unavailable"
        );

        for (key, value) in [
            ("default_thickness", serde_json::json!(-1)),
            ("capture_delay_ms", serde_json::json!(6000)),
            ("jpeg_quality", serde_json::json!(0)),
            ("save_directory", serde_json::json!("")),
        ] {
            let mut invalid = serde_json::to_value(&defaults).unwrap();
            invalid[key] = value;
            std::fs::write(&temp_file, serde_json::to_vec(&invalid).unwrap()).unwrap();
            assert!(Settings::load_from_path(&temp_file).is_err(), "{key}");
        }
        std::fs::write(&temp_file, " ".repeat(65_537)).unwrap();
        assert!(Settings::load_from_path(&temp_file).is_err());
        assert!(crate::hotkey::HotkeyConfig::from_str("Ctrl+A+B").is_none());
        std::fs::remove_dir_all(&temp_root).expect("remove settings test directory");
    }
}
