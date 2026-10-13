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

impl OverlayState {
    fn render_capture_hud(&self) {
        let (Some(selection), Some(bounds)) = (self.committed_selection, self.hud_rect) else {
            return;
        };
        let scale = |value: i32| (value * self.dpi as i32 / 96).max(1);
        let tokens = crate::theme::tokens();
        let label = format!("{} × {} px", selection.width(), selection.height());
        let hovered = bounds.contains(self.pointer.0, self.pointer.1);
        crate::drawing::rounded(
            self.mem_dc,
            bounds,
            scale(6),
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
        self.hud_rect = self.committed_selection.map(|selection| {
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

    /// Text selection highlight and caret: editor chrome drawn above the redactions.
    fn render_text_chrome(&self, buffer: &mut [u8], sel: Rect, edit: &TextEditState) {
        let (width, height) = (self.capture.width, self.capture.height);
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
                edit.pos.1,
                edit.pos.0 + right,
                edit.pos.1 + crate::drawing::measure_text(&edit.text, edit.font_size).1,
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
        if edit.caret_visible {
            let x = edit.caret_offset();
            let caret = Rect::new(
                edit.pos.0 + x,
                edit.pos.1,
                edit.pos.0 + x + 1,
                edit.pos.1 + edit.font_size,
            );
            unsafe {
                let _ = GdiFlush();
            }
            CaptureBuffer::draw_border(buffer, width, height, &caret, edit.color, 1);
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
            && let Some(selection) = self.committed_selection
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
                        &rect,
                        crate::theme::BRAND_AMBER_BGRA,
                        2,
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
                        tb.hovered_item = existing
                            .hovered_item
                            .filter(|item| tb.buttons.iter().any(|button| button.item == *item));
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
