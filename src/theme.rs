//! Design tokens: the single source for colors, metrics and typography
//! across settings, toolbar and overlay chrome. Neutrals, geometry and
//! typography follow Windows 11 Fluent; the accent is the isolmaSS brand
//! amber (with its ink).
//!
//! The light/dark choice follows Windows unless the user picks one; the
//! system theme is read from the registry once and cached until
//! [`invalidate_theme_cache`] runs (window procs call it from `WM_SETTINGCHANGE`).

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use windows::Win32::Foundation::{COLORREF, HWND};

/// Current system theme, derived from `AppsUseLightTheme`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Theme {
    Light,
    Dark,
}
/// User-selected appearance. System tracks the Windows app theme.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemePreference {
    #[default]
    System,
    Light,
    Dark,
}

// 0 = follow Windows, 1 = light, 2 = dark.
static PREFERENCE: AtomicU8 = AtomicU8::new(0);

pub fn set_preference(preference: ThemePreference) {
    let value = match preference {
        ThemePreference::System => 0,
        ThemePreference::Light => 1,
        ThemePreference::Dark => 2,
    };
    PREFERENCE.store(value, Ordering::Release);
    set_app_mode(value);
}

// ---------------------------------------------------------------------------
// Native dark controls — the uxtheme entry points Explorer, Notepad and the
// Windows shell use so scroll bars, combo boxes, edits, list boxes and popup
// menus render in the system dark style. They are exported by ordinal only
// (stable since Windows 10 1809); a missing export keeps the light controls.
// ---------------------------------------------------------------------------

type SetPreferredAppMode = unsafe extern "system" fn(i32) -> i32;
type AllowDarkModeForWindow = unsafe extern "system" fn(HWND, i32) -> i32;
type FlushMenuThemes = unsafe extern "system" fn();

struct UxTheme {
    set_preferred_app_mode: Option<SetPreferredAppMode>,
    allow_dark_mode_for_window: Option<AllowDarkModeForWindow>,
    flush_menu_themes: Option<FlushMenuThemes>,
}

fn uxtheme() -> &'static UxTheme {
    use std::sync::LazyLock;
    use windows::Win32::System::LibraryLoader::{
        GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
    };
    static API: LazyLock<UxTheme> = LazyLock::new(|| {
        let Ok(module) = (unsafe {
            LoadLibraryExW(
                windows::core::w!("uxtheme.dll"),
                None,
                LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        }) else {
            crate::diagnostics::record("theme", "uxtheme.dll could not be loaded");
            return UxTheme {
                set_preferred_app_mode: None,
                allow_dark_mode_for_window: None,
                flush_menu_themes: None,
            };
        };
        let ordinal = |number: usize| unsafe {
            GetProcAddress(module, windows::core::PCSTR(number as *const u8))
        };
        // SAFETY: the ordinals' signatures are fixed by the shipped uxtheme ABI.
        unsafe {
            UxTheme {
                set_preferred_app_mode: ordinal(135).map(|f| std::mem::transmute(f)),
                allow_dark_mode_for_window: ordinal(133).map(|f| std::mem::transmute(f)),
                flush_menu_themes: ordinal(136).map(|f| std::mem::transmute(f)),
            }
        }
    });
    &API
}

/// 0 follows Windows (AllowDark), 1 forces light, 2 forces dark.
fn set_app_mode(preference: u8) {
    const ALLOW_DARK: i32 = 1;
    const FORCE_DARK: i32 = 2;
    const FORCE_LIGHT: i32 = 3;
    let api = uxtheme();
    if let Some(set_mode) = api.set_preferred_app_mode {
        let mode = match preference {
            1 => FORCE_LIGHT,
            2 => FORCE_DARK,
            _ => ALLOW_DARK,
        };
        unsafe {
            set_mode(mode);
            if let Some(flush) = api.flush_menu_themes {
                flush();
            }
        }
    }
}

