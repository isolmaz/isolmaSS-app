//! Layout edge cases, pixel-level rendering and the command menu of the toolbar.
use super::*;
use crate::drawing::canvas::{Canvas, Rgb, assert_color, distance, rgb};
use crate::settings::PRESET_COLORS;
use crate::theme::tests::ForcedTheme;
use crate::theme::{Theme, ThemePreference, tokens_for};
use windows::Win32::UI::WindowsAndMessaging::{
    GetMenuItemCount, GetMenuItemID, GetMenuState, GetMenuStringW, HMENU, MENU_ITEM_FLAGS,
    MF_BYCOMMAND, MF_BYPOSITION, MF_CHECKED, MF_GRAYED,
};

const WHITE: COLORREF = COLORREF(0x00FF_FFFF);
const RED: COLORREF = COLORREF(0x0000_00FF);
const GREEN: COLORREF = COLORREF(0x0000_FF00);
/// A canvas colour no theme token resembles.
const CANVAS: COLORREF = COLORREF(0x0040_8040);

const THEMES: [(ThemePreference, Theme); 2] = [
    (ThemePreference::Light, Theme::Light),
    (ThemePreference::Dark, Theme::Dark),
];

const VIEWPORT: Rect = Rect::new(0, 0, 1600, 1100);
const SELECTION: Rect = Rect::new(200, 500, 800, 900);
/// 2x scale: glyph strokes are wide enough to show their colour.
const DPI: u32 = 192;

fn toolbar_for(
    selection: Rect,
    viewport: Rect,
    expanded: bool,
    thickness_open: bool,
    dpi: u32,
) -> Toolbar {
    Toolbar::layout(
        &selection,
        viewport,
        ToolKind::Rectangle,
        PRESET_COLORS[0],
        5,
        expanded,
        true,
        true,
        thickness_open,
        false,
        true,
        dpi,
        None,
    )
}

/// The 2x toolbar on its roomy viewport: Rectangle is the active tool, nothing can be
/// undone, line width is 5.
fn standard(expanded: bool, thickness_open: bool) -> Toolbar {
    toolbar_for(SELECTION, VIEWPORT, expanded, thickness_open, DPI)
}

fn render_in(toolbar: &Toolbar, viewport: Rect, input: Option<&str>) -> Canvas {
    let canvas = Canvas::new(viewport.right, viewport.bottom, CANVAS);
    toolbar.render(canvas.hdc, input);
    canvas
}

fn render(toolbar: &Toolbar, input: Option<&str>) -> Canvas {
    render_in(toolbar, VIEWPORT, input)
}

fn rect_of(toolbar: &Toolbar, item: ToolbarItem) -> Rect {
    toolbar
        .buttons
        .iter()
        .find(|button| button.item == item)
        .expect("the button exists")
        .rect
}

fn middle(rect: Rect) -> i32 {
    (rect.top + rect.bottom) / 2
}

fn centre(rect: Rect) -> (i32, i32) {
    ((rect.left + rect.right) / 2, middle(rect))
}

/// A button's fill, sampled just inside its left edge where no glyph reaches.
fn fill_of(canvas: &Canvas, rect: Rect) -> Rgb {
    canvas.pixel(rect.left + 4, middle(rect))
}

/// The pixel in `rect` farthest from `background`: the strongest mark a glyph left.
fn strongest(canvas: &Canvas, rect: Rect, background: COLORREF) -> Rgb {
    let pixels = canvas.pixels();
    (rect.top..rect.bottom)
        .flat_map(|y| (rect.left..rect.right).map(move |x| (x, y)))
        .map(|(x, y)| pixels[(y * canvas.width + x) as usize])
        .max_by_key(|&pixel| distance(pixel, rgb(background)))
        .expect("a non-empty rect")
}

/// A glyph or label is drawn in `ink`, not in any of the `rivals`.
#[track_caller]
fn assert_ink(
    canvas: &Canvas,
    area: Rect,
    background: COLORREF,
    ink: COLORREF,
    rivals: &[COLORREF],
    what: &str,
) {
    let mark = strongest(canvas, area, background);
    for rival in rivals {
        assert!(
            distance(mark, rgb(ink)) < distance(mark, rgb(*rival)),
            "{what}: the strongest mark {mark:?} is closer to {:?} than to {:?}",
            rgb(*rival),
            rgb(ink)
        );
    }
}

fn within(inner: (i32, i32, i32, i32), outer: Rect) -> bool {
    inner.0 >= outer.left
        && inner.1 >= outer.top
        && inner.2 <= outer.right
        && inner.3 <= outer.bottom
}

// ---------------------------------------------------------------------------
// Layout and interaction
// ---------------------------------------------------------------------------

#[test]
fn style_controls_can_be_left_out_and_each_takes_its_own_width() {
    let both = toolbar_for(SELECTION, VIEWPORT, false, false, 96);
    let width = |color: bool, thickness: bool| {
        Toolbar::layout(
            &SELECTION,
            VIEWPORT,
            ToolKind::Rectangle,
            PRESET_COLORS[0],
            3,
            false,
            color,
            thickness,
            false,
            true,
            true,
            96,
            None,
        )
        .action_bounds
        .width()
    };
    let plain = width(false, false);
    assert_eq!(width(true, false) - plain, 32 + 8, "the swatch and its gap");
    assert_eq!(
        width(false, true) - plain,
        52 + 8,
        "the width button and its gap"
    );
    assert_eq!(both.action_bounds.width(), width(true, true));
    assert_eq!(width(true, true) - plain, 40 + 60);

    let bare = Toolbar::layout(
        &SELECTION,
        VIEWPORT,
        ToolKind::Rectangle,
        PRESET_COLORS[0],
        3,
        false,
        false,
        false,
        false,
        true,
        true,
        96,
        None,
    );
    assert!(
        bare.buttons
            .iter()
            .all(|button| !matches!(button.item, ToolbarItem::Color | ToolbarItem::Thickness))
    );
}

#[test]
fn the_wheel_cannot_open_without_a_thickness_button() {
    let toolbar = Toolbar::layout(
        &SELECTION,
        VIEWPORT,
        ToolKind::Rectangle,
        PRESET_COLORS[0],
        3,
        false,
        true,
        false,
        true,
        true,
        true,
        96,
        None,
    );
    assert!(toolbar.wheel_bounds.is_none());
    assert!(
        !toolbar
            .buttons
            .iter()
            .any(|button| matches!(button.item, ToolbarItem::ThicknessOption(_)))
    );
}

#[test]
fn a_work_area_too_small_for_the_panels_shrinks_the_grid_cells_until_all_fit() {
    let viewport = Rect::new(0, 0, 100, 100);
    let toolbar = toolbar_for(Rect::new(40, 40, 60, 60), viewport, false, false, 96);
    assert_eq!(toolbar.tool_bounds, toolbar.action_bounds);
    // An open wheel is the one element that sits on top of the grid, inside the work area.
    let with_wheel = toolbar_for(Rect::new(40, 40, 60, 60), viewport, false, true, 96);
    let wheel = with_wheel.wheel_bounds.expect("the wheel is open");
    assert!(within(
        (wheel.left, wheel.top, wheel.right, wheel.bottom),
        viewport
    ));
    let count = toolbar.buttons.len();
    for (index, button) in toolbar.buttons.iter().enumerate() {
        assert!(within(
            (
                button.rect.left,
                button.rect.top,
                button.rect.right,
                button.rect.bottom
            ),
            viewport
        ));
        assert!(button.rect.width() > 4, "{:?}", button.rect);
        assert!(
            button.rect.width() < 28,
            "cells shrank below the 32 px design size: {:?}",
            button.rect
        );
        for other in &toolbar.buttons[index + 1..count] {
            assert!(
                button.rect.right <= other.rect.left
                    || other.rect.right <= button.rect.left
                    || button.rect.bottom <= other.rect.top
                    || other.rect.bottom <= button.rect.top,
                "{:?} overlaps {:?}",
                button.item,
                other.item
            );
        }
    }
    assert!(toolbar.wheel_row_height() < Toolbar::WHEEL_ROW);
    assert!(toolbar.wheel_row_height() >= 12);
}

#[test]
fn wheel_rows_scale_with_the_dpi() {
    let at = |dpi: u32| toolbar_for(SELECTION, VIEWPORT, false, false, dpi).wheel_row_height();
    assert_eq!(at(96), Toolbar::WHEEL_ROW);
    assert_eq!(at(192), Toolbar::WHEEL_ROW * 2);
}

