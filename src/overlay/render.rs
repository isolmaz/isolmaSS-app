use super::*;

impl OverlayState {
    fn render_capture_hud(&self) {
        let Some(selection) = self.committed_selection else {
            return;
        };
        let scale = |value: i32| (value * self.dpi as i32 / 96).max(1);
        let tokens = crate::theme::tokens();
        let label = format!("{} × {} px", selection.width(), selection.height());
        let bounds = capture_hud_bounds(&self.capture, selection, self.dpi);
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
                tokens.accent
            } else {
                tokens.stroke
            },
        );
        crate::drawing::label(self.mem_dc, bounds, &label, scale(13), tokens.text, true);
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
        let width = self.capture.width;
        let height = self.capture.height;
        let len = (width as usize) * (height as usize) * 4;
        let buffer = unsafe { std::slice::from_raw_parts_mut(self.bits_ptr, len) };

        match self.mode {
            OverlayMode::Hovering => {
                buffer.copy_from_slice(&self.capture.dimmed);

                if let Some(snap) = self.hover_snap_rect {
                    self.capture.punch_out(buffer, &snap);
                    CaptureBuffer::draw_border(
                        buffer,
                        width,
                        height,
                        &snap,
                        [246, 130, 59, 255],
                        2,
                    );
                }
            }
            OverlayMode::DraggingSelection => {
                buffer.copy_from_slice(&self.capture.dimmed);

                if let Some(sel) = self.committed_selection {
                    self.capture.punch_out(buffer, &sel);
                    CaptureBuffer::draw_border(buffer, width, height, &sel, [246, 130, 59, 255], 2);
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
                    // Composite in object order. CPU effects see preceding GDI annotations.
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
                            obj.render_blur(buffer, width, height);
                        } else {
                            obj.render_gdi(self.mem_dc);
                        }
                    }
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
                if policy.in_progress_preview
                    && let Some(InProgressDrawing::Blur {
                        start,
                        current,
                        redact,
                    }) = self.drawing_shape
                {
                    unsafe {
                        let _ = GdiFlush();
                    }
                    let rect = Rect::normalized(start, current).clamp(width, height);
                    let kind = if redact {
                        AnnotationKind::Redact { rect }
                    } else {
                        AnnotationKind::Blur {
                            rect,
                            block_size: DEFAULT_BLUR_BLOCK,
                        }
                    };
                    AnnotationObject::new(0, kind).render_blur(buffer, width, height);
                }
                if policy.selection_frame {
                    CaptureBuffer::draw_contrast_selection(
                        buffer,
                        width,
                        height,
                        &sel,
                        [255, 91, 99, 255],
                    );
                }

                // 6. Draw in-progress preview only in the editor frame.
                if policy.in_progress_preview {
                    match &self.drawing_shape {
                        Some(InProgressDrawing::Rectangle { start, current }) => {
                            let r = Rect::normalized(*start, *current).clamp(width, height);
                            let preview = AnnotationObject::new(
                                0,
                                AnnotationKind::Rectangle {
                                    rect: r,
                                    color: self.active_color,
                                    thickness: self.active_thickness,
                                },
                            );
                            preview.render_gdi(self.mem_dc);
                        }
                        Some(InProgressDrawing::Arrow { start, current }) => {
                            let preview = AnnotationObject::new(
                                0,
                                AnnotationKind::Arrow {
                                    start: *start,
                                    end: *current,
                                    color: self.active_color,
                                    thickness: self.active_thickness,
                                },
                            );
                            preview.render_gdi(self.mem_dc);
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
                            } else {
                                render_pen_preview(
                                    self.mem_dc,
                                    points,
                                    self.active_color,
                                    self.active_thickness,
                                );
                            }
                        }
                        _ => {}
                    }
                }

                // 7. Draw selection handles for selected object
                if policy.selected_handles
                    && let Some(obj) = self
                        .selected_id
                        .and_then(|id| self.objects.iter().find(|o| o.id == id))
                {
                    obj.render_selection_indicator(self.mem_dc);
                }

                // 8. Draw active text edit preview with font match & blinking caret
                if policy.caret
                    && let Some(text_edit) = &self.text_edit
                {
                    if let Some(range) = text_edit.selection() {
                        let prefix: String = text_edit.text.chars().take(range.start).collect();
                        let selected: String = text_edit.text.chars().take(range.end).collect();
                        let left = if prefix.is_empty() {
                            0
                        } else {
                            crate::drawing::measure_text(&prefix, text_edit.font_size).0
                        };
                        let right = crate::drawing::measure_text(&selected, text_edit.font_size).0;
                        crate::drawing::rounded(
                            self.mem_dc,
                            Rect::new(
                                text_edit.pos.0 + left,
                                text_edit.pos.1,
                                text_edit.pos.0 + right,
                                text_edit.pos.1
                                    + crate::drawing::measure_text(
                                        &text_edit.text,
                                        text_edit.font_size,
                                    )
                                    .1,
                            ),
                            0,
                            COLORREF(0xffd5c7),
                            COLORREF(0xffd5c7),
                        );
                    }
                    crate::drawing::text(
                        self.mem_dc,
                        text_edit.pos,
                        &text_edit.text,
                        text_edit.font_size,
                        bgra_to_colorref(text_edit.color),
                    );
                    if text_edit.caret_visible {
                        let before: String = text_edit.text.chars().take(text_edit.caret).collect();
                        let x = if before.is_empty() {
                            0
                        } else {
                            crate::drawing::measure_text(&before, text_edit.font_size).0
                        };
                        let caret = Rect::new(
                            text_edit.pos.0 + x,
                            text_edit.pos.1,
                            text_edit.pos.0 + x + 1,
                            text_edit.pos.1 + text_edit.font_size,
                        );
                        unsafe {
                            let _ = GdiFlush();
                        }
                        CaptureBuffer::draw_border(
                            buffer,
                            width,
                            height,
                            &caret,
                            text_edit.color,
                            1,
                        );
                    }
                }

                // Opaque redactions always cover the final annotation pixels.
                unsafe {
                    let _ = GdiFlush();
                }
                for obj in &self.objects {
                    if matches!(obj.kind, AnnotationKind::Redact { .. }) {
                        obj.render_blur(buffer, width, height);
                    }
                }
                // 9. Render the contextual L-shaped editor toolbar.
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
                    let screen = WIN_RECT {
                        left: sel.left + self.capture.x,
                        top: sel.top + self.capture.y,
                        right: sel.right + self.capture.x,
                        bottom: sel.bottom + self.capture.y,
                    };
                    let monitor = unsafe { MonitorFromRect(&screen, MONITOR_DEFAULTTONEAREST) };
                    let mut dpi_x = self.dpi;
                    let mut dpi_y = self.dpi;
                    if unsafe {
                        windows::Win32::UI::HiDpi::GetDpiForMonitor(
                            monitor,
                            windows::Win32::UI::HiDpi::MDT_EFFECTIVE_DPI,
                            &mut dpi_x,
                            &mut dpi_y,
                        )
                    }
                    .is_ok()
                    {
                        self.dpi = dpi_x.max(48);
                    }
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
            self.render_capture_hud();
        }
        // Make the fully rebuilt backbuffer visible to the subsequent WM_PAINT copy.
        unsafe {
            let _ = GdiFlush();
        }
    }
}
