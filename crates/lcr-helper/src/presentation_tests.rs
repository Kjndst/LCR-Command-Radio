use super::*;
use crate::config::Store;

#[repr(C)]
struct BitmapInfo {
    size: u32,
    width: i32,
    height: i32,
    planes: u16,
    bits: u16,
    compression: u32,
    image_size: u32,
    x_resolution: i32,
    y_resolution: i32,
    colors_used: u32,
    colors_important: u32,
    color: u32,
}

#[link(name = "user32")]
extern "system" {
    fn GetThreadDpiAwarenessContext() -> I;
    fn AreDpiAwarenessContextsEqual(first: I, second: I) -> i32;
    fn GetDC(h: W) -> I;
    fn ReleaseDC(h: W, dc: I) -> i32;
    fn UpdateWindow(h: W) -> i32;
    fn RedrawWindow(h: W, rect: *const Rect, region: I, flags: u32) -> i32;
    fn IsWindowVisible(h: W) -> i32;
    fn IsWindow(h: W) -> i32;
    fn DestroyIcon(icon: I) -> i32;
    fn GetClassLongPtrW(h: W, index: i32) -> I;
    fn GetWindowRgn(h: W, region: I) -> i32;
}
#[link(name = "dwmapi")]
extern "system" {
    fn DwmFlush() -> i32;
}
#[link(name = "gdi32")]
extern "system" {
    fn CreateRectRgn(left: i32, top: i32, right: i32, bottom: i32) -> I;
    fn GetRgnBox(region: I, rect: *mut Rect) -> i32;
    fn PtInRegion(region: I, x: i32, y: i32) -> i32;
    fn CreateDIBSection(
        dc: I,
        info: *const BitmapInfo,
        usage: u32,
        pixels: *mut *mut core::ffi::c_void,
        section: I,
        offset: u32,
    ) -> I;
    fn GetDIBits(
        dc: I,
        bitmap: I,
        start: u32,
        lines: u32,
        pixels: *mut u8,
        info: *mut BitmapInfo,
        usage: u32,
    ) -> i32;
}

unsafe fn snapshot(h: W, directory: &Path, name: &str) {
    capture(h, directory, name, false);
}
unsafe fn capture(h: W, directory: &Path, name: &str, hwnd_surface: bool) {
    InvalidateRect(h, core::ptr::null(), 0);
    RedrawWindow(h, core::ptr::null(), 0, 0x0181); // INVALIDATE | ALLCHILDREN | UPDATENOW
    UpdateWindow(h);
    let mut client: Rect = core::mem::zeroed();
    GetClientRect(h, &mut client);
    let width = client.right;
    let height = client.bottom;
    let dc = CreateCompatibleDC(0);
    let mut info = BitmapInfo {
        size: 40,
        width,
        height: -height,
        planes: 1,
        bits: 32,
        compression: 0,
        image_size: 0,
        x_resolution: 0,
        y_resolution: 0,
        colors_used: 0,
        colors_important: 0,
        color: 0,
    };
    let mut bits = core::ptr::null_mut();
    let bitmap = CreateDIBSection(dc, &info, 0, &mut bits, 0, 0);
    assert_ne!(bitmap, 0);
    let previous = SelectObject(dc, bitmap);
    if hwnd_surface {
        DwmFlush();
        let source = GetDC(h);
        assert_ne!(source, 0);
        assert_ne!(
            BitBlt(dc, 0, 0, width, height, source, 0, 0, 0x00CC0020),
            0,
            "Native HWND capture failed"
        );
        ReleaseDC(h, source);
    } else {
        // PrintWindow may obtain compositor output without invoking the custom
        // print callback. Exercise our Direct2D DC target explicitly and require
        // its EndDraw result, then compare separately with visible HWND output.
        assert_eq!(
            SendMessageW(h, 0x0318, dc as usize, 4),
            1,
            "Direct2D print target must complete successfully"
        );
    }
    SelectObject(dc, previous);
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    assert_eq!(
        GetDIBits(
            dc,
            bitmap,
            0,
            height as u32,
            pixels.as_mut_ptr(),
            &mut info,
            0
        ),
        height
    );
    let mut bmp = Vec::with_capacity(54 + pixels.len());
    bmp.extend(b"BM");
    bmp.extend(((54 + pixels.len()) as u32).to_le_bytes());
    bmp.extend([0u8; 4]);
    bmp.extend(54u32.to_le_bytes());
    // A standard top-down 32-bit BI_RGB BMP; artifact conversion drops unused alpha.
    let header = core::slice::from_raw_parts((&info as *const BitmapInfo).cast::<u8>(), 40);
    bmp.extend(header);
    bmp.extend(pixels);
    fs::write(directory.join(format!("{name}.bmp")), bmp).unwrap();
    DeleteObject(bitmap);
    DeleteDC(dc);
}

