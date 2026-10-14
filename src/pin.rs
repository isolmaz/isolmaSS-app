//! Pin to screen: the selection stays on the desktop as a borderless, always-on-top window
//! that the user can move, zoom, copy and close. This module decides what needs no window:
//! where a pin sits and how big it is at a zoom level, how the wheel zooms around the
//! pointer, which keys and menu rows a pin answers to. `window.rs` creates and drives the
//! windows.
use crate::capture::Rect;
use crate::settings::SaveFormat;
use windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE;

mod window;
pub use window::{close_all, show, wait_until_closed};

/// Zoom limits and the levels the wheel steps through, in percent.
const ZOOM_STEPS: [u32; 12] = [25, 33, 50, 67, 75, 100, 125, 150, 200, 250, 300, 400];
/// Wheel units of one notch.
const WHEEL_NOTCH: i32 = 120;
/// Letter key `C`.
const VK_C: u32 = 0x43;

/// The pixels of a pin and where they were on screen.
pub struct PinImage {
    /// Opaque 32-bit BGRA rows, top-down: `width * height * 4` bytes.
    pub pixels: Vec<u8>,
    pub width: i32,
    pub height: i32,
    /// Screen position of the image's top-left corner.
    pub origin: (i32, i32),
    /// What Save as encodes to.
    pub format: SaveFormat,
    pub jpeg_quality: u8,
}

/// Width of the amber border: one pixel at 96 DPI, scaled with the display.
fn border_width(dpi: u32) -> i32 {
    (dpi as i32 / 96).max(1)
}

/// `len` pixels at `zoom` percent, never less than one.
fn scaled(len: i32, zoom: u32) -> i32 {
    (round_div(i64::from(len) * i64::from(zoom), 100) as i32).max(1)
}

/// `numerator / denominator` rounded to the nearest integer, halves up; `denominator > 0`.
fn round_div(numerator: i64, denominator: i64) -> i64 {
    (numerator * 2 + denominator).div_euclid(denominator * 2)
}

/// The zoom level `steps` wheel notches from `zoom` (positive zooms in), kept between the
/// first and last level. Any starting zoom works: a step goes to the next level above or
/// below it.
fn step_zoom(zoom: u32, steps: i32) -> u32 {
    let first = ZOOM_STEPS[0];
    let last = ZOOM_STEPS[ZOOM_STEPS.len() - 1];
    let mut zoom = zoom.clamp(first, last);
    for _ in 0..steps.unsigned_abs() {
        zoom = if steps > 0 {
            ZOOM_STEPS
                .into_iter()
                .find(|&level| level > zoom)
                .unwrap_or(last)
        } else {
            ZOOM_STEPS
                .into_iter()
                .rev()
                .find(|&level| level < zoom)
                .unwrap_or(first)
        };
    }
    zoom
}

/// Whole wheel notches in `delta` after adding what earlier messages left in `remainder`.
/// Wheels and touchpads that report fractions of a notch zoom as steadily as notched ones.
pub fn wheel_steps(remainder: &mut i32, delta: i32) -> i32 {
    *remainder += delta;
    let steps = *remainder / WHEEL_NOTCH;
    *remainder %= WHEEL_NOTCH;
    steps
}

/// Where a pin is and how it is zoomed. The window is the image, scaled, plus the border.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinView {
    /// Size of the image in pixels at 100 %.
    image: (i32, i32),
    /// Screen position of the zoomed image's top-left corner.
    origin: (i32, i32),
    zoom: u32,
    border: i32,
}

impl PinView {
    /// A pin at 100 % over the screen area `selection`, on a display of `dpi`.
    pub fn over(selection: Rect, dpi: u32) -> Self {
        Self {
            image: (selection.width(), selection.height()),
            origin: (selection.left, selection.top),
            zoom: 100,
            border: border_width(dpi),
        }
    }

    /// The pin moved to a display of another `dpi`: only the border changes.
    pub fn set_dpi(&mut self, dpi: u32) {
        self.border = border_width(dpi);
    }

    /// Size of the zoomed image in pixels.
    pub fn image_size(&self) -> (i32, i32) {
        (
            scaled(self.image.0, self.zoom),
            scaled(self.image.1, self.zoom),
        )
    }

    /// Where the zoomed image is painted, in client coordinates.
    pub fn image_rect(&self) -> Rect {
        let (width, height) = self.image_size();
        Rect::new(
            self.border,
            self.border,
            self.border + width,
            self.border + height,
        )
    }