#[test]
fn hit_testing_finds_enabled_buttons_only() {
    let toolbar = standard(false, true);
    let rail = toolbar.tool_bounds;
    let redo = ToolbarItem::Action(ToolbarAction::Redo);
    let undo = ToolbarItem::Action(ToolbarAction::Undo);
    assert_eq!(
        toolbar.hit_test(centre(rect_of(&toolbar, redo))),
        Some(redo)
    );
    assert_eq!(
        toolbar.hit_test(centre(rect_of(&toolbar, undo))),
        None,
        "a disabled button cannot be hit"
    );
    assert_eq!(toolbar.hit_test((-5, -5)), None, "outside every panel");
    assert!(toolbar.contains_point((rail.left + 1, rail.top + 1)));
    assert_eq!(
        toolbar.hit_test((rail.left + 1, middle(rail))),
        None,
        "panel padding is not a button"
    );
    let wheel = toolbar.wheel_bounds.expect("the wheel is open");
    assert!(toolbar.contains_point(centre(wheel)));
    assert!(!toolbar.contains_point((wheel.left - 200, wheel.top - 200)));
}

#[test]
fn hover_follows_the_pointer_and_reports_only_changes() {
    let mut toolbar = standard(false, false);
    let select = ToolbarItem::Tool(ToolKind::Select);
    let undo = ToolbarItem::Action(ToolbarAction::Undo);
    let on_select = centre(rect_of(&toolbar, select));
    assert!(toolbar.update_hover(on_select));
    assert_eq!(toolbar.hovered_item, Some(select));
    assert!(
        !toolbar.update_hover(on_select),
        "same button, nothing changed"
    );
    assert!(toolbar.update_hover((0, 0)));
    assert_eq!(toolbar.hovered_item, None);
    assert!(!toolbar.update_hover((0, 0)));
    assert!(
        toolbar.update_hover(centre(rect_of(&toolbar, undo))),
        "a disabled button is hovered too, for its tooltip"
    );
    assert_eq!(toolbar.hovered_item, Some(undo));
}

#[test]
fn tooltips_open_away_from_a_selection_the_bar_sits_above() {
    let viewport = Rect::new(0, 0, 1100, 700);
    // Room above the bar: the tooltip opens above it, away from the selection.
    // (A selection reaching the bottom edge leaves no room below, so the bar goes above.)
    let roomy = toolbar_for(Rect::new(100, 160, 500, 660), viewport, false, false, 96);
    assert!(roomy.action_bounds.bottom <= roomy.tool_bounds.top + 1);
    let save = rect_of(&roomy, ToolbarItem::Action(ToolbarAction::Save));
    let tip = roomy.tooltip_bounds(save, 150, 30);
    assert!(tip.bottom <= save.top, "{tip:?} vs {save:?}");

    // No room above: the second choice, below the bar, is used.
    let tight = toolbar_for(Rect::new(100, 60, 500, 660), viewport, false, false, 96);
    assert!(tight.action_bounds.bottom <= tight.tool_bounds.top + 1);
    let save = rect_of(&tight, ToolbarItem::Action(ToolbarAction::Save));
    let tip = tight.tooltip_bounds(save, 150, 30);
    assert!(tip.top >= save.bottom, "{tip:?} vs {save:?}");
}

#[test]
fn a_tooltip_that_fits_nowhere_is_clamped_into_the_viewport() {
    let toolbar = standard(false, false);
    let margin = 8; // 4 design px at 2x
    // Wider than the viewport: neither side of the rail fits, so it hugs the left edge.
    let pen = rect_of(&toolbar, ToolbarItem::Tool(ToolKind::Pen));
    let tip = toolbar.tooltip_bounds(pen, 5000, 30);
    assert_eq!(tip.left, VIEWPORT.left + margin);
    // Taller than the viewport: neither above nor below the bar fits, so it hugs the top.
    let save = rect_of(&toolbar, ToolbarItem::Action(ToolbarAction::Save));
    let tip = toolbar.tooltip_bounds(save, 150, 5000);
    assert_eq!(tip.top, VIEWPORT.top + margin);
}

#[test]
fn every_toolbar_command_shows_its_keyboard_shortcut() {
    for (action, keys) in [
        (ToolbarAction::Undo, "Ctrl+Z"),
        (ToolbarAction::Redo, "Ctrl+Y"),
        (ToolbarAction::Save, "Ctrl+S"),
        (ToolbarAction::Copy, "Ctrl+C"),
        (ToolbarAction::Upload, "Ctrl+U"),
        (ToolbarAction::Pin, "Ctrl+T"),
        (ToolbarAction::CopyText, "Ctrl+Shift+C"),
        (ToolbarAction::RedactSensitive, "Ctrl+Shift+R"),
        (ToolbarAction::Settings, "Ctrl+,"),
        (ToolbarAction::Cancel, "Esc"),
    ] {
        assert_eq!(
            shortcut(ToolbarItem::Action(action)),
            Some(keys),
            "{action:?}"
        );
    }
    for item in [
        ToolbarItem::MoreTools,
        ToolbarItem::Color,
        ToolbarItem::Thickness,
        ToolbarItem::ThicknessOption(5),
    ] {
        assert_eq!(shortcut(item), None, "{item:?}");
    }
}

/// Work areas where the expanded rail is too tall as one column: the work area, its dpi
/// and a selection with room for the rail on its right and the action bar below it.
const SHORT_WORK_AREAS: [(Rect, u32, Rect); 3] = [
    (
        Rect::new(0, 0, 1920, 1040),
        192,
        Rect::new(400, 200, 1200, 700),
    ),
    (
        Rect::new(0, 0, 1366, 728),
        144,
        Rect::new(100, 100, 500, 500),
    ),
    (
        Rect::new(0, 0, 1366, 728),
        120,
        Rect::new(100, 100, 500, 500),
    ),
];

/// What the collapsed rail shows, bottom to top; a split rail keeps it as its main column.
const MAIN_COLUMN: [ToolbarItem; 7] = [
    ToolbarItem::Tool(ToolKind::Select),
    ToolbarItem::Tool(ToolKind::Rectangle),
    ToolbarItem::Tool(ToolKind::Arrow),
    ToolbarItem::Tool(ToolKind::Pen),
    ToolbarItem::MoreTools,
    ToolbarItem::Action(ToolbarAction::Undo),
    ToolbarItem::Action(ToolbarAction::Redo),
];

fn is_rail_item(item: ToolbarItem) -> bool {
    MAIN_COLUMN.contains(&item) || is_more_tool(item)
}

fn inside(inner: Rect, outer: Rect) -> bool {
    within((inner.left, inner.top, inner.right, inner.bottom), outer)
}

#[test]
fn an_expanded_rail_too_tall_for_one_column_gets_a_second_one_and_keeps_the_l() {
    for (viewport, dpi, selection) in SHORT_WORK_AREAS {
        let what = format!("{viewport:?} at {dpi} dpi");
        let scale = |value: i32| value * dpi as i32 / 96;
        let (size, pad, gap) = (scale(32), scale(6), scale(2));
        let rows = (MAIN_COLUMN.len() + MORE_TOOLS.len()) as i32;
        let one_column = 2 * pad + rows * size + (rows - 1) * gap + scale(12);
        let safe_height = viewport.height() - 2 * scale(Toolbar::OUTER_PAD);
        assert!(
            one_column + scale(Toolbar::ACTION_HEIGHT) > safe_height,
            "{what}: one column fits"
        );

        let toolbar = toolbar_for(selection, viewport, true, false, dpi);
        let collapsed = toolbar_for(selection, viewport, false, false, dpi);
        let (rail, bar) = (toolbar.tool_bounds, toolbar.action_bounds);
        assert_ne!(rail, bar, "{what}: the grid took over");
        assert_eq!(collapsed.tool_bounds.width(), size + 2 * pad, "{what}");

        // Two columns with the 2 px (design) gap, still an L beside and below the selection.
        assert_eq!(rail.width(), 2 * size + 2 * pad + gap, "{what}");
        assert_eq!(
            rail.height(),
            2 * pad + MORE_TOOLS.len() as i32 * size + (MORE_TOOLS.len() as i32 - 1) * gap,
            "{what}"
        );
        assert_eq!(rail.left, selection.right + scale(Toolbar::GAP), "{what}");
        assert_eq!(bar.top, selection.bottom + scale(Toolbar::GAP), "{what}");
        assert_eq!((rail.bottom, rail.right), (bar.top, bar.right), "{what}");
        assert!(inside(rail, viewport) && inside(bar, viewport), "{what}");
        assert!(!rail.intersects(&selection) && !bar.intersects(&selection));

        // The main column is the collapsed rail, row for row, next to the selection.
        for item in MAIN_COLUMN {
            let rect = rect_of(&toolbar, item);
            let plain = rect_of(&collapsed, item);
            assert_eq!(rect.left, rail.left + pad, "{what}: {item:?}");
            assert_eq!((rect.width(), rect.height()), (size, size), "{what}");
            assert_eq!(
                rect.top - rail.bottom,
                plain.top - collapsed.tool_bounds.bottom,
                "{what}: {item:?}"
            );
        }
        // The tools behind the chevron stack in the second column from the bottom up.
        let more_left = rail.left + pad + size + gap;
        for (index, tool) in MORE_TOOLS.into_iter().enumerate() {
            let bottom = rail.bottom - pad - index as i32 * (size + gap);
            assert_eq!(
                rect_of(&toolbar, ToolbarItem::Tool(tool)),
                Rect::new(more_left, bottom - size, more_left + size, bottom),
                "{what}: {tool:?}"
            );
        }
        assert_eq!(more_left + size + pad, rail.right, "{what}");

        for (index, button) in toolbar.buttons.iter().enumerate() {
            let at = format!("{what}: {:?}", button.item);
            assert!(inside(button.rect, viewport), "{at}");
            if is_rail_item(button.item) {
                assert!(inside(button.rect, rail), "{at}");
                assert!(!button.rect.intersects(&bar), "{at}");
            } else {
                assert!(inside(button.rect, bar), "{at}");
            }
            for other in &toolbar.buttons[index + 1..] {
                assert!(!button.rect.intersects(&other.rect), "{at} overlaps");
            }
        }
    }
}

