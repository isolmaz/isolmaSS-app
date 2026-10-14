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

    /// A Cloudflare connection change merges into the latest on-disk preferences under
    /// the settings lock, so edits still pending in an open Settings window never reach
    /// the file.
    pub fn save_cloud_url(url: Option<String>) -> std::io::Result<()> {
        let _lock = crate::instance::SettingsLock::acquire()?;
        let path =
            Self::config_path().ok_or_else(|| std::io::Error::other("APPDATA is unavailable"))?;
        Self::set_cloud_url_at(&path, url)
    }

    fn set_cloud_url_at(path: &Path, url: Option<String>) -> std::io::Result<()> {
        let mut latest = match Self::load_from_path(path) {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(error) => return Err(error),
        };
        latest.cloud_url = url;
        latest.save_to_path(path)
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
pub(crate) mod test_env;

#[cfg(test)]
mod tests {
    use super::test_env::Sandbox;
    use super::*;
    use std::io::ErrorKind;
    use std::os::windows::fs::OpenOptionsExt;

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
    fn cloud_url_change_keeps_every_other_stored_value() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock should be after the Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "isolmass_unit_test_cloud_url_{}_{unique}",
            std::process::id()
        ));
        let path = root.join("settings.json");

        // A fresh installation gets defaults plus the new address.
        let origin = "https://screens.example.workers.dev".to_string();
        Settings::set_cloud_url_at(&path, Some(origin.clone())).expect("create settings");
        assert_eq!(
            Settings::load_from_path(&path).unwrap().cloud_url,
            Some(origin.clone())
        );

        let stored = Settings {
            hotkey: HotkeyConfig::from_str("Ctrl+Shift+K").expect("supported shortcut"),
            default_thickness: 9,
            start_with_windows: true,
            skipped_update_version: Some("1.2.3".into()),
            cloud_url: Some(origin.clone()),
            ..Settings::default()
        };
        stored.save_to_path(&path).expect("store settings");

        // Connecting a different Worker and disconnecting both leave the rest untouched.
        let other = "https://other.example.workers.dev".to_string();
        Settings::set_cloud_url_at(&path, Some(other.clone())).expect("switch Worker");
        let switched = Settings::load_from_path(&path).unwrap();
        assert_eq!(switched.cloud_url, Some(other));
        assert_eq!(
            Settings {
                cloud_url: stored.cloud_url.clone(),
                ..switched
            },
            stored
        );

        Settings::set_cloud_url_at(&path, None).expect("disconnect");
        let disconnected = Settings::load_from_path(&path).unwrap();
        assert_eq!(disconnected.cloud_url, None);
        assert_eq!(
            Settings {
                cloud_url: stored.cloud_url.clone(),
                ..disconnected
            },
            stored
        );
        std::fs::remove_dir_all(&root).expect("remove settings test directory");
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

    /// Every field set away from its default, saving into the sandbox.
    fn custom_settings(sandbox: &Sandbox) -> Settings {
        Settings {
            hotkey: HotkeyConfig::from_str("Ctrl+Shift+K").expect("supported shortcut"),
            save_directory: sandbox.path().join("Shots"),
            default_color: [1, 2, 3, 255],
            last_custom_color: [4, 5, 6, 255],
            default_thickness: 9,
            enable_window_snap: false,
            close_after_action: false,
            start_with_windows: true,
            notify_after_save: false,
            capture_delay_ms: 250,
            save_format: SaveFormat::Jpeg,
            jpeg_quality: 60,
            check_updates_automatically: false,
            skipped_update_version: Some("1.2.3".into()),
            cloud_url: Some("https://screens.example.workers.dev".into()),
            theme_preference: ThemePreference::Light,
            language: LanguagePreference::Turkish,
            last_tool: ToolKind::Arrow,
        }
    }

    /// Writes `content` as the stored settings file of the sandbox.
    fn write_stored(sandbox: &Sandbox, content: &[u8]) -> PathBuf {
        let path = sandbox.settings_file();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
        path
    }

    fn names(directory: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn missing_fields_take_their_defaults() {
        let loaded: Settings = serde_json::from_str("{}").expect("an empty object");
        assert_eq!(loaded.jpeg_quality, 90);
        assert!(loaded.enable_window_snap && loaded.close_after_action);
        assert!(loaded.notify_after_save && loaded.check_updates_automatically);
        assert_eq!(loaded.save_format, SaveFormat::Png);
        assert_eq!(loaded, Settings::default());
    }

    #[test]
    fn settings_live_in_the_roaming_folder() {
        let sandbox = Sandbox::new("settings_location");
        assert_eq!(Settings::config_path(), Some(sandbox.settings_file()));
        sandbox.remove_appdata();
        assert_eq!(Settings::config_path(), None);
    }

    #[test]
    fn a_new_installation_loads_defaults_without_writing_anything() {
        let sandbox = Sandbox::new("settings_new");
        assert_eq!(
            Settings::load_with_warning().unwrap(),
            (Settings::default(), None)
        );
        assert_eq!(Settings::load().unwrap(), Settings::default());
        assert!(!sandbox.settings_file().exists());
    }

    #[test]
    fn without_a_roaming_folder_defaults_load_with_a_warning() {
        let sandbox = Sandbox::new("settings_no_appdata");
        sandbox.remove_appdata();
        assert_eq!(
            Settings::load_with_warning().unwrap(),
            (
                Settings::default(),
                Some(
                    crate::i18n::t(
                        "Settings could not be loaded because %APPDATA% is unavailable."
                    )
                    .to_string()
                )
            )
        );
    }

    #[test]
    fn saved_settings_load_back_unchanged() {
        let sandbox = Sandbox::new("settings_roundtrip");
        let settings = custom_settings(&sandbox);
        settings.save().expect("save");
        assert_eq!(
            Settings::load_with_warning().unwrap(),
            (settings.clone(), None)
        );
        assert_eq!(Settings::load().unwrap(), settings);

        let changed = Settings {
            default_thickness: 3,
            ..settings
        };
        changed.save().expect("a second save replaces the file");
        assert_eq!(Settings::load().unwrap(), changed);
        assert_eq!(
            names(sandbox.settings_file().parent().unwrap()),
            ["settings.json"],
            "no temporary file is left"
        );
    }

    #[test]
    fn saving_refuses_a_save_folder_that_is_a_file_and_a_missing_profile() {
        let sandbox = Sandbox::new("settings_save_refused");
        let mut settings = custom_settings(&sandbox);
        let file = sandbox.path().join("not-a-folder");
        std::fs::write(&file, b"x").unwrap();
        settings.save_directory = file;
        let error = settings.save().expect_err("a file cannot hold screenshots");
        assert_eq!(error.kind(), ErrorKind::InvalidData);
        assert!(!sandbox.settings_file().exists(), "nothing was written");

        // A folder that does not exist (yet) is fine; screenshots create it on demand.
        settings.save_directory = sandbox.path().join("later");
        settings.save().expect("a missing folder is allowed");

        sandbox.remove_appdata();
        let error = settings.save().expect_err("no roaming folder");
        assert_eq!(error.kind(), ErrorKind::NotFound);
        assert_eq!(
            error.to_string(),
            "cannot save settings: %APPDATA% is unavailable"
        );
    }

    #[test]
    fn the_settings_path_needs_a_folder_to_live_in() {
        let error = Settings::default()
            .save_to_path(Path::new("settings.json"))
            .expect_err("a bare file name has no folder");
        assert_eq!(error.kind(), ErrorKind::InvalidInput);

        let sandbox = Sandbox::new("settings_replace_folder");
        let path = sandbox.path().join("settings.json");
        std::fs::create_dir(&path).unwrap();
        Settings::default()
            .save_to_path(&path)
            .expect_err("a folder cannot be replaced by a file");
        assert!(path.is_dir());
        assert_eq!(
            names(sandbox.path()),
            ["settings.json"],
            "the failed save left no temporary file"
        );
    }

    #[test]
    fn invalid_settings_are_preserved_once_then_reset() {
        let sandbox = Sandbox::new("settings_corrupt");
        let path = write_stored(&sandbox, b"{ not json");
        let backup = path.with_extension("corrupt.json");

        let (settings, warning) = Settings::load_with_warning().expect("recovered");
        assert_eq!(settings, Settings::default());
        assert_eq!(
            warning,
            Some(crate::i18n::tf(
                "Settings were invalid and have been reset. The original was preserved at {}.",
                &[&backup.display()]
            ))
        );
        assert_eq!(std::fs::read(&backup).unwrap(), b"{ not json");
        assert_eq!(
            Settings::load_from_path(&path).unwrap(),
            Settings::default()
        );

        // A later corruption replaces the single backup instead of adding another.
        write_stored(&sandbox, br#"{"default_thickness": 1000}"#);
        let (settings, warning) = Settings::load_with_warning().expect("recovered again");
        assert_eq!(settings, Settings::default());
        assert!(warning.is_some());
        assert_eq!(
            std::fs::read(&backup).unwrap(),
            br#"{"default_thickness": 1000}"#
        );
        assert_eq!(
            names(path.parent().unwrap()),
            ["settings.corrupt.json", "settings.json"]
        );

        // `load` recovers the same way and hands back only the settings.
        write_stored(&sandbox, b"[]");
        assert_eq!(Settings::load().unwrap(), Settings::default());
        assert_eq!(std::fs::read(&backup).unwrap(), b"[]");
    }

    #[test]
    fn unreadable_settings_are_reported_and_never_overwritten() {
        let sandbox = Sandbox::new("settings_unreadable");
        let path = sandbox.settings_file();
        std::fs::create_dir_all(&path).unwrap();
        for result in [
            Settings::load_with_warning().map(|loaded| loaded.0),
            Settings::load(),
        ] {
            let error = result.expect_err("a folder cannot be read as settings");
            assert!(
                !matches!(error.kind(), ErrorKind::NotFound | ErrorKind::InvalidData),
                "{error:?}"
            );
        }
        assert!(path.is_dir());
        assert_eq!(names(path.parent().unwrap()), ["settings.json"]);
    }

    #[test]
    fn recovery_stops_when_the_original_cannot_be_preserved() {
        let sandbox = Sandbox::new("settings_no_backup");
        let path = write_stored(&sandbox, b"{ broken");
        std::fs::create_dir(path.with_extension("corrupt.json")).unwrap();
        let error = Settings::load_with_warning().expect_err("no backup, no reset");
        assert!(
            error
                .to_string()
                .contains("could not be preserved before recovery"),
            "{error}"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"{ broken",
            "the original stays"
        );
    }

    #[test]
    fn recovery_reports_a_reset_that_fails_after_preserving_the_original() {
        let sandbox = Sandbox::new("settings_reset_fails");
        let path = write_stored(&sandbox, b"{ broken");
        // Like a virus scanner: the file may be read but not replaced.
        let _scanner = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&path)
            .unwrap();
        let backup = path.with_extension("corrupt.json");
        let error = Settings::load_with_warning().expect_err("the reset cannot replace the file");
        assert!(
            error.to_string().contains(&format!(
                "was preserved at {}; resetting it failed",
                backup.display()
            )),
            "{error}"
        );
        assert_eq!(std::fs::read(&backup).unwrap(), b"{ broken");
        assert_eq!(std::fs::read(&path).unwrap(), b"{ broken");
        assert_eq!(
            names(path.parent().unwrap()),
            ["settings.corrupt.json", "settings.json"],
            "the failed reset left no temporary file"
        );
    }

    #[test]
    fn skipping_an_update_changes_only_the_skipped_version() {
        let sandbox = Sandbox::new("settings_skip");
        Settings::skip_update_version("2.0").expect("fresh installation");
        assert_eq!(
            Settings::load().unwrap(),
            Settings {
                skipped_update_version: Some("2.0".into()),
                ..Settings::default()
            }
        );

        let stored = custom_settings(&sandbox);
        stored.save().unwrap();
        Settings::skip_update_version("2.0.1").expect("merge");
        assert_eq!(
            Settings::load().unwrap(),
            Settings {
                skipped_update_version: Some("2.0.1".into()),
                ..stored
            }
        );
    }

    #[test]
    fn a_cloudflare_connection_change_keeps_the_rest_of_the_stored_settings() {
        let sandbox = Sandbox::new("settings_cloud");
        let stored = custom_settings(&sandbox);
        stored.save().unwrap();
        let other = "https://other.example.workers.dev".to_string();
        Settings::save_cloud_url(Some(other.clone())).expect("connect");
        assert_eq!(
            Settings::load().unwrap(),
            Settings {
                cloud_url: Some(other),
                ..stored.clone()
            }
        );
        Settings::save_cloud_url(None).expect("disconnect");
        assert_eq!(
            Settings::load().unwrap(),
            Settings {
                cloud_url: None,
                ..stored
            }
        );
    }

    fn editor_session() -> Settings {
        Settings {
            default_color: [9, 8, 7, 255],
            last_custom_color: [6, 5, 4, 255],
            default_thickness: 12,
            last_tool: ToolKind::Rectangle,
            ..Settings::default()
        }
    }

    #[test]
    fn editor_preferences_merge_into_the_latest_stored_settings() {
        let sandbox = Sandbox::new("settings_editor");
        let session = editor_session();
        session
            .save_editor_preferences()
            .expect("fresh installation");
        assert_eq!(Settings::load().unwrap(), session);

        // The Settings window saved other values while the editor was open.
        let stored = custom_settings(&sandbox);
        stored.save().unwrap();
        session.save_editor_preferences().expect("merge");
        assert_eq!(
            Settings::load().unwrap(),
            Settings {
                default_color: session.default_color,
                last_custom_color: session.last_custom_color,
                default_thickness: session.default_thickness,
                last_tool: session.last_tool,
                ..stored
            }
        );
    }

    #[test]
    fn merges_never_touch_an_invalid_file_or_guess_a_missing_location() {
        let sandbox = Sandbox::new("settings_merge_errors");
        let operations: [(&str, &dyn Fn() -> std::io::Result<()>); 3] = [
            ("editor preferences", &|| {
                editor_session().save_editor_preferences()
            }),
            ("skipped version", &|| Settings::skip_update_version("3.1")),
            ("cloud url", &|| {
                Settings::save_cloud_url(Some("https://screens.example.workers.dev".into()))
            }),
        ];
        let path = write_stored(&sandbox, b"{ broken");
        for (name, operation) in &operations {
            let error = operation().expect_err(name);
            assert_eq!(error.kind(), ErrorKind::InvalidData, "{name}");
            assert_eq!(
                std::fs::read(&path).unwrap(),
                b"{ broken",
                "{name} left the file alone"
            );
        }
        assert_eq!(names(path.parent().unwrap()), ["settings.json"]);

        sandbox.remove_appdata();
        for (name, operation) in &operations {
            let error = operation().expect_err(name);
            assert_eq!(error.to_string(), "APPDATA is unavailable", "{name}");
        }
    }

    #[test]
    fn validation_names_the_setting_that_is_out_of_range() {
        let sandbox = Sandbox::new("settings_validate");
        custom_settings(&sandbox)
            .validate()
            .expect("a complete, valid file");

        type Damage = fn(&mut Settings);
        let broken: [(&str, Damage); 12] = [
            ("default_thickness", |s| s.default_thickness = 0),
            ("default_thickness", |s| s.default_thickness = 65),
            ("capture_delay_ms", |s| s.capture_delay_ms = 5_001),
            ("jpeg_quality", |s| s.jpeg_quality = 0),
            ("jpeg_quality", |s| s.jpeg_quality = 101),
            ("save_directory", |s| {
                s.save_directory = PathBuf::from("shots")
            }),
            ("save_directory", |s| {
                s.save_directory = PathBuf::from("C:\\a\0b")
            }),
            ("hotkey", |s| s.hotkey.vk = 0x4C),
            ("skipped_update_version", |s| {
                s.skipped_update_version = Some("1.2.x".into())
            }),
            ("skipped_update_version", |s| {
                s.skipped_update_version = Some("9".repeat(33))
            }),
            ("cloud_url", |s| {
                s.cloud_url = Some("http://a.example.com".into())
            }),
            ("cloud_url", |s| {
                s.cloud_url = Some("https://a.example.com/x".into())
            }),
        ];
        for (field, damage) in broken {
            let mut settings = custom_settings(&sandbox);
            damage(&mut settings);
            let error = settings.validate().expect_err(field);
            assert_eq!(error.kind(), ErrorKind::InvalidData, "{field}");
            assert!(error.to_string().contains(field), "{field}: {error}");
        }
    }
}
