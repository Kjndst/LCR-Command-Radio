//! Windows-only native host and Raw Input seam.

mod final_shell {
    use crate::{
        config::{Config, FileStore},
        distribution::RUNTIME_MUTEX_NAME,
        input::{BrokerObservation, Edge, InputAuthority, InputOutcome, InputProvider, RadioKey},
        input_broker::{
            foreground_requires_broker, physical_key_down, BrokerPoll, BrokerWireObservation,
            InputBrokerClient, BROKER_DOWN_LEASE_MS, BROKER_MAX_DOWN_AGE_MS, BROKER_WAKE_MESSAGE,
        },
        instance::{try_acquire_named_mutex, NamedMutexAcquire},
        localization as l,
        reconciler::Desired,
        runtime::{HelperRuntime, PairPoll, PairStartError},
    };
    use std::{
        cell::RefCell,
        ffi::OsString,
        fs, io,
        iter::once,
        mem::size_of,
        os::windows::ffi::OsStringExt,
        path::{Path, PathBuf},
        sync::OnceLock,
    };
    type W = isize;
    type I = isize;
    type P = unsafe extern "system" fn(W, u32, usize, isize) -> isize;
    type SubclassP = unsafe extern "system" fn(W, u32, usize, isize, usize, usize) -> isize;
    type ProcessHandle = *mut core::ffi::c_void;
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Pt {
        x: i32,
        y: i32,
    }

    #[repr(C)]
    struct TrackMouse {
        size: u32,
        flags: u32,
        hwnd_track: W,
        hover_time: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Rect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }

    #[repr(C)]
    struct PaintStruct {
        hdc: I,
        erase: i32,
        paint: Rect,
        restore: i32,
        inc_update: i32,
        reserved: [u8; 32],
    }

    #[repr(C)]
    struct Msg {
        h: W,
        m: u32,
        w: usize,
        l: isize,
        t: u32,
        p: Pt,
        x: u32,
    }
    #[repr(C)]
    struct C {
        n: u32,
        s: u32,
        p: Option<P>,
        a: i32,
        b: i32,
        i: I,
        ic: I,
        cu: I,
        br: I,
        me: *const u16,
        cl: *const u16,
        sm: I,
    }
    #[repr(C)]
    struct N {
        n: u32,
        h: W,
        id: u32,
        f: u32,
        cb: u32,
        ic: I,
        tip: [u16; 128],
        z: [u8; 416],
    }
    #[repr(C)]
    struct Rid {
        p: u16,
        u: u16,
        f: u32,
        h: W,
    }
    #[repr(C)]
    struct Rh {
        k: u32,
        n: u32,
        d: I,
        w: usize,
    }
    #[derive(Clone, Copy)]
    #[repr(C)]
    struct Rm {
        a: u16,
        b: u16,
        buttons: u32,
        c: u32,
        x: i32,
        y: i32,
        z: u32,
    }
    #[derive(Clone, Copy)]
    #[repr(C)]
    struct Rk {
        a: u16,
        b: u16,
        c: u16,
        v: u16,
        m: u32,
        z: u32,
    }
    #[repr(C)]
    union Rd {
        m: Rm,
        k: Rk,
    }
    #[repr(C)]
    struct Ri {
        h: Rh,
        d: Rd,
    }
    const DEST: u32 = 2;
    const PAINT: u32 = 15;
    const CLOSE: u32 = 16;
    const SYSCOMMAND: u32 = 0x0112;

    // Private application message used only to request the same graceful
    // shutdown path as tray "EXIT LCR". WM_APP..0xBFFF is application-owned.
    const LCR_RUNTIME_EXIT_MESSAGE: u32 = 0x804C;
    const LCR_INPUT_BROKER_RESTART_MESSAGE: u32 = 0x804D;
    // SAFETY INVARIANT: this ID must stay distinct from BROKER_WAKE_MESSAGE (0x804E).
    // Live 2026-10-13: reusing 0x804E caused elevated-Discord Broker wakes to be
    // consumed as runtime activation, killing PTT after the elevated handoff.
    // Change only with a private-message namespace audit plus the elevated-Discord PTT regression test.
    const LCR_RUNTIME_ACTIVATE_MESSAGE: u32 = 0x804F;
    const RUNTIME_WINDOW_CLASS: &str = "LCRFinal";

    const SYNCHRONIZE: u32 = 0x0010_0000;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x0000_1000;
    const WAIT_OBJECT_0: u32 = 0x0000_0000;
    const WAIT_TIMEOUT: u32 = 0x0000_0102;
    const WAIT_FAILED: u32 = 0xFFFF_FFFF;
    const RUNTIME_QUIESCE_TIMEOUT_MS: u32 = 30_000;
    const RUNTIME_ACTIVATE_TIMEOUT_MS: u32 = 2_000;
    const SMTO_BLOCK: u32 = 0x0001;
    const SMTO_ABORTIFHUNG: u32 = 0x0002;
    const NCHITTEST: u32 = 0x0084;
    const SET_CURSOR: u32 = 0x0020;
    const MOUSE_MOVE: u32 = 0x0200;
    const LBUTTON_UP: u32 = 0x0202;
    const MOUSE_LEAVE: u32 = 0x02A3;
    const HTCLIENT: isize = 1;
    const HTCAPTION: isize = 2;
    const TME_LEAVE: u32 = 0x00000002;
    const IDC_HAND: usize = 32649;
    const SC_CLOSE: usize = 0xF060;
    const ERASE_BKGND: u32 = 20;
    const DRAW_ITEM: u32 = 43;
    const SET_FONT: u32 = 48;
    const CMD: u32 = 273;
    const TIMER: u32 = 275;
    const COLOR_EDIT: u32 = 0x0133;
    const COLOR_STATIC: u32 = 0x0138;
    const INPUT: u32 = 255;
    const TRAY: u32 = 0x8001;
    const HIDE: i32 = 0;
    const SHOW: i32 = 5;
    const SWP_NOSIZE: u32 = 0x0001;
    const SWP_NOMOVE: u32 = 0x0002;
    const SWP_NOZORDER: u32 = 0x0004;
    const SWP_NOREDRAW: u32 = 0x0008;
    const SWP_NOACTIVATE: u32 = 0x0010;
    const SWP_SHOWWINDOW: u32 = 0x0040;
    const SWP_HIDEWINDOW: u32 = 0x0080;
    const RESTORE: i32 = 9;
    const MB_OK: u32 = 0x00000000;
    const MB_ICONERROR: u32 = 0x00000010;
    const POPUP: u32 = 0x80000000;
    const EX_APPWINDOW: u32 = 0x00040000;
    const CHILD: u32 = 0x40000000;
    const V: u32 = 0x10000000;
    const TAB: u32 = 0x10000;
    const CENTER_TEXT: u32 = 0x0001;
    const OWNER_DRAW: u32 = 0x0000000B;
    const USER: i32 = -21;
    const ADD: u32 = 0;
    const MOD: u32 = 1;
    const DEL: u32 = 2;
    const NF: u32 = 7;
    const TP: u32 = 0x100;
    const TN: u32 = 0x80;
    const DCLICK: isize = 0x203;
    const RUP: isize = 0x205;
    const MF_SEPARATOR: u32 = 0x00000800;
    const MF_GRAYED: u32 = 0x00000001;
    const MF_DISABLED: u32 = 0x00000002;
    const M4D: u16 = 0x40;
    const M4U: u16 = 0x80;
    const M5D: u16 = 0x100;
    const M5U: u16 = 0x200;
    const BREAK: u16 = 1;
    const RID: u32 = 0x10000003;
    const RIDEV_INPUTSINK: u32 = 0x00000100;
    const PAIR: usize = 1;
    const CHANGE: usize = 2;
    const CONFIG: usize = 3;
    const APPLY: usize = 4;
    const UNPAIR: usize = 5;
    const CODE: usize = 6;
    const CLOSE_BUTTON: usize = 8;
    const SOUND_ON: usize = 9;
    const SOUND_OFF: usize = 10;
    const KEY_DISPLAY: usize = 11;
    const SET_FOCUS: u32 = 0x0007;
    const KILL_FOCUS: u32 = 0x0008;
    const KEY_DOWN: u32 = 0x0100;
    const EM_SETSEL: u32 = 0x00B1;
    const VK_A: usize = 0x41;

    const VK_ESCAPE: usize = 0x1B;
    const VK_CONTROL: i32 = 0x11;
    const WINDING: i32 = 2;
    const SND_ASYNC: u32 = 0x0001;
    const SND_NODEFAULT: u32 = 0x0002;
    const SND_MEMORY: u32 = 0x0004;

    const CLEARTYPE_QUALITY: u32 = 5;

    // COLORREF uses 0x00BBGGRR.
    const COLOR_PANEL: u32 = 0x000F0D0C;
    const COLOR_EDIT_BG: u32 = COLOR_PANEL;
    const COLOR_TEXT: u32 = 0x00F2EEE9;

    include!("presentation.rs");