#[test]
fn the_split_rail_flips_with_the_l_and_the_main_column_stays_next_to_the_selection() {
    let viewport = Rect::new(0, 0, 1920, 1040);
    let (size, pad, gap) = (64, 12, 4);
    // The rail's side of the selection decides which column is the main one; where the
    // chevron stays put when the rail grows, it must not move.
    let cases = [
        (
            "right of the selection",
            Rect::new(400, 200, 1200, 700),
            true,
            true,
        ),
        (
            "left of the selection",
            Rect::new(1100, 200, 1900, 700),
            false,
            true,
        ),
        (
            "beside a left-half window",
            Rect::new(0, 0, 960, 1040),
            true,
            true,
        ),
        (
            "beside a right-half window",
            Rect::new(960, 0, 1920, 1040),
            false,
            true,
        ),
        ("inside a full-screen selection", viewport, true, false),
    ];
    for (what, selection, main_on_left, chevron_stays) in cases {
        let toolbar = toolbar_for(selection, viewport, true, false, 192);
        let (rail, bar) = (toolbar.tool_bounds, toolbar.action_bounds);
        assert_ne!(rail, bar, "{what}: the grid took over");
        assert_eq!(rail.width(), 2 * size + 2 * pad + gap, "{what}");
        assert_eq!(rail.bottom, bar.top, "{what}");
        assert!(inside(rail, viewport) && inside(bar, viewport), "{what}");
        let main = rect_of(&toolbar, ToolbarItem::Tool(ToolKind::Select));
        let more = rect_of(&toolbar, ToolbarItem::Tool(ToolKind::Ellipse));
        if main_on_left {
            assert_eq!(main.left, rail.left + pad, "{what}");
            assert_eq!(more.left, main.right + gap, "{what}");
            assert_eq!(more.right + pad, rail.right, "{what}");
        } else {
            assert_eq!(main.right + pad, rail.right, "{what}");
            assert_eq!(more.right + gap, main.left, "{what}");
            assert_eq!(more.left - pad, rail.left, "{what}");
        }
        if chevron_stays {
            let collapsed = toolbar_for(selection, viewport, false, false, 192);
            assert_eq!(
                rect_of(&toolbar, ToolbarItem::MoreTools),
                rect_of(&collapsed, ToolbarItem::MoreTools),
                "{what}"
            );
        }
    }
}

#[test]
fn at_96_dpi_the_expanded_rail_stays_one_column_with_the_rows_it_always_had() {
    let viewport = Rect::new(0, 0, 1920, 1040);
    let toolbar = toolbar_for(Rect::new(200, 100, 800, 600), viewport, true, false, 96);
    assert_eq!(toolbar.tool_bounds, Rect::new(808, 76, 852, 608));
    // Rows climb from the bottom in 34 px steps; the chevron and Undo add a 6 px gap above.
    let rows = [
        (ToolbarItem::Tool(ToolKind::Select), 570),
        (ToolbarItem::Tool(ToolKind::Rectangle), 536),
        (ToolbarItem::Tool(ToolKind::Arrow), 502),
        (ToolbarItem::Tool(ToolKind::Pen), 468),
        (ToolbarItem::MoreTools, 434),
        (ToolbarItem::Tool(ToolKind::Ellipse), 394),
        (ToolbarItem::Tool(ToolKind::Line), 360),
        (ToolbarItem::Tool(ToolKind::Highlight), 326),
        (ToolbarItem::Tool(ToolKind::Text), 292),
        (ToolbarItem::Tool(ToolKind::Step), 258),
        (ToolbarItem::Tool(ToolKind::Blur), 224),
        (ToolbarItem::Tool(ToolKind::Redact), 190),
        (ToolbarItem::Tool(ToolKind::ColorPicker), 156),
        (ToolbarItem::Action(ToolbarAction::Undo), 122),
        (ToolbarItem::Action(ToolbarAction::Redo), 82),
    ];
    for (item, top) in rows {
        assert_eq!(
            rect_of(&toolbar, item),
            Rect::new(814, top, 846, top + 32),
            "{item:?}"
        );
    }
}

#[test]
fn the_rail_splits_exactly_when_one_column_and_the_bar_overflow_the_work_area() {
    // At 96 dpi one column is 532 px, two are 282 px, the bar 42 px, and the work area
    // keeps 8 px at each edge.
    let rail_in = |height: i32| {
        let viewport = Rect::new(0, 0, 1920, height);
        toolbar_for(Rect::new(300, 100, 900, 300), viewport, true, false, 96)
    };
    let size = |toolbar: &Toolbar| (toolbar.tool_bounds.width(), toolbar.tool_bounds.height());
    assert_eq!(size(&rail_in(532 + 42 + 16)), (44, 532));
    assert_eq!(size(&rail_in(532 + 42 + 16 - 1)), (78, 282));
    assert_eq!(size(&rail_in(282 + 42 + 16)), (78, 282));
    let grid = rail_in(282 + 42 + 16 - 1);
    assert_eq!(grid.tool_bounds, grid.action_bounds);
}

#[test]
fn a_tiny_work_area_still_gets_the_grid_for_the_expanded_rail() {
    for (width, height, dpi) in [(640, 440, 192), (320, 240, 96), (1366, 300, 144)] {
        let viewport = Rect::new(0, 0, width, height);
        let toolbar = toolbar_for(Rect::new(10, 10, 18, 18), viewport, true, false, dpi);
        assert_eq!(toolbar.tool_bounds, toolbar.action_bounds, "{viewport:?}");
        assert!(inside(toolbar.tool_bounds, viewport), "{viewport:?}");
        for button in &toolbar.buttons {
            assert!(inside(button.rect, viewport), "{:?}", button.item);
            if button.is_enabled {
                assert_eq!(
                    toolbar.hit_test(centre(button.rect)),
                    Some(button.item),
                    "{viewport:?} {:?}",
                    button.item
                );
            }
        }
    }
}

#[test]
fn second_column_buttons_are_hit_hovered_and_given_tooltips_like_the_main_column() {
    let viewport = Rect::new(0, 0, 1920, 1040);
    // The rail right of its selection, then left of it.
    let cases = [
        (Rect::new(400, 200, 1200, 700), true),
        (Rect::new(1100, 200, 1900, 700), false),
    ];
    for (selection, tooltip_right_of_rail) in cases {
        let mut toolbar = toolbar_for(selection, viewport, true, false, 192);
        let rail = toolbar.tool_bounds;
        let buttons = toolbar.buttons.clone();
        let on_rail = buttons
            .iter()
            .filter(|button| is_rail_item(button.item) && button.is_enabled);
        for button in on_rail {
            let (item, rect) = (button.item, button.rect);
            assert_eq!(toolbar.hit_test(centre(rect)), Some(item), "{item:?}");
            assert_eq!(toolbar.hit_test((rect.left, rect.top)), Some(item));
            assert_eq!(
                toolbar.hit_test((rect.right - 1, rect.bottom - 1)),
                Some(item)
            );
            toolbar.hovered_item = None;
            assert!(toolbar.update_hover(centre(rect)), "{item:?}");
            assert_eq!(toolbar.hovered_item, Some(item));
            assert!(!toolbar.update_hover(centre(rect)), "{item:?}");

            let tip = toolbar.tooltip_bounds(rect, 300, 60);
            assert!(inside(tip, viewport), "{item:?}: {tip:?}");
            assert!(
                toolbar
                    .buttons
                    .iter()
                    .all(|other| !tip.intersects(&other.rect)),
                "{item:?}: {tip:?} covers a button"
            );
            if tooltip_right_of_rail {
                assert!(tip.left >= rail.right, "{item:?}: {tip:?}");
            } else {
                assert!(tip.right <= rail.left, "{item:?}: {tip:?}");
            }
            assert!(((tip.top + tip.bottom) / 2 - middle(rect)).abs() <= 1);
        }
        // The gutter between the columns is panel, not a button.
        let main = rect_of(&toolbar, ToolbarItem::Tool(ToolKind::Select));
        let more = rect_of(&toolbar, ToolbarItem::Tool(ToolKind::Ellipse));
        let gutter = (main.right.min(more.right), middle(more));
        assert!(toolbar.contains_point(gutter));
        assert_eq!(toolbar.hit_test(gutter), None);
    }
}