/// Gives a standard control the native light or dark visual style. Edits and
/// combo boxes use the common-file-dialog class, everything else Explorer's.
pub fn apply_control_theme(control: HWND, dark: bool, input: bool) {
    use windows::Win32::UI::Controls::SetWindowTheme;
    use windows::core::w;
    if let Some(allow) = uxtheme().allow_dark_mode_for_window {
        unsafe {
            allow(control, dark as i32);
        }
    }
    let class = match (dark, input) {
        (true, true) => w!("DarkMode_CFD"),
        (true, false) => w!("DarkMode_Explorer"),
        (false, _) => w!("Explorer"),
    };
    if let Err(error) = unsafe { SetWindowTheme(control, class, windows::core::PCWSTR::null()) } {
        log_once(
            &CONTROL_THEME_LOGGED,
            &format!("SetWindowTheme failed: {error}"),
        );
    }
}
static CONTROL_THEME_LOGGED: AtomicBool = AtomicBool::new(false);

/// The full color set for one theme.
#[derive(Clone, Copy)]
pub struct Tokens {
    /// Solid page background (visible only when the Mica backdrop is unavailable).
    pub page: COLORREF,
    pub card: COLORREF,
    pub stroke: COLORREF,
    pub text: COLORREF,
    pub text_secondary: COLORREF,
    pub text_disabled: COLORREF,
    /// Brand amber for fills: primary buttons, toggles, pills, progress.
    pub accent: COLORREF,
    /// `accent` under the pointer and while pressed.
    pub accent_hover: COLORREF,
    pub accent_pressed: COLORREF,
    /// Readable text color on top of `accent` fills.
    pub accent_text: COLORREF,
    /// Accent for text, glyphs and thin strokes drawn on `page`/`card`;
    /// plain amber is too light for that on the light theme.
    pub accent_ink: COLORREF,
    /// Subtle accent-derived tint for selected/checked surfaces.
    pub accent_tint: COLORREF,
    pub control_fill: COLORREF,
    pub control_hover: COLORREF,
    /// Ghost and toolbar buttons under the pointer, and while pressed.
    pub subtle_hover: COLORREF,
    pub subtle_pressed: COLORREF,
    /// Failure text and glyphs on `page`/`card`.
    pub critical: COLORREF,
    /// Inner stroke of the focus visual; contrasts with `text`, the outer stroke.
    pub focus_inner: COLORREF,
}

/// Converts `#RRGGBB` to the GDI `0x00BBGGRR` layout.
const fn rgb(hex: u32) -> COLORREF {
    COLORREF(((hex & 0xff) << 16) | (hex & 0xff00) | ((hex >> 16) & 0xff))
}

/// Brand amber, identical in both themes.
pub const BRAND_AMBER: COLORREF = rgb(0xF59E0B);
/// `BRAND_AMBER` as B, G, R, A bytes for the overlay's pixel buffers.
pub const BRAND_AMBER_BGRA: [u8; 4] = [0x0B, 0x9E, 0xF5, 0xFF];
/// `BRAND_INK` as B, G, R, A bytes: selection outlines and redaction fill.
pub const BRAND_INK_BGRA: [u8; 4] = [0x13, 0x14, 0x14, 0xFF];
/// Background behind selected characters while editing overlay text.
pub const TEXT_SELECTION: COLORREF = rgb(0xFDE3A7);
/// Brand ink: the warm near-black of the app icon.
pub const BRAND_INK: COLORREF = rgb(0x141413);
const ACCENT_HOVER: COLORREF = rgb(0xF7AD33);
const ACCENT_PRESSED: COLORREF = rgb(0xD98A06);

// Cached theme: 0 = unread, 1 = light, 2 = dark.
static THEME: AtomicU8 = AtomicU8::new(0);
static REGISTRY_FALLBACK_LOGGED: AtomicBool = AtomicBool::new(false);
// One-shot diagnostics flags for the backdrop/DWM helpers below.
static BACKDROP_CHECK_LOGGED: AtomicBool = AtomicBool::new(false);
static DWM_DARK_LOGGED: AtomicBool = AtomicBool::new(false);
static DWM_BACKDROP_LOGGED: AtomicBool = AtomicBool::new(false);

fn read_dword(subkey: &str, value: &str) -> Option<u32> {
    use std::ffi::c_void;
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};

    let subkey: Vec<u16> = subkey.encode_utf16().chain(Some(0)).collect();
    let value: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
    let mut data = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            windows::core::PCWSTR(subkey.as_ptr()),
            windows::core::PCWSTR(value.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut data as *mut u32 as *mut c_void),
            Some(&mut size),
        )
    };
    (status.0 == 0).then_some(data)
}