unsafe fn alignment_proof(state: &RefCell<S>, directory: &Path, pair: bool) {
    let x = state.borrow();
    let renderer = x.renderer.borrow();
    let layout = renderer.layout;
    let close = |actual: f32, expected: f32| {
        assert!(
            (actual - expected).abs() < 0.01,
            "Alignment {actual} != {expected}"
        )
    };
    let find = |name: &str| renderer.records.iter().find(|r| r.name == name).unwrap();
    if pair {
        let heading = find("heading-group");
        close(heading.origin_x + heading.width / 2.0, layout.viewport.cx());
        let field = find("pair-field");
        close(
            field.baseline - TypeRole::UiLabel.token().0 * renderer.cap_ratio / 2.0,
            layout.pair_content.cy(),
        );
        close(field.origin_x + field.width / 2.0, layout.pair_content.cx());
        let ready = renderer
            .records
            .iter()
            .find(|r| r.name == "measured-group" && r.rect == layout.pair_ready)
            .unwrap();
        let o = find("ready-o-anchor");
        close(o.origin_x, layout.viewport.cx());
        close(o.origin_y, layout.pair_ready.cy());
        assert!(ready.origin_x < o.origin_x);
        assert_eq!(layout.pair_accent.x, 91.0);
        assert_eq!(
            (
                layout.pair_field.x,
                layout.pair_field.y,
                layout.pair_field.w
            ),
            (48.0, 400.0, 384.0)
        );
        assert_eq!(
            (
                layout.pair_action.cx(),
                layout.pair_action.y,
                layout.pair_action.w
            ),
            (240.0, 480.0, 384.0)
        );
        let left = find("descriptor-left");
        let right = find("descriptor-right");
        close(
            (left.rect.right() + right.rect.x) / 2.0,
            layout.viewport.cx(),
        );
        close(
            left.origin_y + left.height / 2.0,
            layout.pair_descriptor.cy(),
        );
        close(
            right.origin_y + right.height / 2.0,
            layout.pair_descriptor.cy(),
        );
        close(layout.pair_icon.cy(), layout.pair_content.cy());
    } else {
        let descriptor = find("header-descriptor");
        close(descriptor.origin_x, layout.header_wordmark.x);
        let cap = TypeRole::Metadata.token().0 * renderer.cap_ratio;
        let fixed_cap = 10.5 * MAIN_BRAND_SCALE * renderer.cap_ratio;
        let ink_bottom = descriptor.baseline;
        close(
            (layout.header_wordmark.y + layout.header_descriptor.cy() + fixed_cap / 2.0) / 2.0,
            layout.header_logo.cy(),
        );
        assert_eq!(layout.key, BoxDip::new(32.0, 287.0, 416.0, 86.0));
        close(
            layout.sound.y - (layout.sound.y - UTILITY_PADDING),
            (layout.sound.bottom() + UTILITY_PADDING) - layout.sound.bottom(),
        );
        close(ink_bottom - cap / 2.0, layout.header_descriptor.cy());
        close(layout.header_wordmark.x, 133.0);
        close(layout.key_caption.cy(), layout.key.bottom());
        close(layout.header_wordmark.w, 104.0 * 1.1);
        close(layout.key.y - layout.section.bottom(), 13.0);
        let value = find("radio-value");
        close(value.origin_x + value.width / 2.0, layout.key.cx());
        close(value.origin_y + value.height / 2.0, layout.key.cy());
        close(layout.key.cy(), 330.0);
        close(layout.section.y, 250.0);
        close(layout.status.cy(), 554.0);
        close(layout.tagline.y, 208.0);
        close(layout.footer.y, 592.0);
        close(
            layout.status.cy() - layout.status_separators[0],
            layout.status_separators[1] - layout.status.cy(),
        );
        for cell in layout.status_cells {
            let group = renderer
                .records
                .iter()
                .find(|r| r.name == "measured-group" && r.rect == cell)
                .unwrap();
            close(group.origin_x + group.width / 2.0, cell.cx());
            close(group.origin_y + group.height / 2.0, cell.cy());
            assert!(group.width < cell.w);
        }
        close(
            find("footer-message").baseline,
            find("footer-version").baseline,
        );
        close(
            find("footer-version").origin_x + find("footer-version").width,
            layout.footer.right(),
        );
    }
    let mut transcript = String::from(
        "name\trect_x\trect_y\trect_w\trect_h\torigin_x\torigin_y\ttext_w\ttext_h\tbaseline\n",
    );
    for r in &renderer.records {
        transcript.push_str(&format!(
            "{}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\n",
            r.name,
            r.rect.x,
            r.rect.y,
            r.rect.w,
            r.rect.h,
            r.origin_x,
            r.origin_y,
            r.width,
            r.height,
            r.baseline
        ));
    }
    fs::write(
        directory.join(if pair {
            "pair-alignment.tsv"
        } else {
            "main-alignment.tsv"
        }),
        transcript,
    )
    .unwrap();
}

unsafe fn grid_snapshot(h: W, state: &RefCell<S>, directory: &Path, pair: bool) {
    state.borrow().renderer.borrow_mut().grid = true;
    snapshot(
        h,
        directory,
        if pair {
            "pair-grid-proof"
        } else {
            "main-grid-proof"
        },
    );
    state.borrow().renderer.borrow_mut().grid = false;
}