    /// The window on screen: the zoomed image with the border around it.
    pub fn window_rect(&self) -> Rect {
        let (width, height) = self.image_size();
        Rect::new(
            self.origin.0 - self.border,
            self.origin.1 - self.border,
            self.origin.0 + width + self.border,
            self.origin.1 + height + self.border,
        )
    }

    /// Zooms `steps` wheel notches (positive zooms in) around `cursor`, a screen position:
    /// the image point under the cursor stays under it. False when the zoom is already at
    /// its limit and nothing changed.
    pub fn zoom_by(&mut self, steps: i32, cursor: (i32, i32)) -> bool {
        let previous = self.zoom;
        let next = step_zoom(previous, steps);
        if next == previous {
            return false;
        }
        let keep_under_cursor = |origin: i32, cursor: i32| {
            let offset = i64::from(origin - cursor) * i64::from(next);
            cursor + round_div(offset, i64::from(previous)) as i32
        };
        self.origin = (
            keep_under_cursor(self.origin.0, cursor.0),
            keep_under_cursor(self.origin.1, cursor.1),
        );
        self.zoom = next;
        true
    }

    /// The distance from `cursor` to the image's corner, kept while the pin is dragged.
    pub fn grab_offset(&self, cursor: (i32, i32)) -> (i32, i32) {
        (self.origin.0 - cursor.0, self.origin.1 - cursor.1)
    }

    /// Moves the pin so that `cursor` stays `offset` away from the image's corner.
    pub fn move_with(&mut self, cursor: (i32, i32), offset: (i32, i32)) {
        self.origin = (cursor.0 + offset.0, cursor.1 + offset.1);
    }
}

/// What a pin can be told to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinCommand {
    Copy,
    SaveAs,
    Close,
}

/// The right-click menu, top to bottom. A row's command id is its position plus one.
const MENU: [PinCommand; 3] = [PinCommand::Copy, PinCommand::SaveAs, PinCommand::Close];

/// The command a key press runs in a pin: Esc closes it, Ctrl+C copies the image.
pub fn command_for_key(vk: u32, ctrl: bool) -> Option<PinCommand> {
    if vk == u32::from(VK_ESCAPE.0) {
        Some(PinCommand::Close)
    } else if vk == VK_C && ctrl {
        Some(PinCommand::Copy)
    } else {
        None
    }
}

/// The text of the right-click menu rows, in menu order; a tab separates the shortcut.
pub fn menu_labels() -> [String; 3] {
    MENU.map(|command| match command {
        PinCommand::Copy => format!("{}\tCtrl+C", crate::i18n::t("Copy")),
        PinCommand::SaveAs => crate::i18n::t("Save as…").to_owned(),
        PinCommand::Close => format!("{}\tEsc", crate::i18n::t("Close")),
    })
}