/// Returns the cached system theme, reading `AppsUseLightTheme` on first use.
pub fn theme() -> Theme {
    match PREFERENCE.load(Ordering::Acquire) {
        1 => return Theme::Light,
        2 => return Theme::Dark,
        _ => {}
    }
    match THEME.load(Ordering::Acquire) {
        1 => Theme::Light,
        2 => Theme::Dark,
        _ => {
            let light = read_dword(
                r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
                "AppsUseLightTheme",
            )
            .map(|value| value != 0)
            .unwrap_or_else(|| {
                log_once(
                    &REGISTRY_FALLBACK_LOGGED,
                    "AppsUseLightTheme is unavailable; using the light theme",
                );
                true
            });
            let known = if light { 1 } else { 2 };
            THEME.store(known, Ordering::Release);
            if light { Theme::Light } else { Theme::Dark }
        }
    }
}

/// The full token set for the current theme.
pub fn tokens() -> Tokens {
    tokens_for(theme())
}

/// The full token set for an explicit theme (used by paint code that must
/// stay consistent for the whole frame even if the system flips mid-paint).
/// Neutrals are the standard Windows 11 Fluent values; only the accent is brand.
pub const fn tokens_for(theme: Theme) -> Tokens {
    match theme {
        Theme::Light => Tokens {
            page: rgb(0xF3F3F3),
            card: rgb(0xFBFBFB),
            stroke: rgb(0xE5E5E5),
            text: rgb(0x1A1A1A),
            text_secondary: rgb(0x5D5D5D),
            text_disabled: rgb(0xA0A0A0),
            accent: BRAND_AMBER,
            accent_hover: ACCENT_HOVER,
            accent_pressed: ACCENT_PRESSED,
            accent_text: BRAND_INK,
            accent_ink: rgb(0xB45309),
            accent_tint: rgb(0xFEF3DC),
            control_fill: rgb(0xFFFFFF),
            control_hover: rgb(0xF9F9F9),
            subtle_hover: rgb(0xF0F0F0),
            subtle_pressed: rgb(0xEAEAEA),
            critical: rgb(0xC42B1C),
            focus_inner: rgb(0xFFFFFF),
        },
        Theme::Dark => Tokens {
            page: rgb(0x202020),
            card: rgb(0x2B2B2B),
            stroke: rgb(0x3A3A3A),
            text: rgb(0xFFFFFF),
            text_secondary: rgb(0xC5C5C5),
            text_disabled: rgb(0x787878),
            accent: BRAND_AMBER,
            accent_hover: ACCENT_HOVER,
            accent_pressed: ACCENT_PRESSED,
            accent_text: BRAND_INK,
            accent_ink: rgb(0xFBBF24),
            accent_tint: rgb(0x3D3220),
            control_fill: rgb(0x2D2D2D),
            control_hover: rgb(0x323232),
            subtle_hover: rgb(0x383838),
            subtle_pressed: rgb(0x333333),
            critical: rgb(0xFF99A4),
            focus_inner: rgb(0x1A1A1A),
        },
    }
}

// ---------------------------------------------------------------------------
// Typography — Segoe UI Variable pixel sizes and weights (GDI takes pixels).
// Body text and semibold section labels, as in Windows 11 dialogs.
// ---------------------------------------------------------------------------
pub const FONT_BODY_PX: i32 = 14;
pub const FONT_WEIGHT_SECTION: i32 = 600;

/// The modern Windows 11 UI face, with a Windows 10 fallback.
///
/// GDI silently substitutes a different face when the requested one is not
/// installed, so the choice is probed once and cached: asking for
/// "Segoe UI Variable Text" on Windows 10 would otherwise fall back to an
/// arbitrary default instead of the classic Segoe UI.
pub fn ui_face() -> &'static str {
    use std::sync::LazyLock;
    static FACE: LazyLock<&'static str> = LazyLock::new(|| {
        const MODERN: &str = "Segoe UI Variable Text";
        const CLASSIC: &str = "Segoe UI";
        if face_installed(MODERN) {
            MODERN
        } else {
            CLASSIC
        }
    });
    *FACE
}

