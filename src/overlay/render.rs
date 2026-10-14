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
                                | ToolKind::Arrow
                                | ToolKind::Pen
                                | ToolKind::Highlight => {
                                    (self.active_color, self.active_thickness, true, true)
                                }
                                ToolKind::Text | ToolKind::Step => {
                                    (self.active_color, self.active_thickness, true, false)
                                }
                                ToolKind::Blur | ToolKind::Redact | ToolKind::Select => {
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
}