/// The command behind the id `TrackPopupMenuEx` returned; `None` when the menu was
/// dismissed (id 0) or the id names no row.
pub fn command_for_id(id: i32) -> Option<PinCommand> {
    let index = usize::try_from(id).ok()?.checked_sub(1)?;
    MENU.get(index).copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::test_env::Sandbox;

    fn view() -> PinView {
        PinView::over(Rect::new(100, 50, 300, 250), 96)
    }

    #[test]
    fn a_new_pin_covers_the_selection_exactly_with_the_border_around_it() {
        let pin = view();
        assert_eq!(pin.window_rect(), Rect::new(99, 49, 301, 251));
        assert_eq!(pin.image_size(), (200, 200));
        assert_eq!(pin.image_rect(), Rect::new(1, 1, 201, 201));
        // Inside the window, the image sits where the selection was on screen.
        let window = pin.window_rect();
        assert_eq!(
            (
                window.left + pin.image_rect().left,
                window.top + pin.image_rect().top
            ),
            (100, 50)
        );
    }

    #[test]
    fn the_border_follows_the_display_scale_in_whole_pixels() {
        let border = |dpi| PinView::over(Rect::new(0, 0, 10, 10), dpi).border;
        assert_eq!(border(96), 1);
        assert_eq!(border(120), 1);
        assert_eq!(border(144), 1);
        assert_eq!(border(192), 2);
        assert_eq!(border(288), 3);
        assert_eq!(border(48), 1, "never thinner than a pixel");
    }

    #[test]
    fn a_pin_on_a_scaled_display_keeps_the_image_where_it_was() {
        let pin = PinView::over(Rect::new(-1920, 40, -1520, 340), 192);
        assert_eq!(pin.window_rect(), Rect::new(-1922, 38, -1518, 342));
        assert_eq!(pin.image_rect(), Rect::new(2, 2, 402, 302));
    }

    #[test]
    fn moving_to_another_display_changes_only_the_border() {
        let mut pin = view();
        pin.set_dpi(192);
        assert_eq!(pin.border, 2);
        assert_eq!(pin.window_rect(), Rect::new(98, 48, 302, 252));
        assert_eq!(pin.image_size(), (200, 200));
    }

    #[test]
    fn the_wheel_steps_through_the_levels_between_25_and_400_percent() {
        assert_eq!(step_zoom(100, 1), 125);
        assert_eq!(step_zoom(100, 3), 200);
        assert_eq!(step_zoom(100, -1), 75);
        assert_eq!(step_zoom(100, -2), 67);
        assert_eq!(step_zoom(100, 0), 100);
        assert_eq!(step_zoom(100, 40), 400, "the top level stops the zoom");
        assert_eq!(step_zoom(100, -40), 25, "the bottom level stops the zoom");
        assert_eq!(step_zoom(400, 1), 400);
        assert_eq!(step_zoom(25, -1), 25);
        // A zoom between two levels goes to the neighbouring one.
        assert_eq!(step_zoom(110, 1), 125);
        assert_eq!(step_zoom(110, -1), 100);
        // A zoom outside the range is pulled in first.
        assert_eq!(step_zoom(1000, -1), 300);
        assert_eq!(step_zoom(5, 1), 33);
    }

    #[test]
    fn every_level_can_be_left_and_returned_to() {
        for level in ZOOM_STEPS {
            let up = step_zoom(level, 1);
            let down = step_zoom(level, -1);
            if level < 400 {
                assert_eq!(step_zoom(up, -1), level, "{level} up and back");
            }
            if level > 25 {
                assert_eq!(step_zoom(down, 1), level, "{level} down and back");
            }
        }
    }

    #[test]
    fn the_image_scales_with_the_zoom_and_stays_at_least_one_pixel() {
        assert_eq!(scaled(200, 100), 200);
        assert_eq!(scaled(200, 25), 50);
        assert_eq!(scaled(200, 400), 800);
        assert_eq!(scaled(10, 33), 3, "3.3 rounds down");
        assert_eq!(scaled(10, 67), 7, "6.7 rounds up");
        assert_eq!(scaled(2, 25), 1, "0.5 rounds up");
        assert_eq!(scaled(1, 25), 1, "a sliver never vanishes");
    }

    #[test]
    fn zooming_keeps_the_image_point_under_the_cursor_fixed() {
        let mut pin = view();
        // Image point (50, 100) is under the cursor at (150, 150).
        assert!(pin.zoom_by(3, (150, 150)));
        assert_eq!(pin.zoom, 200);
        assert_eq!(pin.image_size(), (400, 400));
        let image = pin.window_rect();
        let under_cursor = (
            (150 - (image.left + pin.border)) * 100 / pin.zoom as i32,
            (150 - (image.top + pin.border)) * 100 / pin.zoom as i32,
        );
        assert_eq!(under_cursor, (50, 100));
        assert_eq!(image, Rect::new(49, -51, 451, 351));
    }

    #[test]
    fn zooming_out_around_the_cursor_returns_to_the_start() {
        let mut pin = view();
        let start = pin.window_rect();
        assert!(pin.zoom_by(1, (180, 90)));
        assert!(pin.zoom_by(-1, (180, 90)));
        assert_eq!(pin.window_rect(), start);
        assert_eq!(pin.zoom, 100);
    }

    #[test]
    fn zooming_around_a_cursor_outside_the_image_moves_the_image_away_from_it() {
        let mut pin = view();
        // The cursor is 60 px left of the image; at 200 % the image is 120 px away.
        assert!(pin.zoom_by(3, (40, 50)));
        let window = pin.window_rect();
        assert_eq!(
            (window.left + pin.border, window.top + pin.border),
            (160, 50)
        );
    }

    #[test]
    fn zooming_at_the_limit_changes_nothing() {
        let mut pin = view();
        assert!(pin.zoom_by(100, (150, 150)));
        assert_eq!(pin.zoom, 400);
        let at_limit = pin.window_rect();
        assert!(!pin.zoom_by(1, (10, 10)));
        assert_eq!(pin.window_rect(), at_limit);
        assert!(pin.zoom_by(-100, (150, 150)));
        assert_eq!(pin.zoom, 25);
        assert!(!pin.zoom_by(-1, (150, 150)));
        assert!(!pin.zoom_by(0, (150, 150)));
    }

    #[test]
    fn a_zoomed_out_image_shrinks_towards_the_cursor() {
        let mut pin = view();
        assert!(pin.zoom_by(-100, (100, 50)));
        assert_eq!(pin.image_size(), (50, 50));
        // The cursor was on the image's corner, which therefore stays put.
        assert_eq!(pin.window_rect(), Rect::new(99, 49, 151, 101));
    }

    #[test]
    fn dragging_keeps_the_grabbed_point_under_the_cursor() {
        let mut pin = view();
        let offset = pin.grab_offset((130, 70));
        assert_eq!(offset, (-30, -20));
        pin.move_with((500, 400), offset);
        assert_eq!(pin.window_rect(), Rect::new(469, 379, 671, 581));
        pin.move_with((130, 70), offset);
        assert_eq!(pin.window_rect(), view().window_rect());
    }

    #[test]
    fn a_zoomed_pin_is_moved_by_the_same_distance() {
        let mut pin = view();
        assert!(pin.zoom_by(3, (150, 150)));
        let before = pin.window_rect();
        let offset = pin.grab_offset((200, 200));
        pin.move_with((210, 190), offset);
        let after = pin.window_rect();
        assert_eq!(
            (after.width(), after.height()),
            (before.width(), before.height())
        );
        assert_eq!((after.left, after.top), (before.left + 10, before.top - 10));
    }

    #[test]
    fn wheel_deltas_accumulate_into_whole_notches() {
        let mut remainder = 0;
        assert_eq!(wheel_steps(&mut remainder, 120), 1);
        assert_eq!(wheel_steps(&mut remainder, -240), -2);
        assert_eq!(remainder, 0);
        // A wheel that reports 30 units at a time needs four messages per notch.
        for _ in 0..3 {
            assert_eq!(wheel_steps(&mut remainder, 30), 0);
        }
        assert_eq!(wheel_steps(&mut remainder, 30), 1);
        assert_eq!(remainder, 0);
        assert_eq!(wheel_steps(&mut remainder, -50), 0);
        assert_eq!(wheel_steps(&mut remainder, -100), -1);
        assert_eq!(remainder, -30, "the rest carries over");
    }

    #[test]
    fn escape_closes_and_ctrl_c_copies() {
        assert_eq!(command_for_key(27, false), Some(PinCommand::Close));
        assert_eq!(command_for_key(VK_C, true), Some(PinCommand::Copy));
        assert_eq!(command_for_key(VK_C, false), None, "a plain C does nothing");
        assert_eq!(command_for_key(0x53, true), None);
        assert_eq!(command_for_key(13, false), None);
    }

    #[test]
    fn the_menu_lists_copy_save_as_and_close_with_their_shortcuts() {
        let _sandbox = Sandbox::new("pin_menu");
        assert_eq!(
            menu_labels(),
            ["Copy\tCtrl+C", "Save as…", "Close\tEsc"].map(String::from)
        );
    }

    #[test]
    fn the_menu_is_translated() {
        let _sandbox = Sandbox::new("pin_menu_tr");
        crate::i18n::set_preference(crate::i18n::LanguagePreference::Turkish);
        assert_eq!(
            menu_labels(),
            ["Kopyala\tCtrl+C", "Farklı kaydet…", "Kapat\tEsc"].map(String::from)
        );
    }

    #[test]
    fn a_menu_id_selects_the_row_behind_it() {
        assert_eq!(command_for_id(1), Some(PinCommand::Copy));
        assert_eq!(command_for_id(2), Some(PinCommand::SaveAs));
        assert_eq!(command_for_id(3), Some(PinCommand::Close));
        assert_eq!(
            command_for_id(0),
            None,
            "TrackPopupMenuEx returns 0 when dismissed"
        );
        assert_eq!(command_for_id(4), None);
        assert_eq!(command_for_id(-1), None);
    }
}
