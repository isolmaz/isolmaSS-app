use crate::annotation::{ToolKind, bgra_to_colorref};
use crate::capture::Rect;
use windows::Win32::Foundation::{COLORREF, HWND, POINT};
use windows::Win32::Graphics::Gdi::HDC;
/// Corner radius (96-dpi px, scaled at each use site) for toolbar buttons and
/// color swatches. Panel radii use `crate::theme::RADIUS_CARD` instead.
const RADIUS_CONTROL: i32 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolbarAction {
    Undo,
    Redo,
    Save,
    Copy,
    Upload,
    Settings,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolbarItem {
    Tool(ToolKind),
    MoreTools,
    Action(ToolbarAction),
    /// The active color as a single swatch; a click opens the color dialog.
    Color,
    /// Compact line-width button; a click opens the wheel below it.
    Thickness,
    /// One value row of the open line-width wheel.
    ThicknessOption(i32),
}

#[derive(Debug, Clone)]
pub struct ToolbarButton {
    pub item: ToolbarItem,
    pub rect: Rect,
    pub is_enabled: bool,
    /// Mirrors the toolbar's active tool/color/thickness for menu check marks.
    pub is_checked: bool,
}

pub struct Toolbar {
    pub tool_bounds: Rect,
    pub action_bounds: Rect,
    /// The open line-width wheel, drawn under (or above) the thickness button.
    pub wheel_bounds: Option<Rect>,
    pub buttons: Vec<ToolbarButton>,
    pub hovered_item: Option<ToolbarItem>,
    pub active_color: [u8; 4],
    pub active_thickness: i32,
    pub dpi: u32,
    expanded: bool,
    viewport: Rect,
    wheel_row: i32,
}

impl Toolbar {
    pub const TOOL_BUTTON: i32 = 32;
    pub const ACTION_HEIGHT: i32 = 42;
    /// Air between the panels and the edge of the work area.
    pub const OUTER_PAD: i32 = 8;
    /// Air between the selection and the panels when they sit outside it.
    pub const GAP: i32 = 8;
    /// Air between the selection edge and the panels when they sit inside it.
    pub const INNER_PAD: i32 = 10;
    /// Rows shown by the line-width wheel; the middle row is the active value.
    pub const WHEEL_ROWS: i32 = 5;
    pub const WHEEL_ROW: i32 = 26;

    /// Builds a floating tool rail and action strip. Each panel flips to the
    /// opposite edge when the selection reaches a monitor boundary.
    #[allow(clippy::too_many_arguments)]
    pub fn layout(
        selection: &Rect,
        viewport: Rect,
        active_tool: ToolKind,
        active_color: [u8; 4],
        active_thickness: i32,
        expanded: bool,
        show_color: bool,
        show_thickness: bool,
        thickness_open: bool,
        can_undo: bool,
        can_redo: bool,
        dpi: u32,
    ) -> Self {
        let scale = |value: i32| value * dpi as i32 / 96;
        let outer_pad = scale(Self::OUTER_PAD);
        let gap = scale(Self::GAP);
        let inner_pad = scale(Self::INNER_PAD);
        let pad = scale(6);
        let tool_button = scale(Self::TOOL_BUTTON);
        let tool_items = [
            (ToolbarItem::Tool(ToolKind::Select), true),
            (ToolbarItem::Tool(ToolKind::Rectangle), true),
            (ToolbarItem::Tool(ToolKind::Arrow), true),
            (ToolbarItem::Tool(ToolKind::Pen), true),
            (ToolbarItem::MoreTools, true),
            (ToolbarItem::Tool(ToolKind::Highlight), true),
            (ToolbarItem::Tool(ToolKind::Text), true),
            (ToolbarItem::Tool(ToolKind::Step), true),
            (ToolbarItem::Tool(ToolKind::Blur), true),
            (ToolbarItem::Tool(ToolKind::Redact), true),
            (ToolbarItem::Action(ToolbarAction::Undo), can_undo),
            (ToolbarItem::Action(ToolbarAction::Redo), can_redo),
        ];
        let secondary = |item: ToolbarItem| {
            matches!(
                item,
                ToolbarItem::Tool(
                    ToolKind::Highlight
                        | ToolKind::Text
                        | ToolKind::Step
                        | ToolKind::Blur
                        | ToolKind::Redact
                )
            )
        };
        let visible_count = tool_items.len() as i32 - if expanded { 0 } else { 5 };
        let tool_width = tool_button + pad * 2;
        let tool_height =
            pad * 2 + visible_count * tool_button + (visible_count - 1) * scale(2) + scale(12);

        let action_button = scale(32);
        let action_gap = scale(3);
        let thickness_width = scale(52);
        let section_gap = scale(8);
        let style_width = if show_color {
            action_button + section_gap
        } else {
            0
        } + if show_thickness {
            thickness_width + section_gap
        } else {
            0
        };
        let action_width = pad * 2 + style_width + 5 * action_button + 4 * action_gap;
        let action_height = scale(Self::ACTION_HEIGHT);
        let safe = Rect::new(
            viewport.left + outer_pad,
            viewport.top + outer_pad,
            // Never invert the insets on a degenerate (impossible tiny) work area.
            (viewport.right - outer_pad).max(viewport.left + outer_pad),
            (viewport.bottom - outer_pad).max(viewport.top + outer_pad),
        );
        let contains = |outer: Rect, inner: Rect| {
            inner.left >= outer.left
                && inner.top >= outer.top
                && inner.right <= outer.right
                && inner.bottom <= outer.bottom
        };
        let make_l = |right: bool, below: bool, shared_x: i32, shared_y: i32| {
            let tool = match (right, below) {
                (true, true) => Rect::new(
                    shared_x - tool_width,
                    shared_y - tool_height,
                    shared_x,
                    shared_y,
                ),
                (false, true) => Rect::new(
                    shared_x,
                    shared_y - tool_height,
                    shared_x + tool_width,
                    shared_y,
                ),
                (true, false) => Rect::new(
                    shared_x - tool_width,
                    shared_y,
                    shared_x,
                    shared_y + tool_height,
                ),
                (false, false) => Rect::new(
                    shared_x,
                    shared_y,
                    shared_x + tool_width,
                    shared_y + tool_height,
                ),
            };
            let action = match (right, below) {
                (true, true) => Rect::new(
                    shared_x - action_width,
                    shared_y,
                    shared_x,
                    shared_y + action_height,
                ),
                (false, true) => Rect::new(
                    shared_x,
                    shared_y,
                    shared_x + action_width,
                    shared_y + action_height,
                ),
                (true, false) => Rect::new(
                    shared_x - action_width,
                    shared_y - action_height,
                    shared_x,
                    shared_y,
                ),
                (false, false) => Rect::new(
                    shared_x,
                    shared_y - action_height,
                    shared_x + action_width,
                    shared_y,
                ),
            };
            (tool, action)
        };

        let outside = [
            (
                true,
                true,
                selection.right + gap + tool_width,
                selection.bottom + gap,
            ),
            (
                false,
                true,
                selection.left - gap - tool_width,
                selection.bottom + gap,
            ),
            (
                true,
                false,
                selection.right + gap + tool_width,
                selection.top - gap,
            ),
            (
                false,
                false,
                selection.left - gap - tool_width,
                selection.top - gap,
            ),
        ]
        .into_iter()
        .map(|(right, below, x, y)| make_l(right, below, x, y))
        .find(|(tool, action)| contains(safe, *tool) && contains(safe, *action));

        let inside = if selection.width() >= action_width + inner_pad * 2
            && selection.height() >= tool_height + action_height + inner_pad * 2
        {
            let shared_x = selection.right - inner_pad;
            let shared_y = selection.bottom - inner_pad - action_height;
            let candidate = make_l(true, true, shared_x, shared_y);
            (contains(safe, candidate.0) && contains(safe, candidate.1)).then_some(candidate)
        } else {
            None
        };

        let (tool_bounds, action_bounds) = outside.or(inside).unwrap_or_else(|| {
            // A tiny selection cannot contain the editor. Keep the complete L bounded to
            // the selected monitor's work area rather than leaking onto another monitor.
            make_l(true, true, safe.right, safe.bottom - action_height)
        });

        let mut buttons = Vec::with_capacity(
            visible_count as usize + usize::from(show_color) + usize::from(show_thickness) + 5,
        );
        let mut y = tool_bounds.bottom - pad - tool_button;
        for (item, enabled) in tool_items
            .into_iter()
            .filter(|(item, _)| expanded || !secondary(*item))
        {
            let is_checked = matches!(item, ToolbarItem::Tool(tool) if tool == active_tool)
                || matches!(item, ToolbarItem::MoreTools)
                    && (expanded || secondary(ToolbarItem::Tool(active_tool)));
            buttons.push(ToolbarButton {
                item,
                rect: Rect::new(
                    tool_bounds.left + pad,
                    y,
                    tool_bounds.left + pad + tool_button,
                    y + tool_button,
                ),
                is_enabled: enabled,
                is_checked,
            });
            y -= tool_button
                + scale(2)
                + if matches!(
                    item,
                    ToolbarItem::MoreTools | ToolbarItem::Action(ToolbarAction::Undo)
                ) {
                    scale(6)
                } else {
                    0
                };
        }
        let item_y = action_bounds.top + (action_height - action_button) / 2;
        let mut x = action_bounds.left + pad;
        if show_color {
            buttons.push(ToolbarButton {
                item: ToolbarItem::Color,
                rect: Rect::new(x, item_y, x + action_button, item_y + action_button),
                is_enabled: true,
                is_checked: false,
            });
            x += action_button + section_gap;
        }
        if show_thickness {
            buttons.push(ToolbarButton {
                item: ToolbarItem::Thickness,
                rect: Rect::new(x, item_y, x + thickness_width, item_y + action_button),
                is_enabled: true,
                is_checked: thickness_open,
            });
            x += thickness_width + section_gap;
        }
        for action in [
            ToolbarAction::Save,
            ToolbarAction::Copy,
            ToolbarAction::Upload,
            ToolbarAction::Settings,
            ToolbarAction::Cancel,
        ] {
            buttons.push(ToolbarButton {
                item: ToolbarItem::Action(action),
                rect: Rect::new(x, item_y, x + action_button, item_y + action_button),
                is_enabled: true,
                is_checked: false,
            });
            x += action_button + action_gap;
        }
        let mut toolbar = Self {
            tool_bounds,
            action_bounds,
            wheel_bounds: None,
            buttons,
            hovered_item: None,
            active_color,
            active_thickness,
            dpi,
            expanded,
            viewport,
            wheel_row: scale(Self::WHEEL_ROW),
        };
        if tool_height + action_height > safe.height() || action_width > safe.width() {
            toolbar.arrange_grid(safe);
        }
        if thickness_open {
            toolbar.add_wheel(safe);
        }
        toolbar
    }

    /// A grid keeps every command reachable on short/high-DPI work areas.
    fn arrange_grid(&mut self, safe: Rect) {
        let count = (self.buttons.len() as i32).max(1);
        let mut cell = (32 * self.dpi as i32 / 96).max(8);
        loop {
            let cols = (safe.width() / cell).max(1);
            let rows = (count + cols - 1) / cols;
            if (rows * cell <= safe.height() && cols * cell <= safe.width()) || cell <= 8 {
                break;
            }
            cell -= 1;
        }
        let cols = (safe.width() / cell).max(1).min(count);
        let rows = (count + cols - 1) / cols;
        // Clamp into the safe area: an undersized work area must still yield
        // in-bounds rectangles instead of buttons above or beyond the viewport.
        let bounds = Rect::new(
            safe.left,
            (safe.bottom - rows * cell).max(safe.top),
            (safe.left + cols * cell).min(safe.right).max(safe.left),
            safe.bottom,
        );
        let clamp = |value: i32, low: i32, high: i32| value.clamp(low, high.max(low));
        for (index, button) in self.buttons.iter_mut().enumerate() {
            let x = bounds.left + index as i32 % cols * cell;
            let y = bounds.top + index as i32 / cols * cell;
            let left = clamp(x + 2, bounds.left, bounds.right);
            let right = clamp(x + cell - 2, left, bounds.right);
            let top = clamp(y + 2, bounds.top, bounds.bottom);
            let bottom = clamp(y + cell - 2, top, bounds.bottom);
            button.rect = Rect::new(left, top, right, bottom);
        }
        self.tool_bounds = bounds;
        self.action_bounds = bounds;
        self.dpi = (cell * 96 / 32).max(24) as u32;
        self.wheel_row = (cell * Self::WHEEL_ROW / 32).max(12);
    }

    /// Opens the line-width wheel under the thickness button, or above it when
    /// the work area has no room below. Rows run from thin (top) to thick.
    fn add_wheel(&mut self, safe: Rect) {
        let Some(anchor) = self
            .buttons
            .iter()
            .find(|button| button.item == ToolbarItem::Thickness)
            .map(|button| button.rect)
        else {
            return;
        };
        let row = self.wheel_row;
        let pad = (4 * self.dpi as i32 / 96).max(1);
        let height = Self::WHEEL_ROWS * row + pad * 2;
        let width = anchor.width().max(row * 2);
        let top = if anchor.bottom + pad + height <= safe.bottom {
            anchor.bottom + pad
        } else {
            anchor.top - pad - height
        };
        let top = top.clamp(safe.top, (safe.bottom - height).max(safe.top));
        let left = anchor
            .left
            .clamp(safe.left, (safe.right - width).max(safe.left));
        let bounds = Rect::new(left, top, left + width, top + height);
        let center = Self::WHEEL_ROWS / 2;
        for index in 0..Self::WHEEL_ROWS {
            let value = self.active_thickness + index - center;
            if !(1..=64).contains(&value) {
                continue;
            }
            let y = bounds.top + pad + index * row;
            self.buttons.push(ToolbarButton {
                item: ToolbarItem::ThicknessOption(value),
                rect: Rect::new(bounds.left + pad, y, bounds.right - pad, y + row),
                is_enabled: true,
                is_checked: value == self.active_thickness,
            });
        }
        self.wheel_bounds = Some(bounds);
    }

    /// Height of one wheel row, used to turn a vertical drag into steps.
    pub fn wheel_row_height(&self) -> i32 {
        self.wheel_row.max(1)
    }

    /// Checks if a point is within the toolbar bounds.
    pub fn contains_point(&self, pt: (i32, i32)) -> bool {
        self.tool_bounds.contains(pt.0, pt.1)
            || self.action_bounds.contains(pt.0, pt.1)
            || self
                .wheel_bounds
                .is_some_and(|wheel| wheel.contains(pt.0, pt.1))
    }

    /// Hit-tests for a button under cursor.
    pub fn hit_test(&self, pt: (i32, i32)) -> Option<ToolbarItem> {
        if !self.contains_point(pt) {
            return None;
        }
        // The wheel is pushed last and drawn on top, so it wins any overlap.
        for btn in self.buttons.iter().rev() {
            if btn.rect.contains(pt.0, pt.1) && btn.is_enabled {
                return Some(btn.item);
            }
        }
        None
    }

    /// Updates hover state based on mouse coordinates.
    pub fn update_hover(&mut self, pt: (i32, i32)) -> bool {
        let new_hover = self
            .buttons
            .iter()
            .rev()
            .find(|button| button.rect.contains(pt.0, pt.1))
            .map(|button| button.item);
        if self.hovered_item != new_hover {
            self.hovered_item = new_hover;
            true
        } else {
            false
        }
    }

    /// Flat, high-contrast controls share the settings window's visual language.
    pub fn render(&self, hdc: HDC, thickness_input: Option<&str>) {
        use crate::drawing::{icon, label, rounded};
        let scale = |value: i32| (value * self.dpi as i32 / 96).max(1);
        // Native Fluent chrome from the shared token set (light/dark aware).
        let tokens = crate::theme::tokens();
        let card = tokens.card;
        let stroke = tokens.stroke;
        let text = tokens.text;
        let muted = tokens.text_disabled;
        let accent = tokens.accent;
        let tint = tokens.accent_tint;
        for (index, panel) in [self.tool_bounds, self.action_bounds]
            .into_iter()
            .enumerate()
        {
            if index == 1 && self.tool_bounds == self.action_bounds {
                continue;
            }
            rounded(
                hdc,
                panel.inflate(1, 1),
                scale(crate::theme::RADIUS_CARD),
                stroke,
                stroke,
            );
            rounded(hdc, panel, scale(crate::theme::RADIUS_CARD), card, stroke);
        }
        let mut wheel_drawn = false;
        for button in &self.buttons {
            if !wheel_drawn
                && matches!(button.item, ToolbarItem::ThicknessOption(_))
                && let Some(wheel) = self.wheel_bounds
            {
                wheel_drawn = true;
                rounded(
                    hdc,
                    wheel.inflate(1, 1),
                    scale(crate::theme::RADIUS_CARD),
                    stroke,
                    stroke,
                );
                rounded(hdc, wheel, scale(crate::theme::RADIUS_CARD), card, stroke);
            }
            let hovered = self.hovered_item == Some(button.item) && button.is_enabled;
            let selected = button.is_checked
                && matches!(
                    button.item,
                    ToolbarItem::Tool(_)
                        | ToolbarItem::MoreTools
                        | ToolbarItem::Thickness
                        | ToolbarItem::ThicknessOption(_)
                );
            let primary = button.item == ToolbarItem::Action(ToolbarAction::Copy);
            let fill = if primary {
                accent
            } else if selected || hovered {
                tint
            } else {
                card
            };
            let ink = if !button.is_enabled {
                muted
            } else if primary {
                tokens.accent_text
            } else if selected {
                accent
            } else {
                text
            };
            rounded(
                hdc,
                button.rect,
                scale(RADIUS_CONTROL),
                fill,
                if selected { accent } else { fill },
            );
            match button.item {
                ToolbarItem::Color => {
                    let swatch = button.rect.inflate(-scale(6), -scale(6));
                    rounded(
                        hdc,
                        swatch,
                        scale(RADIUS_CONTROL - 2),
                        bgra_to_colorref(self.active_color),
                        stroke,
                    );
                }
                ToolbarItem::Thickness => {
                    // A dot sized like the stroke, then the value in px.
                    let diameter = scale(self.active_thickness.clamp(2, 14));
                    let dot_center = button.rect.left + scale(15);
                    let cy = (button.rect.top + button.rect.bottom) / 2;
                    rounded(
                        hdc,
                        Rect::new(
                            dot_center - diameter / 2,
                            cy - diameter / 2,
                            dot_center - diameter / 2 + diameter,
                            cy - diameter / 2 + diameter,
                        ),
                        diameter,
                        ink,
                        ink,
                    );
                    let value = match thickness_input {
                        Some(digits) if !digits.is_empty() => digits.to_owned(),
                        _ => self.active_thickness.to_string(),
                    };
                    label(
                        hdc,
                        Rect::new(
                            button.rect.left + scale(26),
                            button.rect.top,
                            button.rect.right - scale(4),
                            button.rect.bottom,
                        ),
                        &value,
                        scale(12),
                        ink,
                        true,
                    );
                }
                ToolbarItem::ThicknessOption(value) => {
                    let ink = if button.is_checked {
                        accent
                    } else if (value - self.active_thickness).abs() > 1 {
                        muted
                    } else {
                        text
                    };
                    label(
                        hdc,
                        button.rect,
                        &value.to_string(),
                        scale(if button.is_checked { 13 } else { 11 }),
                        ink,
                        true,
                    );
                }
                ToolbarItem::Tool(tool) => {
                    draw_tool_icon(hdc, button.rect, tool, ink, card, self.dpi)
                }
                ToolbarItem::MoreTools => {
                    icon(
                        hdc,
                        button.rect,
                        if self.expanded { 0xe70d } else { 0xe70e },
                        scale(16),
                        ink,
                        true,
                    );
                }
                ToolbarItem::Action(action) => {
                    let codepoint = match action {
                        ToolbarAction::Undo => 0xe7a7,
                        ToolbarAction::Redo => 0xe7a6,
                        ToolbarAction::Save => 0xe74e,
                        ToolbarAction::Copy => 0xe8c8,
                        ToolbarAction::Upload => 0xe753,
                        ToolbarAction::Settings => 0xe713,
                        ToolbarAction::Cancel => 0xe711,
                    };
                    icon(hdc, button.rect, codepoint, scale(16), ink, true);
                }
            }
        }
        if let Some(item) = self.hovered_item
            && let Some(button) = self.buttons.iter().find(|button| button.item == item)
        {
            let tip = match item {
                ToolbarItem::Tool(ToolKind::Select) => crate::i18n::t("Select and move"),
                ToolbarItem::Tool(ToolKind::Rectangle) => crate::i18n::t("Frame"),
                ToolbarItem::Tool(ToolKind::Arrow) => crate::i18n::t("Arrow"),
                ToolbarItem::Tool(ToolKind::Pen) => crate::i18n::t("Pen"),
                ToolbarItem::Tool(ToolKind::Highlight) => crate::i18n::t("Highlighter"),
                ToolbarItem::Tool(ToolKind::Step) => crate::i18n::t("Numbered steps"),
                ToolbarItem::Tool(ToolKind::Text) => crate::i18n::t("Text"),
                ToolbarItem::Tool(ToolKind::Blur) => crate::i18n::t("Blur"),
                ToolbarItem::Tool(ToolKind::Redact) => crate::i18n::t("Redact"),
                ToolbarItem::MoreTools if self.expanded => crate::i18n::t("Hide more tools"),
                ToolbarItem::MoreTools => crate::i18n::t("More tools"),
                ToolbarItem::Action(ToolbarAction::Undo) => crate::i18n::t("Undo"),
                ToolbarItem::Action(ToolbarAction::Redo) => crate::i18n::t("Redo"),
                ToolbarItem::Action(ToolbarAction::Save) => crate::i18n::t("Save"),
                ToolbarItem::Action(ToolbarAction::Copy) => crate::i18n::t("Copy"),
                ToolbarItem::Action(ToolbarAction::Upload) => crate::i18n::t("Upload"),
                ToolbarItem::Action(ToolbarAction::Settings) => crate::i18n::t("Settings"),
                ToolbarItem::Action(ToolbarAction::Cancel) => crate::i18n::t("Cancel"),
                ToolbarItem::Color => crate::i18n::t("Color"),
                ToolbarItem::Thickness => crate::i18n::t("Line width"),
                // The wheel rows already show their value.
                ToolbarItem::ThicknessOption(_) => return,
            };
            let margin = scale(4);
            // `label` draws with a negative (character-height) size; measure the same way.
            let width = (crate::drawing::measure_text(tip, -scale(13)).0 + scale(20))
                .min((self.viewport.width() - margin * 2).max(1));
            let height = scale(30);
            let x = button.rect.left.clamp(
                self.viewport.left + margin,
                (self.viewport.right - width - margin).max(self.viewport.left + margin),
            );
            let y = (button.rect.top - height - margin).clamp(
                self.viewport.top + margin,
                (self.viewport.bottom - height - margin).max(self.viewport.top + margin),
            );
            let bounds = Rect::new(x, y, x + width, y + height);
            rounded(
                hdc,
                bounds,
                scale(crate::theme::RADIUS_CARD),
                tokens.card,
                tokens.stroke,
            );
            label(hdc, bounds, tip, scale(13), tokens.text, true);
        }
    }
}

/// Draw tool-specific geometry at a shared stroke weight; no misleading font glyph aliases.
fn draw_tool_icon(
    hdc: HDC,
    rect: Rect,
    tool: ToolKind,
    ink: COLORREF,
    surface: COLORREF,
    dpi: u32,
) {
    use crate::drawing::{icon, label, rounded};
    let unit = |value: i32| (value * dpi as i32 / 96).max(1);
    if tool == ToolKind::Step {
        let cx = (rect.left + rect.right) / 2;
        let cy = (rect.top + rect.bottom) / 2;
        rounded(
            hdc,
            Rect::new(cx - unit(9), cy - unit(9), cx + unit(9), cy + unit(9)),
            unit(18),
            ink,
            ink,
        );
        label(hdc, rect, "1", unit(11), surface, true);
        return;
    }
    // Codepoints shared by Segoe Fluent Icons (Windows 11) and Segoe MDL2 (Windows 10).
    let glyph = match tool {
        ToolKind::Select => 0xe8b0,
        ToolKind::Rectangle => 0xe739,
        ToolKind::Arrow => 0xe8ad,
        ToolKind::Pen => 0xe70f,
        ToolKind::Highlight => 0xe7e6,
        ToolKind::Text => 0xe8d2,
        ToolKind::Blur => 0xe727,
        ToolKind::Redact => 0xe73b,
        ToolKind::Step => unreachable!("drawn above"),
    };
    icon(hdc, rect, glyph, unit(16), ink, true);
}

/// Presents the toolbar's commands as a native popup menu for keyboard and screen-reader
/// access (F10 / Apps key). Returns the chosen command; the overlay is never mutated.
pub fn show_command_menu(
    owner: HWND,
    buttons: &[ToolbarButton],
) -> windows::core::Result<Option<ToolbarItem>> {
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, MF_CHECKED, MF_GRAYED, MF_STRING,
        TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenuEx,
    };
    use windows::core::PCWSTR;
    if buttons.is_empty() {
        return Ok(None);
    }
    // The menu runs a modal loop; suspend overlay hotkeys for its lifetime.
    let _suspension = crate::hotkey::OverlayInputSuspension::new();
    struct MenuGuard(windows::Win32::UI::WindowsAndMessaging::HMENU);
    impl Drop for MenuGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = DestroyMenu(self.0);
            }
        }
    }
    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(Some(0)).collect()
    }
    fn group(item: ToolbarItem) -> &'static str {
        match item {
            ToolbarItem::Tool(
                ToolKind::Highlight
                | ToolKind::Text
                | ToolKind::Step
                | ToolKind::Blur
                | ToolKind::Redact,
            ) => crate::i18n::t("More tools"),
            ToolbarItem::Tool(_) | ToolbarItem::MoreTools => crate::i18n::t("Tools"),
            ToolbarItem::Action(ToolbarAction::Undo | ToolbarAction::Redo) => {
                crate::i18n::t("History")
            }
            ToolbarItem::Color => crate::i18n::t("Color"),
            ToolbarItem::Thickness | ToolbarItem::ThicknessOption(_) => {
                crate::i18n::t("Line width")
            }
            ToolbarItem::Action(_) => crate::i18n::t("Commands"),
        }
    }
    fn label(item: ToolbarItem) -> String {
        match item {
            ToolbarItem::Tool(ToolKind::Select) => crate::i18n::t("Select/move").to_owned(),
            ToolbarItem::Tool(ToolKind::Rectangle) => crate::i18n::t("Frame").to_owned(),
            ToolbarItem::Tool(ToolKind::Arrow) => crate::i18n::t("Arrow").to_owned(),
            ToolbarItem::Tool(ToolKind::Pen) => crate::i18n::t("Pen").to_owned(),
            ToolbarItem::Tool(ToolKind::Highlight) => crate::i18n::t("Highlighter").to_owned(),
            ToolbarItem::Tool(ToolKind::Step) => crate::i18n::t("Numbered step").to_owned(),
            ToolbarItem::Tool(ToolKind::Text) => crate::i18n::t("Text").to_owned(),
            ToolbarItem::Tool(ToolKind::Blur) => {
                crate::i18n::t("Blur (not secure redaction)").to_owned()
            }
            ToolbarItem::Tool(ToolKind::Redact) => crate::i18n::t("Redact (opaque)").to_owned(),
            ToolbarItem::MoreTools => crate::i18n::t("Show or hide extra tools").to_owned(),
            ToolbarItem::Action(ToolbarAction::Undo) => crate::i18n::t("Undo").to_owned(),
            ToolbarItem::Action(ToolbarAction::Redo) => crate::i18n::t("Redo").to_owned(),
            ToolbarItem::Action(ToolbarAction::Save) => {
                crate::i18n::t("Save screenshot").to_owned()
            }
            ToolbarItem::Action(ToolbarAction::Copy) => {
                crate::i18n::t("Copy to clipboard").to_owned()
            }
            ToolbarItem::Action(ToolbarAction::Upload) => {
                crate::i18n::t("Upload and copy link").to_owned()
            }
            ToolbarItem::Action(ToolbarAction::Settings) => crate::i18n::t("Settings").to_owned(),
            ToolbarItem::Action(ToolbarAction::Cancel) => crate::i18n::t("Cancel").to_owned(),
            ToolbarItem::Color => crate::i18n::t("Choose color...").to_owned(),
            ToolbarItem::Thickness => crate::i18n::t("Line width...").to_owned(),
            ToolbarItem::ThicknessOption(value) => format!("{value} px"),
        }
    }

    let menu = MenuGuard(unsafe { CreatePopupMenu()? });
    let mut mapping: Vec<ToolbarItem> = Vec::with_capacity(buttons.len());
    let mut heading: Option<&'static str> = None;
    for button in buttons {
        let group = group(button.item);
        if heading != Some(group) {
            heading = Some(group);
            let text = wide(group);
            unsafe {
                AppendMenuW(menu.0, MF_STRING | MF_GRAYED, 0, PCWSTR(text.as_ptr()))?;
            }
        }
        let text = wide(&label(button.item));
        let mut flags = MF_STRING;
        if !button.is_enabled {
            flags |= MF_GRAYED;
        }
        if button.is_checked {
            flags |= MF_CHECKED;
        }
        unsafe {
            AppendMenuW(menu.0, flags, mapping.len() + 1, PCWSTR(text.as_ptr()))?;
        }
        mapping.push(button.item);
    }
    let mut point = POINT::default();
    if unsafe { GetCursorPos(&mut point) }.is_err() {
        return Ok(None);
    }
    let command = unsafe {
        TrackPopupMenuEx(
            menu.0,
            (TPM_RETURNCMD | TPM_RIGHTBUTTON).0,
            point.x,
            point.y,
            owner,
            None,
        )
    };
    if command.0 <= 0 {
        return Ok(None);
    }
    Ok(mapping.get(command.0 as usize - 1).copied())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::PRESET_COLORS;

    fn layout_with_wheel(selection: Rect, viewport: Rect, thickness_open: bool) -> Toolbar {
        Toolbar::layout(
            &selection,
            viewport,
            ToolKind::Rectangle,
            PRESET_COLORS[0],
            3,
            false,
            true,
            true,
            thickness_open,
            true,
            true,
            96,
        )
    }

    fn layout(selection: Rect, viewport: Rect) -> Toolbar {
        layout_with_wheel(selection, viewport, false)
    }

    #[test]
    fn style_controls_are_one_swatch_and_one_compact_button() {
        let toolbar = layout(Rect::new(200, 100, 800, 600), Rect::new(0, 0, 1920, 1040));
        let count = |wanted: fn(ToolbarItem) -> bool| {
            toolbar
                .buttons
                .iter()
                .filter(|button| wanted(button.item))
                .count()
        };
        assert_eq!(count(|item| item == ToolbarItem::Color), 1);
        assert_eq!(count(|item| item == ToolbarItem::Thickness), 1);
        assert_eq!(
            count(|item| matches!(item, ToolbarItem::ThicknessOption(_))),
            0
        );
        assert!(toolbar.wheel_bounds.is_none());
        assert!(
            toolbar.action_bounds.width() <= 320,
            "{:?}",
            toolbar.action_bounds
        );
    }

    #[test]
    fn open_wheel_sits_below_the_thickness_button_and_centers_the_value() {
        let toolbar = layout_with_wheel(
            Rect::new(200, 100, 800, 600),
            Rect::new(0, 0, 1920, 1040),
            true,
        );
        let anchor = toolbar
            .buttons
            .iter()
            .find(|button| button.item == ToolbarItem::Thickness)
            .unwrap()
            .rect;
        let wheel = toolbar.wheel_bounds.expect("wheel open");
        assert!(wheel.top >= anchor.bottom, "{wheel:?} {anchor:?}");
        let values: Vec<i32> = toolbar
            .buttons
            .iter()
            .filter_map(|button| match button.item {
                ToolbarItem::ThicknessOption(value) => Some(value),
                _ => None,
            })
            .collect();
        assert_eq!(values, vec![1, 2, 3, 4, 5]);
        let center = toolbar
            .buttons
            .iter()
            .find(|button| button.item == ToolbarItem::ThicknessOption(3))
            .unwrap();
        assert!(center.is_checked);
        let mid = (
            (center.rect.left + center.rect.right) / 2,
            (center.rect.top + center.rect.bottom) / 2,
        );
        assert_eq!(toolbar.hit_test(mid), Some(ToolbarItem::ThicknessOption(3)));
    }

    #[test]
    fn wheel_flips_above_when_the_work_area_ends_below() {
        let viewport = Rect::new(0, 0, 1920, 1040);
        let toolbar = layout_with_wheel(Rect::new(200, 100, 800, 1000), viewport, true);
        let wheel = toolbar.wheel_bounds.expect("wheel open");
        assert_in_viewport(wheel, viewport);
    }

    fn assert_in_viewport(rect: Rect, viewport: Rect) {
        assert!(
            rect.left >= viewport.left && rect.top >= viewport.top,
            "{rect:?}"
        );
        assert!(
            rect.right <= viewport.right && rect.bottom <= viewport.bottom,
            "{rect:?}"
        );
    }

    fn assert_exact_l_corner(toolbar: &Toolbar) {
        let tool_corners = [
            (toolbar.tool_bounds.left, toolbar.tool_bounds.top),
            (toolbar.tool_bounds.right, toolbar.tool_bounds.top),
            (toolbar.tool_bounds.left, toolbar.tool_bounds.bottom),
            (toolbar.tool_bounds.right, toolbar.tool_bounds.bottom),
        ];
        let action_corners = [
            (toolbar.action_bounds.left, toolbar.action_bounds.top),
            (toolbar.action_bounds.right, toolbar.action_bounds.top),
            (toolbar.action_bounds.left, toolbar.action_bounds.bottom),
            (toolbar.action_bounds.right, toolbar.action_bounds.bottom),
        ];
        let shared = tool_corners
            .into_iter()
            .filter(|corner| action_corners.contains(corner))
            .count();
        assert_eq!(
            shared, 1,
            "tool={:?}, action={:?}",
            toolbar.tool_bounds, toolbar.action_bounds
        );
    }

    #[test]
    fn normal_region_uses_padded_outside_bottom_right_l() {
        let selection = Rect::new(200, 100, 800, 600);
        let viewport = Rect::new(0, 0, 1920, 1040);
        let toolbar = layout(selection, viewport);
        assert_eq!(toolbar.tool_bounds.left, selection.right + Toolbar::GAP);
        assert_eq!(toolbar.action_bounds.top, selection.bottom + Toolbar::GAP);
        assert_exact_l_corner(&toolbar);
        assert_in_viewport(toolbar.tool_bounds, viewport);
        assert_in_viewport(toolbar.action_bounds, viewport);
    }

    #[test]
    fn selection_on_second_monitor_never_uses_neighbor() {
        let viewport = Rect::new(1920, 0, 3840, 1040);
        let toolbar = layout(Rect::new(1940, 120, 2520, 680), viewport);
        assert!(toolbar.tool_bounds.left >= viewport.left + 8);
        assert!(toolbar.action_bounds.left >= viewport.left + 8);
        assert_in_viewport(toolbar.tool_bounds, viewport);
        assert_in_viewport(toolbar.action_bounds, viewport);
        assert_exact_l_corner(&toolbar);
    }

    #[test]
    fn full_monitor_selection_uses_padded_inside_bottom_right_l() {
        let viewport = Rect::new(0, 0, 1920, 1040);
        let toolbar = layout(viewport, viewport);
        assert_eq!(
            toolbar.tool_bounds.right,
            viewport.right - Toolbar::INNER_PAD
        );
        assert_eq!(
            toolbar.action_bounds.right,
            viewport.right - Toolbar::INNER_PAD
        );
        assert_eq!(
            toolbar.action_bounds.bottom,
            viewport.bottom - Toolbar::INNER_PAD
        );
        assert_exact_l_corner(&toolbar);
        assert_in_viewport(toolbar.tool_bounds, viewport);
        assert_in_viewport(toolbar.action_bounds, viewport);
    }

    #[test]
    fn tiny_selection_fallback_is_complete_and_monitor_bounded() {
        let viewport = Rect::new(1920, 40, 3200, 900);
        let toolbar = layout(Rect::new(2500, 400, 2510, 410), viewport);
        assert_in_viewport(toolbar.tool_bounds, viewport);
        assert_in_viewport(toolbar.action_bounds, viewport);
        assert_exact_l_corner(&toolbar);
        assert!(toolbar.buttons.iter().all(|button| {
            button.rect.left >= toolbar.tool_bounds.left.min(toolbar.action_bounds.left)
                && button.rect.right <= toolbar.tool_bounds.right.max(toolbar.action_bounds.right)
        }));
    }
    #[test]
    fn high_dpi_short_monitor_keeps_every_command_clickable() {
        for (width, height, dpi) in [
            (1366, 728, 144),
            (1920, 1040, 192),
            (640, 440, 192),
            (320, 240, 96),
        ] {
            let viewport = Rect::new(-width, 0, 0, height);
            let toolbar = Toolbar::layout(
                &Rect::new(-40, 10, -32, 18),
                viewport,
                ToolKind::Text,
                PRESET_COLORS[0],
                2,
                false,
                true,
                true,
                true,
                true,
                true,
                dpi,
            );
            assert_in_viewport(toolbar.tool_bounds, viewport);
            assert_in_viewport(toolbar.action_bounds, viewport);
            for button in &toolbar.buttons {
                assert_in_viewport(button.rect, viewport);
                assert_eq!(
                    toolbar.hit_test((
                        (button.rect.left + button.rect.right) / 2,
                        (button.rect.top + button.rect.bottom) / 2
                    )),
                    Some(button.item)
                );
            }
        }
    }
}