/// Creates a DPI-aware Segoe UI Variable font for native dialogs. The owner
/// must delete the returned GDI handle when its window closes.
pub fn create_ui_font(dpi: u32, pixels: i32, weight: i32) -> windows::Win32::Graphics::Gdi::HFONT {
    use windows::Win32::Graphics::Gdi::{
        CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, DEFAULT_CHARSET, DEFAULT_PITCH,
        OUT_DEFAULT_PRECIS,
    };
    let face: Vec<u16> = ui_face().encode_utf16().chain(Some(0)).collect();
    unsafe {
        CreateFontW(
            -(pixels * dpi as i32 / 96),
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET.0 as u32,
            OUT_DEFAULT_PRECIS.0 as u32,
            CLIP_DEFAULT_PRECIS.0 as u32,
            CLEARTYPE_QUALITY.0 as u32,
            DEFAULT_PITCH.0 as u32,
            windows::core::PCWSTR(face.as_ptr()),
        )
    }
}

/// True when `face` resolves to itself on this system. GDI returns the
/// substituted face name, which is the only reliable installation probe.
pub fn face_installed(face: &str) -> bool {
    use windows::Win32::Graphics::Gdi::{
        CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateCompatibleDC, CreateFontW, DEFAULT_CHARSET,
        DEFAULT_PITCH, DeleteDC, DeleteObject, FW_NORMAL, GetTextFaceW, HGDIOBJ,
        OUT_DEFAULT_PRECIS, SelectObject,
    };
    let name: Vec<u16> = face.encode_utf16().chain(Some(0)).collect();
    unsafe {
        let dc = CreateCompatibleDC(None);
        if dc.is_invalid() {
            return false;
        }
        let font = CreateFontW(
            -12,
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET.0 as u32,
            OUT_DEFAULT_PRECIS.0 as u32,
            CLIP_DEFAULT_PRECIS.0 as u32,
            CLEARTYPE_QUALITY.0 as u32,
            DEFAULT_PITCH.0 as u32,
            windows::core::PCWSTR(name.as_ptr()),
        );
        if font.is_invalid() {
            let _ = DeleteDC(dc);
            return false;
        }
        let previous = SelectObject(dc, HGDIOBJ(font.0));
        let mut buffer = [0u16; 64];
        let length = GetTextFaceW(dc, Some(&mut buffer));
        if !previous.is_invalid() {
            SelectObject(dc, previous);
        }
        let _ = DeleteObject(HGDIOBJ(font.0));
        let _ = DeleteDC(dc);
        if length <= 1 {
            return false;
        }
        let selected = String::from_utf16_lossy(&buffer[..(length as usize - 1).min(buffer.len())]);
        selected.eq_ignore_ascii_case(face)
    }
}

// ---------------------------------------------------------------------------
// Metrics — compact Fluent layout grid, all values in 96-DPI pixels.
// ---------------------------------------------------------------------------
/// Visible corner radius of controls: buttons, edits, toolbar buttons.
pub const RADIUS_CONTROL: i32 = 4;
/// Visible corner radius of overlays: flyouts, panels, tooltips and toasts.
pub const RADIUS_OVERLAY: i32 = 8;

// ---------------------------------------------------------------------------
// Cache control — live theme flips.
// ---------------------------------------------------------------------------

/// Drops the theme cache so the next read hits the registry.
pub fn invalidate_theme_cache() {
    THEME.store(0, Ordering::Release);
}

// ---------------------------------------------------------------------------
// Window backdrop — dark title bars and Mica.
// ---------------------------------------------------------------------------

/// System backdrop type values passed with [`DWMWA_SYSTEMBACKDROP_TYPE`].
/// `BACKDROP_NONE` keeps the solid frame; `BACKDROP_MICA` enables Mica.
pub const BACKDROP_NONE: i32 = 1;
pub const BACKDROP_MICA: i32 = 2;
/// `DWMWA_SYSTEMBACKDROP_TYPE` — Windows 11, build 22621+.
pub const DWMWA_SYSTEMBACKDROP_TYPE: i32 = 38;
/// `DWMWA_USE_IMMERSIVE_DARK_MODE` — Windows 10 2004+, build 18985+.
pub const DWMWA_USE_IMMERSIVE_DARK_MODE: i32 = 20;
/// The same attribute under its pre-2004 number.
const DWMWA_USE_IMMERSIVE_DARK_MODE_BEFORE_2004: i32 = 19;

fn log_once(flag: &AtomicBool, message: &str) {
    if flag
        .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
    {
        crate::diagnostics::record("theme", message);
    }
}

