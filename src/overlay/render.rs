use super::*;

/// Bounding box of `points`, one pixel wide at least.
fn points_extent(points: &[(i32, i32)]) -> Rect {
    let Some(&(x, y)) = points.first() else {
        return Rect::default();
    };
    points
        .iter()
        .fold(Rect::new(x, y, x + 1, y + 1), |extent, &(px, py)| {
            Rect::new(
                extent.left.min(px),
                extent.top.min(py),
                extent.right.max(px + 1),
                extent.bottom.max(py + 1),
            )
        })
}

/// Outline colour that keeps a caret of `color` visible: dark around light carets,
/// white around dark ones.
fn caret_halo(color: [u8; 4]) -> [u8; 4] {
    let [blue, green, red, _] = color;
    let luma = (299 * red as u32 + 587 * green as u32 + 114 * blue as u32) / 1000;
    if luma >= 128 {
        crate::theme::BRAND_INK_BGRA
    } else {
        [255, 255, 255, 255]
    }
}

impl OverlayState {
    /// What the size readout measures: the region being selected, else the hovered window.
    fn hud_target(&self) -> Option<Rect> {
        self.committed_selection.or(self.hover_snap_rect)
    }

    fn render_capture_hud(&self) {
        let (Some(selection), Some(bounds)) = (self.hud_target(), self.hud_rect) else {
            return;
        };
        let scale = |value: i32| (value * self.dpi as i32 / 96).max(1);
        let tokens = crate::theme::tokens();
        let label = format!("{} × {} px", selection.width(), selection.height());
        // The readout is only a handle to move the selection once there is one to move.
        let hovered = self.mode == OverlayMode::SelectionActive
            && bounds.contains(self.pointer.0, self.pointer.1);
        crate::drawing::elevation(
            self.mem_dc,
            bounds,
            scale(crate::theme::RADIUS_OVERLAY),
            self.dpi,
        );
        crate::drawing::rounded(
            self.mem_dc,
            bounds,
            crate::drawing::corner_diameter(crate::theme::RADIUS_OVERLAY, self.dpi),
            if hovered {
                tokens.accent_tint
            } else {
                tokens.card
            },
            if hovered {
                tokens.accent_ink
            } else {
                tokens.stroke
            },
        );
        crate::drawing::label(self.mem_dc, bounds, &label, scale(13), tokens.text, true);
    }

    /// Places the size readout after the toolbar layout of this frame, clear of the
    /// panels, the wheel and every selection grip.
    fn place_capture_hud(&mut self) {
        self.hud_rect = self.hud_target().map(|selection| {
            let obstacles = self
                .toolbar
                .as_ref()
                .map_or([Rect::default(); 3], |toolbar| {
                    [
                        toolbar.tool_bounds,
                        toolbar.action_bounds,
                        toolbar.wheel_bounds.unwrap_or_default(),
                    ]
                });
            place_capture_hud(
                selection,
                selection_work_viewport(&self.capture, selection),
                self.dpi,
                &obstacles,
            )
        });
    }

    /// Places the notice of this frame beside the action bar, clear of the tool rail, the
    /// width wheel and the readout. Without a notice or a toolbar there is nothing to place.
    fn place_notice(&mut self) {
        self.notice_rect =
            self.notice
                .as_deref()
                .zip(self.toolbar.as_ref())
                .map(|(text, toolbar)| {
                    let scale = |value: i32| (value * self.dpi as i32 / 96).max(1);
                    let width = crate::drawing::measure_text(text, -scale(13)).0 + scale(12) * 2;
                    let selection = self.committed_selection.unwrap_or_default();
                    place_notice(
                        (width, scale(30)),
                        toolbar.action_bounds,
                        selection,
                        selection_work_viewport(&self.capture, selection),
                        &[
                            toolbar.tool_bounds,
                            toolbar.wheel_bounds.unwrap_or_default(),
                            self.hud_rect.unwrap_or_default(),
                        ],
                        scale(8),
                    )
                });
    }

    /// The notice is an amber card with ink text, the look of a primary button.
    fn render_notice(&self) {
        let (Some(text), Some(bounds)) = (self.notice.as_deref(), self.notice_rect) else {
            return;
        };
        let scale = |value: i32| (value * self.dpi as i32 / 96).max(1);
        let tokens = crate::theme::tokens();
        crate::drawing::elevation(
            self.mem_dc,
            bounds,
            scale(crate::theme::RADIUS_OVERLAY),
            self.dpi,
        );
        crate::drawing::rounded(
            self.mem_dc,
            bounds,
            crate::drawing::corner_diameter(crate::theme::RADIUS_OVERLAY, self.dpi),
            tokens.accent,
            tokens.accent,
        );
        crate::drawing::label(
            self.mem_dc,
            bounds,
            text,
            scale(13),
            tokens.accent_text,
            true,
        );
    }

    /// Runs GDI drawing clipped to `clip`.
    fn with_clip(&self, clip: Rect, draw: impl FnOnce()) {
        unsafe {
            let saved = SaveDC(self.mem_dc);
            let _ = IntersectClipRect(self.mem_dc, clip.left, clip.top, clip.right, clip.bottom);
            draw();
            let _ = RestoreDC(self.mem_dc, saved);
        }
    }

    /// Restores the dimmed capture in the parts of `area` outside `sel`.
    fn restore_outside(&self, buffer: &mut [u8], area: Rect, sel: Rect) {
        for strip in [
            Rect::new(area.left, area.top, area.right, sel.top),
            Rect::new(area.left, sel.bottom, area.right, area.bottom),
            Rect::new(area.left, sel.top, sel.left, sel.bottom),
            Rect::new(sel.right, sel.top, area.right, sel.bottom),
        ] {
            self.capture.restore_dimmed(buffer, &strip);
        }
    }

