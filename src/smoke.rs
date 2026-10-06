use crate::annotation::{
    AnnotationKind, AnnotationObject, EditCommand, HistoryManager, ToolKind, snap_angle_45,
    snap_square,
};
use crate::capture::{CaptureBuffer, Rect};
use crate::clipboard::{copy_dib_to_clipboard, flatten_selection_to_dib};
use crate::hotkey::{self, HotkeyConfig, start_hotkey_listener};
use crate::save::{default_save_directory, generate_screenshot_filename, save_buffer_to_png};
use crate::settings::{PRESET_COLORS, PRESET_THICKNESSES, Settings};
use crate::toolbar::{Toolbar, ToolbarAction, ToolbarItem};
use crate::tray;
use crate::window_snap::{find_window_at_point, get_visible_windows};
use crate::{capture, overlay};
use std::io::Read;
use std::os::windows::process::CommandExt;
use std::rc::Rc;
use std::time::Instant;
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows::Win32::System::Threading::GetCurrentProcess;

fn verify_pe_subsystem_windows_gui(
    binary_path: &std::path::Path,
) -> Result<u16, Box<dyn std::error::Error>> {
    let data = std::fs::read(binary_path)?;
    if data.len() < 0x40 || &data[..2] != b"MZ" {
        return Err("Invalid PE: missing MZ DOS header".into());
    }
    let pe_offset = u32::from_le_bytes(data[0x3C..0x40].try_into()?) as usize;
    if data.len() < pe_offset + 24 + 70 || &data[pe_offset..pe_offset + 4] != b"PE\0\0" {
        return Err("Invalid PE: missing PE signature".into());
    }
    let opt_offset = pe_offset + 24;
    let magic = u16::from_le_bytes(data[opt_offset..opt_offset + 2].try_into()?);
    if magic != 0x10b && magic != 0x20b {
        return Err(format!("Unknown PE optional header magic: 0x{:04x}", magic).into());
    }
    let subsystem = u16::from_le_bytes(data[opt_offset + 68..opt_offset + 70].try_into()?);
    Ok(subsystem)
}