    static APP: &[u8] = include_bytes!("../assets/LCR_Helper.ico");
    static OFF: &[u8] = include_bytes!("../assets/tray/tray_disconnected.ico");
    static ON: &[u8] = include_bytes!("../assets/tray/tray_standby.ico");
    static TX: &[u8] = include_bytes!("../assets/tray/tray_transmitting.ico");
    static CONNECTED_SOUND: &[u8] = include_bytes!("../assets/connected.wav");
    #[cfg(test)]
    std::thread_local! { static CONNECTED_CUE_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
    static PTT_DOWN_SOUND: &[u8] = include_bytes!("../assets/ptt_down.wav");
    static PTT_UP_SOUND: &[u8] = include_bytes!("../assets/ptt_up.wav");
    #[link(name = "user32")]
    extern "system" {
        fn RegisterClassExW(c: *const C) -> u16;
        fn CreateWindowExW(
            e: u32,
            c: *const u16,
            t: *const u16,
            s: u32,
            x: i32,
            y: i32,
            w: i32,
            h: i32,
            p: W,
            m: I,
            i: I,
            a: *mut core::ffi::c_void,
        ) -> W;
        fn DefWindowProcW(h: W, m: u32, w: usize, l: isize) -> isize;
        fn ShowWindow(h: W, c: i32) -> i32;
        fn SetWindowPos(
            h: W,
            insert_after: W,
            x: i32,
            y: i32,
            width: i32,
            height: i32,
            flags: u32,
        ) -> i32;
        fn GetMessageW(m: *mut Msg, h: W, a: u32, b: u32) -> i32;
        fn TranslateMessage(m: *const Msg) -> i32;
        fn DispatchMessageW(m: *const Msg) -> isize;
        fn PostQuitMessage(c: i32);
        fn DestroyWindow(h: W) -> i32;
        fn FindWindowW(class_name: *const u16, window_name: *const u16) -> W;
        fn GetWindowThreadProcessId(window: W, process_id: *mut u32) -> u32;
        fn PostMessageW(window: W, message: u32, wparam: usize, lparam: isize) -> i32;
        fn RegisterWindowMessageW(message: *const u16) -> u32;
        fn SendMessageTimeoutW(
            h: W,
            m: u32,
            w: usize,
            l: isize,
            flags: u32,
            timeout_ms: u32,
            result: *mut usize,
        ) -> isize;
        fn SetWindowLongPtrW(h: W, i: i32, v: isize) -> isize;
        fn GetWindowLongPtrW(h: W, i: i32) -> isize;
        fn RegisterRawInputDevices(d: *const Rid, n: u32, s: u32) -> i32;
        fn GetRawInputData(h: I, c: u32, d: *mut core::ffi::c_void, n: *mut u32, s: u32) -> u32;
        fn CreateIconFromResourceEx(
            b: *mut u8,
            n: u32,
            i: i32,
            v: u32,
            w: i32,
            h: i32,
            f: u32,
        ) -> I;
        fn SetWindowTextW(h: W, t: *const u16) -> i32;
        fn MessageBoxW(h: W, text: *const u16, caption: *const u16, kind: u32) -> i32;
        fn GetWindowTextLengthW(h: W) -> i32;
        fn GetWindowTextW(h: W, t: *mut u16, n: i32) -> i32;
        fn SetTimer(h: W, i: usize, t: u32, c: usize) -> usize;
        fn KillTimer(h: W, i: usize) -> i32;
        fn CreatePopupMenu() -> I;
        fn AppendMenuW(m: I, f: u32, i: usize, t: *const u16) -> i32;
        fn TrackPopupMenu(
            m: I,
            f: u32,
            x: i32,
            y: i32,
            r: i32,
            h: W,
            a: *const core::ffi::c_void,
        ) -> u32;
        fn DestroyMenu(m: I) -> i32;
        fn GetCursorPos(p: *mut Pt) -> i32;
        fn GetKeyState(key: i32) -> i16;
        fn SetFocus(h: W) -> W;
        fn WindowFromPoint(p: Pt) -> W;
        fn LoadCursorW(instance: I, cursor: *const u16) -> I;
        fn SetCursor(cursor: I) -> I;
        fn TrackMouseEvent(track: *mut TrackMouse) -> i32;
        fn SetForegroundWindow(h: W) -> i32;
        fn BeginPaint(h: W, p: *mut PaintStruct) -> I;
        fn EndPaint(h: W, p: *const PaintStruct) -> i32;
        fn GetClientRect(h: W, r: *mut Rect) -> i32;
        fn GetWindowRect(h: W, r: *mut Rect) -> i32;
        fn SetWindowRgn(h: W, region: I, redraw: i32) -> i32;
        fn MoveWindow(h: W, x: i32, y: i32, w: i32, height: i32, repaint: i32) -> i32;
        fn InvalidateRect(h: W, r: *const Rect, erase: i32) -> i32;
        fn SendMessageW(h: W, m: u32, w: usize, l: isize) -> isize;
    }
    #[link(name = "gdi32")]
    extern "system" {
        #[cfg(test)]
        fn CreateCompatibleDC(hdc: I) -> I;
        #[cfg(test)]
        fn DeleteDC(hdc: I) -> i32;
        #[cfg(test)]
        fn BitBlt(
            destination: I,
            x: i32,
            y: i32,
            width: i32,
            height: i32,
            source: I,
            source_x: i32,
            source_y: i32,
            raster_operation: u32,
        ) -> i32;
        fn CreateSolidBrush(color: u32) -> I;
        fn CreatePolygonRgn(points: *const Pt, count: i32, fill_mode: i32) -> I;
        fn DeleteObject(object: I) -> i32;
        fn SetTextColor(hdc: I, color: u32) -> u32;
        fn SetBkColor(hdc: I, color: u32) -> u32;
        #[cfg(test)]
        fn SelectObject(hdc: I, object: I) -> I;
        fn CreateFontW(
            height: i32,
            width: i32,
            escapement: i32,
            orientation: i32,
            weight: i32,
            italic: u32,
            underline: u32,
            strikeout: u32,
            charset: u32,
            out_precision: u32,
            clip_precision: u32,
            quality: u32,
            pitch_and_family: u32,
            face: *const u16,
        ) -> I;
    }
    #[link(name = "shell32")]
    extern "system" {
        #[link_name = "Shell_NotifyIconW"]
        fn shell_notify_icon(m: u32, d: *mut N) -> i32;
        fn ShellExecuteW(
            owner: W,
            operation: *const u16,
            file: *const u16,
            parameters: *const u16,
            directory: *const u16,
            show: i32,
        ) -> I;
    }

    #[link(name = "winmm")]
    extern "system" {
        fn PlaySoundW(sound: *const u16, module: I, flags: u32) -> i32;
    }

    #[link(name = "comctl32")]
    extern "system" {
        fn SetWindowSubclass(hwnd: W, proc: SubclassP, id: usize, ref_data: usize) -> i32;
        fn DefSubclassProc(hwnd: W, message: u32, wparam: usize, lparam: isize) -> isize;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetModuleHandleW(n: *const u16) -> I;
        fn GetTickCount64() -> u64;
        fn OpenProcess(desired_access: u32, inherit_handle: i32, process_id: u32) -> ProcessHandle;
        fn CloseHandle(handle: ProcessHandle) -> i32;
        fn QueryFullProcessImageNameW(
            process: ProcessHandle,
            flags: u32,
            executable_name: *mut u16,
            size: *mut u32,
        ) -> i32;
        fn WaitForSingleObject(handle: ProcessHandle, milliseconds: u32) -> u32;
    }

    fn ws(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(once(0)).collect()
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum ShellLifecycleAction {
        None,
        ReaddTray,
        ReaddTrayAndShow,
    }

    fn shell_lifecycle_action(message: u32, taskbar_created: u32) -> ShellLifecycleAction {
        if taskbar_created != 0 && message == taskbar_created {
            ShellLifecycleAction::ReaddTray
        } else if message == LCR_RUNTIME_ACTIVATE_MESSAGE {
            ShellLifecycleAction::ReaddTrayAndShow
        } else {
            ShellLifecycleAction::None
        }
    }

    fn taskbar_created_message() -> u32 {
        static MESSAGE: OnceLock<u32> = OnceLock::new();

        *MESSAGE.get_or_init(|| {
            let message = ws("TaskbarCreated");
            unsafe { RegisterWindowMessageW(message.as_ptr()) }
        })
    }

    struct OwnedRuntimeProcess {
        handle: ProcessHandle,
    }

    impl OwnedRuntimeProcess {
        fn open(process_id: u32) -> io::Result<Self> {
            let handle = unsafe {
                OpenProcess(
                    SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                    0,
                    process_id,
                )
            };

            if handle.is_null() {
                Err(io::Error::last_os_error())
            } else {
                Ok(Self { handle })
            }
        }
    }

    impl Drop for OwnedRuntimeProcess {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.handle);
            }
        }
    }

    fn runtime_mutex_exists() -> io::Result<bool> {
        match try_acquire_named_mutex(RUNTIME_MUTEX_NAME)? {
            NamedMutexAcquire::Acquired(guard) => {
                drop(guard);
                Ok(false)
            }
            NamedMutexAcquire::AlreadyExists => Ok(true),
        }
    }

    fn runtime_window() -> Option<W> {
        let class = ws(RUNTIME_WINDOW_CLASS);
        let window = unsafe { FindWindowW(class.as_ptr(), core::ptr::null()) };

        if window == 0 {
            None
        } else {
            Some(window)
        }
    }

    fn runtime_window_process_id(window: W) -> io::Result<u32> {
        let mut process_id = 0u32;
        let thread_id = unsafe { GetWindowThreadProcessId(window, &mut process_id) };

        if thread_id == 0 || process_id == 0 {
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                "LCR runtime window has no valid owning process",
            ))
        } else {
            Ok(process_id)
        }
    }

    fn process_image_path(process: &OwnedRuntimeProcess) -> io::Result<PathBuf> {
        // QueryFullProcessImageNameW uses a caller-sized character buffer.
        // 32K UTF-16 units covers the Win32 extended-path ceiling without a
        // retry loop in the uninstall control path.
        let mut buffer = vec![0u16; 32_768];
        let mut length = buffer.len() as u32;

        let queried = unsafe {
            QueryFullProcessImageNameW(process.handle, 0, buffer.as_mut_ptr(), &mut length)
        };

        if queried == 0 {
            return Err(io::Error::last_os_error());
        }

        let length = usize::try_from(length).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "runtime image path length does not fit usize",
            )
        })?;

        if length == 0 || length > buffer.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "runtime image path length is invalid",
            ));
        }

        buffer.truncate(length);
        Ok(PathBuf::from(OsString::from_wide(&buffer)))
    }

    fn canonical_existing_path(path: &Path) -> io::Result<PathBuf> {
        if !path.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "runtime executable authority must be absolute",
            ));
        }

        fs::canonicalize(path)
    }

    pub fn activate_existing_runtime() -> io::Result<()> {
        let current_executable = std::env::current_exe()?;
        activate_existing_runtime_for_executable(&current_executable)
    }

    fn activate_existing_runtime_for_executable(expected_executable: &Path) -> io::Result<()> {
        if !runtime_mutex_exists()? {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "LCR runtime mutex is not held",
            ));
        }

        let expected_executable = canonical_existing_path(expected_executable)?;

        let window = runtime_window().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "LCR runtime mutex exists but its exact window was not found",
            )
        })?;

        let process_id = runtime_window_process_id(window)?;
        let process = OwnedRuntimeProcess::open(process_id)?;
        let actual_executable = canonical_existing_path(&process_image_path(&process)?)?;

        if actual_executable != expected_executable {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "LCR runtime window belongs to an unexpected process image",
            ));
        }

        let mut result = 0usize;
        let delivered = unsafe {
            SendMessageTimeoutW(
                window,
                LCR_RUNTIME_ACTIVATE_MESSAGE,
                0,
                0,
                SMTO_BLOCK | SMTO_ABORTIFHUNG,
                RUNTIME_ACTIVATE_TIMEOUT_MS,
                &mut result,
            )
        };

        if delivered == 0 {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "existing LCR runtime did not acknowledge activation",
            ));
        }

        if result != 1 {
            return Err(io::Error::other("existing LCR runtime rejected activation"));
        }

        unsafe {
            let _ = SetForegroundWindow(window);
        }

        Ok(())
    }

    fn wait_for_runtime_process_exit(
        process: &OwnedRuntimeProcess,
        timeout_ms: u32,
    ) -> io::Result<()> {
        match unsafe { WaitForSingleObject(process.handle, timeout_ms) } {
            WAIT_OBJECT_0 => Ok(()),
            WAIT_TIMEOUT => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "timed out waiting for LCR runtime to exit",
            )),
            WAIT_FAILED => Err(io::Error::last_os_error()),
            unexpected => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unexpected runtime process wait result: {unexpected:#010x}"),
            )),
        }
    }

    /// Requests graceful termination of an already-running canonical Helper
    /// before clean uninstall proceeds.
    ///
    /// This is intentionally process-control only. It performs no uninstall
    /// deletion, configuration mutation, staging, or rollback.
    ///
    /// Contract:
    /// - no runtime mutex => success without signaling anything;
    /// - exact `LCRFinal` top-level window only;
    /// - owning PID is read from that exact HWND;
    /// - owning process image must canonicalize to `expected_executable`;
    /// - the private exit message reaches the normal window thread, which calls
    ///   the same `exit(h)` path as tray "EXIT LCR";
    /// - wait is one bounded kernel wait, never polling;
    /// - after process exit the runtime mutex must be absent;
    /// - no TerminateProcess, shell process, sleep loop, or retry fallback.
    pub fn quiesce_runtime_for_uninstall(expected_executable: &Path) -> io::Result<()> {
        quiesce_runtime_for_uninstall_with_timeout(expected_executable, RUNTIME_QUIESCE_TIMEOUT_MS)
    }

    fn quiesce_runtime_for_uninstall_with_timeout(
        expected_executable: &Path,
        timeout_ms: u32,
    ) -> io::Result<()> {
        if !runtime_mutex_exists()? {
            return Ok(());
        }

        let expected_executable = canonical_existing_path(expected_executable)?;

        let window = match runtime_window() {
            Some(window) => window,
            None => {
                // The runtime may have exited naturally between the mutex
                // observation and window lookup.
                if !runtime_mutex_exists()? {
                    return Ok(());
                }

                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "LCR runtime mutex exists but its exact window was not found",
                ));
            }
        };

        let process_id = runtime_window_process_id(window)?;
        let process = match OwnedRuntimeProcess::open(process_id) {
            Ok(process) => process,
            Err(error) => {
                if !runtime_mutex_exists()? {
                    return Ok(());
                }
                return Err(error);
            }
        };

        let actual_executable = canonical_existing_path(&process_image_path(&process)?)?;
        if actual_executable != expected_executable {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "LCR runtime window belongs to an unexpected process image",
            ));
        }

        let posted = unsafe { PostMessageW(window, LCR_RUNTIME_EXIT_MESSAGE, 0, 0) };
        if posted == 0 {
            return Err(io::Error::last_os_error());
        }

        wait_for_runtime_process_exit(&process, timeout_ms)?;

        if runtime_mutex_exists()? {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "LCR runtime process exited but the runtime mutex still exists",
            ));
        }

        Ok(())
    }

    fn key(c: &Config) -> RadioKey {
        if c.radio_key_kind.eq_ignore_ascii_case("keyboard") && c.radio_key_code > 0 {
            RadioKey::Keyboard(c.radio_key_code)
        } else if c.radio_key.eq_ignore_ascii_case("Mouse4") {
            RadioKey::Mouse4
        } else {
            RadioKey::Mouse5
        }
    }
    fn keyboard_key_s(v: u32) -> String {
        match v {
            0x30..=0x39 | 0x41..=0x5A => char::from_u32(v).unwrap_or('?').to_string(),
            0x60..=0x69 => format!("NUM {}", v - 0x60),
            0x70..=0x87 => format!("F{}", v - 0x6F),
            0x08 => "BACKSPACE".into(),
            0x09 => "TAB".into(),
            0x0D => "ENTER".into(),
            0x10 => "SHIFT".into(),
            0x11 => "CTRL".into(),
            0x12 => "ALT".into(),
            0x14 => "CAPS LOCK".into(),
            0x1B => "ESC".into(),
            0x20 => "SPACE".into(),
            0x21 => "PAGE UP".into(),
            0x22 => "PAGE DOWN".into(),
            0x23 => "END".into(),
            0x24 => "HOME".into(),
            0x25 => "LEFT".into(),
            0x26 => "UP".into(),
            0x27 => "RIGHT".into(),
            0x28 => "DOWN".into(),
            0x2D => "INSERT".into(),
            0x2E => "DELETE".into(),
            0x90 => "NUM LOCK".into(),
            0xA0 => "LSHIFT".into(),
            0xA1 => "RSHIFT".into(),
            0xA2 => "LCTRL".into(),
            0xA3 => "RCTRL".into(),
            0xA4 => "LALT".into(),
            0xA5 => "RALT".into(),
            _ => format!("VK 0x{v:02X}"),
        }
    }

    fn key_s(k: &RadioKey) -> String {
        match k {
            RadioKey::Mouse4 => l::MOUSE4.into(),
            RadioKey::Mouse5 => l::MOUSE5.into(),
            RadioKey::Keyboard(v) => keyboard_key_s(*v),
        }
    }
    unsafe fn ico(b: &[u8], target: i32) -> I {
        if b.len() < 6 {
            return 0;
        }

        let count = u16::from_le_bytes([b[4], b[5]]) as usize;
        let mut best: Option<(usize, usize, i32)> = None;

        for index in 0..count {
            let entry = 6 + index * 16;
            let Some(dir) = b.get(entry..entry + 16) else {
                break;
            };

            let width = if dir[0] == 0 { 256 } else { dir[0] as i32 };
            let bytes = u32::from_le_bytes([dir[8], dir[9], dir[10], dir[11]]) as usize;
            let offset = u32::from_le_bytes([dir[12], dir[13], dir[14], dir[15]]) as usize;

            if bytes == 0 || b.get(offset..offset.saturating_add(bytes)).is_none() {
                continue;
            }

            let score = (width - target).abs();
            if best.map(|(_, _, current)| score < current).unwrap_or(true) {
                best = Some((offset, bytes, score));
            }
        }

        let Some((offset, bytes, _)) = best else {
            return 0;
        };
        let Some(image) = b.get(offset..offset + bytes) else {
            return 0;
        };

        CreateIconFromResourceEx(
            image.as_ptr() as *mut u8,
            image.len() as u32,
            1,
            0x30000,
            target,
            target,
            0,
        )
    }
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum View {
        Pair,
        Main,
        Config,
        Capture,
        ConfirmUnpair,
    }

    struct S {
        r: HelperRuntime,
        input: InputAuthority,
        st: W,
        foot: W,
        code: W,
        key: W,
        pair_btn: W,
        change_btn: W,
        sound_on_btn: W,
        sound_off_btn: W,
        config_btn: W,
        apply_btn: W,
        unpair_btn: W,
        close_btn: W,
        renderer: RefCell<Renderer>,
        dpi: u32,
        _jura: PrivateFont,
        icons: [I; 3],
        edit_brush: I,
        font_edit: I,
        mode: View,
        // Return target for CONFIG -> BACK.
        return_view: View,
        // Return target for transient key-capture only. This must not overwrite
        // the CONFIG back target.
        capture_return_view: View,
        key_notice_until: Option<std::time::Instant>,
        tx: bool,
        ptt_sound: bool,
        broker: Option<InputBrokerClient>,
        broker_attempted: bool,
        hover_button: usize,
        crest_hover: bool,
        focus_edit: W,
        notice: Option<(&'static str, bool)>,
    }
    fn try_with<T, R>(cell: &RefCell<T>, f: impl FnOnce(&T) -> R) -> Option<R> {
        let state = cell.try_borrow().ok()?;
        Some(f(&state))
    }

    fn try_with_mut<T, R>(cell: &RefCell<T>, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        let mut state = cell.try_borrow_mut().ok()?;
        Some(f(&mut state))
    }

    unsafe fn with_s<R>(h: W, f: impl FnOnce(&S) -> R) -> Option<R> {
        let cell = (GetWindowLongPtrW(h, USER) as *const RefCell<S>).as_ref()?;
        try_with(cell, f)
    }

    unsafe fn with_s_mut<R>(h: W, f: impl FnOnce(&mut S) -> R) -> Option<R> {
        let cell = (GetWindowLongPtrW(h, USER) as *const RefCell<S>).as_ref()?;
        try_with_mut(cell, f)
    }
    unsafe fn set(h: W, x: &str) {
        let x = ws(x);
        SetWindowTextW(h, x.as_ptr());
    }
    unsafe fn get(h: W) -> String {
        let n = GetWindowTextLengthW(h).max(0) as usize;
        let mut x = vec![0; n + 1];
        GetWindowTextW(h, x.as_mut_ptr(), x.len() as i32);
        String::from_utf16_lossy(&x[..n])
    }

    unsafe fn startup_error(h: W, message: &str) {
        let message = ws(message);
        let title = ws(l::WINDOW_TITLE);
        MessageBoxW(h, message.as_ptr(), title.as_ptr(), MB_OK | MB_ICONERROR);
    }

    fn default_footer(x: &S) -> &'static str {
        match x.mode {
            View::Pair => l::FOOTER_AWAITING_PAIRING,
            View::Capture => l::FOOTER_CAPTURE_GUIDE,
            View::ConfirmUnpair => l::FOOTER_CONFIRM_UNPAIR,
            View::Main if x.tx => l::FOOTER_TRANSMITTING,
            View::Main => l::FOOTER_READY,
            View::Config => l::FOOTER_CONFIG,
        }
    }

    unsafe fn refresh_footer(x: &S) {
        if let Some((message, _)) = x.notice {
            set(x.foot, message);
        } else {
            set(x.foot, default_footer(x));
        }
    }

    unsafe fn notice(h: W, x: &mut S, message: &'static str, is_error: bool) {
        x.notice = Some((message, is_error));
        refresh_footer(x);
        InvalidateRect(h, core::ptr::null(), 0);
    }

    fn crest_rect(mode: View) -> Rect {
        let layout = Layout::new();
        (if mode == View::Pair {
            layout.pair_logo
        } else {
            layout.header_logo
        })
        .native(96)
    }

    unsafe fn apply_window_region(h: W, width: i32, height: i32) -> bool {
        let scale = width as f32 / WINDOW_WIDTH as f32;
        let edge = (HALF_GRID * scale).floor() as i32;
        // Region ends at the drawn core's outer pixel, not the dark clear gutter.
        let right = width - edge + 1;
        let bottom = height - edge + 1;
        let cut = (GRID * scale).round() as i32;
        let points = [
            Pt {
                x: edge + cut,
                y: edge,
            },
            Pt {
                x: right - cut,
                y: edge,
            },
            Pt {
                x: right,
                y: edge + cut,
            },
            Pt {
                x: right,
                y: bottom - cut,
            },
            Pt {
                x: right - cut,
                y: bottom,
            },
            Pt {
                x: edge + cut,
                y: bottom,
            },
            Pt {
                x: edge,
                y: bottom - cut,
            },
            Pt {
                x: edge,
                y: edge + cut,
            },
        ];
        let region = CreatePolygonRgn(points.as_ptr(), points.len() as i32, WINDING);
        if region == 0 {
            return false;
        }
        if SetWindowRgn(h, region, 1) == 0 {
            DeleteObject(region);
            return false;
        }
        true
    }

    fn point_in_rect(point: Pt, rect: Rect) -> bool {
        point.x >= rect.left && point.x < rect.right && point.y >= rect.top && point.y < rect.bottom
    }

    fn client_point(lparam: isize) -> Pt {
        Pt {
            x: (lparam as u16 as i16) as i32,
            y: ((lparam >> 16) as u16 as i16) as i32,
        }
    }

    unsafe fn interactive_window(x: &S, target: W) -> bool {
        target != 0
            && [
                x.code,
                x.key,
                x.pair_btn,
                x.change_btn,
                x.sound_on_btn,
                x.sound_off_btn,
                x.config_btn,
                x.apply_btn,
                x.unpair_btn,
                x.close_btn,
            ]
            .contains(&target)
    }

    unsafe fn confirm_unpair_escape(parent: W) -> bool {
        with_s_mut(parent, |x| {
            if x.mode != View::ConfirmUnpair {
                return false;
            }

            view(parent, x, View::Main);
            true
        })
        .unwrap_or(false)
    }

    unsafe extern "system" fn button_subclass(
        hwnd: W,
        message: u32,
        wparam: usize,
        lparam: isize,
        id: usize,
        ref_data: usize,
    ) -> isize {
        let parent = ref_data as W;
        match message {
            PAINT => {
                let mut ps: PaintStruct = core::mem::zeroed();
                BeginPaint(hwnd, &mut ps);
                EndPaint(hwnd, &ps);
                return 0;
            }
            ERASE_BKGND | 0x0318 => return 1,
            SET_FOCUS | KILL_FOCUS => {
                InvalidateRect(parent, core::ptr::null(), 0);
            }
            MOUSE_MOVE => {
                let mut client: Rect = core::mem::zeroed();
                GetClientRect(hwnd, &mut client);
                let inside = point_in_rect(client_point(lparam), client);

                let _ = with_s_mut(parent, |x| {
                    if inside {
                        if x.hover_button != id {
                            x.hover_button = id;
                            InvalidateRect(parent, core::ptr::null(), 0);
                        }
                    } else if x.hover_button == id {
                        x.hover_button = 0;
                        InvalidateRect(parent, core::ptr::null(), 0);
                    }
                });

                if inside {
                    let mut track = TrackMouse {
                        size: size_of::<TrackMouse>() as u32,
                        flags: TME_LEAVE,
                        hwnd_track: hwnd,
                        hover_time: 0,
                    };
                    TrackMouseEvent(&mut track);
                }
            }
            MOUSE_LEAVE => {
                let _ = with_s_mut(parent, |x| {
                    if x.hover_button == id {
                        x.hover_button = 0;
                        InvalidateRect(parent, core::ptr::null(), 0);
                    }
                });
            }
            SET_CURSOR => {
                let cursor = LoadCursorW(0, IDC_HAND as *const u16);
                if cursor != 0 {
                    SetCursor(cursor);
                    return 1;
                }
            }
            KEY_DOWN if wparam == VK_ESCAPE && confirm_unpair_escape(parent) => return 0,
            _ => {}
        }
        let result = DefSubclassProc(hwnd, message, wparam, lparam);
        if matches!(message, 0x0201 | LBUTTON_UP | KEY_DOWN | 0x0101 | 0x000C) {
            InvalidateRect(parent, core::ptr::null(), 0);
        }
        result
    }

    unsafe extern "system" fn edit_subclass(
        hwnd: W,
        message: u32,
        wparam: usize,
        lparam: isize,
        _id: usize,
        ref_data: usize,
    ) -> isize {
        let parent = ref_data as W;
        match message {
            PAINT => {
                let mut ps: PaintStruct = core::mem::zeroed();
                BeginPaint(hwnd, &mut ps);
                EndPaint(hwnd, &ps);
                return 0;
            }
            ERASE_BKGND | 0x0318 => return 1,
            0x0102
                if wparam == 0x0D
                    && with_s(parent, |x| x.mode == View::Pair && x.code == hwnd)
                        .unwrap_or(false) =>
            {
                return 0
            }
            // Native editing/selection remains authoritative; request an atomic
            // parent repaint after actual text or caret/selection changes.
            0x000C | 0x0102 | 0x0300 | 0x0302..=0x0304 => {
                let result = DefSubclassProc(hwnd, message, wparam, lparam);
                InvalidateRect(parent, core::ptr::null(), 0);
                return result;
            }
            SET_FOCUS => {
                let _ = with_s_mut(parent, |x| {
                    x.focus_edit = hwnd;
                    InvalidateRect(parent, core::ptr::null(), 0);
                });
            }
            KILL_FOCUS => {
                let _ = with_s_mut(parent, |x| {
                    if x.focus_edit == hwnd {
                        x.focus_edit = 0;
                        InvalidateRect(parent, core::ptr::null(), 0);
                    }
                });
            }
            KEY_DOWN
                if wparam == 0x0D
                    && with_s(parent, |x| {
                        x.mode == View::Pair && x.code == hwnd && GetFocus() == hwnd
                    })
                    .unwrap_or(false) =>
            {
                // Native Enter repeats do not create a second submit stream.
                if lparam & (1 << 30) == 0 {
                    let button = with_s(parent, |x| x.pair_btn).unwrap_or(0);
                    SendMessageW(parent, CMD, PAIR, button);
                }
                return 0;
            }
            KEY_DOWN if wparam == VK_A && GetKeyState(VK_CONTROL) < 0 => {
                SendMessageW(hwnd, EM_SETSEL, 0, -1);
                InvalidateRect(parent, core::ptr::null(), 0);
                return 0;
            }

            KEY_DOWN if wparam == VK_ESCAPE && confirm_unpair_escape(parent) => return 0,
            LBUTTON_UP => {
                // Pair code is replacement-oriented.
                // A click selects the whole value for fast overwrite.
                let result = DefSubclassProc(hwnd, message, wparam, lparam);
                SetFocus(hwnd);
                SendMessageW(hwnd, EM_SETSEL, 0, -1);
                InvalidateRect(parent, core::ptr::null(), 0);
                return result;
            }
            _ => {}
        }
        let result = DefSubclassProc(hwnd, message, wparam, lparam);
        if matches!(message, KEY_DOWN | 0x0101 | 0x0201) {
            InvalidateRect(parent, core::ptr::null(), 0);
        }
        result
    }

    unsafe fn open_llb_discord(h: W) {
        let operation = ws("open");
        let url = ws("https://discord.gg/linhlanbang");
        ShellExecuteW(
            h,
            operation.as_ptr(),
            url.as_ptr(),
            core::ptr::null(),
            core::ptr::null(),
            1,
        );
    }

    unsafe fn paint(h: W, x: &S) {
        let mut ps: PaintStruct = core::mem::zeroed();
        BeginPaint(h, &mut ps);
        if let Ok(mut renderer) = x.renderer.try_borrow_mut() {
            let _ = renderer.paint(h, x);
        }
        EndPaint(h, &ps);
    }

    unsafe fn tray(h: W, x: &S, add: bool) {
        let paired = x.r.is_paired();
        let i = if !paired {
            0
        } else if x.tx {
            2
        } else {
            1
        };
        let tip = if i == 2 {
            l::TRAY_TRANSMITTING
        } else if i == 1 {
            l::TRAY_STANDBY
        } else {
            l::TRAY_DISCONNECTED
        };
        let mut n: N = core::mem::zeroed();
        n.n = size_of::<N>() as u32;
        n.h = h;
        n.id = 1;
        n.f = NF;
        n.cb = TRAY;
        n.ic = x.icons[i];
        for (a, b) in n.tip.iter_mut().zip(tip.encode_utf16()) {
            *a = b;
        }
        shell_notify_icon(if add { ADD } else { MOD }, &mut n);
    }
    unsafe fn set_view_control_show_cmd(control: W, command: i32) {
        let visibility = if command == HIDE {
            SWP_HIDEWINDOW
        } else {
            SWP_SHOWWINDOW
        };

        SetWindowPos(
            control,
            0,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOREDRAW | visibility,
        );
    }

    unsafe fn view(h: W, x: &mut S, m: View) {
        x.mode = m;
        x.hover_button = 0;
        x.notice = None;

        let pair = m == View::Pair;
        let main = m == View::Main;
        let config = m == View::Config;
        let confirm_unpair = m == View::ConfirmUnpair;

        layout_controls(x);

        set_view_control_show_cmd(x.code, if pair { SHOW } else { HIDE });
        set_view_control_show_cmd(x.pair_btn, if pair { SHOW } else { HIDE });
        set_view_control_show_cmd(x.key, if main || config { SHOW } else { HIDE });
        set_view_control_show_cmd(x.change_btn, HIDE);
        set_view_control_show_cmd(x.sound_on_btn, if main || config { SHOW } else { HIDE });
        set_view_control_show_cmd(x.sound_off_btn, if main || config { SHOW } else { HIDE });
        set_view_control_show_cmd(x.apply_btn, if confirm_unpair { SHOW } else { HIDE });
        set_view_control_show_cmd(
            x.unpair_btn,
            if ((main || config) && x.r.is_paired()) || confirm_unpair {
                SHOW
            } else {
                HIDE
            },
        );
        set_view_control_show_cmd(x.config_btn, HIDE);
        // Status/footer text is parent-painted. Keeping the legacy STATIC
        // children hidden prevents synchronous WM_CTLCOLORSTATIC fallback from
        // ever exposing a white default background during rapid PTT changes.
        set_view_control_show_cmd(x.st, HIDE);
        set_view_control_show_cmd(x.foot, HIDE);

        set(x.config_btn, if config { l::BACK } else { l::CONFIG });
        set(x.apply_btn, l::CANCEL);
        set(x.unpair_btn, l::UNPAIR);

        let title = match m {
            View::Pair => {
                if x.r.is_pairing() {
                    l::STATUS_PAIRING
                } else {
                    l::STATUS_READY_FOR_PAIRING
                }
            }
            View::Config => l::STATUS_SETTINGS,
            View::Capture => l::STATUS_CAPTURE,
            View::ConfirmUnpair => l::STATUS_CONFIRM_UNPAIR,
            View::Main if x.tx => l::STATUS_TRANSMITTING,
            View::Main => l::STATUS_STANDBY,
        };
        set(x.st, title);

        refresh_footer(x);

        // SetFocus may synchronously invoke an edit subclass while mutable
        // Helper state is borrowed. Commit the final focus bookkeeping first;
        // a reentrant callback can then safely fail try_borrow_mut.
        match m {
            View::Pair => {}
            View::Config | View::ConfirmUnpair | View::Main | View::Capture => x.focus_edit = 0,
        }

        match m {
            View::Pair => {}
            View::Config => {
                SetFocus(x.config_btn);
            }
            View::ConfirmUnpair => {
                SetFocus(x.apply_btn);
            }
            View::Main => {
                SetFocus(x.key);
            }
            View::Capture => {
                SetFocus(h);
            }
        }

        tray(h, x, false);
        // V13 structural atomic view transition: geometry and visibility
        // changes above avoid intermediate repaints. Repaint only the settled
        // child set, then invalidate the single Direct2D parent surface.
        for child in [
            x.st,
            x.foot,
            x.code,
            x.key,
            x.pair_btn,
            x.change_btn,
            x.sound_on_btn,
            x.sound_off_btn,
            x.config_btn,
            x.apply_btn,
            x.unpair_btn,
            x.close_btn,
        ] {
            InvalidateRect(child, core::ptr::null(), 0);
        }
        InvalidateRect(h, core::ptr::null(), 0);
    }

    unsafe fn error(h: W, x: &mut S, message: &'static str) {
        notice(h, x, message, true);
    }

    unsafe fn play_connected_sound() {
        #[cfg(test)]
        if std::env::var_os("LCR_UI_V6_PROOF_DIR").is_some() {
            CONNECTED_CUE_COUNT.with(|count| count.set(count.get() + 1));
            return;
        }
        let _ = PlaySoundW(
            CONNECTED_SOUND.as_ptr() as *const u16,
            0,
            SND_ASYNC | SND_NODEFAULT | SND_MEMORY,
        );
    }

    unsafe fn complete_pair_presentation(h: W, x: &mut S) {
        let connected = x.mode == View::Pair;
        x.tx = false;
        x.broker_attempted = false;
        x.return_view = View::Main;
        x.capture_return_view = View::Main;
        view(h, x, View::Main);
        if connected {
            play_connected_sound();
        }
    }

    unsafe fn play_ptt_sound(open: bool) {
        let sound = if open { PTT_DOWN_SOUND } else { PTT_UP_SOUND };
        let _ = PlaySoundW(
            sound.as_ptr() as *const u16,
            0,
            SND_ASYNC | SND_NODEFAULT | SND_MEMORY,
        );
    }

    unsafe fn refresh_transmit_presentation(h: W, x: &S) {
        refresh_footer(x);
        // Accepted local PTT state owns immediate shell/tray feedback.
        // Server-proven LIVE remains relay authority, not a presentation gate.
        // re-enter the full view transition path and do not invalidate the
        // entire shell: both caused visible transient repaint artifacts when
        // the Radio Key was pressed/released rapidly.
        tray(h, x, false);

        let status_region = Layout::new().status.native(x.dpi);
        InvalidateRect(h, &status_region, 0);
        let footer_region = Layout::new().footer.native(x.dpi);
        InvalidateRect(h, &footer_region, 0);
    }

    unsafe fn ev(h: W, x: &mut S, o: InputOutcome) {
        match o {
            InputOutcome::Desired(d) => {
                let previous_tx = x.tx;
                if let Some(c) = x.r.control_handle() {
                    c.desired(d);
                    x.tx = d == Desired::Open;
                } else {
                    x.tx = false;
                }
                if previous_tx != x.tx {
                    if x.ptt_sound {
                        play_ptt_sound(x.tx);
                    }

                    refresh_transmit_presentation(h, x);
                }
            }
            InputOutcome::Captured(k) => {
                let return_view = x.capture_return_view;
                if x.r.update_radio_key(k.clone()).is_ok() {
                    let had_broker = x.broker.is_some();
                    x.input.set_selected(k.clone());
                    x.tx = false;
                    view(h, x, return_view);
                    set(x.key, &key_s(&k));
                    x.key_notice_until =
                        Some(std::time::Instant::now() + std::time::Duration::from_secs(3));
                    InvalidateRect(x.key, core::ptr::null(), 0);
                    InvalidateRect(h, core::ptr::null(), 0);

                    if had_broker {
                        PostMessageW(h, LCR_INPUT_BROKER_RESTART_MESSAGE, 0, 0);
                    } else {
                        x.broker_attempted = false;
                    }
                } else {
                    error(h, x, l::SETTINGS_FAILED)
                }
            }
            InputOutcome::Cancelled => {
                let return_view = x.capture_return_view;
                x.tx = false;
                view(h, x, return_view)
            }
            _ => {}
        }
    }
    fn raw_packet_complete(kind: u32, copied: u32, declared: u32) -> bool {
        let payload = match kind {
            0 => size_of::<Rm>(),
            1 => size_of::<Rk>(),
            _ => return false,
        };

        let required = (size_of::<Rh>() + payload) as u32;
        copied >= required && declared >= required
    }

    unsafe fn raw(h: W, x: &mut S, l: isize) {
        let mut r: Ri = core::mem::zeroed();
        let mut n = size_of::<Ri>() as u32;
        let copied = GetRawInputData(
            l,
            RID,
            (&mut r as *mut Ri).cast(),
            &mut n,
            size_of::<Rh>() as u32,
        );

        if copied == u32::MAX || copied < size_of::<Rh>() as u32 {
            return;
        }

        if !raw_packet_complete(r.h.k, copied, r.h.n) {
            return;
        }

        match r.h.k {
            0 => {
                let b = r.d.m.buttons as u16;
                for (k, e, z) in [
                    (RadioKey::Mouse4, Edge::Down, M4D),
                    (RadioKey::Mouse4, Edge::Up, M4U),
                    (RadioKey::Mouse5, Edge::Down, M5D),
                    (RadioKey::Mouse5, Edge::Up, M5U),
                ] {
                    if b & z != 0 {
                        let o = x.input.event(k, e);
                        ev(h, x, o);
                    }
                }
            }
            1 => {
                let k = r.d.k;
                if k.a != 0x00ff && k.v != 0 && k.v != 255 {
                    let o = x.input.event(
                        RadioKey::Keyboard(k.v as u32),
                        if k.b & BREAK != 0 {
                            Edge::Up
                        } else {
                            Edge::Down
                        },
                    );
                    ev(h, x, o);
                }
            }
            _ => {}
        }
    }

    unsafe fn exit(h: W) {
        let _ = with_s_mut(h, |x| {
            x.tx = false;
            x.broker.take();
            x.r.shutdown();
        });

        // Release Helper state before DestroyWindow synchronously enters
        // WM_DESTROY.
        DestroyWindow(h);
    }
    fn tray_state_text(x: &S) -> &'static str {
        if !x.r.is_paired() {
            l::TRAY_NOT_PAIRED
        } else if x.tx {
            l::TRAY_TRANSMITTING_STATE
        } else {
            l::TRAY_STANDBY_STATE
        }
    }

    unsafe fn menu(h: W) {
        // TrackPopupMenu enters a nested Windows message loop. Snapshot all
        // state-derived values first so no Helper state reference survives
        // across the modal call.
        let Some((paired, status_text, radio_key_text)) =
            with_s(h, |x| (x.r.is_paired(), tray_state_text(x), get(x.key)))
        else {
            return;
        };

        let m = CreatePopupMenu();
        if m == 0 {
            return;
        }

        let show = ws(l::SHOW);
        let exit_label = ws(l::EXIT);
        let status = ws(&format!("{} · {}", l::TRAY_STATUS, status_text));
        let radio_key = ws(&format!("{} · {}", l::TRAY_RADIO_KEY, radio_key_text));

        AppendMenuW(m, 0, 10, show.as_ptr());
        AppendMenuW(m, MF_SEPARATOR, 0, core::ptr::null());

        AppendMenuW(m, MF_GRAYED | MF_DISABLED, 12, status.as_ptr());
        if paired {
            AppendMenuW(m, MF_GRAYED | MF_DISABLED, 13, radio_key.as_ptr());
        }

        AppendMenuW(m, MF_SEPARATOR, 0, core::ptr::null());
        AppendMenuW(m, 0, 11, exit_label.as_ptr());

        let mut p = Pt { x: 0, y: 0 };
        GetCursorPos(&mut p);
        SetForegroundWindow(h);
        let c = TrackPopupMenu(m, TP | TN, p.x, p.y, 0, h, core::ptr::null());
        DestroyMenu(m);

        if c == 10 {
            ShowWindow(h, RESTORE);
            SetForegroundWindow(h);
        } else if c == 11 {
            exit(h)
        }
    }
    fn broker_observation_from_wire(wire: BrokerWireObservation) -> BrokerObservation {
        BrokerObservation {
            epoch: wire.epoch,
            key_revision: wire.key_revision,
            sequence: wire.sequence,
            observed_tick: wire.observed_tick,
            level: wire.level,
        }
    }

    unsafe fn fail_broker_closed(h: W, x: &mut S) {
        let released = x.input.force_release();
        ev(h, x, released);

        let (_, closed) = x.input.begin_handoff();
        ev(h, x, closed);

        // Transport failure / overflow is uncertain physical state. Stay with
        // no active provider rather than silently falling back and reopening.
        x.broker.take();
    }

    unsafe fn process_broker_wake(h: W, x: &mut S) {
        if x.broker
            .as_ref()
            .is_some_and(|broker| broker.transport_failed())
        {
            fail_broker_closed(h, x);
            return;
        }

        if !x
            .broker
            .as_ref()
            .is_some_and(|broker| broker.authenticated())
        {
            return;
        }

        loop {
            let poll = match x.broker.as_ref() {
                Some(broker) => broker.poll_observation(),
                None => return,
            };

            match poll {
                BrokerPoll::Overflowed => {
                    fail_broker_closed(h, x);
                    return;
                }
                BrokerPoll::Observation(wire) => {
                    if x.input.provider() == InputProvider::None {
                        if wire.epoch != x.input.epoch()
                            || wire.key_revision != x.input.key_revision()
                            || !x.input.activate_provider(InputProvider::Broker, wire.epoch)
                        {
                            fail_broker_closed(h, x);
                            return;
                        }
                    } else if x.input.provider() != InputProvider::Broker {
                        fail_broker_closed(h, x);
                        return;
                    }

                    let observation = broker_observation_from_wire(wire);
                    let now_tick = GetTickCount64();
                    let outcome =
                        x.input
                            .broker_observation(observation, now_tick, BROKER_MAX_DOWN_AGE_MS);

                    ev(h, x, outcome);
                }
                BrokerPoll::Empty => break,
            }
        }

        if x.broker
            .as_ref()
            .is_some_and(|broker| broker.transport_failed())
        {
            fail_broker_closed(h, x);
        }
    }

    unsafe extern "system" fn proc(h: W, m: u32, w: usize, l_: isize) -> isize {
        match shell_lifecycle_action(m, taskbar_created_message()) {
            ShellLifecycleAction::ReaddTray => {
                let _ = with_s(h, |x| tray(h, x, true));
                return 0;
            }
            ShellLifecycleAction::ReaddTrayAndShow => {
                return with_s(h, |x| {
                    tray(h, x, true);
                    ShowWindow(h, RESTORE);
                    SetForegroundWindow(h);
                })
                .map(|_| 1isize)
                .unwrap_or(0);
            }
            ShellLifecycleAction::None => {}
        }

        match m {
            BROKER_WAKE_MESSAGE => {
                let _ = with_s_mut(h, |x| process_broker_wake(h, x));
                0
            }
            LCR_INPUT_BROKER_RESTART_MESSAGE => {
                let launch = with_s_mut(h, |x| {
                    let previous = x.broker.take();
                    let (epoch, closed) = x.input.begin_handoff();
                    let selected = x.input.selected();
                    let key_revision = x.input.key_revision();

                    ev(h, x, closed);

                    (previous, selected, epoch, key_revision)
                });

                if let Some((previous, selected, epoch, key_revision)) = launch {
                    // Never hold Helper state while Drop joins the old pipe worker.
                    drop(previous);

                    let broker = InputBrokerClient::launch_bound(h, &selected, epoch, key_revision);

                    let _ = with_s_mut(h, move |x| {
                        if x.input.provider() != InputProvider::None
                            || x.input.epoch() != epoch
                            || x.input.key_revision() != key_revision
                        {
                            drop(broker);
                            return;
                        }

                        match broker {
                            Some(broker) => {
                                // Provider activation is deferred until the first
                                // authenticated observation wakes this window.
                                x.broker = Some(broker);
                            }
                            None => {
                                // Launch never became an authoritative Broker.
                                // Cleanly restore the previous Raw provider.
                                let _ = x.input.activate_provider(InputProvider::Raw, epoch);
                            }
                        }
                    });
                }

                0
            }
            LCR_RUNTIME_EXIT_MESSAGE => {
                exit(h);
                0
            }
            PAINT => {
                if with_s(h, |x| paint(h, x)).is_some() {
                    0
                } else {
                    DefWindowProcW(h, m, w, l_)
                }
            }
            ERASE_BKGND => 1,
            // Children are interaction hosts; the parent Direct2D surface owns
            // all custom visuals. Never request native white fallback.
            DRAW_ITEM => 1,
            0x0317 | 0x0318 => with_s(h, |x| {
                let mut rect: Rect = core::mem::zeroed();
                GetClientRect(h, &mut rect);
                x.renderer
                    .try_borrow_mut()
                    .ok()
                    .and_then(|mut renderer| renderer.print(w as I, rect, x).ok())
                    .map(|_| 1)
                    .unwrap_or(0)
            })
            .unwrap_or(0),
            0x02E0 => {
                let suggested = &*(l_ as *const Rect);
                let _ = with_s_mut(h, |x| {
                    set_presentation_dpi(h, x, (w & 0xFFFF) as u32, suggested.left, suggested.top)
                });
                0
            }
            COLOR_EDIT => with_s(h, |x| {
                let hdc = w as I;
                SetTextColor(hdc, COLOR_TEXT);
                SetBkColor(hdc, COLOR_EDIT_BG);
                x.edit_brush
            })
            .unwrap_or(0),
            COLOR_STATIC => with_s(h, |x| x.edit_brush).unwrap_or(0),
            SET_CURSOR => {
                let mut cursor = Pt { x: 0, y: 0 };
                let mut window = Rect {
                    left: 0,
                    top: 0,
                    right: 0,
                    bottom: 0,
                };
                if GetCursorPos(&mut cursor) != 0 && GetWindowRect(h, &mut window) != 0 {
                    let client = Pt {
                        x: cursor.x - window.left,
                        y: cursor.y - window.top,
                    };
                    if with_s(h, |x| {
                        point_in_rect(logical_point(client, x.dpi), crest_rect(x.mode))
                    })
                    .unwrap_or(false)
                    {
                        let hand = LoadCursorW(0, IDC_HAND as *const u16);
                        if hand != 0 {
                            SetCursor(hand);
                            return 1;
                        }
                    }
                }
                DefWindowProcW(h, m, w, l_)
            }
            LBUTTON_UP => {
                if with_s(h, |x| {
                    point_in_rect(logical_point(client_point(l_), x.dpi), crest_rect(x.mode))
                })
                .unwrap_or(false)
                {
                    open_llb_discord(h);
                    0
                } else {
                    DefWindowProcW(h, m, w, l_)
                }
            }
            INPUT => {
                let _ = with_s_mut(h, |x| raw(h, x, l_));

                // RIM_INPUT == 0 is foreground raw input. DefWindowProc must
                // receive it after processing so Windows can perform cleanup.
                if w == 0 {
                    DefWindowProcW(h, m, w, l_)
                } else {
                    0
                }
            }
            TIMER => {
                let request_broker = with_s_mut(h, |x| {
                    match x.r.poll_pair() {
                        PairPoll::Paired => complete_pair_presentation(h, x),
                        PairPoll::Invalidated => {
                            x.tx = false;
                            x.broker.take();
                            x.broker_attempted = false;
                            x.return_view = View::Pair;
                            x.capture_return_view = View::Pair;
                            view(h, x, View::Pair)
                        }
                        PairPoll::Denied => error(h, x, l::PAIR_DENIED),
                        PairPoll::Transient => error(h, x, l::PAIR_TRANSIENT),
                        PairPoll::ActivationFailed => error(h, x, l::PAIR_ACTIVATION_FAILED),
                        _ => {}
                    }

                    if let Some(until) = x.key_notice_until.as_ref() {
                        if std::time::Instant::now() >= *until {
                            x.key_notice_until = None;
                            InvalidateRect(h, core::ptr::null(), 0);
                        }
                    }

                    if x.tx {
                        match x.input.provider() {
                            InputProvider::Broker if x.broker.is_some() => {
                                let outcome = x.input.expire_broker_down_lease(
                                    GetTickCount64(),
                                    BROKER_DOWN_LEASE_MS,
                                );
                                ev(h, x, outcome);
                            }
                            InputProvider::Raw => {
                                let selected = x.input.selected();

                                if !physical_key_down(&selected) {
                                    let outcome = x.input.force_release();
                                    ev(h, x, outcome);
                                }
                            }
                            InputProvider::Broker | InputProvider::None => {
                                let outcome = x.input.force_release();
                                ev(h, x, outcome);
                            }
                        }
                    }

                    if x.r.is_paired()
                        && x.broker.is_none()
                        && !x.broker_attempted
                        && foreground_requires_broker()
                    {
                        x.broker_attempted = true;
                        true
                    } else {
                        false
                    }
                })
                .unwrap_or(false);

                if request_broker {
                    PostMessageW(h, LCR_INPUT_BROKER_RESTART_MESSAGE, 0, 0);
                }
                0
            }
            CMD => {
                let _ = with_s_mut(h, |x| match w & 65535 {
                    PAIR => {
                        if x.mode != View::Pair {
                            return;
                        }

                        let code = get(x.code);
                        match x.r.begin_pair(&code) {
                            Ok(()) => view(h, x, View::Pair),
                            Err(PairStartError::AlreadyPaired) => {
                                x.tx = false;
                                x.return_view = View::Main;
                                x.capture_return_view = View::Main;
                                view(h, x, View::Main);
                            }
                            Err(PairStartError::Busy) => notice(h, x, l::PAIR_BUSY, false),
                            Err(PairStartError::EmptyCode) => error(h, x, l::PAIR_EMPTY_CODE),
                            Err(PairStartError::SpawnFailed) => error(h, x, l::PAIR_START_FAILED),
                        }
                    }
                    CHANGE | KEY_DISPLAY => {
                        if matches!(x.mode, View::Main | View::Config) {
                            x.capture_return_view = x.mode;
                            x.input.begin_capture();
                            view(h, x, View::Capture);
                        }
                    }
                    CONFIG => {
                        if x.mode == View::Config {
                            view(h, x, x.return_view);
                        } else if x.mode != View::Capture {
                            x.return_view = x.mode;
                            view(h, x, View::Config);
                        }
                    }
                    APPLY => {
                        if x.mode == View::ConfirmUnpair {
                            view(h, x, View::Main);
                        }
                    }
                    SOUND_ON | SOUND_OFF => {
                        if matches!(x.mode, View::Main | View::Config) {
                            let next = !x.ptt_sound;
                            match x.r.update_ptt_sound(next) {
                                Ok(()) => {
                                    x.ptt_sound = next;
                                    InvalidateRect(x.sound_on_btn, core::ptr::null(), 0);
                                    InvalidateRect(x.sound_off_btn, core::ptr::null(), 0);
                                }
                                Err(_) => error(h, x, l::SETTINGS_FAILED),
                            }
                        }
                    }
                    UNPAIR => {
                        if x.mode == View::ConfirmUnpair {
                            if x.r.unpair_local().is_ok() {
                                x.tx = false;
                                x.broker.take();
                                x.broker_attempted = false;
                                x.return_view = View::Pair;
                                x.capture_return_view = View::Pair;
                                view(h, x, View::Pair)
                            } else {
                                error(h, x, l::SETTINGS_FAILED)
                            }
                        } else if matches!(x.mode, View::Main | View::Config) {
                            view(h, x, View::ConfirmUnpair);
                        }
                    }
                    CLOSE_BUTTON => {
                        ShowWindow(h, HIDE);
                    }
                    _ => {}
                });
                0
            }
            TRAY => {
                if l_ == DCLICK {
                    ShowWindow(h, RESTORE);
                } else if l_ == RUP {
                    menu(h)
                }
                0
            }
            SYSCOMMAND if (w & 0xFFF0) == SC_CLOSE => {
                ShowWindow(h, HIDE);
                0
            }
            NCHITTEST => {
                let mut window = Rect {
                    left: 0,
                    top: 0,
                    right: 0,
                    bottom: 0,
                };
                GetWindowRect(h, &mut window);

                let screen = Pt {
                    x: (l_ as u16 as i16) as i32,
                    y: ((l_ >> 16) as u16 as i16) as i32,
                };
                let client = Pt {
                    x: screen.x - window.left,
                    y: screen.y - window.top,
                };
                let crest_hot = with_s(h, |x| {
                    point_in_rect(logical_point(client, x.dpi), crest_rect(x.mode))
                })
                .unwrap_or(false);

                with_s_mut(h, |x| {
                    if x.crest_hover != crest_hot {
                        x.crest_hover = crest_hot;
                        let layout = Layout::new();
                        let crest = (if x.mode == View::Pair {
                            layout.pair_logo
                        } else {
                            layout.header_logo
                        })
                        .native(x.dpi);
                        InvalidateRect(h, &crest, 0);
                    }

                    let target = WindowFromPoint(screen);
                    if crest_hot || interactive_window(x, target) {
                        HTCLIENT
                    } else {
                        HTCAPTION
                    }
                })
                .unwrap_or(HTCLIENT)
            }
            CLOSE => {
                ShowWindow(h, HIDE);
                0
            }
            DEST => {
                KillTimer(h, 1);
                let mut n: N = core::mem::zeroed();
                n.n = size_of::<N>() as u32;
                n.h = h;
                n.id = 1;
                shell_notify_icon(DEL, &mut n);

                // Detach HWND authority before cleanup. Any synchronous callback
                // from this point observes no Helper state instead of borrowing
                // storage that is being torn down.
                let state_ptr = GetWindowLongPtrW(h, USER) as *const RefCell<S>;
                SetWindowLongPtrW(h, USER, 0);

                if let Some(cell) = state_ptr.as_ref() {
                    let _ = try_with(cell, |x| {
                        for object in [x.edit_brush, x.font_edit] {
                            if object != 0 {
                                DeleteObject(object);
                            }
                        }
                    });
                }

                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(h, m, w, l_),
        }
    }
    #[allow(clippy::too_many_arguments)]
    unsafe fn child(
        h: W,
        style: I,
        class: &str,
        text: &str,
        rect: BoxDip,
        dpi: u32,
        id: usize,
    ) -> W {
        let class = ws(class);
        let text = ws(text);
        let r = rect.native(dpi);
        CreateWindowExW(
            0x20,
            class.as_ptr(),
            text.as_ptr(),
            CHILD | V | style as u32,
            r.left,
            r.top,
            r.right - r.left,
            r.bottom - r.top,
            h,
            id as I,
            0,
            core::ptr::null_mut(),
        )
    }
    pub fn run() {
        let _ = taskbar_created_message();

        let Ok(a) = std::env::var("APPDATA") else {
            return;
        };
        let f = FileStore::new(FileStore::appdata_path(&a));
        let c = f.load();
        let selected = key(&c);
        let initial_view = if c.device_token.is_some() {
            View::Main
        } else {
            View::Pair
        };
        let Ok(r) = HelperRuntime::start_from_appdata(&a) else {
            return;
        };
        unsafe {
            let _dpi_context = UiDpiContext::enter();
            let initial_dpi = GetDpiForSystem().max(96);
            let i = GetModuleHandleW(core::ptr::null());
            let cl = ws(RUNTIME_WINDOW_CLASS);
            #[cfg(test)]
            let cl = if std::env::var_os("LCR_UI_V6_PROOF_DIR").is_some() {
                ws("LCRUiV6Proof")
            } else {
                cl
            };
            let ti = ws(l::WINDOW_TITLE);
            let q = C {
                n: size_of::<C>() as u32,
                s: 0,
                p: Some(proc),
                a: 0,
                b: 0,
                i,
                ic: ico(APP, 48),
                cu: 0,
                br: 0,
                me: core::ptr::null(),
                cl: cl.as_ptr(),
                sm: ico(APP, 16),
            };
            if RegisterClassExW(&q) == 0 {
                return;
            }
            let h = CreateWindowExW(
                EX_APPWINDOW,
                cl.as_ptr(),
                ti.as_ptr(),
                POPUP,
                200,
                120,
                (WINDOW_WIDTH as f32 * initial_dpi as f32 / 96.0).round() as i32,
                (WINDOW_HEIGHT as f32 * initial_dpi as f32 / 96.0).round() as i32,
                0,
                0,
                i,
                core::ptr::null_mut(),
            );
            if h == 0 {
                return;
            }
            // Bind both explicit per-window and class icon authority to APP.
            SendMessageW(h, 0x0080, 1, q.ic); // WM_SETICON / ICON_BIG
            SendMessageW(h, 0x0080, 0, q.sm); // WM_SETICON / ICON_SMALL
            let dpi = GetDpiForWindow(h).max(96);
            let dimensions = Layout::new().viewport.native(dpi);
            SetWindowPos(
                h,
                0,
                0,
                0,
                dimensions.right,
                dimensions.bottom,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
            if !apply_window_region(h, dimensions.right, dimensions.bottom) {
                DestroyWindow(h);
                return;
            }

            let edit_brush = CreateSolidBrush(COLOR_EDIT_BG);
            let jura = PrivateFont::load();
            let font_edit = native_edit_font(dpi);
            let renderer = match Renderer::new(dpi) {
                Ok(renderer) => renderer,
                Err(_) => {
                    DeleteObject(edit_brush);
                    DeleteObject(font_edit);
                    startup_error(h, "DIRECT2D / DIRECTWRITE INITIALIZATION FAILED");
                    DestroyWindow(h);
                    return;
                }
            };

            let layout = Layout::new();
            let st = child(h, 0, "STATIC", "", layout.status, dpi, 0);
            let code = child(
                h,
                (TAB | CENTER_TEXT) as I,
                "EDIT",
                "",
                layout.edit(),
                dpi,
                CODE,
            );
            let pair_btn = child(
                h,
                (TAB | OWNER_DRAW) as I,
                "BUTTON",
                l::PAIR_BUTTON,
                layout.pair_action,
                dpi,
                PAIR,
            );
            let key_control = child(
                h,
                (TAB | OWNER_DRAW) as I,
                "BUTTON",
                &key_s(&selected),
                layout.key_control(),
                dpi,
                KEY_DISPLAY,
            );
            let change_btn = child(
                h,
                (TAB | OWNER_DRAW) as I,
                "BUTTON",
                l::CHANGE,
                layout.key_control(),
                dpi,
                CHANGE,
            );
            let sound_on_btn = child(
                h,
                (TAB | OWNER_DRAW) as I,
                "BUTTON",
                l::PTT_SOUND_ON,
                layout.sound_hit(true),
                dpi,
                SOUND_ON,
            );
            let sound_off_btn = child(
                h,
                (TAB | OWNER_DRAW) as I,
                "BUTTON",
                l::PTT_SOUND_OFF,
                layout.sound_hit(false),
                dpi,
                SOUND_OFF,
            );
            let config_btn = child(
                h,
                (TAB | OWNER_DRAW) as I,
                "BUTTON",
                l::CONFIG,
                layout.section,
                dpi,
                CONFIG,
            );
            let close_btn = child(
                h,
                (TAB | OWNER_DRAW) as I,
                "BUTTON",
                l::CLOSE_BUTTON,
                layout.close,
                dpi,
                CLOSE_BUTTON,
            );
            let apply_btn = child(
                h,
                (TAB | OWNER_DRAW) as I,
                "BUTTON",
                l::CANCEL,
                layout.confirm_cancel,
                dpi,
                APPLY,
            );
            let unpair_btn = child(
                h,
                (TAB | OWNER_DRAW) as I,
                "BUTTON",
                l::UNPAIR,
                layout.unpair,
                dpi,
                UNPAIR,
            );
            for (control, id) in [
                (pair_btn, PAIR),
                (key_control, KEY_DISPLAY),
                (change_btn, CHANGE),
                (sound_on_btn, SOUND_ON),
                (sound_off_btn, SOUND_OFF),
                (config_btn, CONFIG),
                (apply_btn, APPLY),
                (unpair_btn, UNPAIR),
                (close_btn, CLOSE_BUTTON),
            ] {
                if SetWindowSubclass(control, button_subclass, id, h as usize) == 0 {
                    DestroyWindow(h);
                    return;
                }
            }
            if SetWindowSubclass(code, edit_subclass, CODE, h as usize) == 0 {
                DestroyWindow(h);
                return;
            }

            let foot = child(h, 0, "STATIC", "", layout.footer, dpi, 0);

            SendMessageW(st, SET_FONT, font_edit as usize, 1);
            SendMessageW(foot, SET_FONT, font_edit as usize, 1);
            for control in [code, key_control] {
                SendMessageW(control, SET_FONT, font_edit as usize, 1);
            }
            let x = Box::new(RefCell::new(S {
                r,
                input: InputAuthority::new(selected),
                st,
                foot,
                code,
                key: key_control,
                pair_btn,
                change_btn,
                sound_on_btn,
                sound_off_btn,
                config_btn,
                apply_btn,
                unpair_btn,
                close_btn,
                renderer: RefCell::new(renderer),
                dpi,
                _jura: jura,
                icons: [ico(OFF, 32), ico(ON, 32), ico(TX, 32)],
                edit_brush,
                font_edit,
                mode: initial_view,
                return_view: initial_view,
                capture_return_view: initial_view,
                key_notice_until: None,
                tx: false,
                ptt_sound: c.ptt_sound,
                broker: None,
                broker_attempted: false,
                hover_button: 0,
                crest_hover: false,
                focus_edit: 0,
                notice: None,
            }));
            let state_ptr = (&*x as *const RefCell<S>) as I;

            // A new top-level window must not already carry Helper userdata.
            // Do not enter the message loop unless the exact pointer round-trips.
            if GetWindowLongPtrW(h, USER) != 0 {
                SetWindowLongPtrW(h, USER, 0);
                startup_error(0, l::STARTUP_STATE_FAILED);
                return;
            }

            SetWindowLongPtrW(h, USER, state_ptr);

            if GetWindowLongPtrW(h, USER) != state_ptr {
                SetWindowLongPtrW(h, USER, 0);
                startup_error(0, l::STARTUP_STATE_FAILED);
                return;
            }

            #[cfg(test)]
            if presentation_tests::maybe_run(h, &x) {
                // The isolated UI fixture never registers input, tray, timers,
                // updater, Broker or credentials against the installed Helper.
                // It finishes through the existing graceful Exit entry.
                return;
            }

            let d = [
                Rid {
                    p: 1,
                    u: 2,
                    f: RIDEV_INPUTSINK,
                    h,
                },
                Rid {
                    p: 1,
                    u: 6,
                    f: RIDEV_INPUTSINK,
                    h,
                },
            ];

            if RegisterRawInputDevices(d.as_ptr(), 2, size_of::<Rid>() as u32) == 0 {
                startup_error(h, l::STARTUP_INPUT_FAILED);
                DestroyWindow(h);
                return;
            }

            if with_s(h, |state| tray(h, state, true)).is_none() {
                startup_error(h, l::STARTUP_STATE_FAILED);
                DestroyWindow(h);
                return;
            }

            let Some(mode) = with_s(h, |state| state.mode) else {
                startup_error(h, l::STARTUP_STATE_FAILED);
                DestroyWindow(h);
                return;
            };

            if with_s_mut(h, |state| view(h, state, mode)).is_none() {
                startup_error(h, l::STARTUP_STATE_FAILED);
                DestroyWindow(h);
                return;
            }

            if SetTimer(h, 1, 100, 0) == 0 {
                startup_error(h, l::STARTUP_TIMER_FAILED);
                DestroyWindow(h);
                return;
            }

            ShowWindow(h, SHOW);
            let mut m: Msg = core::mem::zeroed();
            loop {
                let message = GetMessageW(&mut m, 0, 0, 0);
                if message > 0 {
                    TranslateMessage(&m);
                    DispatchMessageW(&m);
                } else {
                    if message < 0 && GetWindowLongPtrW(h, USER) != 0 {
                        // Keep the Box alive while WM_DESTROY detaches userdata.
                        DestroyWindow(h);
                    }
                    break;
                }
            }
        }
    }

    #[cfg(test)]
    mod raw_packet_tests {
        use super::*;

        #[test]
        fn shell_recovery_policy_is_stable_across_10000_events() {
            assert_ne!(
                LCR_RUNTIME_ACTIVATE_MESSAGE, BROKER_WAKE_MESSAGE,
                "runtime activation must not collide with Broker wake"
            );

            let taskbar_created = 0xC123;

            for _ in 0..10_000 {
                assert_eq!(
                    shell_lifecycle_action(taskbar_created, taskbar_created),
                    ShellLifecycleAction::ReaddTray
                );
                assert_eq!(
                    shell_lifecycle_action(LCR_RUNTIME_ACTIVATE_MESSAGE, taskbar_created),
                    ShellLifecycleAction::ReaddTrayAndShow
                );
                assert_eq!(
                    shell_lifecycle_action(0x1234, taskbar_created),
                    ShellLifecycleAction::None
                );
            }

            assert_eq!(
                shell_lifecycle_action(taskbar_created, 0),
                ShellLifecycleAction::None
            );
        }

        #[test]
        fn taskbar_created_message_registration_is_nonzero_and_stable() {
            let first = taskbar_created_message();
            assert_ne!(first, 0);

            for _ in 0..1_000 {
                assert_eq!(taskbar_created_message(), first);
            }
        }

        #[test]
        fn packet_must_contain_header_and_selected_payload() {
            let header = size_of::<Rh>() as u32;
            let mouse = (size_of::<Rh>() + size_of::<Rm>()) as u32;
            let keyboard = (size_of::<Rh>() + size_of::<Rk>()) as u32;

            assert!(!raw_packet_complete(0, header, mouse));
            assert!(!raw_packet_complete(0, mouse, header));
            assert!(raw_packet_complete(0, mouse, mouse));

            assert!(!raw_packet_complete(1, header, keyboard));
            assert!(!raw_packet_complete(1, keyboard, header));
            assert!(raw_packet_complete(1, keyboard, keyboard));

            assert!(!raw_packet_complete(99, mouse, mouse));
        }

        #[test]
        fn reentrant_state_borrow_is_denied_without_panicking() {
            let cell = RefCell::new(0_u32);

            let result = try_with_mut(&cell, |value| {
                *value = 1;

                assert!(try_with(&cell, |_| ()).is_none());
                assert!(try_with_mut(&cell, |_| ()).is_none());

                *value
            });

            assert_eq!(result, Some(1));
            assert_eq!(try_with(&cell, |value| *value), Some(1));
        }
    }
    #[cfg(test)]
    mod runtime_quiesce_tests {
        use super::*;
        use std::{
            process::{Child, Command},
            sync::Mutex,
            thread,
            time::{Duration, Instant},
        };

        static SERIAL: Mutex<()> = Mutex::new(());

        fn serial_guard() -> std::sync::MutexGuard<'static, ()> {
            SERIAL
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        }

        fn test_dir(case: &str) -> PathBuf {
            let directory =
                std::env::temp_dir().join(format!("lcr-c4d1-{case}-{}", std::process::id()));

            let _ = fs::remove_dir_all(&directory);
            fs::create_dir_all(&directory).unwrap();
            directory
        }

        unsafe extern "system" fn probe_proc(
            window: W,
            message: u32,
            wparam: usize,
            lparam: isize,
        ) -> isize {
            match message {
                LCR_RUNTIME_ACTIVATE_MESSAGE => 1,
                LCR_RUNTIME_EXIT_MESSAGE => {
                    // Reuse the same function the production private-message
                    // branch calls. Probe windows have no Helper state, so
                    // `exit` simply destroys the HWND.
                    exit(window);
                    0
                }
                DEST => {
                    PostQuitMessage(0);
                    0
                }
                _ => DefWindowProcW(window, message, wparam, lparam),
            }
        }

        #[test]
        #[ignore = "subprocess target for C4D1 runtime-quiesce integration test"]
        fn c4d1_runtime_quiesce_probe() {
            let _runtime_guard = match try_acquire_named_mutex(RUNTIME_MUTEX_NAME).unwrap() {
                NamedMutexAcquire::Acquired(guard) => guard,
                NamedMutexAcquire::AlreadyExists => {
                    panic!("C4D1 probe runtime mutex unexpectedly already exists")
                }
            };

            let ready = PathBuf::from(
                std::env::var_os("LCR_TEST_C4D1_READY").expect("missing C4D1 ready-file authority"),
            );

            unsafe {
                let instance = GetModuleHandleW(core::ptr::null());
                let class = ws(RUNTIME_WINDOW_CLASS);
                let title = ws("LCR C4D1 Probe");
                let window_class = C {
                    n: size_of::<C>() as u32,
                    s: 0,
                    p: Some(probe_proc),
                    a: 0,
                    b: 0,
                    i: instance,
                    ic: 0,
                    cu: 0,
                    br: 0,
                    me: core::ptr::null(),
                    cl: class.as_ptr(),
                    sm: 0,
                };

                assert_ne!(
                    RegisterClassExW(&window_class),
                    0,
                    "failed to register C4D1 probe window class"
                );

                let window = CreateWindowExW(
                    0,
                    class.as_ptr(),
                    title.as_ptr(),
                    POPUP,
                    0,
                    0,
                    64,
                    64,
                    0,
                    0,
                    instance,
                    core::ptr::null_mut(),
                );

                assert_ne!(window, 0, "failed to create C4D1 probe window");
                fs::write(&ready, b"ready").unwrap();

                let mut message: Msg = core::mem::zeroed();
                loop {
                    let result = GetMessageW(&mut message, 0, 0, 0);
                    if result > 0 {
                        TranslateMessage(&message);
                        DispatchMessageW(&message);
                    } else {
                        assert!(result >= 0, "C4D1 probe GetMessageW failed");
                        break;
                    }
                }
            }
        }

        fn spawn_probe(case: &str) -> (Child, PathBuf) {
            let directory = test_dir(case);
            let ready = directory.join("ready");
            let executable = std::env::current_exe().unwrap();

            let mut child = Command::new(executable)
                .args(["--ignored", "c4d1_runtime_quiesce_probe", "--nocapture"])
                .env("LCR_TEST_C4D1_READY", &ready)
                .spawn()
                .unwrap();

            let deadline = Instant::now() + Duration::from_secs(10);
            while !ready.is_file() {
                if let Some(status) = child.try_wait().unwrap() {
                    panic!("C4D1 probe exited before readiness: {status}");
                }

                if Instant::now() >= deadline {
                    // Test-only emergency cleanup; production C4D1 contains no
                    // process termination fallback.
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("timed out waiting for C4D1 probe readiness");
                }

                thread::sleep(Duration::from_millis(20));
            }

            (child, directory)
        }

        #[test]
        #[ignore = "requires exclusive canonical runtime; exercised by bounded Windows live acceptance"]
        fn existing_runtime_activation_preserves_same_process_1000_times() {
            let _serial = serial_guard();
            let executable = std::env::current_exe().unwrap();
            let (mut child, directory) = spawn_probe("activate-existing");
            let process_id = child.id();

            for _ in 0..1_000 {
                activate_existing_runtime_for_executable(&executable).unwrap();
                assert_eq!(child.id(), process_id);
            }

            assert!(
                child.try_wait().unwrap().is_none(),
                "duplicate activation unexpectedly terminated the existing runtime"
            );

            quiesce_runtime_for_uninstall_with_timeout(&executable, 10_000).unwrap();
            assert!(child.wait().unwrap().success());
            fs::remove_dir_all(directory).unwrap();
        }

        #[test]
        fn c4d1_real_window_process_is_validated_signaled_and_waited() {
            let _serial = serial_guard();
            let executable = std::env::current_exe().unwrap();
            let (mut child, directory) = spawn_probe("real");

            quiesce_runtime_for_uninstall_with_timeout(&executable, 10_000).unwrap();

            let status = child.wait().unwrap();
            assert!(status.success());
            assert!(!runtime_mutex_exists().unwrap());

            fs::remove_dir_all(directory).unwrap();
        }

        #[test]
        fn c4d1_wrong_process_image_fails_before_exit_signal() {
            let _serial = serial_guard();
            let executable = std::env::current_exe().unwrap();
            let (mut child, directory) = spawn_probe("wrong-image");
            let wrong = directory.join("wrong-helper.exe");
            fs::write(&wrong, b"not-the-runtime-image").unwrap();

            let error = quiesce_runtime_for_uninstall_with_timeout(&wrong, 1_000).unwrap_err();

            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert!(
                child.try_wait().unwrap().is_none(),
                "wrong-image validation unexpectedly terminated the runtime"
            );

            // Cleanly release the test child using the valid authority.
            quiesce_runtime_for_uninstall_with_timeout(&executable, 10_000).unwrap();
            assert!(child.wait().unwrap().success());

            fs::remove_dir_all(directory).unwrap();
        }

        #[test]
        fn c4d1_runtime_mutex_without_window_fails_closed() {
            let _serial = serial_guard();
            let executable = std::env::current_exe().unwrap();
            let guard = match try_acquire_named_mutex(RUNTIME_MUTEX_NAME).unwrap() {
                NamedMutexAcquire::Acquired(guard) => guard,
                NamedMutexAcquire::AlreadyExists => {
                    panic!("fresh C4D1 no-window mutex unexpectedly exists")
                }
            };

            let error = quiesce_runtime_for_uninstall_with_timeout(&executable, 0).unwrap_err();

            assert_eq!(error.kind(), io::ErrorKind::NotFound);
            drop(guard);
        }

        #[test]
        fn c4d1_process_wait_timeout_is_bounded() {
            let _serial = serial_guard();
            let process = OwnedRuntimeProcess::open(std::process::id()).unwrap();

            let error = wait_for_runtime_process_exit(&process, 0).unwrap_err();

            assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        }
    }
}

pub use final_shell::{activate_existing_runtime, quiesce_runtime_for_uninstall, run};