#[test]
fn the_wheel_of_a_split_rail_never_covers_the_rail() {
    let tall = Rect::new(0, 0, 1920, 1040);
    let short = Rect::new(0, 0, 1366, 728);
    // Work area, dpi, selection, whether the wheel moves right past the rail, and whether
    // it opens above its button. The rail sits at the bar's left end except in the fourth
    // case, where it is at the right end and the wheel stays under its button.
    let cases = [
        (tall, 192, Rect::new(0, 0, 960, 1040), true, true),
        (tall, 192, Rect::new(1100, 200, 1900, 700), true, true),
        (tall, 192, Rect::new(1100, 300, 1900, 980), true, false),
        (tall, 192, Rect::new(400, 200, 1200, 700), false, true),
        (short, 144, Rect::new(900, 100, 1350, 500), true, true),
    ];
    for (viewport, dpi, selection, past_rail, above) in cases {
        let what = format!("{viewport:?} at {dpi} dpi, {selection:?}");
        let toolbar = toolbar_for(selection, viewport, true, true, dpi);
        let rail = toolbar.tool_bounds;
        assert_ne!(rail, toolbar.action_bounds, "{what}: the grid took over");
        let main = rect_of(&toolbar, ToolbarItem::Tool(ToolKind::Select));
        let more = rect_of(&toolbar, ToolbarItem::Tool(ToolKind::Ellipse));
        assert_ne!(main.left, more.left, "{what}: not a split rail");

        let wheel = toolbar.wheel_bounds.expect("the wheel is open");
        let anchor = rect_of(&toolbar, ToolbarItem::Thickness);
        let pad = 4 * dpi as i32 / 96;
        assert!(inside(wheel, viewport), "{what}: {wheel:?}");
        assert!(
            !wheel.intersects(&rail),
            "{what}: {wheel:?} covers {rail:?}"
        );
        for button in &toolbar.buttons {
            if !matches!(button.item, ToolbarItem::ThicknessOption(_)) {
                assert!(
                    !wheel.intersects(&button.rect),
                    "{what}: covers {:?}",
                    button.item
                );
            }
        }
        let left = if past_rail {
            rail.right + pad
        } else {
            anchor.left
        };
        assert_eq!(wheel.left, left, "{what}");
        if above {
            assert_eq!(wheel.bottom, anchor.top - pad, "{what}");
        } else {
            assert_eq!(wheel.top, anchor.bottom + pad, "{what}");
        }
    }
}

#[test]
fn the_open_wheel_owns_its_whole_panel_for_hits_and_hover() {
    // Line width 1: the first two rows (values -1 and 0) have no button.
    let mut toolbar = Toolbar::layout(
        &SELECTION,
        VIEWPORT,
        ToolKind::Rectangle,
        PRESET_COLORS[0],
        1,
        false,
        true,
        true,
        true,
        true,
        true,
        DPI,
        None,
    );
    let wheel = toolbar.wheel_bounds.expect("the wheel is open");
    // Slide a tool under the whole wheel.
    let select = ToolbarItem::Tool(ToolKind::Select);
    toolbar
        .buttons
        .iter_mut()
        .find(|button| button.item == select)
        .expect("the rail has Select")
        .rect = wheel;
    let (pad, row) = (8, toolbar.wheel_row_height());
    let row_middle = |index: i32| wheel.top + pad + index * row + row / 2;
    let x = (wheel.left + wheel.right) / 2;
    let nothing = [
        ("top-left padding", (wheel.left, wheel.top)),
        ("bottom-right padding", (wheel.right - 1, wheel.bottom - 1)),
        ("side padding", (wheel.left + 1, middle(wheel))),
        ("first empty row", (x, row_middle(0))),
        ("second empty row", (x, row_middle(1))),
    ];
    for (what, point) in nothing {
        assert_eq!(toolbar.hit_test(point), None, "{what}");
        assert!(!toolbar.update_hover(point), "{what} hovers nothing");
        assert_eq!(toolbar.hovered_item, None, "{what}");
    }
    let one = ToolbarItem::ThicknessOption(1);
    assert_eq!(toolbar.hit_test((x, row_middle(2))), Some(one));
    assert!(toolbar.update_hover((x, row_middle(2))));
    assert_eq!(toolbar.hovered_item, Some(one));
}