    /// Draws a CPU effect so it stays inside `sel`: blur and redaction are clamped before
    /// they run (they would otherwise average the dimmed margin), a highlight is trimmed after.
    fn render_effect(&self, obj: &AnnotationObject, buffer: &mut [u8], sel: Rect) {
        let (width, height) = (self.capture.width, self.capture.height);
        match &obj.kind {
            AnnotationKind::Blur { rect, block_size } => {
                let rect = intersect_rects(*rect, sel);
                if !rect.is_empty() {
                    let kind = AnnotationKind::Blur {
                        rect,
                        block_size: *block_size,
                    };
                    AnnotationObject::new(obj.id, kind).render_blur(buffer, width, height);
                }
            }
            AnnotationKind::Redact { rect } => {
                let rect = intersect_rects(*rect, sel);
                if !rect.is_empty() {
                    AnnotationObject::new(obj.id, AnnotationKind::Redact { rect })
                        .render_blur(buffer, width, height);
                }
            }
            AnnotationKind::Highlight { .. } => {
                obj.render_blur(buffer, width, height);
                self.restore_outside(buffer, obj.bounds(), sel);
            }
            _ => {}
        }
    }

    /// In-progress content and the live text glyphs; they sit with the other content so
    /// the redaction pass covers them exactly like the exported image.
    fn render_live_content(&self, buffer: &mut [u8], sel: Rect, policy: CompositionPolicy) {
        let (width, height) = (self.capture.width, self.capture.height);
        if policy.in_progress_preview {
            match &self.drawing_shape {
                Some(InProgressDrawing::Blur {
                    start,
                    current,
                    redact,
                }) => {
                    unsafe {
                        let _ = GdiFlush();
                    }
                    let rect = Rect::normalized(*start, *current);
                    let kind = if *redact {
                        AnnotationKind::Redact { rect }
                    } else {
                        AnnotationKind::Blur {
                            rect,
                            block_size: DEFAULT_BLUR_BLOCK,
                        }
                    };
                    self.render_effect(&AnnotationObject::new(0, kind), buffer, sel);
                }
                Some(InProgressDrawing::Rectangle { start, current }) => {
                    AnnotationObject::new(
                        0,
                        AnnotationKind::Rectangle {
                            rect: Rect::normalized(*start, *current),
                            color: self.active_color,
                            thickness: self.active_thickness,
                        },
                    )
                    .render_gdi(self.mem_dc);
                }
                Some(InProgressDrawing::Ellipse { start, current }) => {
                    AnnotationObject::new(
                        0,
                        AnnotationKind::Ellipse {
                            rect: Rect::normalized(*start, *current),
                            color: self.active_color,
                            thickness: self.active_thickness,
                        },
                    )
                    .render_gdi(self.mem_dc);
                }
                Some(InProgressDrawing::Arrow { start, current }) => {
                    AnnotationObject::new(
                        0,
                        AnnotationKind::Arrow {
                            start: *start,
                            end: *current,
                            color: self.active_color,
                            thickness: self.active_thickness,
                        },
                    )
                    .render_gdi(self.mem_dc);
                }
                Some(InProgressDrawing::Line { start, current }) => {
                    AnnotationObject::new(
                        0,
                        AnnotationKind::Line {
                            start: *start,
                            end: *current,
                            color: self.active_color,
                            thickness: self.active_thickness,
                        },
                    )
                    .render_gdi(self.mem_dc);
                }
                Some(InProgressDrawing::Pen { points }) => {
                    if self.active_tool == ToolKind::Highlight {
                        unsafe {
                            let _ = GdiFlush();
                        }
                        crate::annotation::render_highlight(
                            buffer,
                            width,
                            height,
                            points,
                            self.active_color,
                            self.active_thickness,
                        );
                        let radius = self.active_thickness.clamp(1, 64) * 2 + 1;
                        self.restore_outside(
                            buffer,
                            points_extent(points).inflate(radius, radius),
                            sel,
                        );
                    } else {
                        render_pen_preview(
                            self.mem_dc,
                            points,
                            self.active_color,
                            self.active_thickness,
                        );
                    }
                }
                None => {}
            }
        }
        if policy.caret
            && let Some(edit) = &self.text_edit
        {
            crate::drawing::text(
                self.mem_dc,
                edit.pos,
                &edit.text,
                edit.font_size,
                bgra_to_colorref(edit.color),
            );
        }
    }

    /// Dotted amber frame, the style of the selected-object indicator.
    fn draw_text_frame(&self, frame: Rect) {
        use windows::Win32::Graphics::Gdi::{
            GetStockObject, NULL_BRUSH, PS_DOT, Rectangle, SetBkMode, TRANSPARENT,
        };
        crate::drawing::with_pen(
            self.mem_dc,
            PS_DOT,
            1,
            crate::theme::BRAND_AMBER,
            || unsafe {
                // The gaps of a dotted pen take the background colour unless it is transparent.
                let _ = SetBkMode(self.mem_dc, TRANSPARENT);
                let previous = SelectObject(self.mem_dc, GetStockObject(NULL_BRUSH));
                let _ = Rectangle(
                    self.mem_dc,
                    frame.left,
                    frame.top,
                    frame.right,
                    frame.bottom,
                );
                SelectObject(self.mem_dc, previous);
            },
        );
    }

    /// Text box frame, selection highlight and caret: editor chrome drawn above the redactions.
    fn render_text_chrome(&self, buffer: &mut [u8], sel: Rect, edit: &TextEditState) {
        let (width, height) = (self.capture.width, self.capture.height);
        let bounds = edit.bounds();
        if let Some(range) = edit.selection() {
            let prefix: String = edit.text.chars().take(range.start).collect();
            let selected: String = edit.text.chars().take(range.end).collect();
            let left = if prefix.is_empty() {
                0
            } else {
                crate::drawing::measure_text(&prefix, edit.font_size).0
            };
            let right = crate::drawing::measure_text(&selected, edit.font_size).0;
            let highlight = Rect::new(
                edit.pos.0 + left,
                bounds.top,
                edit.pos.0 + right,
                bounds.bottom,
            );
            crate::drawing::rounded(
                self.mem_dc,
                highlight,
                0,
                crate::theme::TEXT_SELECTION,
                crate::theme::TEXT_SELECTION,
            );
            // The highlight covers the glyphs drawn earlier, so the selected run is drawn again.
            self.with_clip(intersect_rects(highlight, sel), || {
                crate::drawing::text(
                    self.mem_dc,
                    edit.pos,
                    &edit.text,
                    edit.font_size,
                    bgra_to_colorref(edit.color),
                );
            });
        }
        let margin = super::text::text_box_margin(self.dpi);
        self.draw_text_frame(bounds.inflate(margin, margin));
        if edit.caret_visible {
            let x = edit.pos.0 + edit.caret_offset();
            let caret = Rect::new(
                x,
                bounds.top,
                x + (self.dpi / 96).max(1) as i32,
                bounds.bottom,
            );
            unsafe {
                let _ = GdiFlush();
            }
            let halo = caret.inflate(1, 1);
            CaptureBuffer::fill_rect(buffer, width, height, &halo, caret_halo(edit.color));
            CaptureBuffer::fill_rect(buffer, width, height, &caret, edit.color);
        }
    }

