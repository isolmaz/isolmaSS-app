use super::*;
use crate::drawing::canvas::GdiQuota;
use std::ffi::c_void;
use windows::Win32::Foundation::HINSTANCE;
use windows::Win32::Graphics::Dwm::{DWMWINDOWATTRIBUTE, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, DeleteDC, DeleteObject, GetObjectW, GetTextFaceW, HGDIOBJ, LOGFONTW,
    SelectObject,
};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ,
    REG_VALUE_TYPE, RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegSetValueExW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, WINDOW_EX_STYLE, WINDOW_STYLE,
    WNDCLASSW, WS_CHILD, WS_POPUP,
};
use windows::core::{PCSTR, PCWSTR, w};

/// Pins the theme preference for one test and restores the follow-Windows default.
pub(crate) struct ForcedTheme;

impl ForcedTheme {
    pub(crate) fn new(preference: ThemePreference) -> Self {
        set_preference(preference);
        Self
    }
}

impl Drop for ForcedTheme {
    fn drop(&mut self) {
        set_preference(ThemePreference::System);
        invalidate_theme_cache();
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

#[test]
fn color_setting_change_matches_only_the_immersive_color_area() {
    let area = wide("ImmersiveColorSet");
    assert!(is_color_setting_change(LPARAM(area.as_ptr() as isize)));
    for other in [
        "",
        "ImmersiveColor",
        "ImmersiveColorSets",
        "intl",
        "WindowMetrics",
    ] {
        let text = wide(other);
        assert!(
            !is_color_setting_change(LPARAM(text.as_ptr() as isize)),
            "{other:?}"
        );
    }
    assert!(!is_color_setting_change(LPARAM(0)));
}

// ---------------------------------------------------------------------------
// Tokens
// ---------------------------------------------------------------------------

fn luminance(color: COLORREF) -> f64 {
    let channel = |value: u32| {
        let value = f64::from(value & 0xff) / 255.0;
        if value <= 0.03928 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(color.0) + 0.7152 * channel(color.0 >> 8) + 0.0722 * channel(color.0 >> 16)
}

/// WCAG contrast ratio between two colours.
fn contrast(a: COLORREF, b: COLORREF) -> f64 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

const BOTH: [Theme; 2] = [Theme::Light, Theme::Dark];

#[test]
fn text_and_accent_ink_stay_readable_on_every_surface_in_both_themes() {
    for theme in BOTH {
        let tokens = tokens_for(theme);
        for surface in [tokens.page, tokens.card] {
            for (name, ink) in [
                ("text", tokens.text),
                ("text_secondary", tokens.text_secondary),
                ("accent_ink", tokens.accent_ink),
                ("critical", tokens.critical),
            ] {
                let ratio = contrast(ink, surface);
                assert!(ratio >= 4.5, "{theme:?} {name}: {ratio:.2}");
            }
            // Disabled text is exempt from WCAG but still has to be visible, and the
            // three text levels keep their order.
            assert!(contrast(tokens.text, surface) > contrast(tokens.text_secondary, surface));
            assert!(
                contrast(tokens.text_secondary, surface) > contrast(tokens.text_disabled, surface)
            );
            assert!(contrast(tokens.text_disabled, surface) >= 2.0, "{theme:?}");
        }
        for fill in [tokens.accent, tokens.accent_hover, tokens.accent_pressed] {
            assert!(contrast(tokens.accent_text, fill) >= 4.5, "{theme:?}");
        }
        assert!(
            contrast(tokens.focus_inner, tokens.text) >= 7.0,
            "{theme:?}"
        );
    }
}

#[test]
fn plain_amber_is_too_pale_for_text_on_the_light_theme_which_is_why_accent_ink_exists() {
    let light = tokens_for(Theme::Light);
    assert!(contrast(BRAND_AMBER, light.card) < 3.0);
    assert!(contrast(light.accent_ink, light.card) >= 4.5);
}

#[test]
fn accent_fills_are_one_brand_in_both_themes_and_get_darker_when_pressed() {
    let (light, dark) = (tokens_for(Theme::Light), tokens_for(Theme::Dark));
    assert_eq!(light.accent, BRAND_AMBER);
    assert_eq!(light.accent_text, BRAND_INK);
    for (a, b) in [
        (light.accent, dark.accent),
        (light.accent_hover, dark.accent_hover),
        (light.accent_pressed, dark.accent_pressed),
        (light.accent_text, dark.accent_text),
    ] {
        assert_eq!(a, b);
    }
    assert!(luminance(light.accent_hover) > luminance(light.accent));
    assert!(luminance(light.accent) > luminance(light.accent_pressed));
}

#[test]
fn neutral_surfaces_are_distinct_and_the_themes_mirror_each_other() {
    for theme in BOTH {
        let tokens = tokens_for(theme);
        for (a, b) in [
            (tokens.card, tokens.stroke),
            (tokens.card, tokens.accent_tint),
            (tokens.control_fill, tokens.control_hover),
            (tokens.subtle_hover, tokens.subtle_pressed),
            (tokens.card, tokens.page),
        ] {
            assert_ne!(a, b, "{theme:?}");
        }
        assert!(luminance(tokens.card) > luminance(tokens.page), "{theme:?}");
    }
    let (light, dark) = (tokens_for(Theme::Light), tokens_for(Theme::Dark));
    assert!(luminance(light.page) > luminance(light.text));
    assert!(luminance(dark.page) < luminance(dark.text));
    assert!(luminance(light.page) > luminance(dark.page));
}

#[test]
fn rgb_converts_hex_to_the_gdi_byte_order() {
    assert_eq!(rgb(0x112233), COLORREF(0x0033_2211));
    assert_eq!(rgb(0xFF0000), COLORREF(0x0000_00FF));
}

#[test]
fn bgra_constants_match_the_colorref_brand_colors() {
    let bgra = |color: COLORREF| {
        [
            (color.0 >> 16) as u8,
            (color.0 >> 8) as u8,
            color.0 as u8,
            0xFF,
        ]
    };
    assert_eq!(BRAND_AMBER_BGRA, bgra(BRAND_AMBER));
    assert_eq!(BRAND_INK_BGRA, bgra(BRAND_INK));
}

// ---------------------------------------------------------------------------
// Preference, cache and native app mode
// ---------------------------------------------------------------------------

#[test]
fn a_forced_preference_beats_the_cache_and_system_follows_the_registry_once() {
    let _state = ForcedTheme::new(ThemePreference::Light);
    THEME.store(2, Ordering::Release);
    assert_eq!(theme(), Theme::Light);
    set_preference(ThemePreference::Dark);
    THEME.store(1, Ordering::Release);
    assert_eq!(theme(), Theme::Dark);
    assert_eq!(tokens().page, tokens_for(Theme::Dark).page);

    set_preference(ThemePreference::System);
    THEME.store(2, Ordering::Release);
    assert_eq!(theme(), Theme::Dark);
    THEME.store(1, Ordering::Release);
    assert_eq!(theme(), Theme::Light);
    assert_eq!(tokens().page, tokens_for(Theme::Light).page);

    invalidate_theme_cache();
    assert_eq!(THEME.load(Ordering::Acquire), 0);
    let light = theme() == Theme::Light;
    assert_eq!(light, apps_use_light_theme(PERSONALIZE_KEY));
    assert_eq!(THEME.load(Ordering::Acquire), if light { 1 } else { 2 });
}

/// The native app mode, read through `SetPreferredAppMode`'s previous-value result.
fn app_mode() -> i32 {
    let set = uxtheme()
        .set_preferred_app_mode
        .expect("SetPreferredAppMode");
    let previous = unsafe { set(1) };
    unsafe { set(previous) };
    previous
}

const ALLOW_DARK: i32 = 1;
const FORCE_DARK: i32 = 2;
const FORCE_LIGHT: i32 = 3;

#[test]
fn the_preference_selects_the_native_control_mode() {
    let _state = ForcedTheme::new(ThemePreference::Dark);
    assert_eq!(app_mode(), FORCE_DARK);
    set_preference(ThemePreference::Light);
    assert_eq!(app_mode(), FORCE_LIGHT);
    set_preference(ThemePreference::System);
    assert_eq!(app_mode(), ALLOW_DARK);
}

#[test]
fn native_menus_refresh_only_while_following_windows() {
    let set = uxtheme()
        .set_preferred_app_mode
        .expect("SetPreferredAppMode");
    let _state = ForcedTheme::new(ThemePreference::Dark);
    unsafe { set(FORCE_LIGHT) };
    refresh_native_menus();
    assert_eq!(app_mode(), FORCE_LIGHT, "a forced theme keeps its menus");

    set_preference(ThemePreference::System);
    unsafe { set(FORCE_LIGHT) };
    refresh_native_menus();
    assert_eq!(app_mode(), ALLOW_DARK, "System follows the Windows policy");
}

#[test]
fn the_uxtheme_entry_points_resolve_on_this_windows() {
    let api = load_uxtheme(w!("uxtheme.dll"));
    assert!(api.set_preferred_app_mode.is_some());
    assert!(api.allow_dark_mode_for_window.is_some());
    assert!(api.flush_menu_themes.is_some());
    assert!(api.refresh_immersive_color_policy_state.is_some());
}

#[test]
fn a_missing_uxtheme_leaves_every_entry_point_unset() {
    let api = load_uxtheme(w!("isolmass-no-such-library.dll"));
    assert!(api.set_preferred_app_mode.is_none());
    assert!(api.allow_dark_mode_for_window.is_none());
    assert!(api.flush_menu_themes.is_none());
    assert!(api.refresh_immersive_color_policy_state.is_none());
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// A throwaway key under `HKEY_CURRENT_USER`, deleted with everything in it on drop.
struct TestKey(String);

impl TestKey {
    fn new(name: &str) -> Self {
        Self(format!(
            r"Software\isolmaSS-test-theme-{}-{name}",
            std::process::id()
        ))
    }

    fn set(&self, value: &str, kind: REG_VALUE_TYPE, data: &[u8]) {
        let (path, name) = (wide(&self.0), wide(value));
        unsafe {
            let mut key = HKEY::default();
            let created = RegCreateKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(path.as_ptr()),
                0,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                None,
                &mut key,
                None,
            );
            assert_eq!(created.0, 0, "creating {}", self.0);
            let stored = RegSetValueExW(key, PCWSTR(name.as_ptr()), 0, kind, Some(data));
            assert_eq!(stored.0, 0, "writing {value}");
            let _ = RegCloseKey(key);
        }
    }

    fn set_dword(&self, value: &str, data: u32) {
        self.set(value, REG_DWORD, &data.to_le_bytes());
    }

    fn set_string(&self, value: &str, data: &str) {
        let bytes: Vec<u8> = wide(data).into_iter().flat_map(u16::to_le_bytes).collect();
        self.set(value, REG_SZ, &bytes);
    }
}

impl Drop for TestKey {
    fn drop(&mut self) {
        let path = wide(&self.0);
        unsafe {
            let _ = RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(path.as_ptr()));
        }
    }
}

#[test]
fn only_dword_values_that_exist_are_read() {
    let key = TestKey::new("dword");
    key.set_dword("Flag", 7);
    key.set_string("Text", "7");
    assert_eq!(read_dword(&key.0, "Flag"), Some(7));
    assert_eq!(read_dword(&key.0, "Text"), None, "a string is not a DWORD");
    assert_eq!(read_dword(&key.0, "Missing"), None);
    assert_eq!(read_dword(&format!(r"{}\child", key.0), "Flag"), None);
}

#[test]
fn the_light_theme_value_picks_the_theme_and_a_missing_value_means_light() {
    let key = TestKey::new("apps");
    key.set_dword("AppsUseLightTheme", 0);
    assert!(!apps_use_light_theme(&key.0));
    key.set_dword("AppsUseLightTheme", 1);
    assert!(apps_use_light_theme(&key.0));

    REGISTRY_FALLBACK_LOGGED.store(false, Ordering::Relaxed);
    assert!(apps_use_light_theme(
        r"Software\isolmaSS-test-never-created"
    ));
    assert!(
        REGISTRY_FALLBACK_LOGGED.load(Ordering::Relaxed),
        "the fallback is reported"
    );
    REGISTRY_FALLBACK_LOGGED.store(false, Ordering::Relaxed);
}

#[test]
fn the_windows_build_number_gates_the_system_backdrop() {
    let build = read_build_number().expect("CurrentBuildNumber");
    assert!(build >= 10240, "Windows 10 or newer: {build}");
    assert_eq!(backdrop_supported(), build >= 22621);
    assert!(backdrop_supported_for(Some(22621)));
    assert!(!backdrop_supported_for(Some(22620)));

    BACKDROP_CHECK_LOGGED.store(false, Ordering::Relaxed);
    assert!(!backdrop_supported_for(None));
    assert!(BACKDROP_CHECK_LOGGED.load(Ordering::Relaxed));
    BACKDROP_CHECK_LOGGED.store(false, Ordering::Relaxed);
}

#[test]
fn a_diagnostic_flag_reports_once() {
    let flag = AtomicBool::new(false);
    log_once(&flag, "first report");
    assert!(flag.load(Ordering::Relaxed));
    log_once(&flag, "second report");
    assert!(flag.load(Ordering::Relaxed));
}

// ---------------------------------------------------------------------------
// Fonts
// ---------------------------------------------------------------------------

#[test]
fn the_ui_face_is_installed_and_the_probe_prefers_the_modern_face() {
    assert!(face_installed(ui_face()));
    assert_eq!(
        pick_face("Modern", "Classic", |face| face == "Modern"),
        "Modern"
    );
    assert_eq!(pick_face("Modern", "Classic", |_| false), "Classic");
    assert_eq!(
        pick_face("Modern", "Classic", |face| face == "Classic"),
        "Classic",
        "only the preferred face is probed"
    );
}

#[test]
fn face_installed_matches_installed_faces_only() {
    assert!(face_installed("Segoe UI"));
    assert!(face_installed("segoe ui"), "case does not matter");
    assert!(!face_installed("isolmaSS No Such Face"));
    assert!(!face_installed(""), "no name is no face");
}

#[test]
fn face_installed_is_false_when_gdi_runs_out_of_handles() {
    let mut quota = GdiQuota::exhaust();
    assert!(!face_installed("Segoe UI"), "no device context");
    quota.release_one();
    assert!(!face_installed("Segoe UI"), "no handle left for the font");
    drop(quota);
    assert!(face_installed("Segoe UI"), "recovers once handles are free");
}

#[test]
fn ui_fonts_scale_with_dpi_and_carry_weight_and_face() {
    let font = create_ui_font(192, 14, 600);
    let mut logfont = LOGFONTW::default();
    let copied = unsafe {
        GetObjectW(
            HGDIOBJ(font.0),
            size_of::<LOGFONTW>() as i32,
            Some((&mut logfont as *mut LOGFONTW).cast::<c_void>()),
        )
    };
    assert!(copied > 0);
    assert_eq!(logfont.lfHeight, -28, "14 px at 192 DPI is 28 px");
    assert_eq!(logfont.lfWeight, 600);
    let end = logfont.lfFaceName.iter().position(|&unit| unit == 0);
    let name = String::from_utf16_lossy(&logfont.lfFaceName[..end.unwrap_or(32)]);
    assert_eq!(name, ui_face());

    // GDI resolves the same face when the font is selected.
    unsafe {
        let hdc = CreateCompatibleDC(None);
        let previous = SelectObject(hdc, HGDIOBJ(font.0));
        let mut buffer = [0u16; 64];
        let length = GetTextFaceW(hdc, Some(&mut buffer));
        assert_eq!(
            String::from_utf16_lossy(&buffer[..length as usize - 1]),
            ui_face()
        );
        SelectObject(hdc, previous);
        let _ = DeleteDC(hdc);
        let _ = DeleteObject(HGDIOBJ(font.0));
    }
}

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------

const OWN_CLASS: PCWSTR = w!("isolmaSS_ThemeTest");

unsafe extern "system" fn test_window_proc(
    hwnd: HWND,
    message: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

/// A never-shown top-level window, destroyed with its children on drop.
struct TestWindow(HWND);

impl TestWindow {
    fn popup() -> Self {
        let class = WNDCLASSW {
            lpfnWndProc: Some(test_window_proc),
            lpszClassName: OWN_CLASS,
            ..Default::default()
        };
        // The class outlives the test: later tests register it again and get the
        // "already exists" answer.
        let _ = unsafe { RegisterClassW(&class) };
        Self(create(w!("STATIC"), HWND::default(), WS_POPUP))
    }

    fn child(&self, class: PCWSTR) -> HWND {
        create(class, self.0, WS_CHILD)
    }
}

impl Drop for TestWindow {
    fn drop(&mut self) {
        let _ = unsafe { DestroyWindow(self.0) };
    }
}

fn create(class: PCWSTR, parent: HWND, style: WINDOW_STYLE) -> HWND {
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            w!(""),
            style,
            0,
            0,
            80,
            24,
            parent,
            None,
            HINSTANCE::default(),
            None,
        )
    }
    .expect("test window")
}

/// uxtheme's `IsDarkModeAllowedForWindow` (ordinal 137): what `AllowDarkModeForWindow`
/// recorded for the window. It only reports `true` while the app mode allows dark.
fn dark_mode_allowed(window: HWND) -> bool {
    type IsDarkModeAllowedForWindow = unsafe extern "system" fn(HWND) -> u8;
    let module = unsafe { LoadLibraryExW(w!("uxtheme.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) }
        .expect("uxtheme.dll");
    let entry = unsafe { GetProcAddress(module, PCSTR(137 as *const u8)) }
        .expect("IsDarkModeAllowedForWindow");
    let is_allowed: IsDarkModeAllowedForWindow = unsafe { std::mem::transmute(entry) };
    unsafe { is_allowed(window) != 0 }
}

fn dwm_attribute(window: HWND, attribute: i32) -> i32 {
    let mut value = -1i32;
    unsafe {
        DwmGetWindowAttribute(
            window,
            DWMWINDOWATTRIBUTE(attribute),
            (&mut value as *mut i32).cast::<c_void>(),
            size_of::<i32>() as u32,
        )
    }
    .expect("DWM window attribute");
    value
}

#[test]
fn control_theme_records_dark_mode_per_window_and_reports_failures_once() {
    let _state = ForcedTheme::new(ThemePreference::Dark);
    CONTROL_THEME_LOGGED.store(false, Ordering::Relaxed);
    let parent = TestWindow::popup();
    let edit = parent.child(w!("EDIT"));
    let list = parent.child(w!("LISTBOX"));
    assert!(!dark_mode_allowed(edit));

    apply_control_theme(edit, true, true);
    apply_control_theme(list, true, false);
    assert!(dark_mode_allowed(edit));
    assert!(dark_mode_allowed(list));
    apply_control_theme(edit, false, true);
    assert!(!dark_mode_allowed(edit));
    assert!(dark_mode_allowed(list), "controls are themed independently");
    assert!(
        !CONTROL_THEME_LOGGED.load(Ordering::Relaxed),
        "SetWindowTheme succeeded"
    );

    apply_control_theme(HWND(0x1234 as *mut c_void), true, false);
    assert!(
        CONTROL_THEME_LOGGED.load(Ordering::Relaxed),
        "a window that cannot be themed is reported"
    );
    CONTROL_THEME_LOGGED.store(false, Ordering::Relaxed);
}

#[test]
fn child_theming_covers_native_controls_and_own_containers_only() {
    let _state = ForcedTheme::new(ThemePreference::Dark);
    let parent = TestWindow::popup();
    let themed = [
        parent.child(w!("EDIT")),
        parent.child(w!("COMBOBOX")),
        parent.child(w!("LISTBOX")),
        parent.child(OWN_CLASS),
    ];
    let label = parent.child(w!("STATIC"));
    for child in themed.into_iter().chain([label]) {
        assert!(!dark_mode_allowed(child));
        assert!(unsafe { theme_child(child, LPARAM(1)) }.as_bool());
    }
    for child in themed {
        assert!(dark_mode_allowed(child));
    }
    assert!(
        !dark_mode_allowed(label),
        "owner-drawn controls paint themselves"
    );

    for child in themed {
        assert!(unsafe { theme_child(child, LPARAM(0)) }.as_bool());
        assert!(!dark_mode_allowed(child));
    }
}

#[test]
fn window_theme_sets_the_title_bar_backdrop_and_children() {
    let _state = ForcedTheme::new(ThemePreference::Dark);
    let window = TestWindow::popup();
    let edit = window.child(w!("EDIT"));
    let own = window.child(OWN_CLASS);
    let label = window.child(w!("STATIC"));

    unsafe { apply_window_theme(window.0, true, false) };
    assert_eq!(dwm_attribute(window.0, DWMWA_USE_IMMERSIVE_DARK_MODE), 1);
    assert_eq!(
        dwm_attribute(window.0, DWMWA_SYSTEMBACKDROP_TYPE),
        BACKDROP_NONE
    );
    assert!(dark_mode_allowed(window.0));
    assert!(dark_mode_allowed(edit));
    assert!(dark_mode_allowed(own));
    assert!(!dark_mode_allowed(label));

    unsafe { apply_window_theme(window.0, false, true) };
    assert_eq!(dwm_attribute(window.0, DWMWA_USE_IMMERSIVE_DARK_MODE), 0);
    let mica = if backdrop_supported() {
        BACKDROP_MICA
    } else {
        BACKDROP_NONE
    };
    assert_eq!(dwm_attribute(window.0, DWMWA_SYSTEMBACKDROP_TYPE), mica);
    assert!(!dark_mode_allowed(window.0));
    assert!(!dark_mode_allowed(edit));
}

#[test]
fn window_theme_reports_attributes_dwm_rejects() {
    DWM_DARK_LOGGED.store(false, Ordering::Relaxed);
    DWM_BACKDROP_LOGGED.store(false, Ordering::Relaxed);
    unsafe { apply_window_theme(HWND(0x1234 as *mut c_void), true, true) };
    assert!(DWM_DARK_LOGGED.load(Ordering::Relaxed));
    assert!(DWM_BACKDROP_LOGGED.load(Ordering::Relaxed));
    DWM_DARK_LOGGED.store(false, Ordering::Relaxed);
    DWM_BACKDROP_LOGGED.store(false, Ordering::Relaxed);
}