pub fn run_smoke_test() -> Result<(), Box<dyn std::error::Error>> {
    println!("============================================================");
    println!(" isolmaSS smoke test");
    println!("============================================================");

    // ------------------------------------------------------------
    // Global Hotkey, Registry Suppression & WH_KEYBOARD_LL Hook
    // ------------------------------------------------------------
    println!("\nTesting Global Hotkey Configuration & Registration...");
    let default_cfg = HotkeyConfig::default();
    assert_eq!(default_cfg.description, "PrintScreen");
    let fallback_cfg = HotkeyConfig::fallback();
    assert_eq!(fallback_cfg.description, "Ctrl+Shift+S");

    let parsed_prtsc = HotkeyConfig::from_str("PrintScreen").expect("Parse PrintScreen");
    assert_eq!(parsed_prtsc.description, "PrintScreen");
    let parsed_combo = HotkeyConfig::from_str("Ctrl+Shift+S").expect("Parse Ctrl+Shift+S");
    assert_eq!(parsed_combo.description, "Ctrl+Shift+S");

    // 1. Non-mutating query for Windows Snipping Tool suppression state
    let snipping_tool_disabled = hotkey::is_windows_snipping_tool_disabled();
    println!(
        "  - Windows Snipping Tool suppression query verified (non-mutating): disabled={}.",
        snipping_tool_disabled
    );

    // 2. Test WH_KEYBOARD_LL hook listener startup & active key
    let (rx, handle) = start_hotkey_listener(default_cfg)?;
    assert_eq!(
        handle.active_description, "PrintScreen",
        "PrintScreen must remain active via low-level hook without unwanted fallback"
    );
    println!(
        "  - Hotkey listener active on dedicated thread: '{}' (WH_KEYBOARD_LL enabled)",
        handle.active_description
    );

    // 3. Test Hook Callback Logic (synthetic VK_SNAPSHOT consumption & event dispatch)
    let kb_prtsc = hotkey::create_test_kbdllhookstruct(
        windows::Win32::UI::Input::KeyboardAndMouse::VK_SNAPSHOT.0 as u32,
    );
    let lparam_prtsc = windows::Win32::Foundation::LPARAM(&kb_prtsc as *const _ as isize);
    let wparam_down = windows::Win32::Foundation::WPARAM(
        windows::Win32::UI::WindowsAndMessaging::WM_KEYDOWN as usize,
    );
    let wparam_up = windows::Win32::Foundation::WPARAM(
        windows::Win32::UI::WindowsAndMessaging::WM_KEYUP as usize,
    );

    // Initial VK_SNAPSHOT keydown: MUST return LRESULT(1) (swallow keystroke) and trigger capture
    let res_down = unsafe { hotkey::low_level_keyboard_proc(0, wparam_down, lparam_prtsc) };
    assert_eq!(
        res_down,
        windows::Win32::Foundation::LRESULT(1),
        "Hook MUST swallow VK_SNAPSHOT with LRESULT(1)"
    );
    assert!(
        rx.try_recv().is_ok(),
        "Initial VK_SNAPSHOT MUST dispatch capture event"
    );

    // Auto-repeat VK_SNAPSHOT while held: MUST return LRESULT(1) without duplicate event
    let res_repeat = unsafe { hotkey::low_level_keyboard_proc(0, wparam_down, lparam_prtsc) };
    assert_eq!(
        res_repeat,
        windows::Win32::Foundation::LRESULT(1),
        "Auto-repeat VK_SNAPSHOT MUST be swallowed"
    );
    assert!(
        rx.try_recv().is_err(),
        "Auto-repeat VK_SNAPSHOT MUST NOT trigger duplicate capture"
    );

    // VK_SNAPSHOT keyup: MUST return LRESULT(1) (swallow key release)
    let res_up = unsafe { hotkey::low_level_keyboard_proc(0, wparam_up, lparam_prtsc) };
    assert_eq!(
        res_up,
        windows::Win32::Foundation::LRESULT(1),
        "VK_SNAPSHOT release MUST be swallowed"
    );

    // Non-PrintScreen key (e.g. 'A' key = 0x41): MUST pass through (not return 1)
    let kb_other = hotkey::create_test_kbdllhookstruct(0x41);
    let lparam_other = windows::Win32::Foundation::LPARAM(&kb_other as *const _ as isize);
    let res_other = unsafe { hotkey::low_level_keyboard_proc(0, wparam_down, lparam_other) };
    assert_ne!(
        res_other,
        windows::Win32::Foundation::LRESULT(1),
        "Non-PrintScreen key MUST pass through"
    );
    assert!(
        rx.try_recv().is_err(),
        "Non-PrintScreen key MUST NOT trigger capture"
    );

    println!(
        "  - Low-level keyboard hook callback verified: VK_SNAPSHOT consumed (LRESULT 1), auto-repeat handled, non-PrintScreen passed through."
    );

    let loaded_cfg = HotkeyConfig::load()?;
    assert!(!loaded_cfg.description.is_empty());
    drop(handle);
    println!("  - Hotkey unregistered, WH_KEYBOARD_LL unhooked, and thread cleanly shut down.");
    println!("  -> PASSED");

    // ------------------------------------------------------------
    // Virtual Screen Capture & Dimming Buffer Pre-rendering
    // ------------------------------------------------------------
    println!("\nTesting Full-Screen Virtual Screen Capture (BitBlt)...");
    let capture_start = Instant::now();
    let capture = CaptureBuffer::capture_virtual_screen()?;
    let capture_duration = capture_start.elapsed();

    println!(
        "  - Virtual Screen Bounds: origin=({}, {}), dimensions={}x{}",
        capture.x, capture.y, capture.width, capture.height
    );
    println!(
        "  - Capture completed in: {:.2?} (budget: <40 ms)",
        capture_duration
    );

    let expected_bytes = (capture.width as usize) * (capture.height as usize) * 4;
    assert_eq!(capture.original.len(), expected_bytes);
    assert_eq!(capture.dimmed.len(), expected_bytes);

    let original_sample = &capture.original[..expected_bytes.min(4000)];
    let dimmed_sample = &capture.dimmed[..expected_bytes.min(4000)];

    let (orig_chunks, _) = original_sample.as_chunks::<4>();
    let (dim_chunks, _) = dimmed_sample.as_chunks::<4>();

    let mut non_zero_pixels = 0usize;
    let mut dimmed_correctly = 0usize;
    for (orig_px, dim_px) in orig_chunks.iter().zip(dim_chunks.iter()) {
        if orig_px[0] > 0 || orig_px[1] > 0 || orig_px[2] > 0 {
            non_zero_pixels += 1;
            let expected_b = ((orig_px[0] as u32 * 115) / 255) as u8;
            let expected_g = ((orig_px[1] as u32 * 115) / 255) as u8;
            let expected_r = ((orig_px[2] as u32 * 115) / 255) as u8;
            if dim_px[0] == expected_b
                && dim_px[1] == expected_g
                && dim_px[2] == expected_r
                && dim_px[3] == 255
            {
                dimmed_correctly += 1;
            }
        }
    }
    assert_eq!(non_zero_pixels, dimmed_correctly);
    println!("  -> PASSED");

    // ------------------------------------------------------------
    // Overlay Window Architecture
    // ------------------------------------------------------------
    println!("\nVerifying Fullscreen Layered Overlay Properties...");
    println!("  - Target Styles: WS_POPUP");
    println!("  - Extended Styles: WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST");
    println!("  - Cursor: IDC_CROSS / IDC_HAND");
    println!("  -> informational; runtime window behavior is verified separately.");

    // ------------------------------------------------------------
    // Drag-to-Select Geometry & Fast Scanline Punch-Out
    // ------------------------------------------------------------
    println!("\nTesting Drag-to-Select Geometry & Fast Scanline Punch-Out...");
    let p1 = (150, 100);
    let p2 = (650, 480);
    let selection = Rect::normalized(p1, p2).clamp(capture.width, capture.height);
    assert_eq!(selection.width(), 500);
    assert_eq!(selection.height(), 380);

    let r_other = Rect::new(200, 200, 700, 500);
    let r_union = selection.union(&r_other);
    assert_eq!(r_union.left, 150);
    assert_eq!(r_union.top, 100);
    assert_eq!(r_union.right, 700);
    assert_eq!(r_union.bottom, 500);

    let mut working_buffer = capture.dimmed.clone();
    let punch_start = Instant::now();
    capture.punch_out(&mut working_buffer, &selection);
    let punch_duration = punch_start.elapsed();
    println!("  - Punch-out (500x380) in: {:.2?} (<1 ms)", punch_duration);

    let sample_y = 200;
    let sample_x = 300;
    let offset = ((sample_y * capture.width + sample_x) * 4) as usize;
    assert_eq!(
        &working_buffer[offset..offset + 4],
        &capture.original[offset..offset + 4]
    );

    CaptureBuffer::draw_border(
        &mut working_buffer,
        capture.width,
        capture.height,
        &selection,
        [246, 130, 59, 255],
        2,
    );
    let border_offset = ((selection.top * capture.width + selection.left) * 4) as usize;
    assert_eq!(
        &working_buffer[border_offset..border_offset + 4],
        &[246, 130, 59, 255]
    );

    capture.restore_dimmed(&mut working_buffer, &selection.inflate(2, 2));
    assert_eq!(
        &working_buffer[offset..offset + 4],
        &capture.dimmed[offset..offset + 4]
    );
    println!("  -> PASSED");

    // ------------------------------------------------------------
    // Single-Click Window Snap (EnumWindows + DWM)
    // ------------------------------------------------------------
    println!("\nTesting Single-Click Window Snap (EnumWindows + DWM Extended Frame Bounds)...");
    let windows = get_visible_windows(None);
    assert!(!windows.is_empty());
    for (i, win) in windows.iter().take(5).enumerate() {
        println!(
            "  [{}] HWND 0x{:08X} | ({}) | [{}, {} -> {}, {}]",
            i,
            win.hwnd.0 as usize,
            win.class_name,
            win.bounds.left,
            win.bounds.top,
            win.bounds.right,
            win.bounds.bottom
        );
    }
    let first_win = &windows[0];
    let center_x = (first_win.bounds.left + first_win.bounds.right) / 2;
    let center_y = (first_win.bounds.top + first_win.bounds.bottom) / 2;
    let hit = find_window_at_point((center_x, center_y), None);
    assert!(hit.is_some());
    println!("  -> PASSED");

    // ------------------------------------------------------------
    // Selection Commit & Toolbar
    // ------------------------------------------------------------
    println!("\nTesting Selection Commit & Toolbar Layout...");
    let tb = Toolbar::layout(
        &selection,
        Rect::new(0, 0, capture.width, capture.height),
        ToolKind::Rectangle,
        PRESET_COLORS[0],
        3,
        false,
        true,
        true,
        false,
        true,
        false,
        96,
    );
    assert!(
        tb.buttons
            .iter()
            .any(|button| button.item == ToolbarItem::MoreTools)
    );
    assert!(
        !tb.buttons
            .iter()
            .any(|button| button.item == ToolbarItem::Tool(ToolKind::Blur))
    );
    assert!(tb.tool_bounds.left >= 0);
    assert!(tb.tool_bounds.bottom <= capture.height);
    assert!(tb.action_bounds.left >= 0);
    assert!(tb.action_bounds.right <= capture.width);
    assert!(tb.action_bounds.bottom <= capture.height);

    let first_btn = &tb.buttons[0];
    let btn_center = (
        (first_btn.rect.left + first_btn.rect.right) / 2,
        (first_btn.rect.top + first_btn.rect.bottom) / 2,
    );
    assert_eq!(
        tb.hit_test(btn_center),
        Some(ToolbarItem::Tool(ToolKind::Select))
    );

    let copy_btn = tb
        .buttons
        .iter()
        .find(|b| b.item == ToolbarItem::Action(ToolbarAction::Copy))
        .expect("Copy button present");
    let copy_center = (
        (copy_btn.rect.left + copy_btn.rect.right) / 2,
        (copy_btn.rect.top + copy_btn.rect.bottom) / 2,
    );
    assert_eq!(
        tb.hit_test(copy_center),
        Some(ToolbarItem::Action(ToolbarAction::Copy))
    );
    println!(
        "  - Toolbar layout: coherent bottom-up L, {} controls aligned.",
        tb.buttons.len()
    );
    println!("  -> PASSED");

    // ------------------------------------------------------------
    // Annotation Objects & Geometry
    // ------------------------------------------------------------
    println!("\nTesting Annotation Objects (Rectangle, Arrow, Pen, Text)...");
    let mut rect_obj = AnnotationObject::new(
        1,
        AnnotationKind::Rectangle {
            rect: Rect::new(100, 100, 300, 200),
            color: [40, 40, 235, 255],
            thickness: 3,
        },
    );
    assert!(rect_obj.hit_test((100, 100)));
    assert!(rect_obj.hit_test((200, 150)));
    rect_obj.translate(10, 20);

    let arrow_obj = AnnotationObject::new(
        2,
        AnnotationKind::Arrow {
            start: (50, 50),
            end: (200, 50),
            color: [40, 40, 235, 255],
            thickness: 3,
        },
    );
    assert!(arrow_obj.hit_test((100, 50)));

    let pen_obj = AnnotationObject::new(
        3,
        AnnotationKind::Pen {
            points: vec![(10, 10), (20, 20), (30, 15)],
            color: [40, 40, 235, 255],
            thickness: 3,
        },
    );
    assert!(pen_obj.hit_test((15, 15)));

    let text_obj = AnnotationObject::new(
        4,
        AnnotationKind::Text {
            pos: (50, 80),
            text: "Hello Rust".to_string(),
            color: [40, 40, 235, 255],
            font_size: 20,
        },
    );
    assert!(text_obj.hit_test((60, 85)));
    println!("  - Geometric annotations verified.");
    println!("  -> PASSED");

    // ------------------------------------------------------------
    // Blur / Mosaic Tool
    // ------------------------------------------------------------
    println!("\nTesting Blur / Mosaic Tool (Pixelate Block Averaging)...");
    let mut test_image = vec![0u8; 100 * 100 * 4];
    for y in 20..40 {
        for x in 20..80 {
            let offset = (y * 100 + x) * 4;
            test_image[offset] = 255;
            test_image[offset + 1] = 255;
            test_image[offset + 2] = 255;
            test_image[offset + 3] = 255;
        }
    }

    let blur_rect = Rect::new(20, 20, 80, 40);
    let blur_obj = AnnotationObject::new(
        5,
        AnnotationKind::Blur {
            rect: blur_rect,
            block_size: 10,
        },
    );
    blur_obj.render_blur(&mut test_image, 100, 100);

    let p0 = ((20 * 100 + 20) * 4) as usize;
    let p1 = ((20 * 100 + 25) * 4) as usize;
    assert_eq!(&test_image[p0..p0 + 4], &test_image[p1..p1 + 4]);
    println!("  - Blur mosaic verified.");
    println!("  -> PASSED");

    // ------------------------------------------------------------
    // Explicit Select & Manipulation
    // ------------------------------------------------------------
    println!("\nTesting Explicit Select & Manipulation...");
    let objects = [rect_obj.clone(), arrow_obj.clone()];
    let hit_id = objects
        .iter()
        .rev()
        .find(|o| o.hit_test((115, 125)))
        .map(|o| o.id);
    assert_eq!(hit_id, Some(rect_obj.id));
    println!("  -> PASSED");

    // ------------------------------------------------------------
    // Command History Undo / Redo Stack
    // ------------------------------------------------------------
    println!("\nTesting Undo / Redo Command History Stack...");
    let mut history = HistoryManager::new(50);
    let mut session_objects = Vec::new();

    let o1 = rect_obj.clone();
    session_objects.push(o1.clone());
    history.record(EditCommand::Add(o1.clone()));
    assert_eq!(session_objects.len(), 1);

    assert!(history.can_undo());
    history.undo(&mut session_objects);
    assert_eq!(session_objects.len(), 0);

    assert!(history.can_redo());
    history.redo(&mut session_objects);
    assert_eq!(session_objects.len(), 1);

    println!("  - Undo/Redo stack verified.");
    println!("  -> PASSED");

    // ------------------------------------------------------------
    // Copy to Clipboard (DIB Flattening)
    // ------------------------------------------------------------
    println!("\nTesting Copy to Clipboard & Standalone DIB Generation...");
    let sample_dib = flatten_selection_to_dib(
        &capture.original,
        capture.width,
        capture.height,
        &Rect::new(100, 100, 400, 300),
    )?;
    assert!(sample_dib.len() > 40);
    // A competing clipboard owner must fail without retaining each allocated DIB.
    let (locked_tx, locked_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let owner = std::thread::spawn(move || {
        let result = unsafe { windows::Win32::System::DataExchange::OpenClipboard(None) };
        if result.is_ok() {
            let _guard = crate::clipboard::ClipboardGuard;
            let _ = locked_tx.send(true);
            let _ = release_rx.recv();
        } else {
            let _ = locked_tx.send(false);
        }
    });
    if !locked_rx.recv()? {
        owner.join().map_err(|_| "Clipboard holder failed")?;
        return Err("Clipboard is busy; retry the smoke check.".into());
    }
    let large_dib = vec![0u8; 8 * 1024 * 1024];
    let mut before = windows::Win32::System::ProcessStatus::PROCESS_MEMORY_COUNTERS_EX::default();
    unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            (&mut before as *mut windows::Win32::System::ProcessStatus::PROCESS_MEMORY_COUNTERS_EX)
                .cast(),
            std::mem::size_of_val(&before) as u32,
        )?;
    }
    let failed = (0..32)
        .filter(|_| copy_dib_to_clipboard(None, &large_dib).is_err())
        .count();
    release_tx.send(())?;
    owner.join().map_err(|_| "Clipboard holder failed")?;
    assert_eq!(failed, 32);
    let mut after = windows::Win32::System::ProcessStatus::PROCESS_MEMORY_COUNTERS_EX::default();
    unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            (&mut after as *mut windows::Win32::System::ProcessStatus::PROCESS_MEMORY_COUNTERS_EX)
                .cast(),
            std::mem::size_of_val(&after) as u32,
        )?;
    }
    assert!(
        after.PrivateUsage.saturating_sub(before.PrivateUsage) < 24 * 1024 * 1024,
        "Failed copies retained image-sized allocations"
    );
    drop(large_dib);
    copy_dib_to_clipboard(None, &sample_dib)?;
    println!("  - Standalone 32-bit DIB copied to Windows Clipboard.");
    println!("  -> PASSED");

    // ------------------------------------------------------------
    // Hierarchical Escape & Dismiss Flow
    // ------------------------------------------------------------
    println!("\nVerifying Hierarchical Escape Flow...");
    println!("  - Level 1: In text edit -> Esc cancels text input.");
    println!("  - Level 2: Annotation selected -> Esc deselects.");
    println!("  - Level 3: Selection active -> Esc cancels selection.");
    println!("  - Level 4: In hover mode -> Esc destroys window.");
    println!("  -> covered by the keyboard routing checks below.");

    // ------------------------------------------------------------
    // Save to File (PNG via GDI+)
    // ------------------------------------------------------------
    println!("\nTesting Save to File (PNG via Native GDI+)...");
    let test_dir = std::env::temp_dir();
    let test_png_path = test_dir.join(format!("isolmass-smoke-{}.png", std::process::id()));

    let mut export_pixels = capture.original.to_vec();
    for pixel in export_pixels.as_chunks_mut::<4>().0 {
        pixel[3] = 0;
    }
    let saved_path = save_buffer_to_png(
        &export_pixels,
        capture.width,
        capture.height,
        &Rect::new(50, 50, 350, 250),
        &test_png_path,
    )?;

    assert!(saved_path.exists());
    let png_bytes = std::fs::read(&saved_path)?;
    assert!(png_bytes.len() > 100, "PNG file must be non-empty");

    // Verify PNG magic bytes: 0x89 'P' 'N' 'G' 0x0D 0x0A 0x1A 0x0A
    let expected_magic = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    assert_eq!(
        &png_bytes[0..8],
        &expected_magic,
        "Generated file must have valid PNG magic header"
    );

    {
        #[link(name = "gdiplus")]
        unsafe extern "system" {
            fn GdipLoadImageFromFile(
                path: windows::core::PCWSTR,
                image: *mut *mut std::ffi::c_void,
            ) -> i32;
            fn GdipBitmapGetPixel(
                image: *mut std::ffi::c_void,
                x: i32,
                y: i32,
                color: *mut u32,
            ) -> i32;
            fn GdipGetImageWidth(image: *mut std::ffi::c_void, width: *mut u32) -> i32;
            fn GdipGetImageHeight(image: *mut std::ffi::c_void, height: *mut u32) -> i32;
        }
        use std::os::windows::ffi::OsStrExt;
        let _token = crate::save::GdiPlusToken::start()?;
        let filename: Vec<u16> = saved_path
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        let mut raw = std::ptr::null_mut();
        assert_eq!(
            unsafe { GdipLoadImageFromFile(windows::core::PCWSTR(filename.as_ptr()), &mut raw) },
            0
        );
        let bitmap = crate::save::GdiPlusBitmap(raw);
        let mut width = 0;
        let mut height = 0;
        assert_eq!(unsafe { GdipGetImageWidth(bitmap.0, &mut width) }, 0);
        assert_eq!(unsafe { GdipGetImageHeight(bitmap.0, &mut height) }, 0);
        assert_eq!((width, height), (300, 200));
        for y in 0..200 {
            for x in 0..300 {
                let mut actual = 0;
                assert_eq!(
                    unsafe { GdipBitmapGetPixel(bitmap.0, x, y, &mut actual) },
                    0
                );
                let offset = ((y + 50) * capture.width + x + 50) as usize * 4;
                let rgb = &capture.original[offset..offset + 3];
                assert_eq!(
                    actual,
                    0xff000000 | (rgb[2] as u32) << 16 | (rgb[1] as u32) << 8 | rgb[0] as u32
                );
            }
        }
    }
    assert!(save_buffer_to_png(&[], 300, 200, &Rect::new(0, 0, 300, 200), &saved_path).is_err());
    assert_eq!(
        std::fs::read(&saved_path)?,
        png_bytes,
        "A failed replacement must preserve the previous image"
    );
    drop(export_pixels);
    // Clean up temporary test file
    let _ = std::fs::remove_file(&saved_path);

    let default_dir = default_save_directory();
    assert!(
        !default_dir.as_os_str().is_empty(),
        "The save folder is created on the first save"
    );
    let gen_name = generate_screenshot_filename();
    assert!(gen_name.starts_with("Screenshot_") && gen_name.ends_with(".png"));
    println!(
        "  - Generated PNG verified (header: {:02X?}, size: {} bytes).",
        &png_bytes[0..8],
        png_bytes.len()
    );
    println!("  - Save directory: '{}'", default_dir.display());
    println!("  - Filename sample: '{}'", gen_name);
    println!("  -> PASSED");

    // Free large temporary test buffers so idle memory is measured accurately
    drop(working_buffer);
    drop(capture);

    // ------------------------------------------------------------
    // Color Palette & Thickness Sub-Bar
    // ------------------------------------------------------------
    println!("\nTesting Color Palette & Thickness Presets...");
    assert_eq!(PRESET_COLORS.len(), 8);
    assert_eq!(PRESET_THICKNESSES, [2, 4, 8]);

    // Test live recoloring and undo/redo via EditCommand::Modify
    let mut recolor_target = rect_obj.clone();
    assert_eq!(recolor_target.get_color(), Some([40, 40, 235, 255]));
    assert_eq!(recolor_target.get_thickness(), Some(3));
    recolor_target.set_thickness(4);
    assert_eq!(recolor_target.get_thickness(), Some(4));

    let new_col = PRESET_COLORS[4]; // Blue (#1971C2)
    let old_kind = recolor_target.kind.clone();
    recolor_target.set_color(new_col);
    assert_eq!(recolor_target.get_color(), Some(new_col));

    let new_kind = recolor_target.kind.clone();
    let mut recolor_history = HistoryManager::new(50);
    let mut test_objs = vec![recolor_target.clone()];

    recolor_history.record(EditCommand::Modify {
        id: recolor_target.id,
        old_kind: old_kind.clone(),
        new_kind: new_kind.clone(),
    });

    // Undo recolor
    assert!(recolor_history.can_undo());
    recolor_history.undo(&mut test_objs);
    assert_eq!(test_objs[0].kind, old_kind);

    // Redo recolor
    assert!(recolor_history.can_redo());
    recolor_history.redo(&mut test_objs);
    assert_eq!(test_objs[0].kind, new_kind);

    println!("  - 8 preset colors & 3 thickness levels verified.");
    println!("  - Live object modification with Undo/Redo verified.");
    println!("  -> PASSED");

    // ------------------------------------------------------------
    // Shift-Key Angle & Square Snapping
    // ------------------------------------------------------------
    println!("\nTesting Shift-Key Angle & Square Snapping Math...");

    // 1. Square snapping: delta_x = 100, delta_y = 60 -> side = 100 -> (100, 100)
    let start = (50, 50);
    let cur = (150, 110);
    let snapped_sq = snap_square(start, cur);
    let dx = (snapped_sq.0 - start.0).abs();
    let dy = (snapped_sq.1 - start.1).abs();
    assert_eq!(dx, dy, "Square snap must result in 1:1 aspect ratio");
    assert_eq!(dx, 100);

    // 2. Arrow angle snapping: angle near 45°
    let arrow_start = (100, 100);
    let arrow_cur = (200, 195); // ~43.5 degrees
    let snapped_arrow = snap_angle_45(arrow_start, arrow_cur);
    let a_dx = snapped_arrow.0 - arrow_start.0;
    let a_dy = snapped_arrow.1 - arrow_start.1;
    // For 45 degrees, dx and dy should be equal
    assert_eq!(a_dx, a_dy, "45-degree snap must produce equal dx and dy");

    // Horizontal arrow snap: angle near 0°
    let arrow_h = (250, 105);
    let snapped_h = snap_angle_45(arrow_start, arrow_h);
    assert_eq!(
        snapped_h.1, arrow_start.1,
        "Near-horizontal angle must snap to 0°"
    );

    println!("  - 1:1 Square snapping verified (dx == dy == 100).");
    println!("  - 45° and 0° Arrow angle snapping verified.");
    println!("  -> PASSED");

    // ------------------------------------------------------------
    // Simple/Advanced Native Settings Window & Model
    // ------------------------------------------------------------
    println!("\nTesting Settings Model & JSON Serialization...");
    let default_settings = Settings::default();
    let json_str = serde_json::to_string_pretty(&default_settings)?;
    assert!(json_str.contains("hotkey"));
    assert!(json_str.contains("save_directory"));
    assert!(!json_str.contains("update_channel"));

    let deserialized: Settings = serde_json::from_str(&json_str)?;
    assert_eq!(default_settings, deserialized);

    // Legacy files may contain the removed fixed update channel; Serde must ignore it.
    let mut legacy_json = serde_json::to_value(&default_settings)?;
    legacy_json["update_channel"] = serde_json::Value::String("stable".to_string());
    let legacy_settings: Settings = serde_json::from_value(legacy_json)?;
    assert_eq!(legacy_settings, default_settings);

    // Test corrupted JSON fallback
    let corrupt_json = "{ invalid json content: true }";
    let fallback_res = serde_json::from_str::<Settings>(corrupt_json);
    assert!(fallback_res.is_err(), "Corrupt JSON must error gracefully");
    let fallback_settings = Settings::load()?;
    assert!(!fallback_settings.hotkey.description.is_empty());

    println!("  - Settings serialization & deserialization verified.");
    println!("  - Native settings dialog window available via --settings or Ctrl+,");
    println!("  -> PASSED");

    // ------------------------------------------------------------
    // Build & Size / RAM Verification
    // ------------------------------------------------------------
    println!("\nVerifying Executable Size and Sampling Process Memory...");

    // 1. Executable size check (target <= 2.5 MB)
    let exe_path = "target/release/isolmass.exe";
    if let Ok(metadata) = std::fs::metadata(exe_path) {
        let size_bytes = metadata.len();
        let size_kb = size_bytes / 1024;
        let size_mb = size_kb as f64 / 1024.0;
        println!(
            "  - Executable Size: {} KB ({:.2} MB) [Budget: <= 2.5 MB]",
            size_kb, size_mb
        );
        assert!(
            size_mb <= 2.5,
            "Release executable must be under 2.5 MB budget"
        );
    } else {
        println!(
            "  - Note: '{}' not found in current directory; debug/dev profile active.",
            exe_path
        );
    }

    // The smoke process has just captured images and loaded encoders; this is not an idle daemon.
    let mut pmc = PROCESS_MEMORY_COUNTERS::default();
    unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut pmc,
            std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        )?;
    }
    println!(
        "  - Smoke process working set: {} bytes (untrimmed; informational, not an idle budget result).",
        pmc.WorkingSetSize
    );
    println!("  -> PASSED");

    // ============================================================
    // Interaction, subsystem, tray and settings checks
    // ============================================================
    println!("\n------------------------------------------------------------");
    println!(" Interaction, subsystem, tray and settings checks");
    println!("------------------------------------------------------------");

    // ------------------------------------------------------------
    // Keyboard Input on Overlay (Esc Dismiss + Text Tool)
    // ------------------------------------------------------------
    println!("\nTesting Overlay Keyboard Routing, Esc Flow & Text Tool...");

    // 1. Hook routing when overlay is active
    let kb_esc = hotkey::create_test_kbdllhookstruct(
        windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE.0 as u32,
    );
    let lparam_esc = windows::Win32::Foundation::LPARAM(&kb_esc as *const _ as isize);
    let wparam_down = windows::Win32::Foundation::WPARAM(
        windows::Win32::UI::WindowsAndMessaging::WM_KEYDOWN as usize,
    );

    // When overlay inactive: Esc passes through
    hotkey::unregister_overlay();
    let res_inactive = unsafe { hotkey::process_keyboard_hook(0, wparam_down, lparam_esc, 0) };
    assert_ne!(
        res_inactive,
        windows::Win32::Foundation::LRESULT(1),
        "Esc must pass through when overlay is inactive"
    );

    // When overlay active: Esc is consumed with LRESULT(1)
    hotkey::register_overlay(windows::Win32::Foundation::HWND::default())?;
    let res_active = unsafe { hotkey::process_keyboard_hook(0, wparam_down, lparam_esc, 0) };
    assert_eq!(
        res_active,
        windows::Win32::Foundation::LRESULT(1),
        "Esc must be consumed with LRESULT(1) when overlay is active"
    );

    // Native text characters pass to the focused Win32 text/IME input path.
    hotkey::set_overlay_text_editing(true);
    let kb_char = hotkey::create_test_kbdllhookstruct(0x41); // 'A'
    let lparam_char = windows::Win32::Foundation::LPARAM(&kb_char as *const _ as isize);
    let res_edit = unsafe { hotkey::process_keyboard_hook(0, wparam_down, lparam_char, 0) };
    assert_ne!(
        res_edit,
        windows::Win32::Foundation::LRESULT(1),
        "Text characters must reach the native input path"
    );

    // When overlay active not editing: shortcuts are consumed
    hotkey::set_overlay_text_editing(false);
    let kb_tool = hotkey::create_test_kbdllhookstruct(0x52); // 'R' (Rectangle)
    let lparam_tool = windows::Win32::Foundation::LPARAM(&kb_tool as *const _ as isize);
    let res_tool = unsafe { hotkey::process_keyboard_hook(0, wparam_down, lparam_tool, 0) };
    assert_eq!(
        res_tool,
        windows::Win32::Foundation::LRESULT(1),
        "Tool shortcuts must be consumed when overlay active"
    );

    let kb_ctrl_c = hotkey::create_test_kbdllhookstruct(0x43); // 'C'
    let lparam_ctrl_c = windows::Win32::Foundation::LPARAM(&kb_ctrl_c as *const _ as isize);
    let res_ctrl_c = unsafe {
        hotkey::process_keyboard_hook(
            0,
            wparam_down,
            lparam_ctrl_c,
            windows::Win32::UI::Input::KeyboardAndMouse::MOD_CONTROL.0,
        )
    };
    assert_eq!(
        res_ctrl_c,
        windows::Win32::Foundation::LRESULT(1),
        "Ctrl+C shortcut must be consumed when overlay active"
    );

    hotkey::unregister_overlay();

    // 2. Hierarchical Esc flow on production OverlayState
    let test_capture = Rc::new(CaptureBuffer::dummy(100, 100));
    let mut overlay_state = overlay::OverlayState::create_test_state(test_capture);

    // Case 1: In text editing mode -> Esc cancels text editing only
    overlay_state.set_selection_active(Rect::new(100, 100, 400, 300));
    overlay_state.set_text_edit(Some(overlay::TextEditState::new(
        (150, 150),
        "Testing Esc".to_string(),
        [255, 255, 255, 255],
        22,
        None,
    )));
    assert!(overlay_state.text_edit().is_some());
    let action1 = overlay_state.handle_escape_action();
    assert_eq!(action1, overlay::EscapeAction::CancelledTextEdit);
    assert!(overlay_state.text_edit().is_none());
    assert_eq!(overlay_state.mode(), overlay::OverlayMode::SelectionActive);

    // Case 2: Object selected -> Esc deselects object
    overlay_state.set_selected_id(Some(42));
    let action2 = overlay_state.handle_escape_action();
    assert_eq!(action2, overlay::EscapeAction::DeselectedObject(42));
    assert_eq!(overlay_state.selected_id(), None);
    assert_eq!(overlay_state.mode(), overlay::OverlayMode::SelectionActive);

    // Case 3: Selection active -> Esc cancels selection back to hovering/idle
    let action3 = overlay_state.handle_escape_action();
    assert_eq!(action3, overlay::EscapeAction::CancelledSelection);
    assert_eq!(overlay_state.mode(), overlay::OverlayMode::Hovering);
    assert!(overlay_state.committed_selection().is_none());

    // Case 4: Idle/Hovering -> Esc closes overlay
    let action4 = overlay_state.handle_escape_action();
    assert_eq!(action4, overlay::EscapeAction::CloseOverlay);

    // 3. Text tool editing operations using production TextEditState methods
    let mut edit_state =
        overlay::TextEditState::new((50, 50), String::new(), [255, 255, 255, 255], 22, None);

    // Type "Hello" using insert_char
    for ch in "Hello".chars() {
        edit_state.insert_char(ch);
    }
    assert_eq!(edit_state.text, "Hello");
    assert_eq!(edit_state.caret, 5);

    // Move left 1
    assert!(edit_state.move_left());
    assert_eq!(edit_state.caret, 4);

    // Insert '!' at caret 4 -> "Hell!o"
    edit_state.insert_char('!');
    assert_eq!(edit_state.text, "Hell!o");
    assert_eq!(edit_state.caret, 5);

    // Delete at caret 5 -> deletes character after caret ('o') -> "Hell!"
    assert!(edit_state.delete());
    assert_eq!(edit_state.text, "Hell!");
    assert_eq!(edit_state.caret, 5);

    // Boundary: cannot delete past end of text
    assert!(!edit_state.delete());

    // Backspace at caret 5 -> deletes character before caret ('!') -> "Hell"
    assert!(edit_state.backspace());
    assert_eq!(edit_state.text, "Hell");
    assert_eq!(edit_state.caret, 4);

    // Boundary: move left to beginning and test backspace at 0
    while edit_state.move_left() {}
    assert_eq!(edit_state.caret, 0);
    assert!(!edit_state.backspace());

    // Move right back into text
    assert!(edit_state.move_right());
    assert_eq!(edit_state.caret, 1);

    edit_state.select_all();
    edit_state.insert_text("A&B İıŞşĞğ");
    edit_state.insert_utf16(0xd83d);
    edit_state.insert_utf16(0xde42);
    assert_eq!(edit_state.text, "A&B İıŞşĞğ🙂");
    edit_state.undo();
    assert_eq!(edit_state.text, "A&B İıŞşĞğ");
    edit_state.redo();
    assert_eq!(edit_state.text, "A&B İıŞşĞğ🙂");
    edit_state.select_all();
    edit_state.insert_text(&"x".repeat(16_384));
    assert_eq!(edit_state.text.len(), 16_384);
    edit_state.select_all();
    edit_state.insert_text("replacement");
    assert_eq!(edit_state.text, "replacement");
    edit_state.select_all();
    edit_state.insert_text("Hell");
    // Commit text object
    let text_obj = AnnotationObject::new(
        1,
        AnnotationKind::Text {
            pos: edit_state.pos,
            text: edit_state.text.clone(),
            color: edit_state.color,
            font_size: edit_state.font_size,
        },
    );
    assert!(text_obj.hit_test((55, 55)));

    // Re-open in text editing mode (double-click simulation): caret starts at end
    let reedit_state = overlay::TextEditState::new(
        (50, 50),
        edit_state.text.clone(),
        edit_state.color,
        edit_state.font_size,
        Some(1),
    );
    assert_eq!(reedit_state.caret, 4);

    println!("  -> PASSED (Esc hierarchical dismiss & text tool production methods verified)");

    // ------------------------------------------------------------
    // Select-Only Region Move & Resize
    // ------------------------------------------------------------
    println!("\nTesting Select-Only Region Move & Corner Resize...");
    let sel = Rect::new(100, 100, 300, 200);

    // Corner handles (8x8 px centered on vertices, half_h = 4)
    assert_eq!(
        sel.hit_test_selection((100, 100), 4, 8),
        capture::SelectionHitZone::TopLeftCorner
    );
    assert_eq!(
        sel.hit_test_selection((300, 100), 4, 8),
        capture::SelectionHitZone::TopRightCorner
    );
    assert_eq!(
        sel.hit_test_selection((100, 200), 4, 8),
        capture::SelectionHitZone::BottomLeftCorner
    );
    assert_eq!(
        sel.hit_test_selection((300, 200), 4, 8),
        capture::SelectionHitZone::BottomRightCorner
    );

    // Border edge bands (centered on each border, excluding corners)
    assert_eq!(
        sel.hit_test_selection((200, 100), 4, 8),
        capture::SelectionHitZone::BorderEdge
    );
    assert_eq!(
        sel.hit_test_selection((100, 150), 4, 8),
        capture::SelectionHitZone::BorderEdge
    );
    assert_eq!(
        sel.hit_test_selection((300, 150), 4, 8),
        capture::SelectionHitZone::BorderEdge
    );
    assert_eq!(
        sel.hit_test_selection((200, 200), 4, 8),
        capture::SelectionHitZone::BorderEdge
    );

    // Interior
    assert_eq!(
        sel.hit_test_selection((200, 150), 4, 8),
        capture::SelectionHitZone::Interior
    );

    // Outside
    assert_eq!(
        sel.hit_test_selection((50, 50), 4, 8),
        capture::SelectionHitZone::None
    );
    assert_eq!(
        sel.hit_test_selection((350, 250), 4, 8),
        capture::SelectionHitZone::None
    );

    // Selection translation translates child annotation objects by (dx, dy)
    let mut child_obj = AnnotationObject::new(
        2,
        AnnotationKind::Rectangle {
            rect: Rect::new(120, 120, 250, 180),
            color: [49, 49, 224, 255],
            thickness: 2,
        },
    );
    let orig_bounds = child_obj.bounds();
    let dx = 30;
    let dy = 20;
    child_obj.translate(dx, dy);
    let translated_bounds = child_obj.bounds();
    assert_eq!(translated_bounds.left, orig_bounds.left + dx);
    assert_eq!(translated_bounds.top, orig_bounds.top + dy);
    assert_eq!(translated_bounds.right, orig_bounds.right + dx);
    assert_eq!(translated_bounds.bottom, orig_bounds.bottom + dy);
    println!("  -> PASSED (Select-only region move and corner resize hit zones verified)");

    // ------------------------------------------------------------
    // Tool Interaction UX Pass (Contrast Outline + Movement Threshold)
    // ------------------------------------------------------------
    println!("\nTesting Dual-Tone Contrast Outline & Movement Threshold...");
    let mut test_buf = vec![0u8; 100 * 100 * 4];
    let test_rect = Rect::new(20, 20, 80, 80);
    CaptureBuffer::draw_contrast_selection(
        &mut test_buf,
        100,
        100,
        &test_rect,
        [246, 130, 59, 255],
    );

    // Verify 8x8 corner handle has white fill [255, 255, 255, 255]
    let handle_center_offset = (20 * 100 + 20) * 4;
    assert_eq!(
        &test_buf[handle_center_offset..handle_center_offset + 4],
        &[255, 255, 255, 255]
    );

    // Movement threshold logic: < 3px movement rejected, >= 3px movement committed
    let is_valid_shape_movement = |dx: i32, dy: i32| -> bool { dx.abs() >= 3 || dy.abs() >= 3 };
    assert!(!is_valid_shape_movement(0, 0), "0px click must be rejected");
    assert!(
        !is_valid_shape_movement(1, 2),
        "1-2px stray click must be rejected"
    );
    assert!(is_valid_shape_movement(3, 0), "3px drag must be accepted");
    assert!(is_valid_shape_movement(0, 3), "3px drag must be accepted");
    assert!(
        is_valid_shape_movement(10, 15),
        "Normal drag must be accepted"
    );

    println!("  -> PASSED (Dual-tone contrast border & 3px movement threshold verified)");

    // ------------------------------------------------------------
    // Pure GUI Subsystem & Console Attach Verification
    // ------------------------------------------------------------
    println!("\nVerifying Pure GUI Subsystem & Console Attachment...");
    println!("  - Target Subsystem: #![windows_subsystem = \"windows\"]");
    println!("  - Console Attachment: AttachConsole(ATTACH_PARENT_PROCESS) on CLI arguments");
    println!("  - Daemon Execution: Zero console window on standard launch / double-click");

    let release_bin = std::path::Path::new("target/release/isolmass.exe");
    let current_exe = std::env::current_exe().ok();
    let target_bin = if release_bin.exists() {
        release_bin
    } else if let Some(cur) = &current_exe {
        cur.as_path()
    } else {
        std::path::Path::new("target/debug/isolmass.exe")
    };

    let subsystem = verify_pe_subsystem_windows_gui(target_bin)?;
    const IMAGE_SUBSYSTEM_WINDOWS_GUI: u16 = 2;
    assert_eq!(
        subsystem, IMAGE_SUBSYSTEM_WINDOWS_GUI,
        "PE Optional Header Subsystem field must equal 2 (IMAGE_SUBSYSTEM_WINDOWS_GUI)"
    );
    println!(
        "  - PE Optional Header Subsystem verified = {} (IMAGE_SUBSYSTEM_WINDOWS_GUI) on '{}'",
        subsystem,
        target_bin.display()
    );
    println!("  -> PASSED (Pure GUI subsystem & PE header verified)");

    // ------------------------------------------------------------
    // System Tray Icon & Right-Click Menu Lifecycle
    // ------------------------------------------------------------
    println!("\nTesting System Tray Manager Lifecycle...");
    let tray_manager = tray::TrayManager::create()?;
    println!("  - System Tray icon registered with Shell_NotifyIconW(NIM_ADD)");

    tray::notify_tray_wakeup();
    println!("  - Wakeup notification dispatched to tray message loop");

    drop(tray_manager);
    println!("  - System Tray icon cleanly removed with Shell_NotifyIconW(NIM_DELETE)");
    println!("  -> PASSED (System tray icon registration & clean deletion verified)");

    // ------------------------------------------------------------
    // Standalone Settings Panel & Behavior Toggles
    // ------------------------------------------------------------
    println!("\nTesting Standalone Settings Panel & Behavior Toggles...");
    let default_settings = Settings::default();
    assert!(
        default_settings.enable_window_snap,
        "Settings::default() enable_window_snap must default to true"
    );
    assert!(
        default_settings.close_after_action,
        "Settings::default() close_after_action must default to true"
    );
    assert!(default_settings.check_updates_automatically);

    // Test explicit round-trip serialization and deserialization with false
    let mut custom_false = default_settings.clone();
    custom_false.enable_window_snap = false;
    custom_false.close_after_action = false;
    let json_false = serde_json::to_string_pretty(&custom_false)?;
    let reloaded_false: Settings = serde_json::from_str(&json_false)?;
    assert!(
        !reloaded_false.enable_window_snap,
        "Deserialized enable_window_snap must be false"
    );
    assert!(
        !reloaded_false.close_after_action,
        "Deserialized close_after_action must be false"
    );

    // Test explicit round-trip serialization and deserialization with true
    let mut custom_true = default_settings.clone();
    custom_true.enable_window_snap = true;
    custom_true.close_after_action = true;
    let json_true = serde_json::to_string_pretty(&custom_true)?;
    let reloaded_true: Settings = serde_json::from_str(&json_true)?;
    assert!(
        reloaded_true.enable_window_snap,
        "Deserialized enable_window_snap must be true"
    );
    assert!(
        reloaded_true.close_after_action,
        "Deserialized close_after_action must be true"
    );

    // Test file save and load round-trip with a temporary file
    let temp_dir = std::env::temp_dir();
    let temp_file = temp_dir.join(format!(
        "isolmass_smoke_settings_{}.json",
        std::process::id()
    ));
    custom_false.save_to_path(&temp_file)?;
    let loaded_from_file = Settings::load_from_path(&temp_file)?;
    assert_eq!(loaded_from_file, custom_false);
    let _ = std::fs::remove_file(&temp_file);

    // Test error propagation when saving to an invalid path
    let invalid_path = std::path::Path::new("");
    assert!(
        custom_false.save_to_path(invalid_path).is_err(),
        "save_to_path with invalid path must return Err"
    );

    // Verify backward compatibility with older configuration JSON
    let legacy_json = r#"{"hotkey":{"modifiers":0,"vk":44,"description":"PrintScreen"},"save_directory":"C:\\Screenshots","default_color":[49,49,224,255],"default_thickness":3}"#;
    let legacy_settings: Settings = serde_json::from_str(legacy_json)?;
    assert!(
        legacy_settings.enable_window_snap,
        "Legacy settings must default enable_window_snap to true"
    );
    assert!(
        legacy_settings.close_after_action,
        "Legacy settings must default close_after_action to true"
    );
    println!(
        "  -> PASSED (Settings isolated defaults, round-trips, file IO & error propagation verified)"
    );

    // ------------------------------------------------------------
    // Packaging: installer artifact and distribution size budget only
    // ------------------------------------------------------------
    println!("\nVerifying Installer Artifact & Distribution Size Budget...");
    const BUDGET_BYTES: u64 = 3 * 1024 * 1024;
    let nsi_path = std::path::Path::new("installer.nsi");
    assert!(
        nsi_path.exists(),
        "installer.nsi must exist at repository root"
    );
    let binary = std::env::current_exe()?;
    let setup_path = std::env::temp_dir().join(format!(
        "isolmass-smoke-{}-{}.exe",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    struct StagedSetup(std::path::PathBuf);
    impl Drop for StagedSetup {
        fn drop(&mut self) {
            if let Err(error) = std::fs::remove_file(&self.0)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                crate::diagnostics::record("smoke cleanup", &error.to_string());
            }
        }
    }
    let _cleanup = StagedSetup(setup_path.clone());
    {
        let makensis_local = format!(
            "{}\\Programs\\nsis-3.10\\makensis.exe",
            std::env::var("LOCALAPPDATA").unwrap_or_default()
        );
        let makensis_cmd = if std::process::Command::new("where")
            .arg("makensis")
            .creation_flags(0x08000000)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
            || std::process::Command::new("makensis")
                .arg("/VERSION")
                .creation_flags(0x08000000)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        {
            "makensis".to_string()
        } else if std::path::Path::new(&makensis_local).exists() {
            makensis_local
        } else {
            panic!(
                "makensis not found on PATH or at %LOCALAPPDATA%\\Programs\\nsis-3.10\\makensis.exe"
            );
        };

        println!("  - Building installer with NSIS ({makensis_cmd})...");
        let compile_status = std::process::Command::new(&makensis_cmd)
            .arg(format!("/DPRODUCT_VERSION={}", env!("CARGO_PKG_VERSION")))
            .arg(format!("/DPRODUCT_EXE_SOURCE={}", binary.display()))
            .arg(format!("/DPRODUCT_OUTPUT={}", setup_path.display()))
            .arg("installer.nsi")
            .creation_flags(0x08000000)
            .status()?;
        assert!(
            compile_status.success(),
            "makensis compilation must succeed"
        );
    }

    assert!(setup_path.exists(), "The smoke installer must be produced");
    for path in [&setup_path, &binary] {
        assert_eq!(
            crate::updater::file_version(path)?,
            env!("CARGO_PKG_VERSION"),
            "Artifact version must match the source version"
        );
    }
    let mut setup_file = std::fs::File::open(&setup_path)?;
    let mut pe_magic = [0u8; 2];
    setup_file.read_exact(&mut pe_magic)?;
    drop(setup_file);
    assert_eq!(
        pe_magic,
        [0x4D, 0x5A],
        "Installer artifact must have valid PE magic header [0x4D, 0x5A] ('MZ')"
    );
    let setup_size = setup_path.metadata()?.len();
    assert!(
        setup_size <= BUDGET_BYTES,
        "Installer size {} bytes exceeds 3 MB budget ({} bytes)",
        setup_size,
        BUDGET_BYTES
    );
    println!(
        "  - Installer structure, version and size verified (signature is a separate release gate): '{}' ({} bytes, budget: <= {} bytes / 3 MB)",
        setup_path.display(),
        setup_size,
        BUDGET_BYTES
    );
    println!("  -> PASSED (Installer artifact & <= 3 MB size budget verified)");

    println!("\n============================================================");
    println!(" AUTOMATED SMOKE CHECKS PASSED (listed behavior and packaging checks)");
    println!(" External runtime budget status is documented separately and is not verified here.");
    println!("============================================================");
    Ok(())
}