unsafe fn background_snapshot(h: W, state: &RefCell<S>, directory: &Path, pair: bool) {
    state.borrow().renderer.borrow_mut().background_only = true;
    snapshot(
        h,
        directory,
        if pair {
            "pair-background-only"
        } else {
            "main-background-only"
        },
    );
    if !pair {
        state.borrow().renderer.borrow_mut().skip_main_dim = true;
        snapshot(h, directory, "main-background-undimmed");
        state.borrow().renderer.borrow_mut().skip_main_dim = false;
    }
    state.borrow().renderer.borrow_mut().background_only = false;
    snapshot(h, directory, if pair { "pair-100" } else { "main-100" });
}

fn assert_shared_footer(state: &RefCell<S>) {
    let x = state.borrow();
    let r = x.renderer.borrow();
    let message = r
        .records
        .iter()
        .find(|r| r.name == "footer-message")
        .unwrap();
    let version = r
        .records
        .iter()
        .find(|r| r.name == "footer-version")
        .unwrap();
    assert!((message.baseline - version.baseline).abs() < 0.01);
    assert_eq!(message.rect.y, r.layout.footer.y);
}

unsafe fn assert_guidance(state: &RefCell<S>, expected: &str) {
    let x = state.borrow();
    assert_eq!(
        get(x.foot),
        expected,
        "Native footer copy must match visible guidance"
    );
    let mut r = x.renderer.borrow_mut();
    let record = r
        .records
        .iter()
        .find(|r| r.name == "footer-message")
        .unwrap();
    let actual_width = record.width;
    let available = record.rect.w;
    let wanted = r.measure(expected, TypeRole::Metadata).unwrap();
    assert!((actual_width - wanted.width).abs() < 0.01);
    assert!(
        actual_width <= available,
        "Guidance must not collide with version"
    );
}

unsafe fn assert_pair_feedback(state: &RefCell<S>, expected: &str) {
    let x = state.borrow();
    let mut r = x.renderer.borrow_mut();
    let records: Vec<_> = r
        .records
        .iter()
        .filter(|r| r.name == "pair-feedback")
        .collect();
    assert_eq!(records.len(), 1, "Exactly one lower Pair feedback cluster");
    let actual = records[0].width;
    assert_eq!(records[0].rect, r.layout.pair_ready);
    assert!(!r
        .records
        .iter()
        .any(|r| r.name == "pair-notice" || r.name == "ready-o-anchor"));
    let wanted = r.measure(expected, TypeRole::Metadata).unwrap();
    assert!((actual - wanted.width).abs() < 0.01);
    assert!(wanted.width < r.layout.pair_ready.w);
}

unsafe fn interaction_snapshot(
    h: W,
    state: &RefCell<S>,
    directory: &Path,
    button: W,
    id: usize,
    name: &str,
) {
    SetFocus(h);
    state.borrow_mut().hover_button = 0;
    snapshot(h, directory, &format!("{name}-idle"));
    SendMessageW(button, MOUSE_MOVE, 0, (5 << 16) | 5);
    assert_eq!(state.borrow().hover_button, id);
    snapshot(h, directory, &format!("{name}-hover"));
    SendMessageW(button, 0x00F3, 1, 0);
    snapshot(h, directory, &format!("{name}-pressed"));
    SendMessageW(button, 0x00F3, 0, 0);
    SendMessageW(button, MOUSE_LEAVE, 0, 0);
    assert_eq!(state.borrow().hover_button, 0);
}

unsafe fn typography_snapshot(h: W, state: &RefCell<S>, directory: &Path, name: &str) {
    state.borrow().renderer.borrow_mut().hide_text = true;
    snapshot(h, directory, &format!("{name}-text-background"));
    state.borrow().renderer.borrow_mut().hide_text = false;
    snapshot(h, directory, name);
    let x = state.borrow();
    let r = x.renderer.borrow();
    let mut receipt = String::from(
        "name\ttext\tfamily\tsize\tweight\tleading\ttrailing\tx\ty\tw\th\tr\tg\tb\ta\to_anchor\n",
    );
    for line in &r.typography_records {
        receipt.push_str(line);
    }
    fs::write(directory.join(format!("{name}-typography.tsv")), receipt).unwrap();
}