/// True when the OS understands [`DWMWA_SYSTEMBACKDROP_TYPE`] (Windows 11
/// 22H2+, build 22621). A registry read failure assumes "not supported".
pub fn backdrop_supported() -> bool {
    use std::ffi::c_void;
    use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};

    let subkey: Vec<u16> = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let value: Vec<u16> = "CurrentBuildNumber".encode_utf16().chain(Some(0)).collect();
    let mut data = [0u16; 32];
    let mut size = std::mem::size_of_val(&data) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            windows::core::PCWSTR(subkey.as_ptr()),
            windows::core::PCWSTR(value.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(data.as_mut_ptr() as *mut c_void),
            Some(&mut size),
        )
    };
    let build = (status.0 == 0)
        .then(|| {
            let end = data.iter().position(|&c| c == 0).unwrap_or(data.len());
            String::from_utf16(&data[..end])
                .ok()
                .and_then(|text| text.trim().parse::<u32>().ok())
        })
        .flatten();
    match build {
        Some(build) => build >= 22621,
        None => {
            log_once(
                &BACKDROP_CHECK_LOGGED,
                "CurrentBuildNumber is unavailable; assuming no system backdrop",
            );
            false
        }
    }
}

/// One `DwmSetWindowAttribute` call with a 4-byte payload; `false` when DWM
/// rejected the attribute.
unsafe fn set_window_attribute<T>(hwnd: HWND, attribute: i32, value: &T) -> bool {
    use windows::Win32::Graphics::Dwm::{DWMWINDOWATTRIBUTE, DwmSetWindowAttribute};
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWINDOWATTRIBUTE(attribute),
            std::ptr::from_ref(value).cast(),
            std::mem::size_of::<T>() as u32,
        )
        .is_ok()
    }
}

/// Applies the dark title bar and (when requested and supported) the Mica
/// backdrop to `hwnd`. Attribute failures are recorded once through
/// diagnostics instead of being swallowed; the window keeps its default look.
///
/// # Safety
/// `hwnd` must be a valid window handle.
pub unsafe fn apply_window_theme(hwnd: HWND, dark: bool, mica: bool) {
    unsafe {
        // Immersive dark mode is a BOOL (0/1) carried as a u32: attribute 20
        // on modern builds, 19 before Windows 10 2004.
        let dark_value: u32 = if dark { 1 } else { 0 };
        let dark_set = set_window_attribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark_value)
            || set_window_attribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE_BEFORE_2004, &dark_value);
        if !dark_set {
            log_once(
                &DWM_DARK_LOGGED,
                "DwmSetWindowAttribute failed for the immersive dark mode attribute",
            );
        }
        let backdrop = if mica && backdrop_supported() {
            BACKDROP_MICA
        } else {
            BACKDROP_NONE
        };
        if !set_window_attribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &backdrop) {
            log_once(
                &DWM_BACKDROP_LOGGED,
                "DwmSetWindowAttribute failed for the system backdrop attribute",
            );
        }
        if let Some(allow) = uxtheme().allow_dark_mode_for_window {
            allow(hwnd, dark as i32);
        }
        let _ = windows::Win32::UI::WindowsAndMessaging::EnumChildWindows(
            hwnd,
            Some(theme_child),
            windows::Win32::Foundation::LPARAM(dark as isize),
        );
    }
}

/// Native scroll bars, edits, combo and list boxes follow the window theme;
/// owner-drawn buttons and labels paint themselves from the tokens.
unsafe extern "system" fn theme_child(
    child: HWND,
    dark: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::BOOL {
    let mut class = [0u16; 64];
    let length =
        unsafe { windows::Win32::UI::WindowsAndMessaging::GetClassNameW(child, &mut class) };
    let name = String::from_utf16_lossy(&class[..length.max(0) as usize]).to_ascii_lowercase();
    let dark = dark.0 != 0;
    if matches!(name.as_str(), "edit" | "combobox") {
        apply_control_theme(child, dark, true);
    } else if matches!(name.as_str(), "listbox" | "combolbox" | "msctls_trackbar32")
        || name.starts_with("isolmass_")
    {
        // Our own containers carry WS_VSCROLL/WS_HSCROLL bars.
        apply_control_theme(child, dark, false);
    }
    true.into()
}