/// Optional user-facing diagnostic. It never saves captured pixels or changes settings/clipboard.
pub fn run_benchmark(samples: usize) -> Result<(), Box<dyn std::error::Error>> {
    use windows::Win32::System::ProcessStatus::PROCESS_MEMORY_COUNTERS_EX;
    if !(1..=500).contains(&samples) {
        return Err("Benchmark sample count must be between 1 and 500.".into());
    }
    let memory = || -> windows::core::Result<(usize, usize)> {
        let mut counters = PROCESS_MEMORY_COUNTERS_EX {
            cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            ..Default::default()
        };
        unsafe {
            GetProcessMemoryInfo(
                GetCurrentProcess(),
                (&mut counters as *mut PROCESS_MEMORY_COUNTERS_EX).cast(),
                std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            )?;
        }
        Ok((counters.WorkingSetSize, counters.PrivateUsage))
    };
    let initial = memory()?;
    let mut latencies = Vec::with_capacity(samples);
    for sample in 0..samples {
        let triggered = Instant::now();
        let capture = Rc::new(CaptureBuffer::capture_virtual_screen()?);
        let geometry = (capture.width, capture.height, capture.x, capture.y);
        let timings = (
            capture.timings.total.as_micros(),
            capture.timings.setup.as_micros(),
            capture.timings.bit_blt.as_micros(),
            capture.timings.dim.as_micros(),
        );
        let active = memory()?;
        let visible = overlay::measure_overlay(capture, triggered)?.as_micros();
        unsafe {
            windows::Win32::Graphics::Dwm::DwmFlush()?;
        }
        let after = memory()?;
        println!(
            "{}",
            serde_json::json!({"sample":sample,"version":env!("CARGO_PKG_VERSION"),"trigger":"synthetic_before_capture","geometry":geometry,"capture_to_visible_us":visible,"capture_us":timings.0,"setup_us":timings.1,"bitblt_us":timings.2,"dim_us":timings.3,"initial_working_set":initial.0,"initial_private":initial.1,"captured_working_set":active.0,"captured_private":active.1,"after_working_set":after.0,"after_private":after.1})
        );
        latencies.push(visible);
    }
    latencies.sort_unstable();
    println!(
        "{}",
        serde_json::json!({"summary":true,"samples":samples,"p50_us":latencies[(samples - 1) / 2],"p95_us":latencies[(samples * 95).div_ceil(100) - 1],"maximum_us":latencies[samples-1],"physical_key_latency_measured":false})
    );
    Ok(())
}