pub(super) unsafe fn maybe_run(h: W, state: &RefCell<S>) -> bool {
    let Some(directory) = std::env::var_os("LCR_UI_V6_PROOF_DIR") else {
        return false;
    };
    let directory = PathBuf::from(directory);
    fs::create_dir_all(&directory).unwrap();
    assert_ne!(
        AreDpiAwarenessContextsEqual(GetThreadDpiAwarenessContext(), -4),
        0,
        "UI thread must be per-monitor V2 aware"
    );
    assert_eq!(
        GetWindowLongPtrW(h, -16) as u32 & 0x00040000,
        0,
        "Freeform resize style must stay absent"
    );
    assert!(
        get(state.borrow().code).is_empty(),
        "Cue must not become pairing-code content"
    );
    {
        let x = state.borrow();
        let renderer = x.renderer.borrow();
        assert_eq!(
            renderer.fonts.collection.GetFontFamilyCount(),
            1,
            "Private collection must contain only the embedded Jura family"
        );
        let mut index = 0;
        let mut exists = ::windows::core::BOOL(0);
        renderer
            .fonts
            .collection
            .FindFamilyName(w!("Jura"), &mut index, &mut exists)
            .unwrap();
        assert!(
            exists.as_bool(),
            "Exact embedded Jura must be available to DirectWrite"
        );
        assert_eq!(renderer.dpi, x.dpi);
    }
    for (which, class_index) in [(1, -14), (0, -34)] {
        let icon = SendMessageW(h, 0x007F, which, 0); // WM_GETICON
        assert_ne!(icon, 0, "Window icon must be explicit");
        assert_eq!(icon, GetClassLongPtrW(h, class_index));
    }
    let region = CreateRectRgn(0, 0, 0, 0);
    assert_ne!(GetWindowRgn(h, region), 0);
    let mut region_box: Rect = core::mem::zeroed();
    assert_ne!(GetRgnBox(region, &mut region_box), 0);
    assert_eq!(
        (
            region_box.left,
            region_box.top,
            region_box.right,
            region_box.bottom
        ),
        (4, 4, 477, 637)
    );
    assert_eq!(
        PtInRegion(region, 240, 2),
        0,
        "Dark exterior gutter is not part of HWND"
    );
    assert_ne!(PtInRegion(region, 240, 4), 0, "Visible core is retained");
    assert_eq!(PtInRegion(region, 478, 320), 0);
    DeleteObject(region);
    fs::write(directory.join("icon-frame-authority.txt"), "PASS: WM_GETICON BIG/SMALL explicit and equal WNDCLASSEX class authority; approved APP ICO loads all six sizes. HWND region bbox4,4,477,637 removes exterior dark gutter at unchanged client480x640; core remains in region. Shortcut icon0 authority separately covered by canonical ShellLink tests. No cache clearing or installed shortcut mutation.\n").unwrap();
    for size in [256, 128, 64, 48, 32, 16] {
        let icon = ico(APP, size);
        assert_ne!(icon, 0, "App icon failed at {size}px");
        DestroyIcon(icon);
    }
    {
        let mut x = state.borrow_mut();
        assert!(!x.r.is_paired());
        view(h, &mut x, View::Pair);
    }
    ShowWindow(h, SHOW);
    snapshot(h, &directory, "pair-100");
    typography_snapshot(h, state, &directory, "pair-golden");
    background_snapshot(h, state, &directory, true);
    let field_baseline = state
        .borrow()
        .renderer
        .borrow()
        .records
        .iter()
        .find(|r| r.name == "pair-field")
        .unwrap()
        .baseline;
    let assert_field = || {
        let x = state.borrow();
        let renderer = x.renderer.borrow();
        let field = renderer
            .records
            .iter()
            .find(|r| r.name == "pair-field")
            .unwrap();
        assert!((field.baseline - field_baseline).abs() < 0.01);
        assert_eq!(field.rect, renderer.layout.pair_content);
        assert!((field.origin_x + field.width / 2.0 - field.rect.cx()).abs() < 0.01);
    };
    capture(h, &directory, "pair-hwnd-100", true);
    alignment_proof(state, &directory, true);
    grid_snapshot(h, state, &directory, true);
    let code = state.borrow().code;
    for character in "ABC123".chars() {
        SendMessageW(code, 0x0102, character as usize, 0); // WM_CHAR, native edit.
    }
    assert_eq!(get(code), "ABC123", "Cue must yield to actual edit content");
    snapshot(h, &directory, "pair-code-100");
    assert_field();
    SendMessageW(code, LBUTTON_UP, 0, 0);
    let mut selection_start = 0u32;
    let mut selection_end = 0u32;
    SendMessageW(
        code,
        0x00B0,
        (&mut selection_start as *mut u32) as usize,
        (&mut selection_end as *mut u32) as isize,
    );
    assert_eq!(
        (selection_start, selection_end),
        (0, 6),
        "Native click must preserve select-all replacement behavior"
    );
    snapshot(h, &directory, "pair-selection-100");
    SendMessageW(code, 0x0102, 'Z' as usize, 0);
    assert_eq!(get(code), "Z");
    set(code, "");
    assert!(get(code).is_empty());
    snapshot(h, &directory, "pair-cleared-100");
    let pair = state.borrow().pair_btn;
    interaction_snapshot(h, state, &directory, pair, PAIR, "pair-action");
    SetFocus(code);
    state.borrow_mut().notice = None;
    SendMessageW(code, KEY_DOWN, 0x0D, 1 << 30);
    assert!(
        state.borrow().notice.is_none(),
        "Held Enter repeat must not resubmit"
    );
    SendMessageW(code, KEY_DOWN, 0x0D, 0);
    assert_eq!(state.borrow().notice, Some((l::PAIR_EMPTY_CODE, true)));
    SendMessageW(code, 0x0102, 0x0D, 0);
    assert!(
        get(code).is_empty(),
        "Enter character must not enter the code"
    );
    snapshot(h, &directory, "pair-enter-error");
    assert_pair_feedback(state, l::PAIR_EMPTY_CODE);
    CONNECTED_CUE_COUNT.with(|count| assert_eq!(count.get(), 0, "Failure must not play success"));
    state.borrow_mut().hover_button = PAIR;
    snapshot(h, &directory, "pair-hover-100");
    SendMessageW(pair, 0x00F3, 1, 0);
    snapshot(h, &directory, "pair-pressed-100");
    SendMessageW(pair, 0x00F3, 0, 0);
    state.borrow_mut().hover_button = 0;
    SendMessageW(pair, 0x00F5, 0, 0); // BM_CLICK exercises native WM_COMMAND dispatch.
    assert_eq!(state.borrow().notice, Some((l::PAIR_EMPTY_CODE, true)));
    snapshot(h, &directory, "pair-error-100");
    assert_field();
    assert_pair_feedback(state, l::PAIR_EMPTY_CODE);
    for (message, name) in [
        (l::PAIR_DENIED, "pair-denied-100"),
        (l::PAIR_TRANSIENT, "pair-unavailable-100"),
    ] {
        state.borrow_mut().notice = Some((message, true)); // Visual fixture, not network evidence.
        snapshot(h, &directory, name);
        assert_pair_feedback(state, message);
    }
    state.borrow_mut().notice = Some((l::PAIR_EMPTY_CODE, true));
    SendMessageW(code, 0x0102, 'Z' as usize, 0);
    snapshot(h, &directory, "pair-correcting-code-100");
    assert!(!state
        .borrow()
        .renderer
        .borrow()
        .records
        .iter()
        .any(|r| r.name == "pair-feedback"));
    assert_eq!(get(code), "Z");
    set(code, "");
    snapshot(h, &directory, "pair-error-100");
    {
        state.borrow_mut().hover_button = CLOSE_BUTTON;
    }
    snapshot(h, &directory, "close-hover-100");
    SendMessageW(state.borrow().close_btn, 0x00F3, 1, 0); // BM_SETSTATE, presentation only
    snapshot(h, &directory, "close-pressed-100");
    SendMessageW(state.borrow().close_btn, 0x00F3, 0, 0);
    {
        state.borrow_mut().hover_button = 0;
    }
    fs::write(directory.join("guidance-proof.txt"), "PASS: Pair one concise lower error cluster; ordinary field cue/typed value preserved; stale empty-code feedback visually yields once actual code is entered. Native+rendered Main idle/transmitting, Capture, Confirm and settings-error priority guidance measured to fit footer/version. Denied/unavailable Pair screenshots are isolated visual fixtures only.\n").unwrap();
    let mut window: Rect = core::mem::zeroed();
    GetWindowRect(h, &mut window);
    for (px, py) in [
        (window.left + 1, window.top + 1),
        (window.right - 2, window.top + 1),
        (window.left + 1, window.bottom - 2),
        (window.right - 2, window.bottom - 2),
    ] {
        let packed = ((py as u16 as usize) << 16) | (px as u16 as usize);
        let hit = SendMessageW(h, NCHITTEST, 0, packed as isize);
        assert!(
            !(10..=17).contains(&hit),
            "No edge/corner resize hit must be exposed"
        );
    }

    // Explicit visual fixture, never a fake live-pair/network acceptance claim.
    {
        let mut x = state.borrow_mut();
        complete_pair_presentation(h, &mut x); // Successful completion callback fixture only.
        assert_eq!(x.mode, View::Main);
        set_view_control_show_cmd(x.unpair_btn, SHOW);
    }
    CONNECTED_CUE_COUNT.with(|count| assert_eq!(count.get(), 1));
    snapshot(h, &directory, "pair-completed-main");
    {
        complete_pair_presentation(h, &mut state.borrow_mut());
        // This fixture's runtime is intentionally unpaired. Retain the real
        // paired Main surface after the synthetic repeated completion.
        set_view_control_show_cmd(state.borrow().unpair_btn, SHOW);
    }
    snapshot(h, &directory, "pair-completed-redraw");
    CONNECTED_CUE_COUNT.with(|count| {
        assert_eq!(
            count.get(),
            1,
            "Main restore/redraw cannot replay connected cue"
        )
    });
    fs::write(directory.join("enter-connected-proof.txt"), "PASS: active native EDIT Enter uses PAIR command validation; empty Enter yields CODE REQUIRED, held repeats do not submit and CR never becomes field content. Failure plays zero connected cues; successful Pair->Main completion callback schedules one cue; subsequent Main completion/redraw schedules none. Native fixture suppresses actual sound and counts scheduling; valid network pairing/acoustic device acceptance not claimed.\n").unwrap();
    let x = state.borrow();
    for hidden in [x.config_btn, x.change_btn, x.code, x.pair_btn] {
        assert_eq!(IsWindowVisible(hidden), 0);
    }
    let key = x.key;
    let sound_on = x.sound_on_btn;
    let sound_off = x.sound_off_btn;
    let unpair = x.unpair_btn;
    let cancel = x.apply_btn;
    let close = x.close_btn;
    drop(x);
    interaction_snapshot(h, state, &directory, key, KEY_DISPLAY, "radio-key");
    set(key, l::MOUSE5);
    interaction_snapshot(h, state, &directory, key, KEY_DISPLAY, "radio-mouse5");
    set(key, l::MOUSE4);
    interaction_snapshot(h, state, &directory, sound_on, SOUND_ON, "ptt-on-selected");
    interaction_snapshot(
        h,
        state,
        &directory,
        sound_off,
        SOUND_OFF,
        "ptt-off-inactive",
    );
    state.borrow_mut().ptt_sound = false; // Visual state only; isolated persistent test follows.
    interaction_snapshot(h, state, &directory, sound_on, SOUND_ON, "ptt-on-inactive");
    interaction_snapshot(
        h,
        state,
        &directory,
        sound_off,
        SOUND_OFF,
        "ptt-off-selected",
    );
    state.borrow_mut().ptt_sound = true;
    assert_ne!(IsWindowVisible(unpair), 0, "Main must retain UNPAIR");
    snapshot(h, &directory, "main-100");
    typography_snapshot(h, state, &directory, "main-golden");
    assert!(state
        .borrow()
        .renderer
        .borrow()
        .records
        .iter()
        .any(|record| record.name == "unpair"));
    assert_guidance(state, l::FOOTER_READY);
    background_snapshot(h, state, &directory, false);
    capture(h, &directory, "main-hwnd-100", true);
    alignment_proof(state, &directory, false);
    grid_snapshot(h, state, &directory, false);
    state.borrow_mut().hover_button = KEY_DISPLAY;
    snapshot(h, &directory, "key-hover-100");
    SendMessageW(key, 0x00F3, 1, 0);
    snapshot(h, &directory, "key-pressed-100");
    SendMessageW(key, 0x00F3, 0, 0);
    state.borrow_mut().hover_button = 0;
    // Deterministic fault injection exercises the production EndDraw branch;
    // it neither resets the GPU nor mutates runtime/input authority.
    {
        let x = state.borrow();
        let mut renderer = x.renderer.borrow_mut();
        let generation = renderer.target_generation;
        assert!(generation > 0);
        let fault = Err(::windows::core::Error::from_hresult(D2DERR_RECREATE_TARGET));
        assert!(!renderer.finish_draw(h, &fault));
        assert!(renderer.hwnd_surface.is_none());
        assert!(!renderer.finish_draw(h, &fault));
        assert_eq!(renderer.consecutive_losses, 2);
        renderer.paint(h, &x).unwrap();
        assert_eq!(renderer.target_generation, generation + 1);
        assert_eq!(renderer.consecutive_losses, 0);
        assert!(renderer.last_error.is_none());
        fs::write(directory.join("target-recreation.txt"),"PASS: injected D2DERR_RECREATE_TARGET drops dependent resources; repeated loss has bounded immediate retry; successful paint recreates target/assets while private fonts/layouts and product state survive. No physical GPU-loss claim.\n").unwrap();
    }
    // Synthetic WM_DPICHANGED proof: real HWND/control pixel bounds plus D2D
    // targets at 144 DPI. Actual high-DPI monitor/user-input proof stays separate.
    let physical_dpi = GetDpiForWindow(h);
    let mut original: Rect = core::mem::zeroed();
    GetWindowRect(h, &mut original);
    let suggested = Rect {
        left: original.left,
        top: original.top,
        right: original.left + 720,
        bottom: original.top + 960,
    };
    SendMessageW(
        h,
        0x02E0,
        144 | (144 << 16),
        (&suggested as *const Rect) as isize,
    );
    assert_eq!(state.borrow().dpi, 144);
    let mut scaled: Rect = core::mem::zeroed();
    GetClientRect(h, &mut scaled);
    assert_eq!((scaled.right, scaled.bottom), (720, 960));
    {
        let x = state.borrow();
        x.renderer
            .borrow_mut()
            .paint(h, &x)
            .expect("144 DPI HWND target must render successfully");
    }
    snapshot(h, &directory, "main-150-synthetic");
    {
        let x = state.borrow();
        let renderer = x.renderer.borrow();
        for surface in [
            renderer.hwnd_surface.as_ref().unwrap(),
            renderer.print_surface.as_ref().unwrap_or_else(|| {
                panic!(
                    "Print target lost; error={:?}, dpi={}, state_dpi={}",
                    renderer.last_error, renderer.dpi, x.dpi
                )
            }),
        ] {
            let mut dx = 0.0;
            let mut dy = 0.0;
            surface.target.GetDpi(&mut dx, &mut dy);
            assert_eq!((dx, dy), (144.0, 144.0));
        }
    }
    alignment_proof(state, &directory, false);
    {
        let mut x = state.borrow_mut();
        view(h, &mut x, View::Pair);
    }
    snapshot(h, &directory, "pair-150-synthetic");
    alignment_proof(state, &directory, true);
    let fractional = Rect {
        left: original.left,
        top: original.top,
        right: original.left + 600,
        bottom: original.top + 800,
    };
    SendMessageW(
        h,
        0x02E0,
        120 | (120 << 16),
        (&fractional as *const Rect) as isize,
    );
    assert_eq!(state.borrow().dpi, 120);
    snapshot(h, &directory, "pair-125-synthetic");
    alignment_proof(state, &directory, true);
    {
        let mut x = state.borrow_mut();
        view(h, &mut x, View::Main);
    }
    snapshot(h, &directory, "main-125-synthetic");
    alignment_proof(state, &directory, false);
    {
        let x = state.borrow();
        let renderer = x.renderer.borrow();
        for surface in [
            renderer.hwnd_surface.as_ref().unwrap(),
            renderer.print_surface.as_ref().unwrap(),
        ] {
            let mut dx = 0.0;
            let mut dy = 0.0;
            surface.target.GetDpi(&mut dx, &mut dy);
            assert_eq!((dx, dy), (120.0, 120.0));
        }
        assert_eq!(renderer.core_paths.len(), 13);
    }
    SendMessageW(
        h,
        0x02E0,
        96 | (96 << 16),
        (&original as *const Rect) as isize,
    );
    {
        let mut x = state.borrow_mut();
        view(h, &mut x, View::Main);
        set_view_control_show_cmd(x.unpair_btn, SHOW);
    }
    snapshot(h, &directory, "main-after-dpi-100");
    alignment_proof(state, &directory, false);
    fs::write(directory.join("dpi-proof.txt"),format!("PHYSICAL_MONITOR_DPI={physical_dpi}\nREFERENCE_DPI=96\nSYNTHETIC_WM_DPICHANGED_DPI=144,120\nSYNTHETIC_CLIENT_PIXELS=720x960,600x800\nLOGICAL_VIEWPORT=480x640 DIPs\nPASS: same measured alignment/grid, scaled HWND/control bounds and renderer-resource recreation at 96->144->120->96.\nNOT CLAIMED: actual high-DPI monitor transition or high-DPI physical input acceptance.\n")).unwrap();
    {
        let mut x = state.borrow_mut();
        x.tx = true; // Visual fixture only; no control or relay authority is granted.
        refresh_transmit_presentation(h, &x);
    }
    snapshot(h, &directory, "transmitting-100");
    assert_guidance(state, l::FOOTER_TRANSMITTING);
    {
        let mut x = state.borrow_mut();
        x.tx = false;
        refresh_transmit_presentation(h, &x);
    }
    let layout = Layout::new();
    let click_x = (layout.key.cx() - layout.key_control().x) as usize;
    let click_y = (layout.key_caption.cy() - layout.key_control().y + HALF_GRID) as usize;
    let click = (click_y << 16) | click_x;
    SendMessageW(key, 0x0201, 1, click as isize);
    SendMessageW(key, LBUTTON_UP, 0, click as isize);
    assert_eq!(state.borrow().mode, View::Capture);
    snapshot(h, &directory, "capture-key-100");
    assert_shared_footer(state);
    assert_guidance(state, l::FOOTER_CAPTURE_GUIDE);
    {
        let mut x = state.borrow_mut();
        let event = x.input.event(RadioKey::Keyboard(0x1B), Edge::Down);
        ev(h, &mut x, event);
        assert_eq!(x.mode, View::Main);
        assert_eq!(x.input.selected(), RadioKey::Mouse4);
    }
    SendMessageW(key, 0x00F5, 0, 0);
    {
        let mut x = state.borrow_mut();
        let event = x.input.event(RadioKey::Keyboard(0x77), Edge::Down);
        ev(h, &mut x, event);
        assert_eq!(x.mode, View::Main);
        assert_eq!(get(x.key), "F8");
    }
    let appdata = std::env::var("APPDATA").unwrap();
    let store = FileStore::new(FileStore::appdata_path(&appdata));
    assert_eq!(store.load().radio_key_code, 0x77);
    assert!(state.borrow().ptt_sound);
    for (button, expected) in [
        (sound_on, false),
        (sound_on, true),
        (sound_off, false),
        (sound_off, true),
        (sound_on, false),
        (sound_off, true),
    ] {
        SendMessageW(button, 0x00F5, 0, 0);
        assert_eq!(state.borrow().ptt_sound, expected);
        assert_eq!(store.load().ptt_sound, expected);
    }
    for (button, on, expected) in [(sound_on, true, false), (sound_off, false, true)] {
        let mut bounds: Rect = core::mem::zeroed();
        GetWindowRect(button, &mut bounds);
        let expected_bounds = layout.sound_hit(on).native(96);
        assert_eq!(
            bounds.right - bounds.left,
            expected_bounds.right - expected_bounds.left
        );
        assert_eq!(
            bounds.bottom - bounds.top,
            expected_bounds.bottom - expected_bounds.top
        );
        // y=1 within enlarged hit rect means two DIPs above the old shell.
        let point = (1usize << 16) | 40;
        SendMessageW(button, 0x0201, 1, point as isize);
        SendMessageW(button, LBUTTON_UP, 0, point as isize);
        assert_eq!(state.borrow().ptt_sound, expected);
        assert_eq!(store.load().ptt_sound, expected);
    }
    SetFocus(h);
    state.borrow_mut().hover_button = 0;
    snapshot(h, &directory, "ptt-on-100");
    SendMessageW(sound_on, 0x00F5, 0, 0);
    snapshot(h, &directory, "ptt-off-100");
    // Isolated fixture only: force the existing atomic save path to fail.
    let config_path = FileStore::appdata_path(&appdata);
    let backup = config_path.with_extension("proof-backup");
    fs::rename(&config_path, &backup).unwrap();
    fs::create_dir(&config_path).unwrap();
    SendMessageW(sound_off, 0x00F5, 0, 0);
    let retained = !state.borrow().ptt_sound;
    let feedback = state.borrow().notice == Some((l::SETTINGS_FAILED, true));
    fs::remove_dir(&config_path).unwrap();
    fs::rename(&backup, &config_path).unwrap();
    assert!(retained, "Failed save must retain the prior UI selection");
    assert!(
        feedback,
        "Failed save must retain the existing settings feedback"
    );
    assert!(!store.load().ptt_sound);
    snapshot(h, &directory, "ptt-save-failure-100");
    assert_guidance(state, l::SETTINGS_FAILED);
    fs::write(directory.join("toggle-proof.txt"), "PASS: six alternating native clicks on both regions invert and persist current selection, including selected regions. Protruding strip native clicks toggle both halves. Failed isolated save retains UI/persisted OFF and settings error feedback.\n").unwrap();
    // Main returns from capture without a new protocol state or credentials.
    {
        let x = state.borrow();
        set_view_control_show_cmd(x.unpair_btn, SHOW);
    }
    SendMessageW(unpair, 0x00F5, 0, 0);
    assert_eq!(state.borrow().mode, View::ConfirmUnpair);
    interaction_snapshot(h, state, &directory, cancel, APPLY, "confirm-cancel");
    interaction_snapshot(h, state, &directory, unpair, UNPAIR, "confirm-unpair");
    snapshot(h, &directory, "confirm-unpair-100");
    assert_shared_footer(state);
    assert_guidance(state, l::FOOTER_CONFIRM_UNPAIR);
    SendMessageW(cancel, KEY_DOWN, VK_ESCAPE, 0);
    assert_eq!(state.borrow().mode, View::Main);
    SendMessageW(h, CMD, UNPAIR, 0);
    SendMessageW(cancel, 0x00F5, 0, 0);
    assert_eq!(state.borrow().mode, View::Main);
    SendMessageW(h, CMD, UNPAIR, 0);
    SendMessageW(unpair, 0x00F5, 0, 0);
    assert_eq!(state.borrow().mode, View::Pair);
    assert!(store.load().device_token.is_none());
    SendMessageW(close, 0x00F5, 0, 0);
    assert_eq!(IsWindowVisible(h), 0);
    SendMessageW(h, TRAY, 0, DCLICK);
    assert_ne!(IsWindowVisible(h), 0);
    // Same graceful native shutdown entry used by tray Exit; unique fixture HWND.
    SendMessageW(h, LCR_RUNTIME_EXIT_MESSAGE, 0, 0);
    assert_eq!(IsWindow(h), 0);
    fs::write(directory.join("scenario.txt"),
        "PASS: native Pair launch; empty-code error; Main/TRANSMITTING visual fixtures; no Config/CHANGE; Radio Key BM_CLICK; ESC cancel; F8 persists; Sound both regions invert and persist, selected-region clicks included; failed save retains state; Unpair confirmation/cancel/ESC/confirm returns Pair; X hides; tray restore-message path; graceful Exit destroys HWND; all six ICO sizes load.\nNOT CLAIMED: live Pair/network, real tray-menu clicks, real PTT or elevated Discord.\n").unwrap();
    true
}

#[test]
#[ignore = "isolated native UI fixture; set LCR_UI_V6_PROOF_DIR, run alone; no Discord credentials/input/tray"]
fn native_presentation_controls_and_snapshots() {
    let directory =
        PathBuf::from(std::env::var_os("LCR_UI_V6_PROOF_DIR").expect("proof directory required"));
    let appdata = directory.join("isolated-appdata");
    fs::create_dir_all(&appdata).unwrap();
    let mut config = Config {
        radio_key: "Mouse4".into(),
        ..Config::default()
    };
    config.ptt_sound = true;
    FileStore::new(FileStore::appdata_path(appdata.to_str().unwrap()))
        .save(&config)
        .unwrap();
    let original = std::env::var_os("APPDATA");
    std::env::set_var("APPDATA", &appdata);
    let result = std::panic::catch_unwind(run);
    if let Some(original) = original {
        std::env::set_var("APPDATA", original);
    } else {
        std::env::remove_var("APPDATA");
    }
    result.unwrap();
    assert!(
        directory.join("scenario.txt").is_file(),
        "UI fixture did not finish"
    );
}