    pub(super) fn composite_scene(&mut self) {
        self.composite_scene_with(CompositionPolicy::EDITOR);
    }

    pub(super) fn composite_scene_with(&mut self, policy: CompositionPolicy) {
        assert!(
            !self.bits_ptr.is_null(),
            "Overlay pixel buffer is not initialized"
        );
        // Synchronize the DIB's GDI target before replacing its pixels on the CPU.
        unsafe {
            let _ = GdiFlush();
        }
        // Grips, toolbar and readout all follow the DPI of the monitor holding the selection.
        if policy.toolbar_and_tooltip
            && let Some(selection) = self.hud_target()
        {
            self.dpi = selection_monitor_dpi(&self.capture, selection, self.dpi);
        }
        let width = self.capture.width;
        let height = self.capture.height;
        let len = (width as usize) * (height as usize) * 4;
        let buffer = unsafe { std::slice::from_raw_parts_mut(self.bits_ptr, len) };

        match self.mode {
            OverlayMode::Hovering | OverlayMode::DraggingSelection => {
                buffer.copy_from_slice(&self.capture.dimmed);

                // Until a drag leaves the click threshold the hovered window stays highlighted.
                if let Some(rect) = self.committed_selection.or(self.hover_snap_rect) {
                    self.capture.punch_out(buffer, &rect);
                    CaptureBuffer::draw_border(
                        buffer,
                        width,
                        height,
                        &rect.selection_frame(self.dpi),
                        crate::theme::BRAND_AMBER_BGRA,
                        Rect::selection_frame_thickness(self.dpi),
                    );
                }
            }
            OverlayMode::SelectionActive => {
                let Some(sel) = self.committed_selection else {
                    return;
                };

                if policy == CompositionPolicy::EDITOR && self.base_cache.len() == len {
                    buffer.copy_from_slice(&self.base_cache);
                } else {
                    buffer.copy_from_slice(&self.capture.dimmed);
                    self.capture.punch_out(buffer, &sel);
                    // Composite in object order, clipped to the exported region. CPU
                    // effects see preceding GDI annotations.
                    self.with_clip(sel, || {
                        for obj in &self.objects {
                            if policy.caret
                                && self
                                    .text_edit
                                    .as_ref()
                                    .is_some_and(|edit| edit.editing_id == Some(obj.id))
                            {
                                continue;
                            }
                            if matches!(
                                obj.kind,
                                AnnotationKind::Blur { .. } | AnnotationKind::Highlight { .. }
                            ) {
                                unsafe {
                                    let _ = GdiFlush();
                                }
                                self.render_effect(obj, buffer, sel);
                            } else {
                                obj.render_gdi(self.mem_dc);
                            }
                        }
                    });
                    unsafe {
                        let _ = GdiFlush();
                    }
                    if policy == CompositionPolicy::EDITOR
                        && self.cache_requested
                        && !self.objects.is_empty()
                        && len <= 32 * 1024 * 1024
                    {
                        self.base_cache.clear();
                        self.base_cache.extend_from_slice(buffer);
                    }
                }

                self.with_clip(sel, || self.render_live_content(buffer, sel, policy));

                // Opaque redactions cover all content; only the editor chrome stays above them.
                unsafe {
                    let _ = GdiFlush();
                }
                for obj in &self.objects {
                    if matches!(obj.kind, AnnotationKind::Redact { .. }) {
                        self.render_effect(obj, buffer, sel);
                    }
                }

                if policy.selection_frame {
                    CaptureBuffer::draw_contrast_selection(
                        buffer,
                        width,
                        height,
                        &sel,
                        crate::theme::BRAND_AMBER_BGRA,
                        self.dpi,
                    );
                }

                if policy.selected_handles
                    && let Some(obj) = self
                        .selected_id
                        .and_then(|id| self.objects.iter().find(|o| o.id == id))
                {
                    obj.render_selection_indicator(self.mem_dc, self.dpi);
                }

                if policy.caret
                    && let Some(text_edit) = &self.text_edit
                {
                    self.render_text_chrome(buffer, sel, text_edit);
                }

                // Render the contextual L-shaped editor toolbar.
                if policy.toolbar_and_tooltip {
                    let can_undo = self.history.can_undo();
                    let can_redo = self.history.can_redo();
                    let selected = if self.active_tool == ToolKind::Select {
                        self.selected_id
                            .and_then(|id| self.objects.iter().find(|object| object.id == id))
                    } else {
                        None
                    };
                    let (toolbar_color, toolbar_thickness, show_color, show_thickness) =
                        if let Some(object) = selected {
                            (
                                object.get_color().unwrap_or(self.active_color),
                                object.get_thickness().unwrap_or(self.active_thickness),
                                object.get_color().is_some(),
                                object.get_thickness().is_some(),
                            )
                        } else {
                            match self.active_tool {
                                ToolKind::Rectangle
                                | ToolKind::Ellipse
                                | ToolKind::Arrow
                                | ToolKind::Line
                                | ToolKind::Pen
                                | ToolKind::Highlight => {
                                    (self.active_color, self.active_thickness, true, true)
                                }
                                ToolKind::Text | ToolKind::Step => {
                                    (self.active_color, self.active_thickness, true, false)
                                }
                                ToolKind::Blur
                                | ToolKind::Redact
                                | ToolKind::Select
                                | ToolKind::ColorPicker => {
                                    (self.active_color, self.active_thickness, false, false)
                                }
                            }
                        };
                    let viewport = selection_work_viewport(&self.capture, sel);
                    let mut tb = Toolbar::layout(
                        &sel,
                        viewport,
                        self.active_tool,
                        toolbar_color,
                        toolbar_thickness,
                        self.tools_expanded,
                        show_color,
                        show_thickness,
                        self.thickness_input.is_some(),
                        can_undo,
                        can_redo,
                        self.dpi,
                    );
                    if let Some(existing) = &self.toolbar {
                        let still_shown = |item: Option<ToolbarItem>| {
                            item.filter(|item| tb.buttons.iter().any(|button| button.item == *item))
                        };
                        let hovered = still_shown(existing.hovered_item);
                        let pressed = still_shown(existing.pressed_item);
                        tb.hovered_item = hovered;
                        tb.pressed_item = pressed;
                    }
                    tb.render(self.mem_dc, self.thickness_input.as_deref());
                    self.toolbar = Some(tb);
                }
            }
        }
        if policy.toolbar_and_tooltip {
            self.place_capture_hud();
            self.render_capture_hud();
            self.place_notice();
            self.render_notice();
        }
        // Make the fully rebuilt backbuffer visible to the subsequent WM_PAINT copy.
        unsafe {
            let _ = GdiFlush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caret_halo_contrasts_with_the_caret_colour() {
        assert_eq!(caret_halo([0, 0, 0, 255]), [255, 255, 255, 255]);
        assert_eq!(
            caret_halo([255, 255, 255, 255]),
            crate::theme::BRAND_INK_BGRA
        );
        // Blue (BGRA order) is dark, amber is light.
        assert_eq!(caret_halo([224, 49, 49, 255]), [255, 255, 255, 255]);
        assert_eq!(
            caret_halo(crate::theme::BRAND_AMBER_BGRA),
            crate::theme::BRAND_INK_BGRA
        );
    }

    #[test]
    fn size_readout_measures_the_selection_else_the_hovered_window() {
        let mut state = OverlayState::create_test_state(Rc::new(CaptureBuffer::dummy(800, 600)));
        assert_eq!(state.hud_target(), None);
        let window = Rect::new(10, 20, 410, 320);
        state.hover_snap_rect = Some(window);
        assert_eq!(state.hud_target(), Some(window));
        let region = Rect::new(50, 60, 150, 160);
        state.committed_selection = Some(region);
        assert_eq!(state.hud_target(), Some(region));
    }

    use crate::overlay::test_support::{DIMMED, backbuffer_state, count_color, pixel};

    const SEL: Rect = Rect::new(100, 80, 300, 200);
    const RED: [u8; 4] = [49, 49, 224, 255];

    fn rgb_at(state: &OverlayState, x: i32, y: i32) -> [u8; 3] {
        let found = pixel(state, x, y);
        [found[0], found[1], found[2]]
    }

    fn scene() -> OverlayState {
        let mut state = backbuffer_state(640, 480);
        state.set_selection_active(SEL);
        state
    }

    #[test]
    fn the_bounding_box_of_points_is_one_pixel_wider_than_the_extremes() {
        assert_eq!(points_extent(&[]), Rect::default());
        assert_eq!(points_extent(&[(5, 7)]), Rect::new(5, 7, 6, 8));
        assert_eq!(
            points_extent(&[(5, 7), (-2, 20), (9, 3)]),
            Rect::new(-2, 3, 10, 21)
        );
    }

    #[test]
    fn hovering_without_a_target_shows_the_dimmed_capture() {
        let mut state = backbuffer_state(640, 480);
        state.composite_scene();
        for (x, y) in [(0, 0), (320, 240), (639, 479)] {
            assert_eq!(rgb_at(&state, x, y), [DIMMED[0], DIMMED[1], DIMMED[2]]);
        }
        assert!(state.toolbar.is_none());
        assert!(state.hud_rect.is_none());
    }

    #[test]
    fn a_hovered_window_is_punched_out_and_framed_in_amber() {
        let mut state = backbuffer_state(640, 480);
        state.hover_snap_rect = Some(Rect::new(50, 60, 150, 140));
        state.composite_scene();
        let amber = crate::theme::BRAND_AMBER_BGRA;
        assert_eq!(rgb_at(&state, 100, 100), [0, 0, 0]);
        assert_eq!(rgb_at(&state, 49, 100), [amber[0], amber[1], amber[2]]);
        assert_eq!(rgb_at(&state, 10, 10), [DIMMED[0], DIMMED[1], DIMMED[2]]);
        assert!(state.hud_rect.is_some());
    }

    #[test]
    fn an_active_selection_without_a_region_draws_nothing() {
        let mut state = backbuffer_state(640, 480);
        state.mode = OverlayMode::SelectionActive;
        state.composite_scene();
        assert_eq!(rgb_at(&state, 10, 10), [0, 0, 0]);
    }

    #[test]
    #[should_panic(expected = "Overlay pixel buffer is not initialized")]
    fn composing_needs_a_pixel_buffer() {
        let mut state = OverlayState::create_test_state(Rc::new(CaptureBuffer::dummy(64, 64)));
        state.composite_scene();
    }

    #[test]
    fn the_editor_scene_draws_the_frame_and_chrome_that_the_export_leaves_out() {
        let mut editor = scene();
        editor.composite_scene();
        let amber = crate::theme::BRAND_AMBER_BGRA;
        assert_eq!(rgb_at(&editor, 200, 140), [0, 0, 0]);
        assert_eq!(rgb_at(&editor, 10, 10), [DIMMED[0], DIMMED[1], DIMMED[2]]);
        assert_eq!(
            rgb_at(&editor, SEL.left - 1, SEL.top + 20),
            [amber[0], amber[1], amber[2]]
        );
        assert!(editor.toolbar.is_some());
        assert!(editor.hud_rect.is_some());

        let mut export = scene();
        export.composite_scene_with(CompositionPolicy::EXPORT);
        assert_eq!(
            rgb_at(&export, SEL.left - 1, SEL.top + 20),
            [DIMMED[0], DIMMED[1], DIMMED[2]]
        );
        assert!(export.toolbar.is_none());
        assert!(export.hud_rect.is_none());
    }

    #[test]
    fn drawing_stays_inside_the_selection() {
        let mut state = scene();
        state.objects.push(AnnotationObject::new(
            1,
            AnnotationKind::Rectangle {
                rect: Rect::new(60, 100, 250, 160),
                color: RED,
                thickness: 4,
            },
        ));
        state.composite_scene();
        assert!(count_color(&state, Rect::new(100, 90, 300, 170), RED) > 0);
        assert_eq!(count_color(&state, Rect::new(54, 100, 66, 160), RED), 0);
    }

    use windows::Win32::Graphics::Gdi::{CreateSolidBrush, FillRect};

    const INK: [u8; 3] = [0x13, 0x14, 0x14];
    const GREEN: [u8; 4] = [10, 200, 10, 255];
    const PLUM: [u8; 4] = [200, 30, 30, 255];

    fn bgr(color: COLORREF) -> [u8; 3] {
        [(color.0 >> 16) as u8, (color.0 >> 8) as u8, color.0 as u8]
    }

    fn dimmed_rgb() -> [u8; 3] {
        [DIMMED[0], DIMMED[1], DIMMED[2]]
    }

    fn solid(state: &OverlayState, value: u8) -> Vec<u8> {
        vec![value; state.capture.width as usize * state.capture.height as usize * 4]
    }

    fn checker(state: &OverlayState) -> Vec<u8> {
        let mut buffer = solid(state, 0);
        for y in 0..state.capture.height {
            for x in 0..state.capture.width {
                if (x + y) % 2 == 0 {
                    let offset = (y as usize * state.capture.width as usize + x as usize) * 4;
                    buffer[offset..offset + 3].fill(200);
                }
            }
        }
        buffer
    }

    fn at(buffer: &[u8], x: i32, y: i32) -> [u8; 3] {
        let offset = (y as usize * 640 + x as usize) * 4;
        [buffer[offset], buffer[offset + 1], buffer[offset + 2]]
    }

    fn rectangle(id: usize, rect: Rect, thickness: i32) -> AnnotationObject {
        AnnotationObject::new(
            id,
            AnnotationKind::Rectangle {
                rect,
                color: RED,
                thickness,
            },
        )
    }

    #[test]
    fn gdi_drawing_is_clipped_and_the_clip_is_restored_afterwards() {
        let state = scene();
        let brush = unsafe { CreateSolidBrush(COLORREF(0x0000_00ff)) };
        let fill = || unsafe {
            FillRect(
                state.mem_dc,
                &WIN_RECT {
                    left: 0,
                    top: 0,
                    right: 100,
                    bottom: 100,
                },
                brush,
            );
        };
        state.with_clip(Rect::new(10, 10, 50, 50), fill);
        assert_eq!(rgb_at(&state, 20, 20), [0, 0, 255]);
        assert_eq!(rgb_at(&state, 60, 60), [0, 0, 0]);
        fill();
        assert_eq!(rgb_at(&state, 60, 60), [0, 0, 255]);
        unsafe {
            let _ = DeleteObject(HGDIOBJ(brush.0));
        }
    }

    #[test]
    fn restoring_outside_the_selection_leaves_the_inside_alone() {
        let state = scene();
        let mut buffer = solid(&state, 77);
        state.restore_outside(
            &mut buffer,
            Rect::new(10, 10, 100, 100),
            Rect::new(30, 30, 70, 70),
        );
        for (x, y) in [(10, 10), (20, 50), (80, 50), (50, 15), (50, 90)] {
            assert_eq!(at(&buffer, x, y), dimmed_rgb(), "({x}, {y})");
        }
        for (x, y) in [(50, 50), (5, 5), (100, 100)] {
            assert_eq!(at(&buffer, x, y), [77, 77, 77], "({x}, {y})");
        }
    }

    #[test]
    fn blur_pixelates_only_the_part_inside_the_selection() {
        let state = scene();
        let mut buffer = checker(&state);
        let before = buffer.clone();
        let blur = |rect| {
            AnnotationObject::new(
                1,
                AnnotationKind::Blur {
                    rect,
                    block_size: 12,
                },
            )
        };
        state.render_effect(&blur(Rect::new(10, 10, 50, 50)), &mut buffer, SEL);
        assert_eq!(
            buffer, before,
            "a blur wholly outside the selection does nothing"
        );
        state.render_effect(&blur(Rect::new(60, 60, 120, 120)), &mut buffer, SEL);
        // Blocks start at the clipped corner; a 12 px block of a checkerboard averages out.
        assert_eq!(at(&buffer, 100, 80), [100, 100, 100]);
        assert_eq!(at(&buffer, 111, 91), [100, 100, 100]);
        assert_eq!(at(&buffer, 70, 70), [200, 200, 200]);
        assert_eq!(at(&buffer, 71, 70), [0, 0, 0]);
    }

    #[test]
    fn redaction_fills_the_selected_part_with_ink() {
        let state = scene();
        let mut buffer = checker(&state);
        let before = buffer.clone();
        let redact = |rect| AnnotationObject::new(1, AnnotationKind::Redact { rect });
        state.render_effect(&redact(Rect::new(10, 10, 50, 50)), &mut buffer, SEL);
        assert_eq!(buffer, before);
        state.render_effect(&redact(Rect::new(60, 60, 150, 150)), &mut buffer, SEL);
        assert_eq!(at(&buffer, 100, 80), INK);
        assert_eq!(at(&buffer, 149, 149), INK);
        assert_eq!(at(&buffer, 99, 100), checker_value(99, 100));
    }

    fn checker_value(x: i32, y: i32) -> [u8; 3] {
        if (x + y) % 2 == 0 { [200; 3] } else { [0; 3] }
    }

    #[test]
    fn a_highlight_is_trimmed_to_the_selection_and_other_kinds_are_not_effects() {
        let state = scene();
        let mut buffer = solid(&state, 200);
        let highlight = AnnotationObject::new(
            1,
            AnnotationKind::Highlight {
                points: vec![(60, 150), (200, 150)],
                color: [25, 196, 252, 255],
                thickness: 5,
            },
        );
        state.render_effect(&highlight, &mut buffer, SEL);
        assert_eq!(
            at(&buffer, 80, 150),
            dimmed_rgb(),
            "outside the selection it is undone"
        );
        assert_ne!(
            at(&buffer, 150, 150),
            [200, 200, 200],
            "inside it tints the pixels"
        );
        let mut untouched = solid(&state, 5);
        state.render_effect(&rectangle(2, SEL, 4), &mut untouched, SEL);
        assert_eq!(untouched, solid(&state, 5));
    }

    #[test]
    fn the_size_readout_is_a_card_with_its_text() {
        let mut state = scene();
        state.hud_rect = Some(Rect::new(300, 300, 436, 328));
        state.render_capture_hud();
        let card = bgr(crate::theme::tokens().card);
        assert_eq!(rgb_at(&state, 306, 314), card);
        let marked = (306..430)
            .flat_map(|x| (303..325).map(move |y| (x, y)))
            .filter(|&(x, y)| rgb_at(&state, x, y) != card)
            .count();
        assert!(marked > 20, "the label is drawn over the card");
    }

    #[test]
    fn the_readout_lights_up_under_the_pointer_once_a_region_exists() {
        let mut state = scene();
        state.hud_rect = Some(Rect::new(300, 300, 436, 328));
        state.pointer = (310, 310);
        state.render_capture_hud();
        assert_eq!(
            rgb_at(&state, 306, 314),
            bgr(crate::theme::tokens().accent_tint)
        );
    }

    #[test]
    fn nothing_is_drawn_without_a_readout_position_or_a_target() {
        let mut state = scene();
        state.render_capture_hud();
        state.committed_selection = None;
        state.mode = OverlayMode::Hovering;
        state.hud_rect = Some(Rect::new(300, 300, 436, 328));
        state.render_capture_hud();
        assert_eq!(rgb_at(&state, 306, 314), [0, 0, 0]);
    }

    #[test]
    fn the_readout_is_placed_clear_of_the_toolbar_and_the_open_wheel() {
        let mut state = scene();
        state.composite_scene();
        let hud = state.hud_rect.expect("a readout is placed");
        let toolbar = state.toolbar.as_ref().unwrap();
        assert!(!rects_intersect(&hud, &toolbar.tool_bounds));
        assert!(!rects_intersect(&hud, &toolbar.action_bounds));
        assert_eq!(hud.width(), 136 * state.dpi as i32 / 96);
        state.thickness_input = Some(String::new());
        state.composite_scene();
        let wheel = state
            .toolbar
            .as_ref()
            .unwrap()
            .wheel_bounds
            .expect("an open wheel");
        assert!(!rects_intersect(&state.hud_rect.unwrap(), &wheel));
    }

    #[test]
    fn the_readout_needs_a_target_but_not_a_toolbar() {
        let mut state = backbuffer_state(640, 480);
        state.hud_rect = Some(Rect::new(1, 1, 2, 2));
        state.place_capture_hud();
        assert_eq!(state.hud_rect, None);
        state.hover_snap_rect = Some(Rect::new(200, 200, 400, 300));
        state.place_capture_hud();
        let hud = state.hud_rect.expect("placed beside the hovered window");
        assert!(!rects_intersect(&hud, &Rect::new(200, 200, 400, 300)));
    }

    #[test]
    fn the_scene_follows_the_dpi_of_the_monitor_only_in_the_editor() {
        let expected = selection_monitor_dpi(&scene().capture, SEL, 7);
        let mut editor = scene();
        editor.dpi = 7;
        editor.composite_scene();
        assert_eq!(editor.dpi, expected);
        let mut export = scene();
        export.dpi = 7;
        export.composite_scene_with(CompositionPolicy::EXPORT);
        assert_eq!(export.dpi, 7);
    }

    #[test]
    fn unfinished_shapes_are_previewed_in_the_editor_but_never_exported() {
        let red = |state: &OverlayState| count_color(state, SEL, RED);
        let shapes = [
            InProgressDrawing::Rectangle {
                start: (120, 90),
                current: (240, 170),
            },
            InProgressDrawing::Ellipse {
                start: (120, 90),
                current: (240, 170),
            },
            InProgressDrawing::Arrow {
                start: (110, 100),
                current: (250, 170),
            },
            InProgressDrawing::Line {
                start: (110, 100),
                current: (250, 170),
            },
            InProgressDrawing::Pen {
                points: vec![(120, 100), (180, 130), (240, 110)],
            },
        ];
        for shape in shapes {
            let mut editor = scene();
            editor.active_tool = ToolKind::Pen;
            editor.drawing_shape = Some(shape.clone());
            editor.composite_scene();
            assert!(red(&editor) > 20, "{shape:?}");
            let mut export = scene();
            export.active_tool = ToolKind::Pen;
            export.drawing_shape = Some(shape);
            export.composite_scene_with(CompositionPolicy::EXPORT);
            assert_eq!(red(&export), 0);
        }
    }

    #[test]
    fn finished_ellipses_and_lines_are_part_of_the_export() {
        let red = |state: &OverlayState| count_color(state, SEL, RED);
        for kind in [
            AnnotationKind::Ellipse {
                rect: Rect::new(120, 90, 240, 170),
                color: RED,
                thickness: 4,
            },
            AnnotationKind::Line {
                start: (110, 100),
                end: (250, 170),
                color: RED,
                thickness: 4,
            },
        ] {
            let mut export = scene();
            export.objects.push(AnnotationObject::new(1, kind.clone()));
            export.composite_scene_with(CompositionPolicy::EXPORT);
            assert!(red(&export) > 20, "{kind:?}");
        }
    }

    #[test]
    fn a_highlighter_stroke_in_progress_stays_inside_the_selection() {
        let mut state = scene();
        state.active_tool = ToolKind::Highlight;
        state.active_color = [25, 196, 252, 255];
        state.drawing_shape = Some(InProgressDrawing::Pen {
            points: vec![(60, 150), (200, 150)],
        });
        state.composite_scene();
        assert_eq!(rgb_at(&state, 80, 150), dimmed_rgb());
        assert_ne!(rgb_at(&state, 150, 150), [0, 0, 0]);
    }

    #[test]
    fn a_redaction_in_progress_is_shown_as_solid_ink() {
        let mut state = scene();
        state.drawing_shape = Some(InProgressDrawing::Blur {
            start: (120, 100),
            current: (180, 140),
            redact: true,
        });
        state.composite_scene();
        assert_eq!(rgb_at(&state, 150, 120), INK);
    }

    #[test]
    fn a_blur_in_progress_and_a_finished_one_average_what_is_below_them() {
        for finished in [false, true] {
            let mut state = scene();
            state
                .objects
                .push(rectangle(1, Rect::new(150, 100, 250, 170), 8));
            let rect = Rect::new(140, 110, 164, 134);
            if finished {
                state.objects.push(AnnotationObject::new(
                    2,
                    AnnotationKind::Blur {
                        rect,
                        block_size: 12,
                    },
                ));
            } else {
                state.drawing_shape = Some(InProgressDrawing::Blur {
                    start: (140, 110),
                    current: (164, 134),
                    redact: false,
                });
            }
            state.composite_scene();
            let block = rgb_at(&state, 141, 115);
            assert_eq!(
                rgb_at(&state, 145, 115),
                block,
                "uniform block (finished: {finished})"
            );
            assert_ne!(block, [0, 0, 0]);
            assert_ne!(block, [RED[0], RED[1], RED[2]]);
        }
    }

    #[test]
    fn a_text_being_edited_is_drawn_live_instead_of_its_object() {
        let mut state = scene();
        state.objects.push(AnnotationObject::new(
            5,
            AnnotationKind::Text {
                pos: (120, 90),
                text: "AAAA".to_string(),
                color: GREEN,
                font_size: 22,
            },
        ));
        let mut edit = TextEditState::new((200, 150), "BB".to_string(), PLUM, 22, Some(5));
        edit.caret_visible = false;
        state.set_text_edit(Some(edit));
        state.composite_scene();
        assert_eq!(
            count_color(&state, SEL, GREEN),
            0,
            "the object is hidden while it is edited"
        );
        assert!(count_color(&state, SEL, PLUM) > 0, "the live text is drawn");
        let mut export = scene();
        export.objects = state.objects.clone();
        export.composite_scene_with(CompositionPolicy::EXPORT);
        assert!(count_color(&export, SEL, GREEN) > 0);
    }

    #[test]
    fn the_caret_and_its_halo_follow_the_edit_state() {
        let mut state = scene();
        let edit = TextEditState::new((150, 120), String::new(), PLUM, 22, None);
        let middle = {
            let bounds = edit.bounds();
            (bounds.top + bounds.bottom) / 2
        };
        state.set_text_edit(Some(edit));
        state.composite_scene();
        assert_eq!(rgb_at(&state, 150, middle), [PLUM[0], PLUM[1], PLUM[2]]);
        assert_eq!(rgb_at(&state, 149, middle), [255, 255, 255]);
        state.text_edit.as_mut().unwrap().caret_visible = false;
        state.composite_scene();
        assert_eq!(rgb_at(&state, 150, middle), [0, 0, 0]);
    }

    #[test]
    fn selected_text_is_highlighted_and_the_box_gets_a_dotted_amber_frame() {
        let mut state = scene();
        let mut edit = TextEditState::new((150, 120), "hello".to_string(), PLUM, 22, None);
        edit.anchor = Some(1);
        edit.caret = 4;
        let bounds = edit.bounds();
        let left = 150 + crate::drawing::measure_text("h", 22).0;
        let right = 150 + crate::drawing::measure_text("hell", 22).0;
        state.set_text_edit(Some(edit));
        state.composite_scene();
        let highlight = Rect::new(left, bounds.top, right, bounds.bottom);
        let selected = bgr(crate::theme::TEXT_SELECTION);
        assert!(
            count_color(
                &state,
                highlight,
                [selected[0], selected[1], selected[2], 255]
            ) > 20
        );
        assert!(
            count_color(&state, highlight, PLUM) > 0,
            "the glyphs are drawn over it"
        );
        let margin = super::super::text::text_box_margin(state.dpi);
        let frame = bounds.inflate(margin, margin);
        let top_line = Rect::new(frame.left, frame.top, frame.right, frame.top + 1);
        assert!(count_color(&state, top_line, crate::theme::BRAND_AMBER_BGRA) > 0);
    }

    #[test]
    fn redactions_cover_everything_below_them() {
        let mut state = scene();
        state
            .objects
            .push(rectangle(1, Rect::new(150, 100, 250, 170), 8));
        state.objects.push(AnnotationObject::new(
            2,
            AnnotationKind::Redact {
                rect: Rect::new(140, 90, 260, 180),
            },
        ));
        state.composite_scene();
        for (x, y) in [(145, 95), (200, 135), (150, 105)] {
            assert_eq!(rgb_at(&state, x, y), INK, "({x}, {y})");
        }
        assert_eq!(count_color(&state, Rect::new(140, 90, 260, 180), RED), 0);
    }

    #[test]
    fn a_highlight_object_tints_inside_the_selection_only() {
        let mut state = scene();
        state.objects.push(AnnotationObject::new(
            1,
            AnnotationKind::Highlight {
                points: vec![(60, 150), (200, 150)],
                color: [25, 196, 252, 255],
                thickness: 5,
            },
        ));
        state.composite_scene();
        assert_eq!(rgb_at(&state, 80, 150), dimmed_rgb());
        assert_ne!(rgb_at(&state, 150, 150), [0, 0, 0]);
    }

    #[test]
    fn the_selected_object_gets_grips_in_the_editor_only() {
        let mut state = scene();
        state
            .objects
            .push(rectangle(1, Rect::new(150, 100, 250, 170), 4));
        state.set_selected_id(Some(1));
        state.composite_scene();
        let half = (4 * state.dpi as i32 / 96).max(4);
        assert_eq!(rgb_at(&state, 150 - half, 100 - half), [255, 255, 255]);
        let mut export = scene();
        export.objects = state.objects.clone();
        export.set_selected_id(Some(1));
        export.composite_scene_with(CompositionPolicy::EXPORT);
        assert_eq!(rgb_at(&export, 150 - half, 100 - half), [0, 0, 0]);
    }

    #[test]
    fn the_content_layer_is_cached_until_a_full_redraw() {
        let window = crate::overlay::test_support::TestWindow::new();
        let mut state = scene();
        state
            .objects
            .push(rectangle(1, Rect::new(150, 100, 250, 170), 8));
        state.cache_requested = true;
        state.composite_scene();
        assert_eq!(state.base_cache.len(), 640 * 480 * 4);
        let old = Rect::new(144, 120, 156, 150);
        let moved = Rect::new(194, 120, 206, 150);
        state.objects[0].translate(50, 0);
        state.composite_scene();
        assert!(
            count_color(&state, old, RED) > 0,
            "the cached layer is reused"
        );
        assert_eq!(count_color(&state, moved, RED), 0);
        state.redraw(window.0);
        state.composite_scene();
        assert_eq!(count_color(&state, old, RED), 0);
        assert!(count_color(&state, Rect::new(194, 120, 206, 150), RED) > 0);
    }

    #[test]
    fn nothing_is_cached_without_objects_or_for_the_export() {
        let mut empty = scene();
        empty.cache_requested = true;
        empty.composite_scene();
        assert!(empty.base_cache.is_empty());
        let mut export = scene();
        export
            .objects
            .push(rectangle(1, Rect::new(150, 100, 250, 170), 8));
        export.cache_requested = true;
        export.composite_scene_with(CompositionPolicy::EXPORT);
        assert!(export.base_cache.is_empty());
    }

    fn items(state: &OverlayState) -> Vec<ToolbarItem> {
        state
            .toolbar
            .as_ref()
            .unwrap()
            .buttons
            .iter()
            .map(|b| b.item)
            .collect()
    }

    #[test]
    fn the_style_buttons_follow_the_active_tool() {
        for (tool, color, thickness) in [
            (ToolKind::Rectangle, true, true),
            (ToolKind::Highlight, true, true),
            (ToolKind::Text, true, false),
            (ToolKind::Step, true, false),
            (ToolKind::Redact, false, false),
            (ToolKind::Select, false, false),
        ] {
            let mut state = scene();
            state.active_tool = tool;
            state.composite_scene();
            let shown = items(&state);
            assert_eq!(shown.contains(&ToolbarItem::Color), color, "{tool:?}");
            assert_eq!(
                shown.contains(&ToolbarItem::Thickness),
                thickness,
                "{tool:?}"
            );
        }
    }

    #[test]
    fn the_style_buttons_follow_the_selected_object_with_the_select_tool() {
        let step = AnnotationKind::Step {
            pos: (150, 120),
            number: 1,
            color: RED,
            radius: 12,
        };
        let blur = AnnotationKind::Blur {
            rect: Rect::new(150, 100, 200, 150),
            block_size: 12,
        };
        let frame = AnnotationKind::Rectangle {
            rect: Rect::new(150, 100, 200, 150),
            color: RED,
            thickness: 4,
        };
        for (kind, color, thickness) in [
            (step, true, false),
            (blur, false, false),
            (frame, true, true),
        ] {
            let mut state = scene();
            state.active_tool = ToolKind::Select;
            state.objects.push(AnnotationObject::new(1, kind.clone()));
            state.set_selected_id(Some(1));
            state.composite_scene();
            let shown = items(&state);
            assert_eq!(shown.contains(&ToolbarItem::Color), color, "{kind:?}");
            assert_eq!(
                shown.contains(&ToolbarItem::Thickness),
                thickness,
                "{kind:?}"
            );
        }
    }

    #[test]
    fn hover_and_press_survive_a_new_frame_only_while_their_button_is_shown() {
        let mut state = scene();
        state.composite_scene();
        let toolbar = state.toolbar.as_mut().unwrap();
        toolbar.hovered_item = Some(ToolbarItem::Color);
        toolbar.pressed_item = Some(ToolbarItem::Tool(ToolKind::Arrow));
        state.composite_scene();
        let toolbar = state.toolbar.as_ref().unwrap();
        assert_eq!(toolbar.hovered_item, Some(ToolbarItem::Color));
        assert_eq!(
            toolbar.pressed_item,
            Some(ToolbarItem::Tool(ToolKind::Arrow))
        );
        state.active_tool = ToolKind::Redact;
        state.composite_scene();
        let toolbar = state.toolbar.as_ref().unwrap();
        assert_eq!(toolbar.hovered_item, None, "the colour button is gone");
        assert_eq!(
            toolbar.pressed_item,
            Some(ToolbarItem::Tool(ToolKind::Arrow))
        );
    }

    #[test]
    fn a_selection_from_the_start_of_the_text_is_highlighted_from_its_left_edge() {
        let mut state = scene();
        let mut edit = TextEditState::new((150, 120), "hello".to_string(), PLUM, 22, None);
        edit.anchor = Some(0);
        edit.caret = 3;
        let bounds = edit.bounds();
        let right = 150 + crate::drawing::measure_text("hel", 22).0;
        state.set_text_edit(Some(edit));
        state.composite_scene();
        let selected = bgr(crate::theme::TEXT_SELECTION);
        let color = [selected[0], selected[1], selected[2], 255];
        assert!(
            count_color(
                &state,
                Rect::new(150, bounds.top, right, bounds.bottom),
                color
            ) > 20
        );
        assert_eq!(
            count_color(
                &state,
                Rect::new(right + 3, bounds.top, right + 30, bounds.bottom),
                color
            ),
            0
        );
    }

    #[test]
    fn a_notice_is_an_amber_card_beside_the_toolbar_and_never_part_of_an_export() {
        let mut state = scene();
        state.composite_scene();
        assert_eq!(state.notice_rect, None);
        state.notice = Some("Copied #FEAB10".to_string());
        state.composite_scene();
        let notice = state.notice_rect.expect("the notice is placed");
        let toolbar = state.toolbar.as_ref().unwrap();
        assert!(!rects_intersect(&notice, &toolbar.tool_bounds));
        assert!(!rects_intersect(&notice, &toolbar.action_bounds));
        let accent = bgr(crate::theme::tokens().accent);
        let probe = (notice.left + 6, (notice.top + notice.bottom) / 2);
        assert_eq!(rgb_at(&state, probe.0, probe.1), accent);
        let inner = notice.inflate(-12, -8);
        let text_pixels = (inner.top..inner.bottom)
            .flat_map(|y| (inner.left..inner.right).map(move |x| (x, y)))
            .filter(|&(x, y)| rgb_at(&state, x, y) != accent)
            .count();
        assert!(text_pixels > 0, "the text is drawn on the card");

        let mut export = scene();
        export.notice = Some("Copied #FEAB10".to_string());
        export.composite_scene_with(CompositionPolicy::EXPORT);
        assert_ne!(rgb_at(&export, probe.0, probe.1), accent);
    }

    #[test]
    fn a_notice_needs_a_toolbar_to_sit_beside() {
        let mut state = scene();
        state.notice = Some("Copied #FEAB10".to_string());
        state.notice_rect = Some(Rect::new(1, 1, 2, 2));
        state.place_notice();
        assert_eq!(state.notice_rect, None);
    }
}