#[test]
fn splitting_the_rail_keeps_the_button_order_the_command_menu_follows() {
    let viewport = Rect::new(0, 0, 1920, 1040);
    let selection = Rect::new(400, 200, 1200, 700);
    let layout_at = |dpi: u32| {
        let toolbar = toolbar_for(selection, viewport, true, false, dpi);
        let menu = build_command_menu(&toolbar.buttons)
            .expect("the menu is built")
            .expect("there are buttons");
        let items: Vec<ToolbarItem> = toolbar.buttons.iter().map(|button| button.item).collect();
        (toolbar.tool_bounds.width(), items, menu.items)
    };
    let (one_column, one_items, one_menu) = layout_at(96);
    let (two_columns, two_items, two_menu) = layout_at(192);
    assert_eq!((one_column, two_columns), (44, 156), "one rail splits");
    assert_eq!(one_items, two_items);
    assert_eq!(one_menu, two_menu);
    assert_eq!(two_menu, two_items);
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

#[test]
fn panels_float_on_a_shadow_with_a_card_fill_and_stroke_border() {
    for (preference, theme) in THEMES {
        let _theme = ForcedTheme::new(preference);
        let t = tokens_for(theme);
        let toolbar = standard(false, false);
        let canvas = render(&toolbar, None);
        for panel in [toolbar.tool_bounds, toolbar.action_bounds] {
            let cy = middle(panel);
            assert_color(canvas.pixel(panel.left, cy), t.stroke, 4, "border");
            assert_color(canvas.pixel(panel.left + 4, cy), t.card, 2, "fill");
            let shadow = canvas.pixel(panel.right + 1, cy);
            assert!(
                shadow[1] < 0x80 && shadow[0] <= 0x40 && shadow[2] <= 0x40,
                "the panel casts a shadow: {shadow:?}"
            );
            assert_eq!(
                canvas.pixel(panel.left - 40, cy),
                rgb(CANVAS),
                "the shadow is soft and short"
            );
        }
    }
}

#[test]
fn a_grid_layout_draws_its_single_panel_once() {
    let viewport = Rect::new(0, 0, 300, 200);
    let toolbar = toolbar_for(Rect::new(100, 80, 140, 120), viewport, false, false, 96);
    assert_eq!(toolbar.tool_bounds, toolbar.action_bounds);
    let size = Rect::new(0, 0, 340, 240);
    let rendered = render_in(&toolbar, size, None);
    // One elevation under the panel, however many panels the toolbar has.
    let reference = Canvas::new(size.right, size.bottom, CANVAS);
    let radius = (8 * toolbar.dpi as i32 / 96).max(1);
    crate::drawing::elevation(reference.hdc, toolbar.tool_bounds, radius, toolbar.dpi);
    let panel = toolbar.tool_bounds;
    let x = (panel.left + panel.right) / 2;
    for y in panel.bottom + 1..panel.bottom + 12 {
        assert!(
            distance(rendered.pixel(x, y), reference.pixel(x, y)) <= 3,
            "row {y}: {:?} vs {:?}",
            rendered.pixel(x, y),
            reference.pixel(x, y)
        );
    }
    assert!(
        reference.pixel(x, panel.bottom + 1)[1] < 0x80,
        "there is a shadow to compare"
    );
}

#[test]
fn button_fill_follows_selection_hover_and_press() {
    let rectangle = ToolbarItem::Tool(ToolKind::Rectangle);
    let select = ToolbarItem::Tool(ToolKind::Select);
    let pen = ToolbarItem::Tool(ToolKind::Pen);
    let copy = ToolbarItem::Action(ToolbarAction::Copy);
    let undo = ToolbarItem::Action(ToolbarAction::Undo);
    for (preference, theme) in THEMES {
        let _theme = ForcedTheme::new(preference);
        let t = tokens_for(theme);
        let cases = [
            (
                "idle",
                None,
                None,
                vec![
                    (select, t.card),
                    (rectangle, t.accent_tint),
                    (pen, t.card),
                    (copy, t.accent),
                ],
            ),
            (
                "hovered",
                Some(pen),
                None,
                vec![(pen, t.subtle_hover), (select, t.card)],
            ),
            (
                "pressed",
                Some(pen),
                Some(pen),
                vec![(pen, t.subtle_pressed)],
            ),
            (
                "dragged off",
                Some(select),
                Some(pen),
                vec![(pen, t.card), (select, t.subtle_hover)],
            ),
            (
                "hovered selection",
                Some(rectangle),
                None,
                vec![(rectangle, t.accent_tint)],
            ),
            (
                "pressed selection",
                Some(rectangle),
                Some(rectangle),
                vec![(rectangle, t.accent_tint)],
            ),
            (
                "hovered primary",
                Some(copy),
                None,
                vec![(copy, t.accent_hover)],
            ),
            (
                "pressed primary",
                Some(copy),
                Some(copy),
                vec![(copy, t.accent_pressed)],
            ),
            ("hovered disabled", Some(undo), None, vec![(undo, t.card)]),
        ];
        for (name, hover, press, expectations) in cases {
            let mut toolbar = standard(false, false);
            toolbar.hovered_item = hover;
            toolbar.pressed_item = press;
            let canvas = render(&toolbar, None);
            for (item, fill) in expectations {
                let what = format!("{theme:?} {name} {item:?}");
                assert_color(fill_of(&canvas, rect_of(&toolbar, item)), fill, 2, &what);
            }
        }
        // The selected tool keeps an accent outline on its edge.
        let toolbar = standard(false, false);
        let canvas = render(&toolbar, None);
        let edge = |item: ToolbarItem| {
            let rect = rect_of(&toolbar, item);
            canvas.pixel(rect.left, middle(rect))
        };
        assert_color(edge(rectangle), t.accent_ink, 4, "selected outline");
        assert_color(
            edge(select),
            t.card,
            2,
            "unselected buttons have no outline",
        );
    }
}

#[test]
fn a_check_mark_only_selects_tools_and_the_width_controls() {
    let _theme = ForcedTheme::new(ThemePreference::Light);
    let t = tokens_for(Theme::Light);
    let mut toolbar = standard(false, false);
    for button in &mut toolbar.buttons {
        button.is_checked = true;
    }
    let canvas = render(&toolbar, None);
    let fill = |item: ToolbarItem| fill_of(&canvas, rect_of(&toolbar, item));
    for item in [
        ToolbarItem::Tool(ToolKind::Pen),
        ToolbarItem::MoreTools,
        ToolbarItem::Thickness,
    ] {
        assert_color(
            fill(item),
            t.accent_tint,
            2,
            &format!("{item:?} is selected"),
        );
    }
    for item in [
        ToolbarItem::Action(ToolbarAction::Save),
        ToolbarItem::Action(ToolbarAction::Redo),
        ToolbarItem::Color,
    ] {
        assert_color(
            fill(item),
            t.card,
            2,
            &format!("{item:?} is not selectable"),
        );
    }
}

#[test]
fn glyphs_are_drawn_in_the_ink_of_their_state() {
    for (preference, theme) in THEMES {
        let _theme = ForcedTheme::new(preference);
        let t = tokens_for(theme);
        let toolbar = standard(false, false);
        let canvas = render(&toolbar, None);
        let glyph = |item: ToolbarItem| rect_of(&toolbar, item).inflate(-12, -12);
        let tool = |kind: ToolKind| ToolbarItem::Tool(kind);
        let action = ToolbarItem::Action;
        assert_ink(
            &canvas,
            glyph(tool(ToolKind::Select)),
            t.card,
            t.text,
            &[t.text_disabled, t.accent_ink],
            "idle tool",
        );
        assert_ink(
            &canvas,
            glyph(action(ToolbarAction::Redo)),
            t.card,
            t.text,
            &[t.text_disabled, t.accent_ink],
            "enabled action",
        );
        assert_ink(
            &canvas,
            glyph(tool(ToolKind::Rectangle)),
            t.accent_tint,
            t.accent_ink,
            &[t.text, t.text_disabled],
            "selected tool",
        );
        assert_ink(
            &canvas,
            glyph(action(ToolbarAction::Undo)),
            t.card,
            t.text_disabled,
            &[t.text, t.accent_ink],
            "disabled action",
        );
        assert_ink(
            &canvas,
            glyph(action(ToolbarAction::Copy)),
            t.accent,
            t.accent_text,
            &[t.text_disabled, t.accent_ink],
            "primary action",
        );
        assert_ink(
            &canvas,
            glyph(ToolbarItem::MoreTools),
            t.card,
            t.text,
            &[t.text_disabled, t.accent_ink],
            "more tools",
        );
    }
}

#[test]
fn the_more_tools_button_is_lit_while_expanded_or_hiding_the_active_tool() {
    let _theme = ForcedTheme::new(ThemePreference::Light);
    let t = tokens_for(Theme::Light);
    let lit = |toolbar: &Toolbar| {
        let canvas = render(toolbar, None);
        fill_of(&canvas, rect_of(toolbar, ToolbarItem::MoreTools))
    };
    assert_color(
        lit(&standard(false, false)),
        t.card,
        2,
        "collapsed, active tool visible",
    );
    assert_color(lit(&standard(true, false)), t.accent_tint, 2, "expanded");
    // An active tool hidden behind "more" lights the button even while collapsed.
    let hidden = Toolbar::layout(
        &SELECTION,
        VIEWPORT,
        ToolKind::Blur,
        PRESET_COLORS[0],
        5,
        false,
        true,
        true,
        false,
        true,
        true,
        DPI,
        None,
    );
    assert_color(lit(&hidden), t.accent_tint, 2, "hidden active tool");
}

#[test]
fn the_more_tools_glyph_changes_with_the_state() {
    let _theme = ForcedTheme::new(ThemePreference::Light);
    // Where the glyph left ink, judged against the button's own fill.
    let mask = |toolbar: &Toolbar| {
        let canvas = render(toolbar, None);
        let rect = rect_of(toolbar, ToolbarItem::MoreTools);
        let fill = fill_of(&canvas, rect);
        let glyph = rect.inflate(-12, -12);
        (glyph.top..glyph.bottom)
            .flat_map(|y| (glyph.left..glyph.right).map(move |x| (x, y)))
            .map(|(x, y)| distance(canvas.pixel(x, y), fill) > 90)
            .collect::<Vec<bool>>()
    };
    let collapsed = mask(&standard(false, false));
    let expanded = mask(&standard(true, false));
    assert!(collapsed.iter().any(|&ink| ink), "a chevron is drawn");
    assert!(expanded.iter().any(|&ink| ink), "a chevron is drawn");
    assert_ne!(collapsed, expanded, "the chevron flips");
}

#[test]
fn the_swatch_shows_the_active_colour_and_the_width_button_a_dot_and_the_value() {
    for (preference, theme) in THEMES {
        let _theme = ForcedTheme::new(preference);
        let t = tokens_for(theme);
        let toolbar = standard(false, false);
        let canvas = render(&toolbar, None);

        let swatch = rect_of(&toolbar, ToolbarItem::Color);
        let (cx, cy) = centre(swatch);
        let colour = bgra_to_colorref(PRESET_COLORS[0]);
        assert_color(canvas.pixel(cx, cy), colour, 2, "swatch");
        assert_color(
            canvas.pixel(swatch.left + 12, cy),
            t.stroke,
            4,
            "swatch outline, 6 design px inside the button",
        );

        let width = rect_of(&toolbar, ToolbarItem::Thickness);
        let dot = (width.left + 30, middle(width));
        assert_color(canvas.pixel(dot.0, dot.1), t.text, 2, "dot");
        let digits = Rect::new(
            width.left + 52,
            width.top + 6,
            width.right - 8,
            width.bottom - 6,
        );
        assert_ink(
            &canvas,
            digits,
            t.card,
            t.text,
            &[t.text_disabled, t.accent_ink],
            "value",
        );

        // With the wheel open the button is selected: tinted, accent ink.
        let open = standard(false, true);
        let canvas = render(&open, None);
        let width = rect_of(&open, ToolbarItem::Thickness);
        assert_color(fill_of(&canvas, width), t.accent_tint, 2, "selected button");
        assert_color(
            canvas.pixel(width.left + 30, middle(width)),
            t.accent_ink,
            2,
            "selected dot",
        );
    }
}

#[test]
fn typed_digits_replace_the_line_width_in_its_button_only() {
    let toolbar = standard(false, false);
    let width = rect_of(&toolbar, ToolbarItem::Thickness);
    let plain = render(&toolbar, None).pixels();
    assert_eq!(
        render(&toolbar, Some("")).pixels(),
        plain,
        "an empty entry still shows the value"
    );
    let typed = render(&toolbar, Some("12"));
    let changed = typed
        .changed_since(&plain)
        .expect("digits differ from the value");
    assert!(within(changed, width), "{changed:?} vs {width:?}");
}

#[test]
fn the_open_wheel_is_a_panel_with_the_active_row_selected_and_far_rows_dimmed() {
    for (preference, theme) in THEMES {
        let _theme = ForcedTheme::new(preference);
        let t = tokens_for(theme);
        let toolbar = standard(false, true);
        let canvas = render(&toolbar, None);
        let wheel = toolbar.wheel_bounds.expect("the wheel is open");
        let cy = middle(wheel);
        assert_color(canvas.pixel(wheel.left, cy), t.stroke, 4, "border");
        assert_color(canvas.pixel(wheel.left + 3, cy), t.card, 2, "fill");
        assert!(
            canvas.pixel(wheel.right + 1, cy)[1] < 0x80,
            "the wheel casts a shadow"
        );

        let row = |value: i32| rect_of(&toolbar, ToolbarItem::ThicknessOption(value));
        assert_color(fill_of(&canvas, row(5)), t.accent_tint, 2, "active row");
        assert_color(
            canvas.pixel(row(5).left, middle(row(5))),
            t.accent_ink,
            4,
            "active row outline",
        );
        for value in [3, 4, 6, 7] {
            assert_color(fill_of(&canvas, row(value)), t.card, 2, "other rows");
        }
        let label = |value: i32| row(value).inflate(-8, -4);
        assert_ink(
            &canvas,
            label(5),
            t.accent_tint,
            t.accent_ink,
            &[t.text, t.text_disabled],
            "active",
        );
        for value in [4, 6] {
            assert_ink(
                &canvas,
                label(value),
                t.card,
                t.text,
                &[t.text_disabled, t.accent_ink],
                "near",
            );
        }
        for value in [3, 7] {
            assert_ink(
                &canvas,
                label(value),
                t.card,
                t.text_disabled,
                &[t.text, t.accent_ink],
                "far",
            );
        }
    }
}

#[test]
fn every_hovered_button_gets_a_tooltip_except_the_wheel_rows() {
    let viewport = Rect::new(0, 0, 1100, 700);
    let selection = Rect::new(100, 300, 500, 560);
    for expanded in [false, true] {
        let mut toolbar = toolbar_for(selection, viewport, expanded, true, 96);
        let resting = render_in(&toolbar, viewport, None).pixels();
        for button in toolbar.buttons.clone() {
            toolbar.hovered_item = Some(button.item);
            let changed = render_in(&toolbar, viewport, None).changed_since(&resting);
            if let ToolbarItem::ThicknessOption(_) = button.item {
                assert!(
                    changed.is_none_or(|bounds| within(bounds, button.rect)),
                    "{:?} shows its value already",
                    button.item
                );
            } else {
                let bounds = changed.expect("hovering changes the picture");
                assert!(
                    !within(bounds, button.rect),
                    "{:?} has no tooltip",
                    button.item
                );
                assert!(within(bounds, viewport), "{bounds:?} leaves the viewport");
            }
        }
    }
}

#[test]
fn tooltips_open_beside_the_rail_and_below_the_bar_in_card_colour() {
    let viewport = Rect::new(0, 0, 1100, 700);
    let selection = Rect::new(100, 300, 500, 560);
    for (preference, theme) in THEMES {
        let _theme = ForcedTheme::new(preference);
        let t = tokens_for(theme);
        let tooltip_of = |item: ToolbarItem| {
            let mut toolbar = toolbar_for(selection, viewport, false, false, 96);
            toolbar.hovered_item = Some(item);
            (render_in(&toolbar, viewport, None), toolbar)
        };
        let pen = ToolbarItem::Tool(ToolKind::Pen);
        let (canvas, toolbar) = tooltip_of(pen);
        // The rail sits right of the selection: the tooltip opens to its right, centred
        // on the button, 4 px away (3 px in is the card, before the text).
        let button = rect_of(&toolbar, pen);
        assert_color(
            canvas.pixel(toolbar.tool_bounds.right + 4 + 3, middle(button)),
            t.card,
            2,
            "rail tooltip",
        );
        // The bar sits below the selection: the tooltip opens below the button.
        let save = ToolbarItem::Action(ToolbarAction::Save);
        let (canvas, toolbar) = tooltip_of(save);
        let button = rect_of(&toolbar, save);
        assert_color(
            canvas.pixel(button.left + 3, button.bottom + 4 + 15),
            t.card,
            2,
            "bar tooltip",
        );
    }
}

#[test]
fn second_column_buttons_are_drawn_like_the_main_column() {
    let viewport = Rect::new(0, 0, 1920, 1040);
    let ellipse = ToolbarItem::Tool(ToolKind::Ellipse);
    let highlight = ToolbarItem::Tool(ToolKind::Highlight);
    let picker = ToolbarItem::Tool(ToolKind::ColorPicker);
    for (preference, theme) in THEMES {
        let _theme = ForcedTheme::new(preference);
        let t = tokens_for(theme);
        let mut toolbar = Toolbar::layout(
            &Rect::new(400, 200, 1200, 700),
            viewport,
            ToolKind::Highlight,
            PRESET_COLORS[0],
            5,
            true,
            true,
            true,
            false,
            false,
            true,
            192,
            None,
        );
        assert_ne!(toolbar.tool_bounds, toolbar.action_bounds, "a split rail");
        let fill = |toolbar: &Toolbar, item: ToolbarItem| {
            let canvas = render_in(toolbar, viewport, None);
            fill_of(&canvas, rect_of(toolbar, item))
        };
        assert_color(fill(&toolbar, ellipse), t.card, 2, "idle");
        assert_color(fill(&toolbar, highlight), t.accent_tint, 2, "active tool");
        let canvas = render_in(&toolbar, viewport, None);
        let active = rect_of(&toolbar, highlight);
        assert_color(
            canvas.pixel(active.left, middle(active)),
            t.accent_ink,
            4,
            "active outline",
        );
        toolbar.hovered_item = Some(ellipse);
        assert_color(fill(&toolbar, ellipse), t.subtle_hover, 2, "hovered");
        toolbar.pressed_item = Some(ellipse);
        assert_color(fill(&toolbar, ellipse), t.subtle_pressed, 2, "pressed");

        // The tooltip opens beside the whole rail, 4 design px (8 px) from its edge.
        toolbar.hovered_item = Some(picker);
        toolbar.pressed_item = None;
        let canvas = render_in(&toolbar, viewport, None);
        let probe = (
            toolbar.tool_bounds.right + 8 + 6,
            middle(rect_of(&toolbar, picker)),
        );
        assert_color(canvas.pixel(probe.0, probe.1), t.card, 2, "tooltip");
    }
}

// ---------------------------------------------------------------------------
// Tool icons
// ---------------------------------------------------------------------------

fn text_toolbar(text_background: Option<bool>, dpi: u32) -> Toolbar {
    Toolbar::layout(
        &SELECTION,
        VIEWPORT,
        ToolKind::Text,
        PRESET_COLORS[0],
        5,
        false,
        true,
        false,
        false,
        true,
        true,
        dpi,
        text_background,
    )
}

#[test]
fn the_text_background_switch_is_a_square_button_after_the_style_controls() {
    let item = ToolbarItem::TextBackground;
    let without = text_toolbar(None, 96);
    assert!(without.buttons.iter().all(|button| button.item != item));
    for on in [false, true] {
        let toolbar = text_toolbar(Some(on), 96);
        assert_eq!(
            toolbar.action_bounds.width() - without.action_bounds.width(),
            32 + 8,
            "the button and its gap"
        );
        let button = toolbar.buttons.iter().find(|b| b.item == item).unwrap();
        assert_eq!(button.is_checked, on);
        assert!(button.is_enabled);
        assert_eq!((button.rect.width(), button.rect.height()), (32, 32));
        assert_eq!(toolbar.hit_test(centre(button.rect)), Some(item));
        let swatch = rect_of(&toolbar, ToolbarItem::Color);
        let save = rect_of(&toolbar, ToolbarItem::Action(ToolbarAction::Save));
        assert!(swatch.right < button.rect.left && button.rect.right < save.left);
    }
    assert_eq!(shortcut(item), Some("Ctrl+B"));
}

#[test]
fn the_text_background_icon_is_a_letter_in_a_box_that_fills_while_it_is_on() {
    let _theme = ForcedTheme::new(ThemePreference::Light);
    let t = tokens_for(Theme::Light);
    let item = ToolbarItem::TextBackground;
    for on in [false, true] {
        let toolbar = text_toolbar(Some(on), DPI);
        let canvas = render(&toolbar, None);
        let rect = rect_of(&toolbar, item);
        let (cx, cy) = centre(rect);
        let inner = Rect::new(cx - 14, cy - 14, cx + 14, cy + 14);
        // Sampled inside the box, left of the letter.
        let inside = canvas.pixel(cx - 16, cy);
        if on {
            assert_color(fill_of(&canvas, rect), t.accent_tint, 2, "selected button");
            assert_color(inside, t.accent_ink, 3, "filled box");
            assert_ink(
                &canvas,
                inner,
                t.accent_ink,
                t.accent_tint,
                &[t.accent_ink],
                "letter",
            );
        } else {
            assert_color(fill_of(&canvas, rect), t.card, 2, "idle button");
            assert_color(inside, t.card, 3, "empty box");
            assert_ink(&canvas, inner, t.card, t.text, &[t.accent_ink], "letter");
        }
    }
}

#[test]
fn hovering_the_text_background_switch_shows_a_tooltip_outside_the_button() {
    let viewport = Rect::new(0, 0, 1100, 700);
    let mut toolbar = Toolbar::layout(
        &Rect::new(100, 300, 500, 560),
        viewport,
        ToolKind::Text,
        PRESET_COLORS[0],
        5,
        false,
        true,
        false,
        false,
        true,
        true,
        96,
        Some(false),
    );
    let resting = render_in(&toolbar, viewport, None).pixels();
    toolbar.hovered_item = Some(ToolbarItem::TextBackground);
    let changed = render_in(&toolbar, viewport, None)
        .changed_since(&resting)
        .expect("hovering changes the picture");
    let button = rect_of(&toolbar, ToolbarItem::TextBackground);
    assert!(
        !within(changed, button),
        "the tooltip opens beside the button"
    );
    assert!(within(changed, viewport));
}

#[test]
fn the_command_menu_lists_the_text_background_switch_under_text() {
    let _english = English::new();
    let toolbar = text_toolbar(Some(true), 96);
    let menu = build_command_menu(&toolbar.buttons)
        .expect("the menu is built")
        .expect("there are buttons");
    let index = toolbar
        .buttons
        .iter()
        .position(|button| button.item == ToolbarItem::TextBackground)
        .unwrap();
    let id = index as u32 + 1;
    assert_eq!(
        menu_string(menu.menu.0, id, MF_BYCOMMAND),
        "Text background"
    );
    assert_ne!(
        unsafe { GetMenuState(menu.menu.0, id, MF_BYCOMMAND) } & MF_CHECKED.0,
        0
    );
    assert_eq!(menu.item(id as i32), Some(ToolbarItem::TextBackground));
    // It opens its own group, headed "Text".
    let position = (0..unsafe { GetMenuItemCount(menu.menu.0) })
        .find(|&position| unsafe { GetMenuItemID(menu.menu.0, position) } == id)
        .expect("the item is in the menu");
    let heading = menu_string(menu.menu.0, position as u32 - 1, MF_BYPOSITION);
    assert_eq!(heading, "Text");
}

#[test]
fn the_step_icon_is_a_filled_badge_with_its_number_in_the_surface_colour() {
    let canvas = Canvas::new(64, 64, WHITE);
    let rect = Rect::new(0, 0, 64, 64);
    draw_tool_icon(canvas.hdc, rect, ToolKind::Step, RED, GREEN, DPI);
    assert_color(canvas.pixel(32 - 14, 32), RED, 2, "badge");
    assert_color(canvas.pixel(32, 32 - 14), RED, 2, "badge");
    assert_eq!(canvas.pixel(32 - 22, 32), rgb(WHITE), "outside the badge");
    let canvas = &canvas;
    let badge = (14..50)
        .flat_map(|y| (14..50).map(move |x| canvas.pixel(x, y)))
        .collect::<Vec<Rgb>>();
    assert!(
        badge.iter().any(|&pixel| distance(pixel, rgb(GREEN)) == 0),
        "the numeral is drawn in the surface colour"
    );
}

#[test]
fn the_ellipse_icon_is_a_hollow_outline_and_the_line_icon_a_diagonal_stroke() {
    let rect = Rect::new(0, 0, 64, 64);
    let ellipse = Canvas::new(64, 64, WHITE);
    draw_tool_icon(ellipse.hdc, rect, ToolKind::Ellipse, RED, GREEN, DPI);
    // The 28 x 20 outline is solid on its left vertex and hollow inside; the corners of
    // its box are empty.
    assert_color(ellipse.pixel(18, 32), RED, 2, "outline");
    assert_eq!(ellipse.pixel(32, 32), rgb(WHITE), "hollow");
    assert_eq!(ellipse.pixel(19, 23), rgb(WHITE), "corner");

    let line = Canvas::new(64, 64, WHITE);
    draw_tool_icon(line.hdc, rect, ToolKind::Line, RED, GREEN, DPI);
    // Rising from bottom left to top right through the centre.
    assert_color(line.pixel(31, 32), RED, 2, "stroke");
    assert_color(line.pixel(24, 39), RED, 2, "stroke");
    assert_eq!(line.pixel(20, 20), rgb(WHITE), "above the stroke");
    assert_eq!(line.pixel(44, 44), rgb(WHITE), "below the stroke");
}

#[test]
fn every_other_tool_has_its_own_centred_glyph_in_the_ink_colour() {
    let rect = Rect::new(0, 0, 64, 64);
    let tools = [
        ToolKind::Select,
        ToolKind::Rectangle,
        ToolKind::Arrow,
        ToolKind::Ellipse,
        ToolKind::Line,
        ToolKind::Pen,
        ToolKind::Highlight,
        ToolKind::Text,
        ToolKind::Blur,
        ToolKind::Redact,
        ToolKind::ColorPicker,
    ];
    let renders: Vec<Vec<Rgb>> = tools
        .iter()
        .map(|&tool| {
            let canvas = Canvas::new(64, 64, WHITE);
            draw_tool_icon(canvas.hdc, rect, tool, RED, WHITE, DPI);
            let (left, top, right, bottom) = canvas
                .changed_since(&vec![rgb(WHITE); 64 * 64])
                .expect("the glyph is drawn");
            assert!(
                ((left + right) / 2 - 32).abs() <= 8 && ((top + bottom) / 2 - 32).abs() <= 8,
                "{tool:?} is centred: {left},{top},{right},{bottom}"
            );
            let mark = strongest(&canvas, rect, WHITE);
            assert!(
                mark[0] > 200 && mark[1] < 120 && mark[2] < 120,
                "{tool:?} is drawn in the ink: {mark:?}"
            );
            canvas.pixels()
        })
        .collect();
    for (index, first) in renders.iter().enumerate() {
        for (offset, second) in renders[index + 1..].iter().enumerate() {
            assert_ne!(
                first,
                second,
                "{:?} and {:?}",
                tools[index],
                tools[index + 1 + offset]
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Command menu
// ---------------------------------------------------------------------------

/// Pins the interface language to English for one test.
struct English {
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl English {
    fn new() -> Self {
        let lock = crate::i18n::TEST_LANGUAGE_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        crate::i18n::set_preference(crate::i18n::LanguagePreference::English);
        Self { _lock: lock }
    }
}

impl Drop for English {
    fn drop(&mut self) {
        crate::i18n::set_preference(crate::i18n::LanguagePreference::System);
    }
}

fn menu_string(menu: HMENU, id_or_position: u32, flags: MENU_ITEM_FLAGS) -> String {
    let mut buffer = [0u16; 128];
    let length = unsafe { GetMenuStringW(menu, id_or_position, Some(&mut buffer), flags) };
    String::from_utf16_lossy(&buffer[..length as usize])
}

/// What the menu shows for each item (English).
fn menu_label(item: ToolbarItem) -> String {
    use ToolbarAction::*;
    use ToolbarItem::*;
    match item {
        Tool(ToolKind::Select) => "Select/move".into(),
        Tool(ToolKind::Rectangle) => "Frame".into(),
        Tool(ToolKind::Arrow) => "Arrow".into(),
        Tool(ToolKind::Ellipse) => "Ellipse".into(),
        Tool(ToolKind::Line) => "Line".into(),
        Tool(ToolKind::Pen) => "Pen".into(),
        Tool(ToolKind::Highlight) => "Highlighter".into(),
        Tool(ToolKind::Step) => "Numbered step".into(),
        Tool(ToolKind::Text) => "Text".into(),
        Tool(ToolKind::Blur) => "Blur (not secure redaction)".into(),
        Tool(ToolKind::Redact) => "Redact (opaque)".into(),
        Tool(ToolKind::ColorPicker) => "Pick color".into(),
        MoreTools => "Show or hide extra tools".into(),
        Action(Undo) => "Undo".into(),
        Action(Redo) => "Redo".into(),
        Action(Save) => "Save screenshot".into(),
        Action(Copy) => "Copy to clipboard".into(),
        Action(Upload) => "Upload and copy link".into(),
        Action(Pin) => "Pin to screen".into(),
        Action(CopyText) => "Copy text".into(),
        Action(RedactSensitive) => "Redact emails and phone numbers".into(),
        Action(Settings) => "Settings".into(),
        Action(Cancel) => "Cancel".into(),
        Color => "Choose color…".into(),
        Thickness => "Line width…".into(),
        TextBackground => "Text background".into(),
        ThicknessOption(value) => format!("{value} px"),
    }
}

#[test]
fn the_command_menu_lists_every_button_under_group_headings() {
    let _english = English::new();
    // Expanded rail, open wheel: every kind of item is present. Undo is disabled;
    // Rectangle, "more tools", the width button and the active width are checked.
    let toolbar = standard(true, true);
    let menu = build_command_menu(&toolbar.buttons)
        .expect("the menu is built")
        .expect("there are buttons");
    let hmenu = menu.menu.0;
    let count = unsafe { GetMenuItemCount(hmenu) };

    let mut headings = Vec::new();
    for position in 0..count {
        if unsafe { GetMenuItemID(hmenu, position) } == 0 {
            let state = unsafe { GetMenuState(hmenu, position as u32, MF_BYPOSITION) };
            assert_ne!(state & MF_GRAYED.0, 0, "headings are not selectable");
            headings.push(menu_string(hmenu, position as u32, MF_BYPOSITION));
        }
    }
    assert_eq!(
        headings,
        [
            "Tools",
            "More tools",
            "History",
            "Color",
            "Line width",
            "Commands",
            "Line width"
        ]
    );
    assert_eq!(count as usize, headings.len() + toolbar.buttons.len());

    let state_of = |item: ToolbarItem| {
        let index = toolbar
            .buttons
            .iter()
            .position(|button| button.item == item);
        unsafe {
            GetMenuState(
                hmenu,
                index.expect("the item is listed") as u32 + 1,
                MF_BYCOMMAND,
            )
        }
    };
    for (index, button) in toolbar.buttons.iter().enumerate() {
        let id = index as u32 + 1;
        assert_eq!(
            menu_string(hmenu, id, MF_BYCOMMAND),
            menu_label(button.item),
            "{:?}",
            button.item
        );
        let state = state_of(button.item);
        assert_eq!(
            state & MF_GRAYED.0 != 0,
            !button.is_enabled,
            "{:?}",
            button.item
        );
        assert_eq!(
            state & MF_CHECKED.0 != 0,
            button.is_checked,
            "{:?}",
            button.item
        );
        assert_eq!(menu.item(id as i32), Some(button.item));
    }
    let grayed = |item| state_of(item) & MF_GRAYED.0 != 0;
    let checked = |item| state_of(item) & MF_CHECKED.0 != 0;
    assert!(grayed(ToolbarItem::Action(ToolbarAction::Undo)));
    assert!(!grayed(ToolbarItem::Action(ToolbarAction::Redo)));
    assert!(checked(ToolbarItem::Tool(ToolKind::Rectangle)));
    assert!(!checked(ToolbarItem::Tool(ToolKind::Select)));
    assert!(checked(ToolbarItem::MoreTools));
    assert!(checked(ToolbarItem::Thickness));
    assert!(checked(ToolbarItem::ThicknessOption(5)));
    assert!(!checked(ToolbarItem::ThicknessOption(4)));
}

#[test]
fn a_command_id_selects_the_item_behind_it_and_dismissal_selects_nothing() {
    let toolbar = standard(false, false);
    let menu = build_command_menu(&toolbar.buttons)
        .expect("the menu is built")
        .expect("there are buttons");
    let last = toolbar.buttons.len() as i32;
    assert_eq!(menu.item(1), Some(toolbar.buttons[0].item));
    assert_eq!(
        menu.item(last),
        Some(toolbar.buttons[last as usize - 1].item)
    );
    assert_eq!(
        menu.item(0),
        None,
        "TrackPopupMenuEx returns 0 when dismissed"
    );
    assert_eq!(menu.item(-1), None);
    assert_eq!(menu.item(last + 1), None);
}

#[test]
fn the_command_menu_lists_pin_to_screen_by_its_label() {
    let _english = English::new();
    let pin = ToolbarItem::Action(ToolbarAction::Pin);
    let buttons = [ToolbarButton {
        item: pin,
        rect: Rect::default(),
        is_enabled: true,
        is_checked: false,
    }];
    let menu = build_command_menu(&buttons)
        .expect("the menu is built")
        .expect("there is a button");
    assert_eq!(menu_string(menu.menu.0, 1, MF_BYCOMMAND), "Pin to screen");
    assert_eq!(menu_label(pin), "Pin to screen");
    assert_eq!(menu.item(1), Some(pin));
}

#[test]
fn the_pin_command_has_its_own_glyph_and_a_tooltip() {
    let mut toolbar = standard(false, false);
    let cancel = ToolbarItem::Action(ToolbarAction::Cancel);
    let rect = rect_of(&toolbar, cancel);
    let before = render(&toolbar, None).pixels();
    toolbar
        .buttons
        .iter_mut()
        .find(|button| button.item == cancel)
        .expect("the Cancel button")
        .item = ToolbarItem::Action(ToolbarAction::Pin);
    let canvas = render(&toolbar, None);
    let glyph = canvas
        .changed_since(&before)
        .expect("the Pin glyph replaces the Cancel glyph");
    assert!(within(glyph, rect), "{glyph:?} stays inside the button");
    let resting = canvas.pixels();
    toolbar.hovered_item = Some(ToolbarItem::Action(ToolbarAction::Pin));
    let hovered = render(&toolbar, None)
        .changed_since(&resting)
        .expect("hovering changes the picture");
    assert!(
        !within(hovered, rect),
        "the tooltip opens beside the button"
    );
}

#[test]
fn there_is_no_menu_without_buttons() {
    assert!(build_command_menu(&[]).expect("no error").is_none());
}

const RECOGNITION_COMMANDS: [ToolbarAction; 2] =
    [ToolbarAction::CopyText, ToolbarAction::RedactSensitive];

#[test]
fn text_recognition_commands_draw_a_glyph_and_a_tooltip_when_given_a_button() {
    let viewport = Rect::new(0, 0, 1100, 700);
    let mut toolbar = toolbar_for(Rect::new(100, 300, 500, 560), viewport, false, false, 96);
    let bare = render_in(&toolbar, viewport, None).pixels();
    let rect = Rect::new(700, 100, 744, 142);
    for action in RECOGNITION_COMMANDS {
        let item = ToolbarItem::Action(action);
        toolbar.buttons.push(ToolbarButton {
            item,
            rect,
            is_enabled: true,
            is_checked: false,
        });
        let resting = render_in(&toolbar, viewport, None);
        let drawn = resting.changed_since(&bare).expect("the button is drawn");
        assert!(within(drawn, rect), "{item:?} draws inside its button");
        toolbar.hovered_item = Some(item);
        let tooltip = render_in(&toolbar, viewport, None)
            .changed_since(&resting.pixels())
            .expect("hovering changes the picture");
        assert!(!within(tooltip, rect), "{item:?} has a tooltip");
        assert!(within(tooltip, viewport), "{tooltip:?} leaves the viewport");
        toolbar.hovered_item = None;
        toolbar.buttons.pop();
    }
}

#[test]
fn text_recognition_commands_join_the_commands_group_of_the_menu() {
    let _english = English::new();
    let toolbar = standard(false, false);
    let mut buttons = toolbar.buttons.clone();
    let upload = buttons
        .iter()
        .position(|button| button.item == ToolbarItem::Action(ToolbarAction::Upload))
        .expect("Upload is a button");
    for (offset, action) in RECOGNITION_COMMANDS.into_iter().enumerate() {
        buttons.insert(
            upload + 1 + offset,
            ToolbarButton {
                item: ToolbarItem::Action(action),
                rect: Rect::default(),
                is_enabled: true,
                is_checked: false,
            },
        );
    }
    let menu = build_command_menu(&buttons)
        .expect("the menu is built")
        .expect("there are buttons");
    let hmenu = menu.menu.0;
    for (index, button) in buttons.iter().enumerate() {
        assert_eq!(
            menu_string(hmenu, index as u32 + 1, MF_BYCOMMAND),
            menu_label(button.item),
            "{:?}",
            button.item
        );
    }
    let headings = (0..unsafe { GetMenuItemCount(hmenu) })
        .filter(|&position| unsafe { GetMenuItemID(hmenu, position) } == 0)
        .count();
    let without = build_command_menu(&toolbar.buttons)
        .expect("the menu is built")
        .expect("there are buttons");
    let headings_without = (0..unsafe { GetMenuItemCount(without.menu.0) })
        .filter(|&position| unsafe { GetMenuItemID(without.menu.0, position) } == 0)
        .count();
    assert_eq!(headings, headings_without, "no extra group heading");
}
