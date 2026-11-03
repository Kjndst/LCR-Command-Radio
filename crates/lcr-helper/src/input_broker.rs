//! Minimal elevated physical-input broker transport.
//!
//! The Broker owns authenticated local IPC plus event-driven physical input
//! observation. `InputAuthority` in the normal Helper remains the sole owner of
//! PTT authority, epoch/revision admission, freshness, DOWN lease acceptance,
//! pairing, network transport, Discord authority, and Helper UI state.
//! The Broker observes only; it never suppresses or captures user input.
//!
//! The parent creates one local Named Pipe instance with an explicit DACL,
//! demand-runs the already-proven fixed protected Broker Task, then binds the
//! actual connecting pipe-client PID to the protected Broker image and current
//! Windows session. The elevated Broker performs the reverse
//! server-PID/process/session/image verification.
//!
//! PostMessage is deliberately NOT an authority transport in R1B. R1C will
//! connect authenticated observations to `InputAuthority`.

use crate::{
    broker_artifact::{
        verify_current_broker_bundle, verify_current_broker_manifest, VerifiedBrokerArtifact,
        BROKER_SIGNED_MANIFEST_MAX_BYTES,
    },
    broker_install::{
        current_protected_broker_path, protected_broker_path_for_version, ProtectedBrokerPath,
    },
    broker_security::{
        establish_protected_broker_update_directory,
        establish_uninstall_handoff_filesystem_security, protect_and_verify_broker_candidate,
        protected_broker_filesystem_security_matches,
        uninstall_handoff_filesystem_security_matches, validate_uninstall_handoff_filesystem_shape,
    },
    distribution::{
        self, executable_product_version, prepare_streaming_executable_install,
        StreamingExecutableInstall,
    },
    input::{Edge, RadioKey},
    task_authority::{
        delete_broker_task_and_verify_absent_via_scheduler, demand_run_broker_task_via_scheduler,
        recognize_broker_task_via_scheduler, register_or_update_broker_task_via_scheduler,
        verify_broker_task_via_scheduler, BrokerTaskContract, BrokerTaskVerification,
        RecognizedBrokerTaskVerification,
    },
};
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    ffi::{OsStr, OsString},
    fmt::Write as _,
    fs::{self, File},
    io::{self, Read},
    iter::once,
    mem::size_of,
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Instant,
};

type Hwnd = isize;
type Hinstance = isize;
type Handle = *mut core::ffi::c_void;

const BROKER_ARG: &str = "--lcr-input-broker";

/// Private WM_APP message used only as a queue/status wake hint.
///
/// wParam/lParam carry no input state and have zero PTT authority.
pub const BROKER_WAKE_MESSAGE: u32 = 0x804E;

const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x0000_1000;
const TOKEN_QUERY: u32 = 0x0008;
const TOKEN_USER: u32 = 1;
const TOKEN_ELEVATION: u32 = 20;
const SYNCHRONIZE: u32 = 0x0010_0000;

const WAIT_OBJECT_0: u32 = 0x0000_0000;
const WAIT_FAILED: u32 = 0xFFFF_FFFF;
const INFINITE: u32 = 0xFFFF_FFFF;

const SEE_MASK_NOCLOSEPROCESS: u32 = 0x0000_0040;
const SW_HIDE: i32 = 0;

const DUPLICATE_SAME_ACCESS: u32 = 0x0000_0002;

const BCRYPT_USE_SYSTEM_PREFERRED_RNG: u32 = 0x0000_0002;

const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const OPEN_EXISTING: u32 = 3;
const FILE_ATTRIBUTE_NORMAL: u32 = 0x0000_0080;
const SECURITY_SQOS_PRESENT: u32 = 0x0010_0000;
const SECURITY_IDENTIFICATION: u32 = 0x0001_0000;

const PIPE_ACCESS_DUPLEX: u32 = 0x0000_0003;
const FILE_FLAG_FIRST_PIPE_INSTANCE: u32 = 0x0008_0000;
const FILE_FLAG_OVERLAPPED: u32 = 0x4000_0000;
const PIPE_TYPE_BYTE: u32 = 0x0000_0000;
const PIPE_READMODE_BYTE: u32 = 0x0000_0000;
const PIPE_WAIT: u32 = 0x0000_0000;
const PIPE_REJECT_REMOTE_CLIENTS: u32 = 0x0000_0008;

const ERROR_PIPE_CONNECTED: u32 = 535;
const ERROR_IO_PENDING: u32 = 997;

const SECURITY_DESCRIPTOR_REVISION: u32 = 1;

const VK_XBUTTON1: i32 = 0x05;
const VK_XBUTTON2: i32 = 0x06;

const WIRE_MAGIC: [u8; 4] = *b"LCRB";

/// Wire v2 adds one authenticated session-selection frame after HELLO/ACK.
///
/// v1 immediately entered the observation stream after ACK. v2 deliberately
/// fails closed against that ambiguous shape: INPUT and MAINTENANCE are now
/// explicit before either Raw Input or maintenance payload handling begins.
const WIRE_VERSION: u16 = 2;

const FRAME_HELLO: u16 = 1;
const FRAME_ACK: u16 = 2;
const FRAME_OBSERVATION: u16 = 3;

const FRAME_SESSION_INPUT: u16 = 4;
const FRAME_SESSION_MAINTENANCE: u16 = 5;

const FRAME_MAINTENANCE_VERSION_LEN: u16 = 6;
const FRAME_MAINTENANCE_PAYLOAD_LEN: u16 = 7;

/// Transport receipt only. This does NOT mean a protected install or Task
/// migration succeeded; P2B4A3B3 owns the later wire result boundary.
const FRAME_MAINTENANCE_RECEIVED: u16 = 8;

/// Appended to wire v2 without renumbering the already-staged A2 receipt.
/// This frame precedes the payload body and carries only the bounded signed
/// manifest byte length; the manifest bytes immediately follow it.
const FRAME_MAINTENANCE_MANIFEST_LEN: u16 = 9;

/// Persistent Task authority is committed only after exact typed readback.
/// This acknowledgment is deliberately later and stronger than RECEIVED.
const FRAME_MAINTENANCE_COMMITTED: u16 = 10;

/// Append-only wire-v2 capability. Existing INPUT/MAINTENANCE byte meanings do
/// not change.
const FRAME_SESSION_UNINSTALL: u16 = 11;

/// The fixed privileged Task is already absent and the protected Broker
/// filesystem has been handed off for ordinary cleanup.
const FRAME_UNINSTALL_COMMITTED: u16 = 12;

const FRAME_LEN: usize = 48;
const OBSERVATION_FRAME_LEN: usize = 48;

/// Maintenance target SemVer is tiny metadata. Keep a hard protocol bound so
/// corrupted authenticated input cannot request unbounded allocation.
const MAINTENANCE_MAX_VERSION_BYTES: usize = 128;

/// Hard privileged-transport safety ceiling, not an expected Broker size.
/// Current Broker is far below this. Payload is streamed and never allocated
/// as one Vec by the elevated Broker.
const MAINTENANCE_MAX_PAYLOAD_BYTES: u64 = 64 * 1024 * 1024;

/// Match the existing Named Pipe buffer quantum and bound Broker stack memory.
const MAINTENANCE_CHUNK_BYTES: usize = 4096;

// The fixed Task has exactly six dynamic arguments. Maintenance reuses the
// parser-valid identity slots but never enters Raw Input, so the smallest valid
// VK plus zero lifecycle counters are deterministic and inert.
const MAINTENANCE_INERT_KEY_VK: u32 = 1;
const MAINTENANCE_INERT_AUTHORITY_EPOCH: u64 = 0;
const MAINTENANCE_INERT_KEY_REVISION: u64 = 0;

const WM_INPUT: u32 = 0x00FF;
const RID_INPUT: u32 = 0x1000_0003;
const RIM_INPUT: usize = 0;
const RIDEV_REMOVE: u32 = 0x0000_0001;
const RIDEV_INPUTSINK: u32 = 0x0000_0100;
const PM_REMOVE: u32 = 0x0000_0001;
const QS_RAWINPUT: u32 = 0x0000_0400;
const WAIT_TIMEOUT: u32 = 0x0000_0102;
const HWND_MESSAGE: Hwnd = -3isize;

const HID_USAGE_PAGE_GENERIC: u16 = 0x01;
const HID_USAGE_GENERIC_MOUSE: u16 = 0x02;
const HID_USAGE_GENERIC_KEYBOARD: u16 = 0x06;

const RIM_TYPEMOUSE: u32 = 0;
const RIM_TYPEKEYBOARD: u32 = 1;
const RI_MOUSE_BUTTON_4_DOWN: u16 = 0x0040;
const RI_MOUSE_BUTTON_4_UP: u16 = 0x0080;
const RI_MOUSE_BUTTON_5_DOWN: u16 = 0x0100;
const RI_MOUSE_BUTTON_5_UP: u16 = 0x0200;
const RI_KEY_BREAK: u16 = 0x0001;
const KEYBOARD_OVERRUN_MAKE_CODE: u16 = 0x00FF;

const BROKER_PENDING_CAPACITY: usize = 8;

/// Historical cadence quantum retained only to preserve the already-reviewed
/// 64 ms positive-DOWN refresh contract.
///
/// The production Broker is now Raw-Input/event driven and performs no 16 ms
/// idle physical-key polling.
pub const BROKER_POLL_INTERVAL_MS: u64 = 16;

/// Four historical cadence quanta define the locked 64 ms DOWN refresh period.
/// The Broker emits this refresh only while Raw Input has established DOWN.
pub const BROKER_DOWN_REFRESH_POLLS: u8 = 4;

pub const BROKER_DOWN_REFRESH_MS: u64 = BROKER_POLL_INTERVAL_MS * BROKER_DOWN_REFRESH_POLLS as u64;

/// A received DOWN older than this is not fresh authority.
///
/// 160 ms is greater than two measured worst-case refresh windows and therefore
/// tolerates ordinary local scheduling/IPC jitter without allowing an old frame
/// to become authoritative long after observation.
pub const BROKER_MAX_DOWN_AGE_MS: u64 = 160;

/// Maximum age of the latest positive DOWN proof before fail-closed quarantine.
///
/// 320 ms is roughly five measured refresh windows. This is local broker input
/// safety only; it is unrelated to the server Helper heartbeat/fail-close.
pub const BROKER_DOWN_LEASE_MS: u64 = 320;

const _: () = {
    assert!(BROKER_POLL_INTERVAL_MS == 16);
    assert!(BROKER_DOWN_REFRESH_POLLS == 4);
    assert!(BROKER_DOWN_REFRESH_MS == 64);
    assert!(BROKER_MAX_DOWN_AGE_MS < BROKER_DOWN_LEASE_MS);
    assert!(BROKER_DOWN_LEASE_MS == BROKER_MAX_DOWN_AGE_MS * 2);
    assert!(BROKER_PENDING_CAPACITY > 1);
    assert!(BROKER_PENDING_CAPACITY <= 16);
};

const TRANSPORT_STARTING: u8 = 0;
const TRANSPORT_AUTHENTICATED: u8 = 1;
const TRANSPORT_FAILED: u8 = 2;

#[repr(C)]
struct ShellExecuteInfoW {
    cb_size: u32,
    mask: u32,
    hwnd: Hwnd,
    verb: *const u16,
    file: *const u16,
    parameters: *const u16,
    directory: *const u16,
    show: i32,
    instance: Hinstance,
    id_list: *mut core::ffi::c_void,
    class: *const u16,
    class_key: Handle,
    hot_key: u32,
    icon_or_monitor: Handle,
    process: Handle,
}

#[repr(C)]
struct SecurityAttributes {
    length: u32,
    security_descriptor: *mut core::ffi::c_void,
    inherit_handle: i32,
}

#[repr(C)]
struct Overlapped {
    internal: usize,
    internal_high: usize,
    offset: u32,
    offset_high: u32,
    event: Handle,
}

#[repr(C)]
struct SidAndAttributes {
    sid: *mut core::ffi::c_void,
    attributes: u32,
}

#[repr(C)]
struct TokenUser {
    user: SidAndAttributes,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RawInputDevice {
    usage_page: u16,
    usage: u16,
    flags: u32,
    target: Hwnd,
}

#[repr(C)]
struct RawInputHeader {
    kind: u32,
    size: u32,
    _device: Handle,
    _wparam: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RawMouse {
    _flags: u16,
    _padding: u16,
    buttons: u32,
    _raw_buttons: u32,
    _last_x: i32,
    _last_y: i32,
    _extra_information: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RawKeyboard {
    _make_code: u16,
    flags: u16,
    _reserved: u16,
    vkey: u16,
    _message: u32,
    _extra_information: u32,
}

#[repr(C)]
union RawInputData {
    mouse: RawMouse,
    keyboard: RawKeyboard,
}

#[repr(C)]
struct RawInput {
    header: RawInputHeader,
    data: RawInputData,
}

#[repr(C)]
struct RawPoint {
    _x: i32,
    _y: i32,
}

#[repr(C)]
struct RawMessage {
    hwnd: Hwnd,
    message: u32,
    wparam: usize,
    lparam: isize,
    _time: u32,
    _point: RawPoint,
    _private: u32,
}

#[link(name = "user32")]
extern "system" {
    fn GetAsyncKeyState(vkey: i32) -> i16;
    fn GetForegroundWindow() -> Hwnd;
    fn GetWindowThreadProcessId(hwnd: Hwnd, process_id: *mut u32) -> u32;
    fn PostMessageW(hwnd: Hwnd, message: u32, wparam: usize, lparam: isize) -> i32;

    fn CreateWindowExW(
        extended_style: u32,
        class_name: *const u16,
        window_name: *const u16,
        style: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        parent: Hwnd,
        menu: isize,
        instance: Hinstance,
        parameter: *mut core::ffi::c_void,
    ) -> Hwnd;

    fn DestroyWindow(window: Hwnd) -> i32;

    fn RegisterRawInputDevices(
        devices: *const RawInputDevice,
        device_count: u32,
        device_size: u32,
    ) -> i32;

    fn GetRawInputData(
        raw_input: isize,
        command: u32,
        data: *mut core::ffi::c_void,
        size: *mut u32,
        header_size: u32,
    ) -> u32;

    fn PeekMessageW(
        message: *mut RawMessage,
        window: Hwnd,
        filter_min: u32,
        filter_max: u32,
        remove_message: u32,
    ) -> i32;

    fn MsgWaitForMultipleObjects(
        count: u32,
        handles: *const Handle,
        wait_all: i32,
        milliseconds: u32,
        wake_mask: u32,
    ) -> u32;

    fn DefWindowProcW(window: Hwnd, message: u32, wparam: usize, lparam: isize) -> isize;
}

#[link(name = "kernel32")]
extern "system" {
    fn CreateEventW(
        security_attributes: *const core::ffi::c_void,
        manual_reset: i32,
        initial_state: i32,
        name: *const u16,
    ) -> Handle;

    fn SetEvent(event: Handle) -> i32;
    fn ResetEvent(event: Handle) -> i32;
    fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32;
    fn GetExitCodeProcess(process: Handle, exit_code: *mut u32) -> i32;

    fn WaitForMultipleObjects(
        count: u32,
        handles: *const Handle,
        wait_all: i32,
        milliseconds: u32,
    ) -> u32;

    fn CloseHandle(handle: Handle) -> i32;

    fn OpenProcess(desired_access: u32, inherit_handle: i32, process_id: u32) -> Handle;

    fn GetCurrentProcess() -> Handle;
    fn GetCurrentProcessId() -> u32;
    fn GetProcessId(process: Handle) -> u32;
    fn GetTickCount64() -> u64;

    fn CancelIoEx(file: Handle, overlapped: *mut Overlapped) -> i32;

    fn GetOverlappedResult(
        file: Handle,
        overlapped: *mut Overlapped,
        bytes_transferred: *mut u32,
        wait: i32,
    ) -> i32;

    fn DuplicateHandle(
        source_process: Handle,
        source_handle: Handle,
        target_process: Handle,
        target_handle: *mut Handle,
        desired_access: u32,
        inherit_handle: i32,
        options: u32,
    ) -> i32;

    fn ProcessIdToSessionId(process_id: u32, session_id: *mut u32) -> i32;

    fn QueryFullProcessImageNameW(
        process: Handle,
        flags: u32,
        executable_name: *mut u16,
        size: *mut u32,
    ) -> i32;

    fn CreateNamedPipeW(
        name: *const u16,
        open_mode: u32,
        pipe_mode: u32,
        max_instances: u32,
        out_buffer_size: u32,
        in_buffer_size: u32,
        default_timeout: u32,
        security_attributes: *mut SecurityAttributes,
    ) -> Handle;

    fn ConnectNamedPipe(named_pipe: Handle, overlapped: *mut Overlapped) -> i32;

    fn DisconnectNamedPipe(named_pipe: Handle) -> i32;

    fn GetNamedPipeClientProcessId(pipe: Handle, client_process_id: *mut u32) -> i32;

    fn GetNamedPipeServerProcessId(pipe: Handle, server_process_id: *mut u32) -> i32;

    fn CreateFileW(
        file_name: *const u16,
        desired_access: u32,
        share_mode: u32,
        security_attributes: *const core::ffi::c_void,
        creation_disposition: u32,
        flags_and_attributes: u32,
        template_file: Handle,
    ) -> Handle;

    fn ReadFile(
        file: Handle,
        buffer: *mut core::ffi::c_void,
        bytes_to_read: u32,
        bytes_read: *mut u32,
        overlapped: *mut Overlapped,
    ) -> i32;

    fn WriteFile(
        file: Handle,
        buffer: *const core::ffi::c_void,
        bytes_to_write: u32,
        bytes_written: *mut u32,
        overlapped: *mut Overlapped,
    ) -> i32;

    fn GetLastError() -> u32;

    fn LocalFree(memory: Handle) -> Handle;
}

#[link(name = "advapi32")]
extern "system" {
    fn OpenProcessToken(process: Handle, desired_access: u32, token: *mut Handle) -> i32;

    fn GetTokenInformation(
        token: Handle,
        information_class: u32,
        information: *mut core::ffi::c_void,
        information_length: u32,
        return_length: *mut u32,
    ) -> i32;

    fn ConvertSidToStringSidW(sid: *mut core::ffi::c_void, string_sid: *mut *mut u16) -> i32;

    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        string_security_descriptor: *const u16,
        string_sd_revision: u32,
        security_descriptor: *mut *mut core::ffi::c_void,
        security_descriptor_size: *mut u32,
    ) -> i32;
}

#[link(name = "shell32")]
extern "system" {
    fn ShellExecuteExW(info: *mut ShellExecuteInfoW) -> i32;
}

#[link(name = "bcrypt")]
extern "system" {
    fn BCryptGenRandom(algorithm: Handle, buffer: *mut u8, buffer_length: u32, flags: u32) -> i32;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BrokerWireObservation {
    pub epoch: u64,
    pub key_revision: u64,
    pub sequence: u64,
    pub observed_tick: u64,
    pub level: Edge,
}

#[derive(Debug)]
struct BrokerSampler {
    down: bool,
    next_sequence: u64,
}

impl BrokerSampler {
    fn new() -> Self {
        Self {
            down: false,
            next_sequence: 1,
        }
    }

    fn observation(
        &mut self,
        level: Edge,
        observed_tick: u64,
        epoch: u64,
        key_revision: u64,
    ) -> Option<BrokerWireObservation> {
        if self.next_sequence == u64::MAX {
            // Sequence exhaustion is terminal for this broker session. Never
            // alias a previous observation sequence.
            return None;
        }

        let sequence = self.next_sequence;
        self.next_sequence += 1;

        Some(BrokerWireObservation {
            epoch,
            key_revision,
            sequence,
            observed_tick,
            level,
        })
    }

    fn initial_up(
        &mut self,
        observed_tick: u64,
        epoch: u64,
        key_revision: u64,
    ) -> Option<BrokerWireObservation> {
        self.observation(Edge::Up, observed_tick, epoch, key_revision)
    }

    fn edge(
        &mut self,
        level: Edge,
        observed_tick: u64,
        epoch: u64,
        key_revision: u64,
    ) -> Option<BrokerWireObservation> {
        let next_down = level == Edge::Down;

        if self.down == next_down {
            return None;
        }

        let observation = self.observation(level, observed_tick, epoch, key_revision)?;

        self.down = next_down;

        Some(observation)
    }

    fn refresh_down(
        &mut self,
        observed_tick: u64,
        epoch: u64,
        key_revision: u64,
    ) -> Option<BrokerWireObservation> {
        if !self.down {
            return None;
        }

        self.observation(Edge::Down, observed_tick, epoch, key_revision)
    }

    fn is_down(&self) -> bool {
        self.down
    }
}

fn encode_observation(observation: BrokerWireObservation) -> [u8; OBSERVATION_FRAME_LEN] {
    let mut frame = [0u8; OBSERVATION_FRAME_LEN];

    frame[0..4].copy_from_slice(&WIRE_MAGIC);
    frame[4..6].copy_from_slice(&WIRE_VERSION.to_le_bytes());
    frame[6..8].copy_from_slice(&FRAME_OBSERVATION.to_le_bytes());

    frame[8..16].copy_from_slice(&observation.epoch.to_le_bytes());
    frame[16..24].copy_from_slice(&observation.key_revision.to_le_bytes());
    frame[24..32].copy_from_slice(&observation.sequence.to_le_bytes());
    frame[32..40].copy_from_slice(&observation.observed_tick.to_le_bytes());

    frame[40] = match observation.level {
        Edge::Up => 0,
        Edge::Down => 1,
    };

    frame
}

fn decode_observation(frame: &[u8; OBSERVATION_FRAME_LEN]) -> Option<BrokerWireObservation> {
    if frame[0..4] != WIRE_MAGIC {
        return None;
    }

    if u16::from_le_bytes(frame[4..6].try_into().ok()?) != WIRE_VERSION {
        return None;
    }

    if u16::from_le_bytes(frame[6..8].try_into().ok()?) != FRAME_OBSERVATION {
        return None;
    }

    if frame[41..OBSERVATION_FRAME_LEN]
        .iter()
        .any(|byte| *byte != 0)
    {
        return None;
    }

    let level = match frame[40] {
        0 => Edge::Up,
        1 => Edge::Down,
        _ => return None,
    };

    Some(BrokerWireObservation {
        epoch: u64::from_le_bytes(frame[8..16].try_into().ok()?),
        key_revision: u64::from_le_bytes(frame[16..24].try_into().ok()?),
        sequence: u64::from_le_bytes(frame[24..32].try_into().ok()?),
        observed_tick: u64::from_le_bytes(frame[32..40].try_into().ok()?),
        level,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrokerPoll {
    Overflowed,
    Observation(BrokerWireObservation),
    Empty,
}

#[derive(Debug)]
struct PendingObservations {
    items: VecDeque<BrokerWireObservation>,
    overflowed: bool,
}

impl PendingObservations {
    fn new() -> Self {
        Self {
            items: VecDeque::with_capacity(BROKER_PENDING_CAPACITY),
            overflowed: false,
        }
    }

    fn push(&mut self, observation: BrokerWireObservation) -> bool {
        if let Some(back) = self.items.back_mut() {
            if back.level == observation.level {
                *back = observation;
                return false;
            }
        }

        if self.items.len() >= BROKER_PENDING_CAPACITY {
            self.overflowed = true;
            self.items.clear();
            self.items.push_back(observation);
            return true;
        }

        let needs_wake = self.items.is_empty();
        self.items.push_back(observation);
        needs_wake
    }

    fn poll(&mut self) -> BrokerPoll {
        if self.overflowed {
            self.overflowed = false;
            return BrokerPoll::Overflowed;
        }

        match self.items.pop_front() {
            Some(observation) => BrokerPoll::Observation(observation),
            None => BrokerPoll::Empty,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct HandshakeIdentity {
    parent_pid: u32,
    broker_pid: u32,
    session_id: u32,
    key_vk: u32,
    nonce: [u8; 16],
}

fn invalid_handle(handle: Handle) -> bool {
    handle.is_null() || handle == (-1isize as Handle)
}

fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(once(0)).collect()
}

unsafe fn wide_ptr_to_string(value: *const u16) -> Option<String> {
    if value.is_null() {
        return None;
    }

    let mut len = 0usize;
    while *value.add(len) != 0 {
        len = len.checked_add(1)?;
    }

    Some(String::from_utf16_lossy(std::slice::from_raw_parts(
        value, len,
    )))
}

fn key_to_vk(key: &RadioKey) -> Option<i32> {
    match key {
        RadioKey::Mouse4 => Some(VK_XBUTTON1),
        RadioKey::Mouse5 => Some(VK_XBUTTON2),
        RadioKey::Keyboard(code) if (1..=254).contains(code) => i32::try_from(*code).ok(),
        RadioKey::Keyboard(_) => None,
    }
}

fn random_nonce() -> Option<[u8; 16]> {
    let mut nonce = [0u8; 16];

    let status = unsafe {
        BCryptGenRandom(
            core::ptr::null_mut(),
            nonce.as_mut_ptr(),
            nonce.len() as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };

    (status == 0).then_some(nonce)
}

fn nonce_hex(nonce: &[u8; 16]) -> String {
    let mut token = String::with_capacity(32);

    for byte in nonce {
        let _ = write!(&mut token, "{byte:02x}");
    }

    token
}

fn canonical_broker_pipe_name(nonce: &[u8; 16]) -> OsString {
    let token = nonce_hex(nonce);
    OsString::from(format!(r"\\.\pipe\LCR.InputBroker.{token}"))
}

fn validated_broker_pipe_name<'a>(supplied: &'a OsStr, nonce: &[u8; 16]) -> Option<&'a OsStr> {
    let expected = canonical_broker_pipe_name(nonce);
    (supplied == expected.as_os_str()).then_some(supplied)
}

fn parse_nonce_hex(value: &OsStr) -> Option<[u8; 16]> {
    let value = value.to_string_lossy();

    if value.len() != 32 || !value.is_ascii() {
        return None;
    }

    let mut nonce = [0u8; 16];
    let bytes = value.as_bytes();

    for (index, slot) in nonce.iter_mut().enumerate() {
        let high = hex_nibble(bytes[index * 2])?;
        let low = hex_nibble(bytes[index * 2 + 1])?;
        *slot = (high << 4) | low;
    }

    Some(nonce)
}

fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

pub(crate) fn current_user_sid_string() -> Option<String> {
    unsafe {
        let process = GetCurrentProcess();

        let mut token: Handle = core::ptr::null_mut();

        if OpenProcessToken(process, TOKEN_QUERY, &mut token) == 0 || token.is_null() {
            return None;
        }

        let mut needed = 0u32;

        let _ = GetTokenInformation(token, TOKEN_USER, core::ptr::null_mut(), 0, &mut needed);

        if needed == 0 {
            let _ = CloseHandle(token);
            return None;
        }

        let mut buffer = vec![0u8; needed as usize];

        let ok = GetTokenInformation(
            token,
            TOKEN_USER,
            buffer.as_mut_ptr().cast(),
            needed,
            &mut needed,
        );

        let _ = CloseHandle(token);

        if ok == 0 {
            return None;
        }

        if buffer.len() < size_of::<TokenUser>() {
            return None;
        }

        // GetTokenInformation writes a byte buffer. Vec<u8> only guarantees
        // byte alignment, so dereferencing it directly as TokenUser would be
        // undefined behavior even though the Windows allocator is commonly
        // more aligned in practice.
        let token_user = core::ptr::read_unaligned(buffer.as_ptr().cast::<TokenUser>());

        if token_user.user.sid.is_null() {
            return None;
        }

        let mut sid_string: *mut u16 = core::ptr::null_mut();

        if ConvertSidToStringSidW(token_user.user.sid, &mut sid_string) == 0 || sid_string.is_null()
        {
            return None;
        }

        let result = wide_ptr_to_string(sid_string);

        let _ = LocalFree(sid_string.cast());

        result
    }
}

fn private_security_descriptor() -> Option<Handle> {
    let user_sid = current_user_sid_string()?;

    // Protected DACL: only LocalSystem and this exact user SID receive access.
    let sddl = format!("D:P(A;;GA;;;SY)(A;;GA;;;{user_sid})");
    let sddl = wide(OsStr::new(&sddl));

    unsafe {
        let mut descriptor: *mut core::ffi::c_void = core::ptr::null_mut();

        let ok = ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SECURITY_DESCRIPTOR_REVISION,
            &mut descriptor,
            core::ptr::null_mut(),
        );

        if ok == 0 || descriptor.is_null() {
            None
        } else {
            Some(descriptor)
        }
    }
}

fn create_private_pipe(name: &OsStr) -> Option<Handle> {
    let descriptor = private_security_descriptor()?;
    let name = wide(name);

    unsafe {
        let mut attributes = SecurityAttributes {
            length: size_of::<SecurityAttributes>() as u32,
            security_descriptor: descriptor,
            inherit_handle: 0,
        };

        let pipe = CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            4096,
            4096,
            0,
            &mut attributes,
        );

        let _ = LocalFree(descriptor);

        if invalid_handle(pipe) {
            None
        } else {
            Some(pipe)
        }
    }
}

fn open_pipe(name: &OsStr) -> Option<Handle> {
    let name = wide(name);

    unsafe {
        let pipe = CreateFileW(
            name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            core::ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL
                | FILE_FLAG_OVERLAPPED
                | SECURITY_SQOS_PRESENT
                | SECURITY_IDENTIFICATION,
            core::ptr::null_mut(),
        );

        if invalid_handle(pipe) {
            None
        } else {
            Some(pipe)
        }
    }
}

fn process_session_id(process_id: u32) -> Option<u32> {
    unsafe {
        let mut session_id = 0u32;

        if ProcessIdToSessionId(process_id, &mut session_id) == 0 {
            None
        } else {
            Some(session_id)
        }
    }
}

fn same_session(left: u32, right: u32) -> bool {
    match (process_session_id(left), process_session_id(right)) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

fn query_process_image(process: Handle) -> Option<PathBuf> {
    let mut buffer = vec![0u16; 32_768];
    let mut length = buffer.len() as u32;

    let ok = unsafe { QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut length) };

    if ok == 0 || length == 0 {
        return None;
    }

    buffer.truncate(length as usize);

    Some(PathBuf::from(OsString::from_wide(&buffer)))
}

fn normalized_path_text(path: &Path) -> String {
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    let text = path.to_string_lossy();

    text.strip_prefix(r"\\?\").unwrap_or(&text).to_string()
}

fn process_image_matches(process: Handle, expected: &Path) -> bool {
    let Some(actual) = query_process_image(process) else {
        return false;
    };

    normalized_path_text(&actual).eq_ignore_ascii_case(&normalized_path_text(expected))
}

#[repr(C)]
#[derive(Clone, Copy)]
struct BrokerKnownFolderId {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

const BROKER_FOLDERID_LOCAL_APP_DATA: BrokerKnownFolderId = BrokerKnownFolderId {
    data1: 0xF1B3_2785,
    data2: 0x6FBA,
    data3: 0x4FCF,
    data4: [0x9D, 0x55, 0x7B, 0x8E, 0x7F, 0x15, 0x70, 0x91],
};

type BrokerHResult = i32;

const BROKER_COINIT_MULTITHREADED: u32 = 0;
const BROKER_COINIT_DISABLE_OLE1DDE: u32 = 0x0000_0004;
const BROKER_RPC_E_CHANGED_MODE: BrokerHResult = 0x8001_0106u32 as i32;

#[link(name = "ole32")]
extern "system" {
    fn CoInitializeEx(reserved: *mut core::ffi::c_void, coinit: u32) -> BrokerHResult;
    fn CoUninitialize();
    fn CoTaskMemFree(memory: *mut core::ffi::c_void);
}

#[link(name = "shell32")]
extern "system" {
    fn SHGetKnownFolderPath(
        folder_id: *const BrokerKnownFolderId,
        flags: u32,
        token: Handle,
        path: *mut *mut u16,
    ) -> BrokerHResult;
}

struct BrokerComInitialization {
    should_uninitialize: bool,
}

impl BrokerComInitialization {
    fn current_thread() -> io::Result<Self> {
        let result = unsafe {
            CoInitializeEx(
                core::ptr::null_mut(),
                BROKER_COINIT_MULTITHREADED | BROKER_COINIT_DISABLE_OLE1DDE,
            )
        };

        if result >= 0 {
            Ok(Self {
                should_uninitialize: true,
            })
        } else if result == BROKER_RPC_E_CHANGED_MODE {
            Ok(Self {
                should_uninitialize: false,
            })
        } else {
            Err(io::Error::other(format!(
                "CoInitializeEx failed with HRESULT 0x{:08X}",
                result as u32
            )))
        }
    }
}

impl Drop for BrokerComInitialization {
    fn drop(&mut self) {
        if self.should_uninitialize {
            unsafe {
                CoUninitialize();
            }
        }
    }
}

struct BrokerCoTaskMemWide(*mut u16);

impl BrokerCoTaskMemWide {
    fn to_path_buf(&self) -> io::Result<PathBuf> {
        if self.0.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "SHGetKnownFolderPath returned a null path",
            ));
        }

        let mut len = 0usize;

        while unsafe { *self.0.add(len) } != 0 {
            len = len.checked_add(1).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "known-folder path length overflow",
                )
            })?;
        }

        let wide = unsafe { std::slice::from_raw_parts(self.0, len) };

        Ok(PathBuf::from(OsString::from_wide(wide)))
    }
}

impl Drop for BrokerCoTaskMemWide {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                CoTaskMemFree(self.0.cast());
            }
        }
    }
}

fn current_local_app_data_known_folder() -> io::Result<PathBuf> {
    let _com = BrokerComInitialization::current_thread()?;
    let mut raw = core::ptr::null_mut();

    let result = unsafe {
        SHGetKnownFolderPath(
            &BROKER_FOLDERID_LOCAL_APP_DATA,
            0,
            core::ptr::null_mut(),
            &mut raw,
        )
    };

    let raw = BrokerCoTaskMemWide(raw);

    if result < 0 {
        return Err(io::Error::other(format!(
            "SHGetKnownFolderPath failed with HRESULT 0x{:08X}",
            result as u32
        )));
    }

    raw.to_path_buf()
}

fn canonical_helper_path_from_local_app_data(local_app_data: &Path) -> PathBuf {
    distribution::install_path(local_app_data)
}

fn canonical_installed_helper_path() -> io::Result<PathBuf> {
    let local_app_data = current_local_app_data_known_folder()?;
    Ok(canonical_helper_path_from_local_app_data(&local_app_data))
}
/// Runs the exact executable through the existing Windows `runas` seam,
/// waits for that launched process, and returns its exit code.
///
/// Install-time utility only. Broker PTT transport, sampling, IPC and timing
/// semantics are unchanged.
pub(crate) fn runas_and_wait(executable: &Path, parameters: &OsStr) -> io::Result<u32> {
    let executable_wide = wide(executable.as_os_str());
    let verb = wide(OsStr::new("runas"));
    let parameters_wide = wide(parameters);

    let mut info: ShellExecuteInfoW = unsafe { core::mem::zeroed() };

    info.cb_size = size_of::<ShellExecuteInfoW>() as u32;
    info.mask = SEE_MASK_NOCLOSEPROCESS;
    info.hwnd = 0;
    info.verb = verb.as_ptr();
    info.file = executable_wide.as_ptr();
    info.parameters = parameters_wide.as_ptr();
    info.show = SW_HIDE;

    let launched = unsafe { ShellExecuteExW(&mut info) };

    if launched == 0 || info.process.is_null() || info.process as usize == usize::MAX {
        return Err(io::Error::last_os_error());
    }

    let process = info.process;

    let result = (|| -> io::Result<u32> {
        let child_pid = unsafe { GetProcessId(process) };

        if child_pid == 0
            || !same_session(unsafe { GetCurrentProcessId() }, child_pid)
            || !process_image_matches(process, executable)
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "elevated process identity does not match requested Helper",
            ));
        }

        match unsafe { WaitForSingleObject(process, INFINITE) } {
            WAIT_OBJECT_0 => {}

            WAIT_FAILED => {
                return Err(io::Error::last_os_error());
            }

            unexpected => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "unexpected elevated process wait result: \
                         {unexpected:#010x}"
                    ),
                ));
            }
        }

        let mut exit_code = 0u32;

        if unsafe { GetExitCodeProcess(process, &mut exit_code) } == 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(exit_code)
    })();

    unsafe {
        let _ = CloseHandle(process);
    }

    result
}
fn encode_frame(kind: u16, identity: HandshakeIdentity) -> [u8; FRAME_LEN] {
    let mut frame = [0u8; FRAME_LEN];

    frame[0..4].copy_from_slice(&WIRE_MAGIC);
    frame[4..6].copy_from_slice(&WIRE_VERSION.to_le_bytes());
    frame[6..8].copy_from_slice(&kind.to_le_bytes());
    frame[8..12].copy_from_slice(&identity.parent_pid.to_le_bytes());
    frame[12..16].copy_from_slice(&identity.broker_pid.to_le_bytes());
    frame[16..20].copy_from_slice(&identity.session_id.to_le_bytes());
    frame[20..24].copy_from_slice(&identity.key_vk.to_le_bytes());
    frame[24..40].copy_from_slice(&identity.nonce);

    frame
}

fn decode_frame(frame: &[u8; FRAME_LEN]) -> Option<(u16, HandshakeIdentity)> {
    if frame[0..4] != WIRE_MAGIC {
        return None;
    }

    let version = u16::from_le_bytes(frame[4..6].try_into().ok()?);

    if version != WIRE_VERSION {
        return None;
    }

    if frame[40..48].iter().any(|byte| *byte != 0) {
        return None;
    }

    let kind = u16::from_le_bytes(frame[6..8].try_into().ok()?);
    let parent_pid = u32::from_le_bytes(frame[8..12].try_into().ok()?);
    let broker_pid = u32::from_le_bytes(frame[12..16].try_into().ok()?);
    let session_id = u32::from_le_bytes(frame[16..20].try_into().ok()?);
    let key_vk = u32::from_le_bytes(frame[20..24].try_into().ok()?);

    let mut nonce = [0u8; 16];
    nonce.copy_from_slice(&frame[24..40]);

    Some((
        kind,
        HandshakeIdentity {
            parent_pid,
            broker_pid,
            session_id,
            key_vk,
            nonce,
        },
    ))
}

fn frame_matches(frame: &[u8; FRAME_LEN], expected_kind: u16, expected: HandshakeIdentity) -> bool {
    matches!(
        decode_frame(frame),
        Some((kind, identity))
            if kind == expected_kind && identity == expected
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BrokerSessionMode {
    Input,
    Maintenance,
    Uninstall,
}

fn encode_session_mode(mode: BrokerSessionMode, identity: HandshakeIdentity) -> [u8; FRAME_LEN] {
    let kind = match mode {
        BrokerSessionMode::Input => FRAME_SESSION_INPUT,
        BrokerSessionMode::Maintenance => FRAME_SESSION_MAINTENANCE,
        BrokerSessionMode::Uninstall => FRAME_SESSION_UNINSTALL,
    };

    encode_frame(kind, identity)
}

fn decode_session_mode(
    frame: &[u8; FRAME_LEN],
    expected: HandshakeIdentity,
) -> Option<BrokerSessionMode> {
    if frame_matches(frame, FRAME_SESSION_INPUT, expected) {
        Some(BrokerSessionMode::Input)
    } else if frame_matches(frame, FRAME_SESSION_MAINTENANCE, expected) {
        Some(BrokerSessionMode::Maintenance)
    } else if frame_matches(frame, FRAME_SESSION_UNINSTALL, expected) {
        Some(BrokerSessionMode::Uninstall)
    } else {
        None
    }
}

/// Control frames reuse the authenticated identity projection while allowing
/// the final 8 bytes, reserved by HELLO/ACK/session frames, to carry one u64.
///
/// Callers must still check the exact expected frame kind.
fn encode_control_frame(kind: u16, identity: HandshakeIdentity, value: u64) -> [u8; FRAME_LEN] {
    let mut frame = encode_frame(kind, identity);
    frame[40..48].copy_from_slice(&value.to_le_bytes());
    frame
}

fn decode_control_frame(frame: &[u8; FRAME_LEN]) -> Option<(u16, HandshakeIdentity, u64)> {
    if frame[0..4] != WIRE_MAGIC {
        return None;
    }

    if u16::from_le_bytes(frame[4..6].try_into().ok()?) != WIRE_VERSION {
        return None;
    }

    let kind = u16::from_le_bytes(frame[6..8].try_into().ok()?);

    let parent_pid = u32::from_le_bytes(frame[8..12].try_into().ok()?);

    let broker_pid = u32::from_le_bytes(frame[12..16].try_into().ok()?);

    let session_id = u32::from_le_bytes(frame[16..20].try_into().ok()?);

    let key_vk = u32::from_le_bytes(frame[20..24].try_into().ok()?);

    let mut nonce = [0u8; 16];
    nonce.copy_from_slice(&frame[24..40]);

    let value = u64::from_le_bytes(frame[40..48].try_into().ok()?);

    Some((
        kind,
        HandshakeIdentity {
            parent_pid,
            broker_pid,
            session_id,
            key_vk,
            nonce,
        },
        value,
    ))
}

fn maintenance_manifest_length_valid(length: u64) -> bool {
    (1..=BROKER_SIGNED_MANIFEST_MAX_BYTES as u64).contains(&length)
}

fn maintenance_version_length_valid(length: u64) -> bool {
    (1..=MAINTENANCE_MAX_VERSION_BYTES as u64).contains(&length)
}

fn maintenance_payload_length_valid(length: u64) -> bool {
    (1..=MAINTENANCE_MAX_PAYLOAD_BYTES).contains(&length)
}

fn maintenance_transport_lengths(
    target_version: &str,
    signed_manifest: &[u8],
    payload: &[u8],
) -> Option<(u64, u64, u64)> {
    let version_length = u64::try_from(target_version.len()).ok()?;
    let payload_length = u64::try_from(payload.len()).ok()?;
    let manifest_length = u64::try_from(signed_manifest.len()).ok()?;

    (maintenance_version_length_valid(version_length)
        && maintenance_payload_length_valid(payload_length)
        && maintenance_manifest_length_valid(manifest_length))
    .then_some((version_length, payload_length, manifest_length))
}

/// Exact publisher-authenticated identity bound to the outer maintenance
/// framing before any payload byte or protected staging operation is accepted.
#[derive(Clone, Debug, Eq, PartialEq)]
struct AuthenticatedMaintenanceMetadata {
    target_version: String,
    payload_length: u64,
    payload_sha256: [u8; 32],
}

impl From<VerifiedBrokerArtifact> for AuthenticatedMaintenanceMetadata {
    fn from(artifact: VerifiedBrokerArtifact) -> Self {
        Self {
            target_version: artifact.target_version().to_owned(),
            payload_length: artifact.payload_length(),
            payload_sha256: *artifact.payload_sha256(),
        }
    }
}

fn authenticate_current_maintenance_manifest(
    manifest: &[u8],
) -> Option<AuthenticatedMaintenanceMetadata> {
    verify_current_broker_manifest(manifest)
        .ok()
        .map(AuthenticatedMaintenanceMetadata::from)
}

fn maintenance_metadata_matches_outer(
    metadata: &AuthenticatedMaintenanceMetadata,
    target_version: &str,
    payload_length: u64,
) -> bool {
    metadata.target_version == target_version && metadata.payload_length == payload_length
}

fn executable_length_and_sha256(path: &Path, maximum_length: u64) -> io::Result<(u64, [u8; 32])> {
    let mut file = File::open(path)?;
    let mut observed_length = 0u64;
    let mut hasher = Sha256::new();
    let mut chunk = [0u8; MAINTENANCE_CHUNK_BYTES];

    loop {
        let read = file.read(&mut chunk)?;
        if read == 0 {
            break;
        }

        observed_length = observed_length
            .checked_add(read as u64)
            .ok_or_else(|| io::Error::other("Broker executable length overflow"))?;
        if observed_length > maximum_length {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Broker executable exceeds signed length during readback",
            ));
        }
        hasher.update(&chunk[..read]);
    }

    Ok((observed_length, hasher.finalize().into()))
}

fn executable_identity_matches_with_version_reader<F>(
    path: &Path,
    expected_length: u64,
    expected_sha256: &[u8; 32],
    expected_version: &str,
    version_reader: F,
) -> io::Result<bool>
where
    F: FnOnce(&Path) -> io::Result<String>,
{
    if fs::symlink_metadata(path)?.len() != expected_length {
        return Ok(false);
    }

    let (observed_length, observed_sha256) = executable_length_and_sha256(path, expected_length)?;

    if observed_length != expected_length || &observed_sha256 != expected_sha256 {
        return Ok(false);
    }

    Ok(version_reader(path)? == expected_version)
}

fn executable_identity_matches(
    path: &Path,
    metadata: &AuthenticatedMaintenanceMetadata,
) -> io::Result<bool> {
    executable_identity_matches_with_version_reader(
        path,
        metadata.payload_length,
        &metadata.payload_sha256,
        &metadata.target_version,
        executable_product_version,
    )
}

fn protected_broker_target_is_exact(
    user_sid: &str,
    target: &ProtectedBrokerPath,
    metadata: &AuthenticatedMaintenanceMetadata,
) -> io::Result<bool> {
    if !protected_broker_filesystem_security_matches(user_sid, target)? {
        return Ok(false);
    }

    executable_identity_matches(target.as_path(), metadata)
}

/// Read-only startup seam for proving one typed protected Broker target against
/// an already publisher-authenticated signed artifact.
///
/// This deliberately reuses the A3B2 executable/security validator. The caller
/// cannot supply an arbitrary path, hash, ProductVersion, or trust key.
pub(crate) fn verified_protected_broker_target_is_exact(
    user_sid: &str,
    target: &ProtectedBrokerPath,
    artifact: &VerifiedBrokerArtifact,
) -> io::Result<bool> {
    let metadata = AuthenticatedMaintenanceMetadata {
        target_version: artifact.target_version().to_owned(),
        payload_length: artifact.payload_length(),
        payload_sha256: *artifact.payload_sha256(),
    };

    protected_broker_target_is_exact(user_sid, target, &metadata)
}

struct ObservedMaintenancePayload {
    expected_length: u64,
    expected_sha256: [u8; 32],
    observed_length: u64,
    hasher: Sha256,
}

impl ObservedMaintenancePayload {
    fn new(metadata: &AuthenticatedMaintenanceMetadata) -> Self {
        Self {
            expected_length: metadata.payload_length,
            expected_sha256: metadata.payload_sha256,
            observed_length: 0,
            hasher: Sha256::new(),
        }
    }

    fn observe(&mut self, chunk: &[u8]) -> io::Result<()> {
        let observed_length = self
            .observed_length
            .checked_add(chunk.len() as u64)
            .ok_or_else(|| io::Error::other("Broker maintenance payload length overflow"))?;

        if observed_length > self.expected_length {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Broker maintenance payload exceeds signed length",
            ));
        }

        self.observed_length = observed_length;
        self.hasher.update(chunk);
        Ok(())
    }

    fn finish(self) -> io::Result<()> {
        let observed_sha256: [u8; 32] = self.hasher.finalize().into();

        if self.observed_length != self.expected_length || observed_sha256 != self.expected_sha256 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Broker maintenance payload differs from signed identity",
            ));
        }

        Ok(())
    }
}

enum BrokerMaintenanceStageMode {
    ExistingExact,
    StreamNew(StreamingExecutableInstall),
}

struct AuthenticatedBrokerStaging {
    user_sid: String,
    target: ProtectedBrokerPath,
    metadata: AuthenticatedMaintenanceMetadata,
    observed: ObservedMaintenancePayload,
    mode: BrokerMaintenanceStageMode,
}

struct VerifiedProtectedBrokerTarget {
    user_sid: String,
    target: ProtectedBrokerPath,
    payload_length: u64,
}

impl AuthenticatedBrokerStaging {
    fn prepare(metadata: &AuthenticatedMaintenanceMetadata) -> io::Result<Self> {
        let user_sid = current_user_sid_string()
            .ok_or_else(|| io::Error::other("cannot resolve current Windows user SID"))?;
        let target = protected_broker_path_for_version(&metadata.target_version)?;

        establish_protected_broker_update_directory(&user_sid, &target)?;

        let mode = match fs::symlink_metadata(target.as_path()) {
            Ok(_) => {
                if !protected_broker_target_is_exact(&user_sid, &target, metadata)? {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "existing protected Broker target differs from signed identity",
                    ));
                }

                BrokerMaintenanceStageMode::ExistingExact
            }

            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                BrokerMaintenanceStageMode::StreamNew(prepare_streaming_executable_install(
                    target.as_path(),
                )?)
            }

            Err(error) => return Err(error),
        };

        Ok(Self {
            user_sid,
            target,
            metadata: metadata.clone(),
            observed: ObservedMaintenancePayload::new(metadata),
            mode,
        })
    }

    fn receive_chunk(&mut self, chunk: &[u8]) -> io::Result<()> {
        self.observed.observe(chunk)?;

        match &mut self.mode {
            BrokerMaintenanceStageMode::ExistingExact => Ok(()),
            BrokerMaintenanceStageMode::StreamNew(streaming) => streaming.write_chunk(chunk),
        }
    }

    fn finish(self) -> io::Result<VerifiedProtectedBrokerTarget> {
        let Self {
            user_sid,
            target,
            metadata,
            observed,
            mode,
        } = self;

        observed.finish()?;

        match mode {
            BrokerMaintenanceStageMode::ExistingExact => {
                if !protected_broker_target_is_exact(&user_sid, &target, &metadata)? {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "existing protected Broker target changed during maintenance receipt",
                    ));
                }
            }

            BrokerMaintenanceStageMode::StreamNew(streaming) => {
                let prepared = streaming.finish()?;

                if !executable_identity_matches(prepared.path(), &metadata)? {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "prepared Broker candidate differs from signed executable identity",
                    ));
                }

                protect_and_verify_broker_candidate(&user_sid, &target, prepared.path())?;
                prepared.commit_new()?;

                if !protected_broker_target_is_exact(&user_sid, &target, &metadata)? {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "committed protected Broker target failed final validation",
                    ));
                }
            }
        }

        Ok(VerifiedProtectedBrokerTarget {
            user_sid,
            target,
            payload_length: metadata.payload_length,
        })
    }
}

enum OldBrokerTaskAuthority {
    Exact(BrokerTaskContract),
    Absent,
    Mismatch,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BrokerTaskMigrationOutcome {
    Committed,
    FailedBeforeMutation,
    FailedWithExactRollback,
    FailedWithUnprovenRollback,
}

fn migrate_broker_task_with<R, G, V>(
    user_sid: &str,
    target: &ProtectedBrokerPath,
    recognize_old: R,
    mut register: G,
    mut verify: V,
) -> BrokerTaskMigrationOutcome
where
    R: FnOnce(&str) -> Result<OldBrokerTaskAuthority, ()>,
    G: FnMut(&BrokerTaskContract) -> Result<(), ()>,
    V: FnMut(&BrokerTaskContract) -> Result<BrokerTaskVerification, ()>,
{
    let old_contract = match recognize_old(user_sid) {
        Ok(OldBrokerTaskAuthority::Exact(contract)) => contract,
        Ok(OldBrokerTaskAuthority::Absent | OldBrokerTaskAuthority::Mismatch) | Err(()) => {
            return BrokerTaskMigrationOutcome::FailedBeforeMutation;
        }
    };

    let Ok(new_contract) = BrokerTaskContract::new(user_sid, target) else {
        return BrokerTaskMigrationOutcome::FailedBeforeMutation;
    };

    let new_exact = register(&new_contract).is_ok()
        && matches!(verify(&new_contract), Ok(BrokerTaskVerification::Exact));

    if new_exact {
        return BrokerTaskMigrationOutcome::Committed;
    }

    // Registration of the new contract was attempted, so every failure from
    // that point must attempt to restore and exactly read back the old one.
    if register(&old_contract).is_err() {
        return BrokerTaskMigrationOutcome::FailedWithUnprovenRollback;
    }

    if matches!(verify(&old_contract), Ok(BrokerTaskVerification::Exact)) {
        BrokerTaskMigrationOutcome::FailedWithExactRollback
    } else {
        BrokerTaskMigrationOutcome::FailedWithUnprovenRollback
    }
}

fn migrate_broker_task(target: &VerifiedProtectedBrokerTarget) -> bool {
    matches!(
        migrate_broker_task_with(
            &target.user_sid,
            &target.target,
            |user_sid| {
                recognize_broker_task_via_scheduler(user_sid)
                    .map(|verification| match verification {
                        RecognizedBrokerTaskVerification::Exact(authority) => {
                            OldBrokerTaskAuthority::Exact(authority.contract().clone())
                        }
                        RecognizedBrokerTaskVerification::Absent => OldBrokerTaskAuthority::Absent,
                        RecognizedBrokerTaskVerification::Mismatch => {
                            OldBrokerTaskAuthority::Mismatch
                        }
                    })
                    .map_err(|_| ())
            },
            |contract| register_or_update_broker_task_via_scheduler(contract).map_err(|_| ()),
            |contract| verify_broker_task_via_scheduler(contract).map_err(|_| ()),
        ),
        BrokerTaskMigrationOutcome::Committed
    )
}

fn complete_broker_maintenance_with<F, M, W>(
    identity: HandshakeIdentity,
    finish_staging: F,
    migrate_task: M,
    write_committed: W,
) -> bool
where
    F: FnOnce() -> io::Result<VerifiedProtectedBrokerTarget>,
    M: FnOnce(&VerifiedProtectedBrokerTarget) -> bool,
    W: FnOnce(&[u8; FRAME_LEN]) -> bool,
{
    let Ok(target) = finish_staging() else {
        return false;
    };

    if !migrate_task(&target) {
        return false;
    }

    let committed =
        encode_control_frame(FRAME_MAINTENANCE_COMMITTED, identity, target.payload_length);

    write_committed(&committed)
}

fn broker_receive_maintenance_transport_with_sink_and_verifier<F, V, P, S>(
    pipe: &mut BrokerPipeIo,
    identity: HandshakeIdentity,
    mut verify_manifest: V,
    prepare: P,
    mut sink: F,
) -> Option<(AuthenticatedMaintenanceMetadata, S)>
where
    F: FnMut(&mut S, &[u8]) -> bool,
    V: FnMut(&[u8]) -> Option<AuthenticatedMaintenanceMetadata>,
    P: FnOnce(&AuthenticatedMaintenanceMetadata) -> Option<S>,
{
    let mut version_len_frame = [0u8; FRAME_LEN];

    if !pipe.read(&mut version_len_frame) {
        return None;
    }

    let (version_kind, version_identity, version_length) =
        decode_control_frame(&version_len_frame)?;

    if version_kind != FRAME_MAINTENANCE_VERSION_LEN
        || version_identity != identity
        || !maintenance_version_length_valid(version_length)
    {
        return None;
    }

    let version_length = usize::try_from(version_length).ok()?;
    let mut version_bytes = vec![0u8; version_length];

    if !pipe.read(&mut version_bytes) {
        return None;
    }

    let target_version = String::from_utf8(version_bytes).ok()?;

    let mut payload_len_frame = [0u8; FRAME_LEN];

    if !pipe.read(&mut payload_len_frame) {
        return None;
    }

    let (payload_kind, payload_identity, payload_length) =
        decode_control_frame(&payload_len_frame)?;

    if payload_kind != FRAME_MAINTENANCE_PAYLOAD_LEN
        || payload_identity != identity
        || !maintenance_payload_length_valid(payload_length)
    {
        return None;
    }

    let mut manifest_len_frame = [0u8; FRAME_LEN];

    if !pipe.read(&mut manifest_len_frame) {
        return None;
    }

    let (manifest_kind, manifest_identity, manifest_length) =
        decode_control_frame(&manifest_len_frame)?;

    if manifest_kind != FRAME_MAINTENANCE_MANIFEST_LEN
        || manifest_identity != identity
        || !maintenance_manifest_length_valid(manifest_length)
    {
        return None;
    }

    let manifest_length = usize::try_from(manifest_length).ok()?;
    let mut manifest_bytes = vec![0u8; manifest_length];

    if !pipe.read(&mut manifest_bytes) {
        return None;
    }

    // Publisher authentication happens before the first payload byte is read.
    let authenticated = verify_manifest(&manifest_bytes)?;

    if !maintenance_metadata_matches_outer(&authenticated, &target_version, payload_length) {
        return None;
    }

    // Protected staging preparation runs only after publisher authentication
    // and outer binding, and must complete before the first payload read.
    let mut staging = prepare(&authenticated)?;

    let mut remaining = payload_length;
    let mut chunk = [0u8; MAINTENANCE_CHUNK_BYTES];

    while remaining != 0 {
        let take = usize::try_from(remaining.min(MAINTENANCE_CHUNK_BYTES as u64)).ok()?;

        if !pipe.read(&mut chunk[..take]) {
            return None;
        }

        if !sink(&mut staging, &chunk[..take]) {
            return None;
        }

        remaining -= take as u64;
    }

    let receipt = encode_control_frame(FRAME_MAINTENANCE_RECEIVED, identity, payload_length);

    if !pipe.write(&receipt) {
        return None;
    }

    Some((authenticated, staging))
}

#[cfg(test)]
fn broker_receive_maintenance_transport_with_sink<F>(
    pipe: &mut BrokerPipeIo,
    identity: HandshakeIdentity,
    mut sink: F,
) -> Option<(String, u64)>
where
    F: FnMut(&[u8]) -> bool,
{
    let (authenticated, ()) = broker_receive_maintenance_transport_with_sink_and_verifier(
        pipe,
        identity,
        authenticate_current_maintenance_manifest,
        |_| Some(()),
        |_, chunk| sink(chunk),
    )?;

    // The transport-only regression seam intentionally discards the digest;
    // production A3B2 staging compares it incrementally before finalization.
    let _authenticated_payload_sha256 = authenticated.payload_sha256;

    Some((authenticated.target_version, authenticated.payload_length))
}

fn broker_receive_maintenance_transport(
    pipe: &mut BrokerPipeIo,
    identity: HandshakeIdentity,
) -> bool {
    let Some((_, staging)) = broker_receive_maintenance_transport_with_sink_and_verifier(
        pipe,
        identity,
        authenticate_current_maintenance_manifest,
        |metadata| AuthenticatedBrokerStaging::prepare(metadata).ok(),
        |staging, chunk| staging.receive_chunk(chunk).is_ok(),
    ) else {
        return false;
    };

    // RECEIVED has already been sent. Persistent authority becomes committed
    // only after final staging validation, fixed-Task migration, and exact
    // typed readback. A failed COMMITTED write does not undo that authority.
    complete_broker_maintenance_with(
        identity,
        || staging.finish(),
        migrate_broker_task,
        |committed| pipe.write(committed),
    )
}

/// Monotonic privileged uninstall transaction.
///
/// The fixed Task deletion is the irreversible security commit barrier. No
/// filesystem ACL is relaxed unless Task deletion AND absence readback succeed.
/// Once the Task is absent, later failure never recreates it.
fn complete_broker_uninstall_with<P, D, H, W>(
    identity: HandshakeIdentity,
    preflight: P,
    delete_task: D,
    establish_handoff: H,
    write_committed: W,
) -> bool
where
    P: FnOnce() -> io::Result<()>,
    D: FnOnce() -> io::Result<()>,
    H: FnOnce() -> io::Result<()>,
    W: FnOnce(&[u8; FRAME_LEN]) -> bool,
{
    if preflight().is_err() {
        return false;
    }

    if delete_task().is_err() {
        return false;
    }

    if establish_handoff().is_err() {
        return false;
    }

    let committed = encode_control_frame(FRAME_UNINSTALL_COMMITTED, identity, 0);
    write_committed(&committed)
}

fn uninstall_task_contract_matches(
    contract: Option<&BrokerTaskContract>,
    user_sid: &str,
    target: &ProtectedBrokerPath,
) -> bool {
    let Some(contract) = contract else {
        return false;
    };

    contract.user_sid() == user_sid && contract.broker_path() == target.as_path()
}

fn broker_receive_uninstall_transport(
    pipe: &mut BrokerPipeIo,
    identity: HandshakeIdentity,
) -> bool {
    let Some(user_sid) = current_user_sid_string() else {
        return false;
    };

    let Ok(target) = current_protected_broker_path() else {
        return false;
    };

    complete_broker_uninstall_with(
        identity,
        || {
            // Re-read the fixed Task immediately before the destructive commit.
            // Parent-side recognition is not sufficient for an elevated
            // destructive operation: the Broker itself must prove that the
            // persistent Task still binds this exact user + protected target.
            let verification = recognize_broker_task_via_scheduler(&user_sid).map_err(|error| {
                io::Error::other(format!(
                    "fixed Broker Task uninstall preflight failed: {error}"
                ))
            })?;

            let exact_contract = match &verification {
                RecognizedBrokerTaskVerification::Exact(authority) => Some(authority.contract()),
                RecognizedBrokerTaskVerification::Absent
                | RecognizedBrokerTaskVerification::Mismatch => None,
            };

            if !uninstall_task_contract_matches(exact_contract, &user_sid, &target) {
                return Err(io::Error::other(
                    "fixed Broker Task does not exactly bind uninstall Broker authority",
                ));
            }

            // Also prove the protected executable authority and the complete
            // InputBroker subtree shape before destroying Task authority.
            if !protected_broker_filesystem_security_matches(&user_sid, &target)? {
                return Err(io::Error::other(
                    "Broker uninstall preflight found non-exact protected authority",
                ));
            }

            validate_uninstall_handoff_filesystem_shape(&target)
        },
        || {
            delete_broker_task_and_verify_absent_via_scheduler().map_err(|error| {
                io::Error::other(format!(
                    "fixed Broker Task uninstall deletion/readback failed: {error}"
                ))
            })
        },
        || establish_uninstall_handoff_filesystem_security(&user_sid, &target),
        |committed| pipe.write(committed),
    )
}

// The client must also use overlapped I/O. A blocking read on a duplicate of
// a synchronous pipe handle serializes later writes to that same file object.
// Reuse one event for sequential handshake/observation I/O; the disconnect
// watch below owns a distinct event and stable buffer for its concurrent read.
struct BrokerPipeIo {
    pipe: Handle,
    parent_process: Handle,
    event: Handle,
}

impl BrokerPipeIo {
    fn new(pipe: Handle, parent_process: Handle) -> Option<Self> {
        let event = unsafe { CreateEventW(core::ptr::null(), 1, 0, core::ptr::null()) };
        (!event.is_null()).then_some(Self {
            pipe,
            parent_process,
            event,
        })
    }

    fn read(&mut self, buffer: &mut [u8]) -> bool {
        read_exact_pipe_overlapped(
            self.pipe,
            buffer,
            self.event,
            core::ptr::null_mut(),
            self.parent_process,
        ) == PipeIoOutcome::Complete
    }

    fn write(&mut self, buffer: &[u8]) -> bool {
        write_all_pipe_overlapped(
            self.pipe,
            buffer,
            self.event,
            core::ptr::null_mut(),
            self.parent_process,
        ) == PipeIoOutcome::Complete
    }
}

impl Drop for BrokerPipeIo {
    fn drop(&mut self) {
        // The shared I/O helpers always complete or cancel-and-reap before
        // returning, so no operation can still reference this event.
        unsafe {
            let _ = CloseHandle(self.event);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PipeIoOutcome {
    Complete,
    Stopped,
    Failed,
}

fn new_overlapped(event: Handle) -> Option<Overlapped> {
    if unsafe { ResetEvent(event) } == 0 {
        return None;
    }

    Some(Overlapped {
        internal: 0,
        internal_high: 0,
        offset: 0,
        offset_high: 0,
        event,
    })
}

fn cancel_and_reap(pipe: Handle, overlapped: &mut Overlapped) {
    unsafe {
        // CancelIoEx is operation-specific here. Whether cancellation wins or
        // the I/O completes normally, GetOverlappedResult waits until the
        // operation has reached a terminal state before this OVERLAPPED can be
        // reused or dropped.
        let _ = CancelIoEx(pipe, overlapped);

        let mut transferred = 0u32;
        let _ = GetOverlappedResult(pipe, overlapped, &mut transferred, 1);
    }
}

fn wait_overlapped(
    pipe: Handle,
    overlapped: &mut Overlapped,
    stop_event: Handle,
    process: Handle,
) -> Result<u32, PipeIoOutcome> {
    let handles = [stop_event, process, overlapped.event];
    // The Broker has a parent-lifetime cancellation boundary but no separate
    // Helper worker-stop event. Never pass a null handle to a Windows wait.
    let handles = if stop_event.is_null() {
        &handles[1..]
    } else {
        &handles[..]
    };
    let process_index = u32::from(!stop_event.is_null());

    let wait =
        unsafe { WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), 0, INFINITE) };

    if !stop_event.is_null() && wait == WAIT_OBJECT_0 {
        cancel_and_reap(pipe, overlapped);
        return Err(PipeIoOutcome::Stopped);
    }

    if wait == WAIT_OBJECT_0 + process_index {
        cancel_and_reap(pipe, overlapped);
        return Err(PipeIoOutcome::Failed);
    }

    if wait != WAIT_OBJECT_0 + process_index + 1 {
        cancel_and_reap(pipe, overlapped);
        return Err(PipeIoOutcome::Failed);
    }

    let mut transferred = 0u32;

    if unsafe { GetOverlappedResult(pipe, overlapped, &mut transferred, 0) } == 0 {
        Err(PipeIoOutcome::Failed)
    } else {
        Ok(transferred)
    }
}

fn connect_pipe_server(
    pipe: Handle,
    io_event: Handle,
    stop_event: Handle,
    process: Handle,
) -> PipeIoOutcome {
    let Some(mut overlapped) = new_overlapped(io_event) else {
        return PipeIoOutcome::Failed;
    };

    let connected = unsafe { ConnectNamedPipe(pipe, &mut overlapped) };

    if connected != 0 {
        return PipeIoOutcome::Complete;
    }

    match unsafe { GetLastError() } {
        ERROR_PIPE_CONNECTED => PipeIoOutcome::Complete,
        ERROR_IO_PENDING => match wait_overlapped(pipe, &mut overlapped, stop_event, process) {
            Ok(_) => PipeIoOutcome::Complete,
            Err(outcome) => outcome,
        },
        _ => PipeIoOutcome::Failed,
    }
}

fn read_exact_pipe_overlapped(
    pipe: Handle,
    buffer: &mut [u8],
    io_event: Handle,
    stop_event: Handle,
    process: Handle,
) -> PipeIoOutcome {
    let mut offset = 0usize;

    while offset < buffer.len() {
        let Some(mut overlapped) = new_overlapped(io_event) else {
            return PipeIoOutcome::Failed;
        };

        let started = unsafe {
            ReadFile(
                pipe,
                buffer[offset..].as_mut_ptr().cast(),
                (buffer.len() - offset) as u32,
                core::ptr::null_mut(),
                &mut overlapped,
            )
        };

        let transferred = if started != 0 {
            let mut transferred = 0u32;

            if unsafe { GetOverlappedResult(pipe, &mut overlapped, &mut transferred, 0) } == 0 {
                return PipeIoOutcome::Failed;
            }

            transferred
        } else if unsafe { GetLastError() } == ERROR_IO_PENDING {
            match wait_overlapped(pipe, &mut overlapped, stop_event, process) {
                Ok(transferred) => transferred,
                Err(outcome) => return outcome,
            }
        } else {
            return PipeIoOutcome::Failed;
        };

        if transferred == 0 {
            return PipeIoOutcome::Failed;
        }

        offset += transferred as usize;
    }

    PipeIoOutcome::Complete
}

fn write_all_pipe_overlapped(
    pipe: Handle,
    buffer: &[u8],
    io_event: Handle,
    stop_event: Handle,
    process: Handle,
) -> PipeIoOutcome {
    let mut offset = 0usize;

    while offset < buffer.len() {
        let Some(mut overlapped) = new_overlapped(io_event) else {
            return PipeIoOutcome::Failed;
        };

        let started = unsafe {
            WriteFile(
                pipe,
                buffer[offset..].as_ptr().cast(),
                (buffer.len() - offset) as u32,
                core::ptr::null_mut(),
                &mut overlapped,
            )
        };

        let transferred = if started != 0 {
            let mut transferred = 0u32;

            if unsafe { GetOverlappedResult(pipe, &mut overlapped, &mut transferred, 0) } == 0 {
                return PipeIoOutcome::Failed;
            }

            transferred
        } else if unsafe { GetLastError() } == ERROR_IO_PENDING {
            match wait_overlapped(pipe, &mut overlapped, stop_event, process) {
                Ok(transferred) => transferred,
                Err(outcome) => return outcome,
            }
        } else {
            return PipeIoOutcome::Failed;
        };

        if transferred == 0 {
            return PipeIoOutcome::Failed;
        }

        offset += transferred as usize;
    }

    PipeIoOutcome::Complete
}

struct ParentAuthenticationExpectation<'a> {
    parent_pid: u32,
    session_id: u32,
    key_vk: u32,
    nonce: [u8; 16],
    broker_path: &'a Path,
}

struct ParentAuthenticatedPeer {
    process: Handle,
    identity: HandshakeIdentity,
}

fn authenticate_connected_parent_pipe(
    pipe: Handle,
    io_event: Handle,
    stop_event: Handle,
    expectation: ParentAuthenticationExpectation<'_>,
) -> Result<ParentAuthenticatedPeer, PipeIoOutcome> {
    let mut client_pid = 0u32;

    if unsafe { GetNamedPipeClientProcessId(pipe, &mut client_pid) } == 0 || client_pid == 0 {
        return Err(PipeIoOutcome::Failed);
    }

    // Security identity starts with the actual Named Pipe client PID. Task
    // EnginePID is intentionally absent from this proof.
    let process = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE,
            0,
            client_pid,
        )
    };

    if invalid_handle(process) {
        return Err(PipeIoOutcome::Failed);
    }

    if process_session_id(client_pid) != Some(expectation.session_id)
        || !same_session(expectation.parent_pid, client_pid)
        || !process_image_matches(process, expectation.broker_path)
    {
        unsafe {
            let _ = CloseHandle(process);
        }

        return Err(PipeIoOutcome::Failed);
    }

    let identity = HandshakeIdentity {
        parent_pid: expectation.parent_pid,
        broker_pid: client_pid,
        session_id: expectation.session_id,
        key_vk: expectation.key_vk,
        nonce: expectation.nonce,
    };

    let mut hello = [0u8; FRAME_LEN];
    let outcome = read_exact_pipe_overlapped(pipe, &mut hello, io_event, stop_event, process);

    if outcome != PipeIoOutcome::Complete {
        unsafe {
            let _ = CloseHandle(process);
        }

        return Err(outcome);
    }

    if !frame_matches(&hello, FRAME_HELLO, identity) {
        unsafe {
            let _ = CloseHandle(process);
        }

        return Err(PipeIoOutcome::Failed);
    }

    let ack = encode_frame(FRAME_ACK, identity);
    let outcome = write_all_pipe_overlapped(pipe, &ack, io_event, stop_event, process);

    if outcome != PipeIoOutcome::Complete {
        unsafe {
            let _ = CloseHandle(process);
        }

        return Err(outcome);
    }

    Ok(ParentAuthenticatedPeer { process, identity })
}

fn connect_and_authenticate_parent_pipe(
    pipe: Handle,
    io_event: Handle,
    stop_event: Handle,
    engine_process: Handle,
    expectation: ParentAuthenticationExpectation<'_>,
) -> Result<ParentAuthenticatedPeer, PipeIoOutcome> {
    // EnginePID is lifecycle-only before connect. Once the pipe client exists,
    // all liveness and identity checks use the actual client process handle.
    match connect_pipe_server(pipe, io_event, stop_event, engine_process) {
        PipeIoOutcome::Complete => {}
        outcome => return Err(outcome),
    }

    authenticate_connected_parent_pipe(pipe, io_event, stop_event, expectation)
}

trait ParentWireIo {
    fn read_exact(&mut self, buffer: &mut [u8]) -> PipeIoOutcome;
    fn write_all(&mut self, buffer: &[u8]) -> PipeIoOutcome;
}

struct ParentPipeIo {
    pipe: Handle,
    event: Handle,
    stop_event: Handle,
    peer_process: Handle,
}

impl ParentWireIo for ParentPipeIo {
    fn read_exact(&mut self, buffer: &mut [u8]) -> PipeIoOutcome {
        read_exact_pipe_overlapped(
            self.pipe,
            buffer,
            self.event,
            self.stop_event,
            self.peer_process,
        )
    }

    fn write_all(&mut self, buffer: &[u8]) -> PipeIoOutcome {
        write_all_pipe_overlapped(
            self.pipe,
            buffer,
            self.event,
            self.stop_event,
            self.peer_process,
        )
    }
}

fn parent_send_session_mode(
    io: &mut impl ParentWireIo,
    mode: BrokerSessionMode,
    identity: HandshakeIdentity,
) -> PipeIoOutcome {
    io.write_all(&encode_session_mode(mode, identity))
}

fn parent_read_expected_control(
    io: &mut impl ParentWireIo,
    expected_kind: u16,
    identity: HandshakeIdentity,
    expected_value: u64,
) -> bool {
    let mut frame = [0u8; FRAME_LEN];

    io.read_exact(&mut frame) == PipeIoOutcome::Complete
        && decode_control_frame(&frame) == Some((expected_kind, identity, expected_value))
}

fn parent_send_maintenance_transport(
    io: &mut impl ParentWireIo,
    identity: HandshakeIdentity,
    target_version: &str,
    signed_manifest: &[u8],
    payload: &[u8],
) -> bool {
    let Some((version_length, payload_length, manifest_length)) =
        maintenance_transport_lengths(target_version, signed_manifest, payload)
    else {
        return false;
    };

    if parent_send_session_mode(io, BrokerSessionMode::Maintenance, identity)
        != PipeIoOutcome::Complete
    {
        return false;
    }

    let version_len = encode_control_frame(FRAME_MAINTENANCE_VERSION_LEN, identity, version_length);
    let payload_len = encode_control_frame(FRAME_MAINTENANCE_PAYLOAD_LEN, identity, payload_length);
    let manifest_len =
        encode_control_frame(FRAME_MAINTENANCE_MANIFEST_LEN, identity, manifest_length);

    if io.write_all(&version_len) != PipeIoOutcome::Complete
        || io.write_all(target_version.as_bytes()) != PipeIoOutcome::Complete
        || io.write_all(&payload_len) != PipeIoOutcome::Complete
        || io.write_all(&manifest_len) != PipeIoOutcome::Complete
        || io.write_all(signed_manifest) != PipeIoOutcome::Complete
    {
        return false;
    }

    for chunk in payload.chunks(MAINTENANCE_CHUNK_BYTES) {
        if io.write_all(chunk) != PipeIoOutcome::Complete {
            return false;
        }
    }

    parent_read_expected_control(io, FRAME_MAINTENANCE_RECEIVED, identity, payload_length)
        && parent_read_expected_control(io, FRAME_MAINTENANCE_COMMITTED, identity, payload_length)
}

fn parent_send_uninstall_transport(
    io: &mut impl ParentWireIo,
    identity: HandshakeIdentity,
) -> bool {
    parent_send_session_mode(io, BrokerSessionMode::Uninstall, identity) == PipeIoOutcome::Complete
        && parent_read_expected_control(io, FRAME_UNINSTALL_COMMITTED, identity, 0)
}

fn cleanup_parent_thread(pipe: Handle, process: Handle, stop_event: Handle) {
    unsafe {
        if !invalid_handle(pipe) {
            let _ = DisconnectNamedPipe(pipe);
            let _ = CloseHandle(pipe);
        }

        if !invalid_handle(process) {
            let _ = CloseHandle(process);
        }

        if !invalid_handle(stop_event) {
            let _ = CloseHandle(stop_event);
        }
    }
}

struct ParentPipeContext {
    parent_pid: u32,
    session_id: u32,
    key_vk: u32,
    nonce: [u8; 16],
    broker_path: PathBuf,
    authority_epoch: u64,
    key_revision: u64,
    wake_hwnd: Hwnd,
    pending: Arc<Mutex<PendingObservations>>,
    status: Arc<AtomicU8>,
}

fn parent_pipe_thread(
    pipe: Handle,
    engine_process: Handle,
    stop_event: Handle,
    context: ParentPipeContext,
) {
    let ParentPipeContext {
        parent_pid,
        session_id,
        key_vk,
        nonce,
        broker_path,
        authority_epoch,
        key_revision,
        wake_hwnd,
        pending,
        status,
    } = context;

    let io_event = unsafe { CreateEventW(core::ptr::null(), 1, 0, core::ptr::null()) };

    if io_event.is_null() {
        status.store(TRANSPORT_FAILED, Ordering::Release);

        unsafe {
            let _ = PostMessageW(wake_hwnd, BROKER_WAKE_MESSAGE, 0, 0);
        }

        cleanup_parent_thread(pipe, engine_process, stop_event);

        return;
    }

    let mut process: Handle = core::ptr::null_mut();

    let outcome = (|| {
        let authenticated = match connect_and_authenticate_parent_pipe(
            pipe,
            io_event,
            stop_event,
            engine_process,
            ParentAuthenticationExpectation {
                parent_pid,
                session_id,
                key_vk,
                nonce,
                broker_path: &broker_path,
            },
        ) {
            Ok(authenticated) => authenticated,
            Err(outcome) => return outcome,
        };

        process = authenticated.process;
        let expected = authenticated.identity;

        let mut io = ParentPipeIo {
            pipe,
            event: io_event,
            stop_event,
            peer_process: process,
        };

        // Wire v2 requires explicit session selection. Existing Helper runtime
        // owns only INPUT here; startup migration is not wired in A3C1.
        match parent_send_session_mode(&mut io, BrokerSessionMode::Input, expected) {
            PipeIoOutcome::Complete => {}
            outcome => return outcome,
        }

        status.store(TRANSPORT_AUTHENTICATED, Ordering::Release);

        let mut last_sequence = 0u64;

        loop {
            let mut frame = [0u8; OBSERVATION_FRAME_LEN];

            match io.read_exact(&mut frame) {
                PipeIoOutcome::Complete => {}
                outcome => return outcome,
            }

            let Some(observation) = decode_observation(&frame) else {
                return PipeIoOutcome::Failed;
            };

            if observation.epoch != authority_epoch
                || observation.key_revision != key_revision
                || observation.sequence <= last_sequence
            {
                return PipeIoOutcome::Failed;
            }

            last_sequence = observation.sequence;

            let needs_wake = {
                let Ok(mut queue) = pending.lock() else {
                    return PipeIoOutcome::Failed;
                };

                queue.push(observation)
            };

            if needs_wake && unsafe { PostMessageW(wake_hwnd, BROKER_WAKE_MESSAGE, 0, 0) } == 0 {
                return PipeIoOutcome::Failed;
            }
        }
    })();

    unsafe {
        let _ = CloseHandle(io_event);

        // This handle is lifecycle-only and is never used as Broker identity.
        let _ = CloseHandle(engine_process);
    }

    if outcome == PipeIoOutcome::Failed {
        status.store(TRANSPORT_FAILED, Ordering::Release);

        unsafe {
            let _ = PostMessageW(wake_hwnd, BROKER_WAKE_MESSAGE, 0, 0);
        }
    }

    cleanup_parent_thread(pipe, process, stop_event);
}
pub fn monotonic_tick_ms() -> u64 {
    unsafe { GetTickCount64() }
}

fn broker_raw_input_usage(key_vk: u32) -> u16 {
    if key_vk == VK_XBUTTON1 as u32 || key_vk == VK_XBUTTON2 as u32 {
        HID_USAGE_GENERIC_MOUSE
    } else {
        HID_USAGE_GENERIC_KEYBOARD
    }
}

fn broker_raw_input_device(window: Hwnd, key_vk: u32, flags: u32) -> RawInputDevice {
    RawInputDevice {
        usage_page: HID_USAGE_PAGE_GENERIC,
        usage: broker_raw_input_usage(key_vk),
        flags,
        target: if flags == RIDEV_REMOVE { 0 } else { window },
    }
}

fn create_broker_raw_input_window() -> Option<Hwnd> {
    let class = wide(OsStr::new("STATIC"));

    let window = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            core::ptr::null(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            0,
            0,
            core::ptr::null_mut(),
        )
    };

    (window != 0).then_some(window)
}

fn register_broker_raw_input(window: Hwnd, key_vk: u32) -> bool {
    let device = broker_raw_input_device(window, key_vk, RIDEV_INPUTSINK);

    unsafe { RegisterRawInputDevices(&device, 1, size_of::<RawInputDevice>() as u32) != 0 }
}

fn unregister_broker_raw_input(key_vk: u32) {
    let device = broker_raw_input_device(0, key_vk, RIDEV_REMOVE);

    unsafe {
        let _ = RegisterRawInputDevices(&device, 1, size_of::<RawInputDevice>() as u32);
    }
}

fn raw_packet_complete(kind: u32, copied: u32, declared: u32) -> bool {
    let payload = match kind {
        RIM_TYPEMOUSE => size_of::<RawMouse>(),
        RIM_TYPEKEYBOARD => size_of::<RawKeyboard>(),
        _ => return false,
    };

    let required = (size_of::<RawInputHeader>() + payload) as u32;

    copied >= required && declared >= required
}

fn read_broker_raw_input(lparam: isize) -> Option<RawInput> {
    let mut raw: RawInput = unsafe { core::mem::zeroed() };
    let mut size = size_of::<RawInput>() as u32;

    let copied = unsafe {
        GetRawInputData(
            lparam,
            RID_INPUT,
            (&mut raw as *mut RawInput).cast(),
            &mut size,
            size_of::<RawInputHeader>() as u32,
        )
    };

    if copied == u32::MAX
        || copied < size_of::<RawInputHeader>() as u32
        || !raw_packet_complete(raw.header.kind, copied, raw.header.size)
    {
        return None;
    }

    Some(raw)
}

fn mouse_button_edges(button_flags: u16, key_vk: u32) -> [Option<Edge>; 2] {
    let (down_flag, up_flag) = match key_vk {
        value if value == VK_XBUTTON1 as u32 => (RI_MOUSE_BUTTON_4_DOWN, RI_MOUSE_BUTTON_4_UP),
        value if value == VK_XBUTTON2 as u32 => (RI_MOUSE_BUTTON_5_DOWN, RI_MOUSE_BUTTON_5_UP),
        _ => return [None, None],
    };

    [
        (button_flags & down_flag != 0).then_some(Edge::Down),
        (button_flags & up_flag != 0).then_some(Edge::Up),
    ]
}

fn keyboard_edge(make_code: u16, vkey: u16, flags: u16, key_vk: u32) -> Option<Edge> {
    if make_code == KEYBOARD_OVERRUN_MAKE_CODE
        || vkey == 0
        || vkey == 255
        || u32::from(vkey) != key_vk
    {
        return None;
    }

    Some(if flags & RI_KEY_BREAK != 0 {
        Edge::Up
    } else {
        Edge::Down
    })
}

fn raw_input_edges(raw: &RawInput, key_vk: u32) -> [Option<Edge>; 2] {
    match raw.header.kind {
        RIM_TYPEMOUSE => {
            let mouse = unsafe { raw.data.mouse };
            mouse_button_edges(mouse.buttons as u16, key_vk)
        }

        RIM_TYPEKEYBOARD => {
            let keyboard = unsafe { raw.data.keyboard };
            [
                keyboard_edge(keyboard._make_code, keyboard.vkey, keyboard.flags, key_vk),
                None,
            ]
        }

        _ => [None, None],
    }
}

fn write_broker_observation(pipe: &mut BrokerPipeIo, observation: BrokerWireObservation) -> bool {
    pipe.write(&encode_observation(observation))
}

// PHYSICAL_DOWN_REFRESH_PROOF:
// Raw Input supplies immediate edges, but the locked 64/160/320 ms lease
// contract requires each periodic DOWN refresh to be backed by a fresh
// physical level sample. This sample runs only while the Broker is latched
// DOWN; the idle path remains fully event-driven.
fn physical_vk_down(vkey: u32) -> bool {
    let Ok(vkey) = i32::try_from(vkey) else {
        return false;
    };

    unsafe { GetAsyncKeyState(vkey) & (0x8000u16 as i16) != 0 }
}

fn down_refresh_deadline(now_tick: u64) -> Option<u64> {
    now_tick.checked_add(BROKER_DOWN_REFRESH_MS)
}
fn down_refresh_due(now_tick: u64, deadline: Option<u64>, down: bool) -> bool {
    match deadline {
        Some(deadline) => down && now_tick >= deadline,
        None => false,
    }
}

fn refresh_down_if_due(
    sampler: &mut BrokerSampler,
    pipe: &mut BrokerPipeIo,
    key_vk: u32,
    now_tick: u64,
    refresh_deadline: &mut Option<u64>,
    epoch: u64,
    key_revision: u64,
) -> bool {
    if !down_refresh_due(now_tick, *refresh_deadline, sampler.is_down()) {
        return true;
    }

    if !physical_vk_down(key_vk) {
        let Some(up) = sampler.edge(Edge::Up, now_tick, epoch, key_revision) else {
            return false;
        };

        if !write_broker_observation(pipe, up) {
            return false;
        }

        *refresh_deadline = None;
        return true;
    }

    let Some(refresh) = sampler.refresh_down(now_tick, epoch, key_revision) else {
        return false;
    };

    if !write_broker_observation(pipe, refresh) {
        return false;
    }

    let Some(next_deadline) = down_refresh_deadline(now_tick) else {
        return false;
    };

    *refresh_deadline = Some(next_deadline);

    true
}

fn down_refresh_timeout(now_tick: u64, deadline: Option<u64>, down: bool) -> u32 {
    if !down {
        return INFINITE;
    }

    let Some(deadline) = deadline else {
        return 0;
    };

    deadline.saturating_sub(now_tick).min(u64::from(u32::MAX)) as u32
}

struct DisconnectRead {
    overlapped: Overlapped,
    byte: u8,
}

// Stable allocation: Windows retains pointers to both fields until completion.
// The watch is owned by the sampler scope, not a detached blocking-read thread.
struct BrokerDisconnectWatch {
    pipe: Handle,
    read: Box<DisconnectRead>,
    armed: bool,
}

impl BrokerDisconnectWatch {
    fn start(pipe: Handle) -> Option<Self> {
        let event = unsafe { CreateEventW(core::ptr::null(), 1, 0, core::ptr::null()) };
        if event.is_null() {
            return None;
        }
        let Some(overlapped) = new_overlapped(event) else {
            unsafe {
                let _ = CloseHandle(event);
            }
            return None;
        };
        let mut watch = Self {
            pipe,
            read: Box::new(DisconnectRead {
                overlapped,
                byte: 0,
            }),
            armed: false,
        };
        let started = unsafe {
            ReadFile(
                pipe,
                (&mut watch.read.byte as *mut u8).cast(),
                1,
                core::ptr::null_mut(),
                &mut watch.read.overlapped,
            )
        };
        if started == 0 && unsafe { GetLastError() } != ERROR_IO_PENDING {
            return None;
        }
        watch.armed = true;
        // Once INPUT session selection is complete, any further inbound byte
        // is unexpected and revokes authority just like EOF. Both synchronous
        // completion and pending completion signal the read event; neither can
        // authorize a physical level.
        Some(watch)
    }

    fn event(&self) -> Handle {
        self.read.overlapped.event
    }
}

impl Drop for BrokerDisconnectWatch {
    fn drop(&mut self) {
        // Includes successful completion racing cancellation. Reap before the
        // heap buffer/OVERLAPPED or event can be freed, on every exit path.
        if self.armed {
            cancel_and_reap(self.pipe, &mut self.read.overlapped);
        }
        unsafe {
            let _ = CloseHandle(self.event());
        }
    }
}

fn broker_stream_observations(
    pipe: &mut BrokerPipeIo,
    parent_process: Handle,
    key_vk: u32,
    epoch: u64,
    key_revision: u64,
) -> bool {
    let Some(disconnect) = BrokerDisconnectWatch::start(pipe.pipe) else {
        return false;
    };
    let pipe_dead_event = disconnect.event();

    let Some(window) = create_broker_raw_input_window() else {
        return false;
    };

    if !register_broker_raw_input(window, key_vk) {
        unsafe {
            let _ = DestroyWindow(window);
        }
        return false;
    }

    let mut sampler = BrokerSampler::new();

    // Fail closed across startup. An already-held key does not gain authority
    // until a trustworthy Raw Input transition is observed after registration.
    let Some(initial_up) = sampler.initial_up(unsafe { GetTickCount64() }, epoch, key_revision)
    else {
        unregister_broker_raw_input(key_vk);
        unsafe {
            let _ = DestroyWindow(window);
        }
        return false;
    };

    if !write_broker_observation(pipe, initial_up) {
        unregister_broker_raw_input(key_vk);
        unsafe {
            let _ = DestroyWindow(window);
        }
        return false;
    }

    let handles = [parent_process, pipe_dead_event];
    let message_wait = WAIT_OBJECT_0 + handles.len() as u32;
    let mut refresh_deadline: Option<u64> = None;
    let mut result = true;

    'stream: loop {
        if sampler.next_sequence == u64::MAX {
            result = false;
            break;
        }

        let now_tick = unsafe { GetTickCount64() };
        if !refresh_down_if_due(
            &mut sampler,
            pipe,
            key_vk,
            now_tick,
            &mut refresh_deadline,
            epoch,
            key_revision,
        ) {
            result = false;
            break;
        }
        let timeout = down_refresh_timeout(now_tick, refresh_deadline, sampler.is_down());

        let wait = unsafe {
            MsgWaitForMultipleObjects(
                handles.len() as u32,
                handles.as_ptr(),
                0,
                timeout,
                QS_RAWINPUT,
            )
        };

        if wait == WAIT_OBJECT_0 || wait == WAIT_OBJECT_0 + 1 {
            break;
        }

        if wait == WAIT_TIMEOUT {
            let observed_tick = unsafe { GetTickCount64() };

            if !refresh_down_if_due(
                &mut sampler,
                pipe,
                key_vk,
                observed_tick,
                &mut refresh_deadline,
                epoch,
                key_revision,
            ) {
                result = false;
                break;
            }

            continue;
        }

        if wait == message_wait {
            loop {
                let refresh_tick = unsafe { GetTickCount64() };

                if !refresh_down_if_due(
                    &mut sampler,
                    pipe,
                    key_vk,
                    refresh_tick,
                    &mut refresh_deadline,
                    epoch,
                    key_revision,
                ) {
                    result = false;
                    break 'stream;
                }

                let mut message: RawMessage = unsafe { core::mem::zeroed() };

                let available =
                    unsafe { PeekMessageW(&mut message, window, WM_INPUT, WM_INPUT, PM_REMOVE) };

                if available == 0 {
                    break;
                }

                let raw = read_broker_raw_input(message.lparam);

                if message.wparam & 0xff == RIM_INPUT {
                    unsafe {
                        let _ = DefWindowProcW(
                            message.hwnd,
                            message.message,
                            message.wparam,
                            message.lparam,
                        );
                    }
                }

                let Some(raw) = raw else {
                    result = false;
                    break 'stream;
                };

                for edge in raw_input_edges(&raw, key_vk).into_iter().flatten() {
                    let was_down = sampler.is_down();
                    let observed_tick = unsafe { GetTickCount64() };

                    let observation = sampler.edge(edge, observed_tick, epoch, key_revision);

                    if let Some(observation) = observation {
                        if !write_broker_observation(pipe, observation) {
                            result = false;
                            break 'stream;
                        }
                    }

                    if !was_down && sampler.is_down() {
                        let Some(next_deadline) = down_refresh_deadline(observed_tick) else {
                            result = false;
                            break 'stream;
                        };

                        refresh_deadline = Some(next_deadline);
                    } else if was_down && !sampler.is_down() {
                        refresh_deadline = None;
                    }

                    if sampler.next_sequence == u64::MAX {
                        result = false;
                        break 'stream;
                    }
                }
            }

            continue;
        }

        result = false;
        break;
    }

    unregister_broker_raw_input(key_vk);

    unsafe {
        let _ = DestroyWindow(window);
    }

    result
}

pub fn physical_key_down(key: &RadioKey) -> bool {
    let Some(vkey) = key_to_vk(key) else {
        return false;
    };

    unsafe { GetAsyncKeyState(vkey) & (0x8000u16 as i16) != 0 }
}

/// Returns true only while the foreground process is known elevated or cannot
/// expose its token to the normal-integrity Helper.
pub fn foreground_requires_broker() -> bool {
    unsafe {
        let hwnd = GetForegroundWindow();

        if hwnd == 0 {
            return false;
        }

        let mut process_id = 0u32;

        if GetWindowThreadProcessId(hwnd, &mut process_id) == 0 || process_id == 0 {
            return false;
        }

        if process_id == GetCurrentProcessId() {
            return false;
        }

        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id);

        if process.is_null() {
            return true;
        }

        let mut token: Handle = core::ptr::null_mut();

        let token_open = OpenProcessToken(process, TOKEN_QUERY, &mut token);

        let _ = CloseHandle(process);

        if token_open == 0 || token.is_null() {
            return true;
        }

        let mut elevated = 0u32;
        let mut returned = 0u32;

        let ok = GetTokenInformation(
            token,
            TOKEN_ELEVATION,
            (&mut elevated as *mut u32).cast(),
            size_of::<u32>() as u32,
            &mut returned,
        );

        let _ = CloseHandle(token);

        ok == 0 || elevated != 0
    }
}

/// R1B transport owner.
///
/// This object proves the elevated child is the exact process launched by this
/// Helper, in the same Windows session, running this same executable, over a
/// local-only Named Pipe instance protected by an explicit DACL.
///
/// R1B deliberately does not expose a physical level to `windows.rs`.
fn broker_task_run_arguments(
    parent_pid: u32,
    pipe_name: &OsStr,
    nonce_token: &str,
    key_vk: u32,
    authority_epoch: u64,
    key_revision: u64,
) -> [String; 6] {
    [
        parent_pid.to_string(),
        pipe_name.to_string_lossy().into_owned(),
        nonce_token.to_owned(),
        key_vk.to_string(),
        authority_epoch.to_string(),
        key_revision.to_string(),
    ]
}

fn demand_run_maintenance_task_with<R>(
    contract: &BrokerTaskContract,
    arguments: &[String; 6],
    demand_run: R,
) -> io::Result<u32>
where
    R: FnOnce(&BrokerTaskContract, &[String; 6]) -> io::Result<u32>,
{
    match demand_run(contract, arguments)? {
        0 => Err(io::Error::other(
            "Broker Task maintenance demand-run returned zero EnginePID",
        )),
        engine_pid => Ok(engine_pid),
    }
}

/// Runs one synchronous authenticated maintenance session against the exact
/// already-recognized Broker Task contract supplied by the caller.
///
/// This is a parent-side primitive only: it neither discovers nor mutates Task
/// authority, acquires lifecycle mutexes, invokes UAC, starts Raw Input, or
/// performs network/update work. The payload is borrowed and emitted in
/// bounded slices without creating a second whole-buffer copy.
pub(crate) fn run_broker_maintenance_client(
    existing_contract: &BrokerTaskContract,
    target_version: &str,
    signed_manifest: &[u8],
    payload: &[u8],
) -> io::Result<()> {
    let artifact = verify_current_broker_bundle(signed_manifest, payload).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("embedded Broker bundle verification failed: {error:?}"),
        )
    })?;

    if artifact.target_version() != target_version {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "maintenance target version does not match the verified current Broker bundle",
        ));
    }

    if maintenance_transport_lengths(target_version, signed_manifest, payload).is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "verified Broker bundle exceeds maintenance transport bounds",
        ));
    }

    let parent_pid = unsafe { GetCurrentProcessId() };
    let session_id = process_session_id(parent_pid)
        .ok_or_else(|| io::Error::other("failed to resolve Helper Windows session"))?;
    let nonce = random_nonce().ok_or_else(|| io::Error::other("failed to create Broker nonce"))?;
    let nonce_token = nonce_hex(&nonce);
    let pipe_name = canonical_broker_pipe_name(&nonce);
    let pipe = create_private_pipe(&pipe_name)
        .ok_or_else(|| io::Error::other("failed to create private Broker maintenance pipe"))?;

    let io_event = unsafe { CreateEventW(core::ptr::null(), 1, 0, core::ptr::null()) };

    if io_event.is_null() {
        unsafe {
            let _ = CloseHandle(pipe);
        }

        return Err(io::Error::last_os_error());
    }

    let task_arguments = broker_task_run_arguments(
        parent_pid,
        pipe_name.as_os_str(),
        &nonce_token,
        MAINTENANCE_INERT_KEY_VK,
        MAINTENANCE_INERT_AUTHORITY_EPOCH,
        MAINTENANCE_INERT_KEY_REVISION,
    );

    let mut engine_process: Handle = core::ptr::null_mut();
    let mut broker_process: Handle = core::ptr::null_mut();

    let result = (|| {
        let engine_pid = demand_run_maintenance_task_with(
            existing_contract,
            &task_arguments,
            |contract, arguments| {
                demand_run_broker_task_via_scheduler(contract, arguments).map_err(|error| {
                    io::Error::other(format!(
                        "Broker Task maintenance demand-run failed: {error}"
                    ))
                })
            },
        )?;

        // EnginePID remains a pre-connect liveness signal only.
        engine_process = unsafe { OpenProcess(SYNCHRONIZE, 0, engine_pid) };

        if invalid_handle(engine_process) {
            engine_process = core::ptr::null_mut();
            return Err(io::Error::last_os_error());
        }

        let authenticated = connect_and_authenticate_parent_pipe(
            pipe,
            io_event,
            core::ptr::null_mut(),
            engine_process,
            ParentAuthenticationExpectation {
                parent_pid,
                session_id,
                key_vk: MAINTENANCE_INERT_KEY_VK,
                nonce,
                // Migration launches and authenticates the exact recognized
                // older Broker path, never the current-version projection.
                broker_path: existing_contract.broker_path(),
            },
        )
        .map_err(|_| io::Error::other("Broker maintenance authentication failed"))?;

        broker_process = authenticated.process;

        let mut io = ParentPipeIo {
            pipe,
            event: io_event,
            stop_event: core::ptr::null_mut(),
            peer_process: broker_process,
        };

        if !parent_send_maintenance_transport(
            &mut io,
            authenticated.identity,
            target_version,
            signed_manifest,
            payload,
        ) {
            return Err(io::Error::other(
                "Broker maintenance transport did not reach COMMITTED",
            ));
        }

        Ok(())
    })();

    unsafe {
        let _ = DisconnectNamedPipe(pipe);

        if !invalid_handle(broker_process) {
            let _ = CloseHandle(broker_process);
        }

        if !invalid_handle(engine_process) {
            let _ = CloseHandle(engine_process);
        }

        let _ = CloseHandle(io_event);
        let _ = CloseHandle(pipe);
    }

    result
}

// A3C1 deliberately has no startup caller. This compile-time signature anchor
// keeps the crate-private primitive lint-reachable without invoking it.
const _: fn(&BrokerTaskContract, &str, &[u8], &[u8]) -> io::Result<()> =
    run_broker_maintenance_client;

fn wait_for_authenticated_broker_exit(process: Handle) -> io::Result<()> {
    let wait = unsafe { WaitForSingleObject(process, INFINITE) };

    match wait {
        WAIT_OBJECT_0 => Ok(()),
        WAIT_FAILED => Err(io::Error::last_os_error()),
        unexpected => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected Broker uninstall process wait result: {unexpected:#010x}"),
        )),
    }
}

fn broker_uninstall_persistent_state_is_committed(
    existing_contract: &BrokerTaskContract,
) -> io::Result<bool> {
    let task_absent = matches!(
        recognize_broker_task_via_scheduler(existing_contract.user_sid()).map_err(|error| {
            io::Error::other(format!(
                "Broker uninstall Task post-readback failed: {error}"
            ))
        })?,
        RecognizedBrokerTaskVerification::Absent
    );

    if !task_absent {
        return Ok(false);
    }

    uninstall_handoff_filesystem_security_matches(
        existing_contract.user_sid(),
        existing_contract.protected_broker_path(),
    )
}

fn uninstall_then_persistent_readback<T, P>(transport: T, persistent_readback: P) -> io::Result<()>
where
    T: FnOnce() -> io::Result<()>,
    P: FnOnce() -> io::Result<bool>,
{
    let transport_result = transport();

    match persistent_readback()? {
        true => Ok(()),

        false => match transport_result {
            Ok(()) => Err(io::Error::other(
                "Broker uninstall transport returned success without persistent committed state",
            )),

            Err(error) => Err(error),
        },
    }
}

fn run_broker_uninstall_transport_client(existing_contract: &BrokerTaskContract) -> io::Result<()> {
    let parent_pid = unsafe { GetCurrentProcessId() };

    let session_id = process_session_id(parent_pid).ok_or_else(|| {
        io::Error::other("failed to resolve Helper Windows session for Broker uninstall")
    })?;

    let nonce = random_nonce()
        .ok_or_else(|| io::Error::other("failed to create Broker uninstall nonce"))?;

    let nonce_token = nonce_hex(&nonce);

    let pipe_name = canonical_broker_pipe_name(&nonce);

    let pipe = create_private_pipe(&pipe_name)
        .ok_or_else(|| io::Error::other("failed to create private Broker uninstall pipe"))?;

    let io_event = unsafe { CreateEventW(core::ptr::null(), 1, 0, core::ptr::null()) };

    if io_event.is_null() {
        unsafe {
            let _ = CloseHandle(pipe);
        }

        return Err(io::Error::last_os_error());
    }

    let task_arguments = broker_task_run_arguments(
        parent_pid,
        pipe_name.as_os_str(),
        &nonce_token,
        MAINTENANCE_INERT_KEY_VK,
        MAINTENANCE_INERT_AUTHORITY_EPOCH,
        MAINTENANCE_INERT_KEY_REVISION,
    );

    let mut engine_process: Handle = core::ptr::null_mut();
    let mut broker_process: Handle = core::ptr::null_mut();

    let mut result = (|| {
        let engine_pid = demand_run_broker_task_via_scheduler(existing_contract, &task_arguments)
            .map_err(|error| {
            io::Error::other(format!("Broker Task uninstall demand-run failed: {error}"))
        })?;

        if engine_pid == 0 {
            return Err(io::Error::other(
                "Broker Task uninstall demand-run returned zero EnginePID",
            ));
        }

        // EnginePID is pre-connect liveness only. Actual authority is the
        // Named Pipe client process authenticated below.
        engine_process = unsafe { OpenProcess(SYNCHRONIZE, 0, engine_pid) };

        if invalid_handle(engine_process) {
            engine_process = core::ptr::null_mut();
            return Err(io::Error::last_os_error());
        }

        let authenticated = connect_and_authenticate_parent_pipe(
            pipe,
            io_event,
            core::ptr::null_mut(),
            engine_process,
            ParentAuthenticationExpectation {
                parent_pid,
                session_id,
                key_vk: MAINTENANCE_INERT_KEY_VK,
                nonce,
                broker_path: existing_contract.broker_path(),
            },
        )
        .map_err(|_| io::Error::other("Broker uninstall authentication failed"))?;

        broker_process = authenticated.process;

        let mut io = ParentPipeIo {
            pipe,
            event: io_event,
            stop_event: core::ptr::null_mut(),
            peer_process: broker_process,
        };

        if parent_send_uninstall_transport(&mut io, authenticated.identity) {
            Ok(())
        } else {
            Err(io::Error::other(
                "Broker uninstall transport did not reach COMMITTED",
            ))
        }
    })();

    // Closing the pipe is the cancellation boundary if COMMITTED was lost or
    // the Broker failed before finishing. After this point the authenticated
    // Broker must exit before protected executable cleanup can ever proceed.
    unsafe {
        let _ = DisconnectNamedPipe(pipe);
    }

    if !invalid_handle(broker_process) {
        if let Err(wait_error) = wait_for_authenticated_broker_exit(broker_process) {
            if result.is_ok() {
                result = Err(wait_error);
            }
        }
    }

    unsafe {
        if !invalid_handle(broker_process) {
            let _ = CloseHandle(broker_process);
        }

        if !invalid_handle(engine_process) {
            let _ = CloseHandle(engine_process);
        }

        let _ = CloseHandle(io_event);
        let _ = CloseHandle(pipe);
    }

    result
}

/// Runs one authenticated privileged-uninstall session through the SAME fixed
/// demand-only Broker Task.
///
/// Transport success is not authoritative. After the authenticated Broker has
/// exited, persistent state is always re-read. Therefore a lost COMMITTED frame
/// is accepted only when the fixed Task is absent AND the exact protected
/// Broker namespace already has the required uninstall-handoff ACL state.
pub(crate) fn run_broker_uninstall_client(
    existing_contract: &BrokerTaskContract,
) -> io::Result<()> {
    uninstall_then_persistent_readback(
        || run_broker_uninstall_transport_client(existing_contract),
        || broker_uninstall_persistent_state_is_committed(existing_contract),
    )
}

pub struct InputBrokerClient {
    stop_event: Handle,
    status: Arc<AtomicU8>,
    pending: Arc<Mutex<PendingObservations>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl InputBrokerClient {
    pub fn launch(parent: Hwnd, key: &RadioKey) -> Option<Self> {
        Self::launch_bound(parent, key, 0, 0)
    }

    pub fn launch_bound(
        parent: Hwnd,
        key: &RadioKey,
        authority_epoch: u64,
        key_revision: u64,
    ) -> Option<Self> {
        let key_vk = u32::try_from(key_to_vk(key)?).ok()?;

        let nonce = random_nonce()?;
        let token = nonce_hex(&nonce);

        let pipe_name = canonical_broker_pipe_name(&nonce);

        let pipe = create_private_pipe(&pipe_name)?;

        let stop_event = unsafe { CreateEventW(core::ptr::null(), 1, 0, core::ptr::null()) };

        if stop_event.is_null() {
            unsafe {
                let _ = CloseHandle(pipe);
            }

            return None;
        }

        let current_process = unsafe { GetCurrentProcess() };

        let mut thread_stop_event: Handle = core::ptr::null_mut();

        let duplicated = unsafe {
            DuplicateHandle(
                current_process,
                stop_event,
                current_process,
                &mut thread_stop_event,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        };

        if duplicated == 0 || thread_stop_event.is_null() {
            unsafe {
                let _ = CloseHandle(pipe);
                let _ = CloseHandle(stop_event);
            }

            return None;
        }

        let cleanup_launch = || unsafe {
            let _ = CloseHandle(pipe);
            let _ = CloseHandle(stop_event);
            let _ = CloseHandle(thread_stop_event);
        };

        let parent_pid = unsafe { GetCurrentProcessId() };

        let session_id = match process_session_id(parent_pid) {
            Some(session_id) => session_id,

            None => {
                cleanup_launch();
                return None;
            }
        };

        let protected_broker = match current_protected_broker_path() {
            Ok(path) => path,

            Err(_) => {
                cleanup_launch();
                return None;
            }
        };

        let user_sid = match current_user_sid_string() {
            Some(user_sid) => user_sid,

            None => {
                cleanup_launch();
                return None;
            }
        };

        let contract = match BrokerTaskContract::new(&user_sid, &protected_broker) {
            Ok(contract) => contract,

            Err(_) => {
                cleanup_launch();
                return None;
            }
        };

        let task_arguments = broker_task_run_arguments(
            parent_pid,
            pipe_name.as_os_str(),
            &token,
            key_vk,
            authority_epoch,
            key_revision,
        );

        let engine_pid = match demand_run_broker_task_via_scheduler(&contract, &task_arguments) {
            Ok(engine_pid) if engine_pid != 0 => engine_pid,

            _ => {
                cleanup_launch();
                return None;
            }
        };

        // EnginePID is task-instance liveness only.
        // It is deliberately NOT accepted as Broker identity.
        let engine_process = unsafe { OpenProcess(SYNCHRONIZE, 0, engine_pid) };

        if invalid_handle(engine_process) {
            cleanup_launch();
            return None;
        }

        let broker_path = contract.broker_path().to_path_buf();

        let status = Arc::new(AtomicU8::new(TRANSPORT_STARTING));

        let thread_status = Arc::clone(&status);

        let pending = Arc::new(Mutex::new(PendingObservations::new()));

        let thread_pending = Arc::clone(&pending);

        let pipe_raw = pipe as usize;
        let engine_raw = engine_process as usize;
        let stop_raw = thread_stop_event as usize;

        let spawn = thread::Builder::new()
            .name("lcr-input-broker-pipe".to_string())
            .spawn(move || {
                parent_pipe_thread(
                    pipe_raw as Handle,
                    engine_raw as Handle,
                    stop_raw as Handle,
                    ParentPipeContext {
                        parent_pid,
                        session_id,
                        key_vk,
                        nonce,
                        broker_path,
                        authority_epoch,
                        key_revision,
                        wake_hwnd: parent,
                        pending: thread_pending,
                        status: thread_status,
                    },
                );
            });

        let worker = match spawn {
            Ok(worker) => worker,

            Err(_) => {
                unsafe {
                    let _ = CloseHandle(engine_process);
                }

                cleanup_launch();
                return None;
            }
        };

        Some(Self {
            stop_event,
            status,
            pending,
            worker: Some(worker),
        })
    }
    pub fn poll_observation(&self) -> BrokerPoll {
        match self.pending.lock() {
            Ok(mut pending) => pending.poll(),
            Err(_) => BrokerPoll::Overflowed,
        }
    }

    pub fn authenticated(&self) -> bool {
        self.status.load(Ordering::Acquire) == TRANSPORT_AUTHENTICATED
    }

    pub fn transport_failed(&self) -> bool {
        self.status.load(Ordering::Acquire) == TRANSPORT_FAILED
    }

    /// Compatibility seam for the pre-R1C Windows call-site.
    ///
    /// Named Pipe authentication is NOT exposed as a physical level here.
    /// PostMessage/wParam therefore has zero PTT authority in R1B.
    pub fn observe_message(
        &mut self,
        _message: u32,
        _wparam: usize,
        _now: Instant,
    ) -> Option<bool> {
        None
    }

    /// Compatibility seam for pre-R1C fail-closed behavior.
    ///
    /// Until R1C wires authenticated pipe observations into `InputAuthority`,
    /// the broker must never be treated as a fresh physical-level provider.
    pub fn is_stale(&self, _now: Instant) -> bool {
        true
    }
}

impl Drop for InputBrokerClient {
    fn drop(&mut self) {
        unsafe {
            let _ = SetEvent(self.stop_event);
        }

        if let Some(worker) = self.worker.take() {
            // The worker owns only overlapped server-side pipe I/O. Its wait set
            // includes this stop event, so no check-then-block cancellation gap
            // exists before join.
            let _ = worker.join();
        }

        unsafe {
            let _ = CloseHandle(self.stop_event);
        }
    }
}

pub fn maybe_run_from_args<I>(mut args: I) -> Option<i32>
where
    I: Iterator<Item = OsString>,
{
    let first = args.next()?;

    if first != OsStr::new(BROKER_ARG) {
        return None;
    }

    Some(run_broker(args.collect()))
}

fn run_broker(args: Vec<OsString>) -> i32 {
    if args.len() != 6 {
        return 2;
    }

    let parent_pid = match args[0].to_string_lossy().parse::<u32>() {
        Ok(parent_pid) if parent_pid != 0 => parent_pid,
        _ => return 2,
    };

    let supplied_pipe_name = &args[1];

    let nonce = match parse_nonce_hex(&args[2]) {
        Some(nonce) => nonce,
        None => return 2,
    };

    let pipe_name = match validated_broker_pipe_name(supplied_pipe_name, &nonce) {
        Some(pipe_name) => pipe_name,
        None => return 2,
    };

    let key_vk = match args[3].to_string_lossy().parse::<u32>() {
        Ok(key_vk) if (1..=254).contains(&key_vk) => key_vk,
        _ => return 2,
    };

    let authority_epoch = match args[4].to_string_lossy().parse::<u64>() {
        Ok(epoch) => epoch,
        Err(_) => return 2,
    };

    let key_revision = match args[5].to_string_lossy().parse::<u64>() {
        Ok(revision) => revision,
        Err(_) => return 2,
    };

    let helper_executable = match canonical_installed_helper_path() {
        Ok(executable) => executable,
        Err(_) => return 1,
    };

    let parent_process = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE,
            0,
            parent_pid,
        )
    };

    if parent_process.is_null() {
        return 1;
    }

    let broker_pid = unsafe { GetCurrentProcessId() };

    if !same_session(parent_pid, broker_pid)
        || !process_image_matches(parent_process, &helper_executable)
    {
        unsafe {
            let _ = CloseHandle(parent_process);
        }
        return 1;
    }

    let pipe = match open_pipe(pipe_name) {
        Some(pipe) => pipe,
        None => {
            unsafe {
                let _ = CloseHandle(parent_process);
            }
            return 1;
        }
    };

    let mut server_pid = 0u32;

    if unsafe { GetNamedPipeServerProcessId(pipe, &mut server_pid) } == 0
        || server_pid != parent_pid
    {
        unsafe {
            let _ = CloseHandle(pipe);
            let _ = CloseHandle(parent_process);
        }
        return 1;
    }

    let Some(session_id) = process_session_id(broker_pid) else {
        unsafe {
            let _ = CloseHandle(pipe);
            let _ = CloseHandle(parent_process);
        }
        return 1;
    };

    let identity = HandshakeIdentity {
        parent_pid,
        broker_pid,
        session_id,
        key_vk,
        nonce,
    };

    let Some(mut io) = BrokerPipeIo::new(pipe, parent_process) else {
        unsafe {
            let _ = CloseHandle(pipe);
            let _ = CloseHandle(parent_process);
        }
        return 1;
    };
    let hello = encode_frame(FRAME_HELLO, identity);

    if !io.write(&hello) {
        unsafe {
            let _ = CloseHandle(pipe);
            let _ = CloseHandle(parent_process);
        }
        return 1;
    }

    let mut ack = [0u8; FRAME_LEN];

    if !io.read(&mut ack) || !frame_matches(&ack, FRAME_ACK, identity) {
        unsafe {
            let _ = CloseHandle(pipe);
            let _ = CloseHandle(parent_process);
        }
        return 1;
    }

    // Mutual HELLO/ACK authentication is complete, but no capability is active
    // until the parent explicitly selects exactly one wire-v2 session mode.
    let mut session_frame = [0u8; FRAME_LEN];

    let completed = if !io.read(&mut session_frame) {
        false
    } else {
        match decode_session_mode(&session_frame, identity) {
            Some(BrokerSessionMode::Input) => {
                // Existing physical-input semantics begin only here.
                broker_stream_observations(
                    &mut io,
                    parent_process,
                    key_vk,
                    authority_epoch,
                    key_revision,
                )
            }

            Some(BrokerSessionMode::Maintenance) => {
                // Existing authenticated maintenance dispatch. A3B2 adds only
                // protected Broker staging; Raw Input and Task authority stay
                // outside this path.
                broker_receive_maintenance_transport(&mut io, identity)
            }

            Some(BrokerSessionMode::Uninstall) => {
                // Destructive authority teardown is reachable only after the
                // same bidirectional process/session/image/nonce authentication
                // as INPUT and MAINTENANCE.
                broker_receive_uninstall_transport(&mut io, identity)
            }

            None => false,
        }
    };

    unsafe {
        let _ = CloseHandle(pipe);
        let _ = CloseHandle(parent_process);
    }

    if completed {
        0
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker_install::protected_broker_path_for_test;
    use std::cell::{Cell, RefCell};

    const TASK_TEST_USER_SID: &str = "S-1-5-21-111111111-222222222-333333333-1001";
    const TOKEN_IMPERSONATION_LEVEL: u32 = 9;
    const SECURITY_IDENTIFICATION_LEVEL: u32 = 1;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentThread() -> Handle;
    }

    #[link(name = "advapi32")]
    extern "system" {
        fn ImpersonateNamedPipeClient(named_pipe: Handle) -> i32;
        fn RevertToSelf() -> i32;
        fn OpenThreadToken(
            thread: Handle,
            desired_access: u32,
            open_as_self: i32,
            token: *mut Handle,
        ) -> i32;
    }

    struct RevertToSelfGuard {
        active: bool,
    }

    impl RevertToSelfGuard {
        fn new() -> Self {
            Self { active: true }
        }

        fn revert(&mut self) -> bool {
            if !self.active {
                return true;
            }

            if unsafe { RevertToSelf() } == 0 {
                return false;
            }

            self.active = false;
            true
        }
    }

    impl Drop for RevertToSelfGuard {
        fn drop(&mut self) {
            if self.active {
                unsafe {
                    let _ = RevertToSelf();
                }
            }
        }
    }

    fn connected_pipe_client_impersonation_level(pipe: Handle) -> Option<u32> {
        if unsafe { ImpersonateNamedPipeClient(pipe) } == 0 {
            return None;
        }

        let mut revert = RevertToSelfGuard::new();
        let mut token: Handle = core::ptr::null_mut();

        if unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) } == 0
            || token.is_null()
        {
            return None;
        }

        let mut level = u32::MAX;
        let mut returned = 0u32;
        let queried = unsafe {
            GetTokenInformation(
                token,
                TOKEN_IMPERSONATION_LEVEL,
                (&mut level as *mut u32).cast(),
                size_of::<u32>() as u32,
                &mut returned,
            ) != 0
        };

        unsafe {
            let _ = CloseHandle(token);
        }

        if !revert.revert() {
            return None;
        }

        if !queried || returned != size_of::<u32>() as u32 {
            return None;
        }

        Some(level)
    }

    #[test]
    fn broker_pipe_client_limits_server_to_security_identification() {
        let result = with_duplex_pipe(
            |pipe, parent| {
                let mut io = BrokerPipeIo::new(pipe, parent).expect("client I/O");
                let mut ack = [0u8; 1];

                io.write(&[0xA5]) && io.read(&mut ack) && ack == [0x5A]
            },
            |server, event, stop, peer| {
                let mut ready = [0u8; 1];

                if read_exact_pipe_overlapped(server, &mut ready, event, stop, peer)
                    != PipeIoOutcome::Complete
                    || ready != [0xA5]
                {
                    return false;
                }

                if connected_pipe_client_impersonation_level(server)
                    != Some(SECURITY_IDENTIFICATION_LEVEL)
                {
                    return false;
                }

                write_all_pipe_overlapped(server, &[0x5A], event, stop, peer)
                    == PipeIoOutcome::Complete
            },
        );

        assert_eq!(result, (true, true));
    }

    #[test]
    fn canonical_broker_pipe_name_is_exact_local_nonce_bound() {
        let nonce = [0x3a; 16];
        let token = nonce_hex(&nonce);
        let expected = OsString::from(format!(r"\\.\pipe\LCR.InputBroker.{token}"));

        assert_eq!(canonical_broker_pipe_name(&nonce), expected);
        assert_eq!(
            validated_broker_pipe_name(expected.as_os_str(), &nonce),
            Some(expected.as_os_str())
        );
    }

    #[test]
    fn broker_pipe_name_validation_rejects_noncanonical_endpoint_before_open() {
        let nonce = [0x3a; 16];
        let token = nonce_hex(&nonce);
        let wrong_nonce = [0x3b; 16];

        let mut embedded_nul = format!(r"\\.\pipe\LCR.InputBroker.{token}");
        embedded_nul.push('\0');
        embedded_nul.push_str("suffix");

        let invalid = [
            OsString::from(format!(r"\\localhost\pipe\LCR.InputBroker.{token}")),
            OsString::from(format!(r"\\server\pipe\LCR.InputBroker.{token}")),
            OsString::from(format!(r"\\?\pipe\LCR.InputBroker.{token}")),
            OsString::from(format!(r"\Device\NamedPipe\LCR.InputBroker.{token}")),
            OsString::from(format!(r"\\.\PIPE\LCR.InputBroker.{token}")),
            OsString::from(format!(
                r"\\.\pipe\LCR.InputBroker.{}",
                token.to_ascii_uppercase()
            )),
            OsString::from(format!(r"\\.\pipe\LCR.InputBroker.{token}.extra")),
            OsString::from(embedded_nul),
            canonical_broker_pipe_name(&wrong_nonce),
        ];

        for candidate in invalid {
            assert!(
                validated_broker_pipe_name(candidate.as_os_str(), &nonce).is_none(),
                "unexpectedly accepted noncanonical Broker endpoint: {candidate:?}"
            );
        }
    }

    fn task_migration_test_contracts(
    ) -> (BrokerTaskContract, ProtectedBrokerPath, BrokerTaskContract) {
        let program_files = Path::new(r"C:\Program Files");
        let old_target =
            protected_broker_path_for_test(program_files, "0.0.4-beta").expect("old target");
        let new_target =
            protected_broker_path_for_test(program_files, "0.0.5-beta").expect("new target");
        let old_contract =
            BrokerTaskContract::new(TASK_TEST_USER_SID, &old_target).expect("old task contract");
        let new_contract =
            BrokerTaskContract::new(TASK_TEST_USER_SID, &new_target).expect("new task contract");

        (old_contract, new_target, new_contract)
    }

    fn verified_maintenance_target_for_test(payload_length: u64) -> VerifiedProtectedBrokerTarget {
        let (_, target, _) = task_migration_test_contracts();

        VerifiedProtectedBrokerTarget {
            user_sid: TASK_TEST_USER_SID.to_owned(),
            target,
            payload_length,
        }
    }

    fn maintenance_identity_for_test() -> HandshakeIdentity {
        HandshakeIdentity {
            parent_pid: 501,
            broker_pid: 502,
            session_id: 503,
            key_vk: 5,
            nonce: [0x8a; 16],
        }
    }

    #[derive(Default)]
    struct RecordingParentWireIo {
        writes: Vec<Vec<u8>>,
        reads: VecDeque<Vec<u8>>,
    }

    impl RecordingParentWireIo {
        fn with_reads(reads: impl IntoIterator<Item = Vec<u8>>) -> Self {
            Self {
                writes: Vec::new(),
                reads: reads.into_iter().collect(),
            }
        }
    }

    impl ParentWireIo for RecordingParentWireIo {
        fn read_exact(&mut self, buffer: &mut [u8]) -> PipeIoOutcome {
            let Some(next) = self.reads.pop_front() else {
                return PipeIoOutcome::Failed;
            };

            if next.len() != buffer.len() {
                return PipeIoOutcome::Failed;
            }

            buffer.copy_from_slice(&next);
            PipeIoOutcome::Complete
        }

        fn write_all(&mut self, buffer: &[u8]) -> PipeIoOutcome {
            self.writes.push(buffer.to_vec());
            PipeIoOutcome::Complete
        }
    }

    fn assert_old_authority_precondition_failure(recognition: Result<OldBrokerTaskAuthority, ()>) {
        let (_, target, _) = task_migration_test_contracts();
        let register_calls = Cell::new(0usize);

        let migrated = migrate_broker_task_with(
            TASK_TEST_USER_SID,
            &target,
            |_| recognition,
            |_| {
                register_calls.set(register_calls.get() + 1);
                Ok(())
            },
            |_| Ok(BrokerTaskVerification::Exact),
        );

        assert_eq!(migrated, BrokerTaskMigrationOutcome::FailedBeforeMutation);
        assert_eq!(register_calls.get(), 0);
    }

    fn assert_new_verification_failure_rolls_back(
        new_verification: Result<BrokerTaskVerification, ()>,
    ) {
        let (old_contract, target, new_contract) = task_migration_test_contracts();
        let events = RefCell::new(Vec::new());

        let migrated = migrate_broker_task_with(
            TASK_TEST_USER_SID,
            &target,
            |_| Ok(OldBrokerTaskAuthority::Exact(old_contract.clone())),
            |contract| {
                events.borrow_mut().push(if contract == &new_contract {
                    "register_new"
                } else {
                    assert_eq!(contract, &old_contract);
                    "register_old"
                });
                Ok(())
            },
            |contract| {
                if contract == &new_contract {
                    events.borrow_mut().push("verify_new");
                    new_verification
                } else {
                    assert_eq!(contract, &old_contract);
                    events.borrow_mut().push("verify_old");
                    Ok(BrokerTaskVerification::Exact)
                }
            },
        );

        assert_eq!(
            migrated,
            BrokerTaskMigrationOutcome::FailedWithExactRollback
        );
        assert_eq!(
            events.into_inner(),
            ["register_new", "verify_new", "register_old", "verify_old"]
        );
    }

    fn assert_rollback_verification_failure_fails_closed(
        rollback_verification: Result<BrokerTaskVerification, ()>,
    ) {
        let (old_contract, target, new_contract) = task_migration_test_contracts();
        let events = RefCell::new(Vec::new());

        let migrated = migrate_broker_task_with(
            TASK_TEST_USER_SID,
            &target,
            |_| Ok(OldBrokerTaskAuthority::Exact(old_contract.clone())),
            |contract| {
                events.borrow_mut().push(if contract == &new_contract {
                    "register_new"
                } else {
                    assert_eq!(contract, &old_contract);
                    "register_old"
                });
                Ok(())
            },
            |contract| {
                if contract == &new_contract {
                    events.borrow_mut().push("verify_new");
                    Ok(BrokerTaskVerification::Mismatch)
                } else {
                    assert_eq!(contract, &old_contract);
                    events.borrow_mut().push("verify_old");
                    rollback_verification
                }
            },
        );

        assert_eq!(
            migrated,
            BrokerTaskMigrationOutcome::FailedWithUnprovenRollback
        );
        assert_eq!(
            events.into_inner(),
            ["register_new", "verify_new", "register_old", "verify_old"]
        );
    }

    #[test]
    fn task_migration_rejects_absent_old_authority_before_registration() {
        assert_old_authority_precondition_failure(Ok(OldBrokerTaskAuthority::Absent));
    }

    #[test]
    fn task_migration_rejects_mismatched_old_authority_before_registration() {
        assert_old_authority_precondition_failure(Ok(OldBrokerTaskAuthority::Mismatch));
    }

    #[test]
    fn task_migration_rejects_old_authority_read_error_before_registration() {
        assert_old_authority_precondition_failure(Err(()));
    }

    #[test]
    fn task_migration_commits_exact_new_authority_without_rollback() {
        let (old_contract, target, new_contract) = task_migration_test_contracts();
        let events = RefCell::new(Vec::new());

        let migrated = migrate_broker_task_with(
            TASK_TEST_USER_SID,
            &target,
            |_| Ok(OldBrokerTaskAuthority::Exact(old_contract)),
            |contract| {
                assert_eq!(contract, &new_contract);
                events.borrow_mut().push("register_new");
                Ok(())
            },
            |contract| {
                assert_eq!(contract, &new_contract);
                events.borrow_mut().push("verify_new");
                Ok(BrokerTaskVerification::Exact)
            },
        );

        assert_eq!(migrated, BrokerTaskMigrationOutcome::Committed);
        assert_eq!(events.into_inner(), ["register_new", "verify_new"]);
    }

    #[test]
    fn new_registration_error_attempts_exact_old_rollback_and_still_fails() {
        let (old_contract, target, new_contract) = task_migration_test_contracts();
        let events = RefCell::new(Vec::new());

        let migrated = migrate_broker_task_with(
            TASK_TEST_USER_SID,
            &target,
            |_| Ok(OldBrokerTaskAuthority::Exact(old_contract.clone())),
            |contract| {
                if contract == &new_contract {
                    events.borrow_mut().push("register_new");
                    Err(())
                } else {
                    assert_eq!(contract, &old_contract);
                    events.borrow_mut().push("register_old");
                    Ok(())
                }
            },
            |contract| {
                assert_eq!(contract, &old_contract);
                events.borrow_mut().push("verify_old");
                Ok(BrokerTaskVerification::Exact)
            },
        );

        assert_eq!(
            migrated,
            BrokerTaskMigrationOutcome::FailedWithExactRollback
        );
        assert_eq!(
            events.into_inner(),
            ["register_new", "register_old", "verify_old"]
        );
    }

    #[test]
    fn new_verification_absent_attempts_exact_old_rollback() {
        assert_new_verification_failure_rolls_back(Ok(BrokerTaskVerification::Absent));
    }

    #[test]
    fn new_verification_mismatch_attempts_exact_old_rollback() {
        assert_new_verification_failure_rolls_back(Ok(BrokerTaskVerification::Mismatch));
    }

    #[test]
    fn new_verification_error_attempts_exact_old_rollback() {
        assert_new_verification_failure_rolls_back(Err(()));
    }

    #[test]
    fn rollback_registration_error_fails_closed_without_rollback_readback() {
        let (old_contract, target, new_contract) = task_migration_test_contracts();
        let events = RefCell::new(Vec::new());

        let migrated = migrate_broker_task_with(
            TASK_TEST_USER_SID,
            &target,
            |_| Ok(OldBrokerTaskAuthority::Exact(old_contract.clone())),
            |contract| {
                if contract == &new_contract {
                    events.borrow_mut().push("register_new");
                    Ok(())
                } else {
                    assert_eq!(contract, &old_contract);
                    events.borrow_mut().push("register_old");
                    Err(())
                }
            },
            |contract| {
                assert_eq!(contract, &new_contract);
                events.borrow_mut().push("verify_new");
                Ok(BrokerTaskVerification::Mismatch)
            },
        );

        assert_eq!(
            migrated,
            BrokerTaskMigrationOutcome::FailedWithUnprovenRollback
        );
        assert_eq!(
            events.into_inner(),
            ["register_new", "verify_new", "register_old"]
        );
    }

    #[test]
    fn rollback_verification_absent_fails_closed() {
        assert_rollback_verification_failure_fails_closed(Ok(BrokerTaskVerification::Absent));
    }

    #[test]
    fn rollback_verification_mismatch_fails_closed() {
        assert_rollback_verification_failure_fails_closed(Ok(BrokerTaskVerification::Mismatch));
    }

    #[test]
    fn rollback_verification_error_fails_closed() {
        assert_rollback_verification_failure_fails_closed(Err(()));
    }

    #[test]
    fn maintenance_completion_stops_before_committed_write_when_staging_fails() {
        let events = RefCell::new(Vec::new());

        let completed = complete_broker_maintenance_with(
            maintenance_identity_for_test(),
            || {
                events.borrow_mut().push("finish_staging");
                Err(io::Error::new(io::ErrorKind::InvalidData, "test failure"))
            },
            |_| {
                events.borrow_mut().push("migrate_task");
                true
            },
            |_| {
                events.borrow_mut().push("write_committed");
                true
            },
        );

        assert!(!completed);
        assert_eq!(events.into_inner(), ["finish_staging"]);
    }

    #[test]
    fn maintenance_completion_stops_before_committed_write_when_migration_fails() {
        let events = RefCell::new(Vec::new());

        let completed = complete_broker_maintenance_with(
            maintenance_identity_for_test(),
            || {
                events.borrow_mut().push("finish_staging");
                Ok(verified_maintenance_target_for_test(9_137))
            },
            |_| {
                events.borrow_mut().push("migrate_task");
                false
            },
            |_| {
                events.borrow_mut().push("write_committed");
                true
            },
        );

        assert!(!completed);
        assert_eq!(events.into_inner(), ["finish_staging", "migrate_task"]);
    }

    #[test]
    fn maintenance_completion_orders_staging_migration_then_committed_write() {
        let identity = maintenance_identity_for_test();
        let payload_length = 9_137;
        let events = RefCell::new(Vec::new());

        let completed = complete_broker_maintenance_with(
            identity,
            || {
                events.borrow_mut().push("finish_staging");
                Ok(verified_maintenance_target_for_test(payload_length))
            },
            |_| {
                events.borrow_mut().push("migrate_task");
                true
            },
            |frame| {
                events.borrow_mut().push("write_committed");
                assert_eq!(
                    decode_control_frame(frame),
                    Some((FRAME_MAINTENANCE_COMMITTED, identity, payload_length))
                );
                true
            },
        );

        assert!(completed);
        assert_eq!(
            events.into_inner(),
            ["finish_staging", "migrate_task", "write_committed"]
        );
    }

    #[test]
    fn committed_write_failure_after_exact_migration_does_not_rollback_task() {
        let identity = maintenance_identity_for_test();
        let payload_length = 9_137;
        let (old_contract, _, new_contract) = task_migration_test_contracts();
        let registrations = RefCell::new(Vec::new());
        let committed_writes = Cell::new(0usize);

        let completed = complete_broker_maintenance_with(
            identity,
            || Ok(verified_maintenance_target_for_test(payload_length)),
            |target| {
                matches!(
                    migrate_broker_task_with(
                        &target.user_sid,
                        &target.target,
                        |_| Ok(OldBrokerTaskAuthority::Exact(old_contract)),
                        |contract| {
                            registrations.borrow_mut().push(contract.clone());
                            Ok(())
                        },
                        |contract| {
                            assert_eq!(contract, &new_contract);
                            Ok(BrokerTaskVerification::Exact)
                        },
                    ),
                    BrokerTaskMigrationOutcome::Committed
                )
            },
            |frame| {
                committed_writes.set(committed_writes.get() + 1);
                assert_eq!(
                    decode_control_frame(frame),
                    Some((FRAME_MAINTENANCE_COMMITTED, identity, payload_length))
                );
                false
            },
        );

        assert!(!completed);
        assert_eq!(committed_writes.get(), 1);
        assert_eq!(registrations.into_inner(), [new_contract]);
    }

    // Actual Windows duplex I/O, with a test-only deadline that disconnects the
    // server before joining. A synchronous-client regression must fail a test,
    // never leave cargo test hung in a blocking read/write.
    fn with_duplex_pipe(
        client_action: impl FnOnce(Handle, Handle) -> bool + Send + 'static,
        server_action: impl FnOnce(Handle, Handle, Handle, Handle) -> bool,
    ) -> (bool, bool) {
        let name = OsString::from(format!(
            r"\\.\pipe\LCR.InputBroker.DuplexTest.{}",
            nonce_hex(&random_nonce().expect("system RNG"))
        ));
        let server = create_private_pipe(&name).expect("private test pipe");
        let events: Vec<Handle> = (0..4)
            .map(|_| unsafe { CreateEventW(core::ptr::null(), 1, 0, core::ptr::null()) })
            .collect();
        assert!(events.iter().all(|event| !event.is_null()));
        let (io_event, stop, peer, done) = (events[0], events[1], events[2], events[3]);
        let (server_raw, stop_raw, done_raw) = (server as usize, stop as usize, done as usize);
        let deadline = thread::spawn(move || {
            let timed_out =
                unsafe { WaitForSingleObject(done_raw as Handle, 3_000) } != WAIT_OBJECT_0;
            if timed_out {
                unsafe {
                    let _ = SetEvent(stop_raw as Handle);
                    let _ = DisconnectNamedPipe(server_raw as Handle);
                }
            }
            timed_out
        });
        let client = thread::spawn(move || {
            let Some(pipe) = open_pipe(&name) else {
                return false;
            };
            let result = client_action(pipe, stop_raw as Handle);
            unsafe {
                let _ = CloseHandle(pipe);
            }
            result
        });
        let server_ok = connect_pipe_server(server, io_event, stop, peer)
            == PipeIoOutcome::Complete
            && server_action(server, io_event, stop, peer);
        // Release pending client I/O on an ordinary server-side failure too.
        if !server_ok {
            unsafe {
                let _ = DisconnectNamedPipe(server);
            }
        }
        let client_ok = client.join().expect("client worker");
        unsafe {
            let _ = SetEvent(done);
        }
        let timed_out = deadline.join().expect("deadline worker");
        unsafe {
            let _ = DisconnectNamedPipe(server);
            for event in events {
                let _ = CloseHandle(event);
            }
            let _ = CloseHandle(server);
        }
        assert!(!timed_out, "duplex I/O exceeded test deadline");
        (client_ok, server_ok)
    }

    #[test]
    fn shared_parent_authentication_binds_actual_client_pid_session_image_and_hello() {
        let current_pid = unsafe { GetCurrentProcessId() };
        let session_id = process_session_id(current_pid).expect("current process session");
        let nonce = [0x53; 16];
        let identity = HandshakeIdentity {
            parent_pid: current_pid,
            broker_pid: current_pid,
            session_id,
            key_vk: MAINTENANCE_INERT_KEY_VK,
            nonce,
        };
        let expected_image = std::env::current_exe().expect("current test image");

        let result = with_duplex_pipe(
            move |pipe, parent| {
                let mut io = BrokerPipeIo::new(pipe, parent).expect("client I/O");

                if !io.write(&encode_frame(FRAME_HELLO, identity)) {
                    return false;
                }

                let mut ack = [0u8; FRAME_LEN];
                io.read(&mut ack) && frame_matches(&ack, FRAME_ACK, identity)
            },
            move |server, event, stop, _peer| {
                let authenticated = authenticate_connected_parent_pipe(
                    server,
                    event,
                    stop,
                    ParentAuthenticationExpectation {
                        parent_pid: current_pid,
                        session_id,
                        key_vk: MAINTENANCE_INERT_KEY_VK,
                        nonce,
                        broker_path: &expected_image,
                    },
                )
                .expect("shared parent authentication");

                let exact = authenticated.identity == identity;

                unsafe {
                    let _ = CloseHandle(authenticated.process);
                }

                exact
            },
        );

        assert_eq!(result, (true, true));
    }

    #[test]
    fn shared_parent_authentication_rejects_wrong_session_image_and_hello_identity() {
        let current_pid = unsafe { GetCurrentProcessId() };
        let session_id = process_session_id(current_pid).expect("current process session");
        let nonce = [0x64; 16];
        let identity = HandshakeIdentity {
            parent_pid: current_pid,
            broker_pid: current_pid,
            session_id,
            key_vk: MAINTENANCE_INERT_KEY_VK,
            nonce,
        };
        let wrong_image = std::env::current_exe()
            .expect("current test image")
            .with_file_name("wrong.exe");

        let wrong_image_result = with_duplex_pipe(
            move |pipe, parent| {
                let mut io = BrokerPipeIo::new(pipe, parent).expect("client I/O");
                io.write(&encode_frame(FRAME_HELLO, identity))
            },
            move |server, event, stop, _peer| {
                authenticate_connected_parent_pipe(
                    server,
                    event,
                    stop,
                    ParentAuthenticationExpectation {
                        parent_pid: current_pid,
                        session_id,
                        key_vk: MAINTENANCE_INERT_KEY_VK,
                        nonce,
                        broker_path: &wrong_image,
                    },
                )
                .is_err()
            },
        );

        assert_eq!(wrong_image_result, (true, true));

        let wrong_session = session_id ^ u32::MAX;
        let expected_image = std::env::current_exe().expect("current test image");

        let wrong_session_result = with_duplex_pipe(
            move |pipe, parent| {
                let mut io = BrokerPipeIo::new(pipe, parent).expect("client I/O");
                io.write(&encode_frame(FRAME_HELLO, identity))
            },
            move |server, event, stop, _peer| {
                authenticate_connected_parent_pipe(
                    server,
                    event,
                    stop,
                    ParentAuthenticationExpectation {
                        parent_pid: current_pid,
                        session_id: wrong_session,
                        key_vk: MAINTENANCE_INERT_KEY_VK,
                        nonce,
                        broker_path: &expected_image,
                    },
                )
                .is_err()
            },
        );

        assert_eq!(wrong_session_result, (true, true));

        let wrong_nonce = [0x65; 16];
        let expected_image = std::env::current_exe().expect("current test image");

        let wrong_identity_result = with_duplex_pipe(
            move |pipe, parent| {
                let mut io = BrokerPipeIo::new(pipe, parent).expect("client I/O");
                let wrong = HandshakeIdentity {
                    nonce: wrong_nonce,
                    ..identity
                };
                io.write(&encode_frame(FRAME_HELLO, wrong))
            },
            move |server, event, stop, _peer| {
                authenticate_connected_parent_pipe(
                    server,
                    event,
                    stop,
                    ParentAuthenticationExpectation {
                        parent_pid: current_pid,
                        session_id,
                        key_vk: MAINTENANCE_INERT_KEY_VK,
                        nonce,
                        broker_path: &expected_image,
                    },
                )
                .is_err()
            },
        );

        assert_eq!(wrong_identity_result, (true, true));
    }

    #[test]
    fn parent_maintenance_disconnect_before_committed_fails_closed() {
        let identity = maintenance_identity_for_test();
        let target_version = "0.0.5-beta";
        let manifest = b"bounded-signed-manifest".to_vec();
        let payload = (0..(MAINTENANCE_CHUNK_BYTES + 31))
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        let sent_manifest = manifest.clone();
        let sent_payload = payload.clone();

        let result = with_duplex_pipe(
            move |pipe, parent| {
                let mut io = BrokerPipeIo::new(pipe, parent).expect("client I/O");
                let mut frame = [0u8; FRAME_LEN];

                if !io.read(&mut frame)
                    || decode_session_mode(&frame, identity) != Some(BrokerSessionMode::Maintenance)
                    || !io.read(&mut frame)
                    || decode_control_frame(&frame)
                        != Some((
                            FRAME_MAINTENANCE_VERSION_LEN,
                            identity,
                            target_version.len() as u64,
                        ))
                {
                    return false;
                }

                let mut version = vec![0u8; target_version.len()];
                if !io.read(&mut version)
                    || version != target_version.as_bytes()
                    || !io.read(&mut frame)
                    || decode_control_frame(&frame)
                        != Some((
                            FRAME_MAINTENANCE_PAYLOAD_LEN,
                            identity,
                            sent_payload.len() as u64,
                        ))
                    || !io.read(&mut frame)
                    || decode_control_frame(&frame)
                        != Some((
                            FRAME_MAINTENANCE_MANIFEST_LEN,
                            identity,
                            sent_manifest.len() as u64,
                        ))
                {
                    return false;
                }

                let mut received_manifest = vec![0u8; sent_manifest.len()];
                let mut received_payload = vec![0u8; sent_payload.len()];

                if !io.read(&mut received_manifest)
                    || received_manifest != sent_manifest
                    || !io.read(&mut received_payload)
                    || received_payload != sent_payload
                {
                    return false;
                }

                io.write(&encode_control_frame(
                    FRAME_MAINTENANCE_RECEIVED,
                    identity,
                    received_payload.len() as u64,
                ))
                // Return without COMMITTED; dropping the pipe is the tested
                // disconnect boundary.
            },
            move |server, event, stop, peer| {
                let mut io = ParentPipeIo {
                    pipe: server,
                    event,
                    stop_event: stop,
                    peer_process: peer,
                };

                !parent_send_maintenance_transport(
                    &mut io,
                    identity,
                    target_version,
                    &manifest,
                    &payload,
                )
            },
        );

        assert_eq!(result, (true, true));
    }

    #[test]
    fn broker_pending_disconnect_read_allows_initial_down_refresh_and_up_writes() {
        let result = with_duplex_pipe(
            |pipe, parent| {
                let mut io = BrokerPipeIo::new(pipe, parent).expect("client I/O");
                let watch = BrokerDisconnectWatch::start(pipe).expect("disconnect read");
                // Pending read is established before ANY observation is written.
                if unsafe { WaitForSingleObject(watch.event(), 0) } != WAIT_TIMEOUT {
                    return false;
                }
                let mut sampler = BrokerSampler::new();
                let frames = [
                    sampler.initial_up(10, 1, 1).unwrap(),
                    sampler.edge(Edge::Down, 20, 1, 1).unwrap(),
                    sampler.refresh_down(84, 1, 1).unwrap(),
                    sampler.edge(Edge::Up, 90, 1, 1).unwrap(),
                ];
                if !frames
                    .into_iter()
                    .all(|frame| write_broker_observation(&mut io, frame))
                {
                    return false;
                }
                // The server sends one unexpected byte after reading all frames.
                // It must wake the fail-closed monitor, with no PTT authority.
                unsafe { WaitForSingleObject(watch.event(), 2_000) == WAIT_OBJECT_0 }
            },
            |server, event, stop, peer| {
                for (index, level) in [Edge::Up, Edge::Down, Edge::Down, Edge::Up]
                    .into_iter()
                    .enumerate()
                {
                    let mut frame = [0; OBSERVATION_FRAME_LEN];
                    if read_exact_pipe_overlapped(server, &mut frame, event, stop, peer)
                        != PipeIoOutcome::Complete
                    {
                        return false;
                    }
                    let Some(observation) = decode_observation(&frame) else {
                        return false;
                    };
                    if observation.level != level || observation.sequence != index as u64 + 1 {
                        return false;
                    }
                }
                write_all_pipe_overlapped(server, &[0], event, stop, peer)
                    == PipeIoOutcome::Complete
            },
        );
        assert_eq!(result, (true, true));
    }

    #[test]
    fn broker_disconnect_watch_drop_reaps_only_its_own_pending_read() {
        let result = with_duplex_pipe(
            |pipe, parent| {
                let mut io = BrokerPipeIo::new(pipe, parent).expect("client I/O");
                let watch = BrokerDisconnectWatch::start(pipe).expect("disconnect read");
                if unsafe { WaitForSingleObject(watch.event(), 0) } != WAIT_TIMEOUT {
                    return false;
                }
                drop(watch);
                // Cancelled read cannot poison subsequent writes on the pipe.
                io.write(&[42])
            },
            |server, event, stop, peer| {
                let mut byte = [0];
                read_exact_pipe_overlapped(server, &mut byte, event, stop, peer)
                    == PipeIoOutcome::Complete
                    && byte == [42]
            },
        );
        assert_eq!(result, (true, true));
    }

    #[test]
    fn broker_disconnect_watch_wakes_on_pipe_loss() {
        let result = with_duplex_pipe(
            |pipe, parent| {
                let mut io = BrokerPipeIo::new(pipe, parent).expect("client I/O");
                let watch = BrokerDisconnectWatch::start(pipe).expect("disconnect read");
                io.write(&[42])
                    && unsafe { WaitForSingleObject(watch.event(), 2_000) == WAIT_OBJECT_0 }
            },
            |server, event, stop, peer| {
                let mut byte = [0];
                let received = read_exact_pipe_overlapped(server, &mut byte, event, stop, peer)
                    == PipeIoOutcome::Complete;
                received && unsafe { DisconnectNamedPipe(server) != 0 }
            },
        );
        assert_eq!(result, (true, true));
    }

    #[test]
    fn broker_parent_loss_cancels_pending_handshake_read() {
        let result = with_duplex_pipe(
            |pipe, parent| {
                let mut io = BrokerPipeIo::new(pipe, parent).expect("client I/O");
                io.write(&[42]) && !io.read(&mut [0])
            },
            |server, event, stop, peer| {
                let mut byte = [0];
                let received = read_exact_pipe_overlapped(server, &mut byte, event, stop, peer)
                    == PipeIoOutcome::Complete;
                // The test event stands in for the exact parent's lifetime.
                received && unsafe { SetEvent(stop) != 0 }
            },
        );
        assert_eq!(result, (true, true));
    }

    #[test]
    fn broker_parent_loss_cancels_backpressured_write() {
        let result = with_duplex_pipe(
            |pipe, parent| {
                let mut io = BrokerPipeIo::new(pipe, parent).expect("client I/O");
                // Exceeds the test pipe's 4096-byte quota. Server intentionally
                // leaves this data unread and signals only parent disappearance.
                io.write(&[42]) && !io.write(&[0; 65_536])
            },
            |server, event, stop, peer| {
                let mut byte = [0];
                let received = read_exact_pipe_overlapped(server, &mut byte, event, stop, peer)
                    == PipeIoOutcome::Complete;
                received && unsafe { SetEvent(stop) != 0 }
            },
        );
        assert_eq!(result, (true, true));
    }

    #[test]
    fn wire_v2_session_modes_are_explicit_and_identity_bound() {
        let identity = HandshakeIdentity {
            parent_pid: 11,
            broker_pid: 22,
            session_id: 33,
            key_vk: 5,
            nonce: [0x5a; 16],
        };

        let input = encode_session_mode(BrokerSessionMode::Input, identity);

        let maintenance = encode_session_mode(BrokerSessionMode::Maintenance, identity);

        assert_eq!(WIRE_VERSION, 2);

        assert_eq!(
            decode_session_mode(&input, identity),
            Some(BrokerSessionMode::Input)
        );

        assert_eq!(
            decode_session_mode(&maintenance, identity),
            Some(BrokerSessionMode::Maintenance)
        );

        let wrong_identity = HandshakeIdentity {
            key_vk: 6,
            ..identity
        };

        assert_eq!(decode_session_mode(&input, wrong_identity), None);

        let mut nonzero_reserved = input;
        nonzero_reserved[47] = 1;

        assert_eq!(decode_session_mode(&nonzero_reserved, identity), None);
    }

    #[test]
    fn uninstall_session_mode_is_append_only_and_identity_bound() {
        let identity = maintenance_identity_for_test();
        let uninstall = encode_session_mode(BrokerSessionMode::Uninstall, identity);

        assert_eq!(WIRE_VERSION, 2);
        assert_eq!(FRAME_SESSION_INPUT, 4);
        assert_eq!(FRAME_SESSION_MAINTENANCE, 5);
        assert_eq!(FRAME_MAINTENANCE_COMMITTED, 10);
        assert_eq!(FRAME_SESSION_UNINSTALL, 11);
        assert_eq!(FRAME_UNINSTALL_COMMITTED, 12);

        assert_eq!(
            decode_session_mode(&uninstall, identity),
            Some(BrokerSessionMode::Uninstall)
        );

        let wrong_identity = HandshakeIdentity {
            key_vk: identity.key_vk + 1,
            ..identity
        };

        assert_eq!(decode_session_mode(&uninstall, wrong_identity), None);

        let mut nonzero_reserved = uninstall;
        nonzero_reserved[47] = 1;

        assert_eq!(decode_session_mode(&nonzero_reserved, identity), None);
    }

    #[test]
    fn uninstall_task_contract_must_bind_exact_user_and_target() {
        let program_files = Path::new(r"C:\Program Files");

        let target = protected_broker_path_for_test(program_files, "0.0.4-beta").expect("target");

        let other_target =
            protected_broker_path_for_test(program_files, "0.0.5-beta").expect("other target");

        let exact = BrokerTaskContract::new(TASK_TEST_USER_SID, &target).expect("exact contract");

        let wrong_target = BrokerTaskContract::new(TASK_TEST_USER_SID, &other_target)
            .expect("wrong-target contract");

        let wrong_user =
            BrokerTaskContract::new("S-1-5-21-111111111-222222222-333333333-1002", &target)
                .expect("wrong-user contract");

        assert!(uninstall_task_contract_matches(
            Some(&exact),
            TASK_TEST_USER_SID,
            &target
        ));

        assert!(!uninstall_task_contract_matches(
            None,
            TASK_TEST_USER_SID,
            &target
        ));

        assert!(!uninstall_task_contract_matches(
            Some(&wrong_target),
            TASK_TEST_USER_SID,
            &target
        ));

        assert!(!uninstall_task_contract_matches(
            Some(&wrong_user),
            TASK_TEST_USER_SID,
            &target
        ));
    }

    #[test]
    fn uninstall_commit_orders_preflight_delete_handoff_then_committed() {
        let identity = maintenance_identity_for_test();
        let events = RefCell::new(Vec::new());
        let committed_frame = RefCell::new(None);

        let completed = complete_broker_uninstall_with(
            identity,
            || {
                events.borrow_mut().push("preflight");
                Ok(())
            },
            || {
                events.borrow_mut().push("delete_task");
                Ok(())
            },
            || {
                events.borrow_mut().push("handoff");
                Ok(())
            },
            |frame| {
                events.borrow_mut().push("committed");
                *committed_frame.borrow_mut() = Some(*frame);
                true
            },
        );

        assert!(completed);
        assert_eq!(
            events.into_inner(),
            ["preflight", "delete_task", "handoff", "committed"]
        );

        assert_eq!(
            decode_control_frame(
                &committed_frame
                    .into_inner()
                    .expect("uninstall committed frame")
            ),
            Some((FRAME_UNINSTALL_COMMITTED, identity, 0))
        );
    }

    #[test]
    fn uninstall_preflight_failure_never_deletes_task_or_relaxes_acl() {
        let identity = maintenance_identity_for_test();
        let delete_calls = Cell::new(0usize);
        let handoff_calls = Cell::new(0usize);
        let committed_calls = Cell::new(0usize);

        let completed = complete_broker_uninstall_with(
            identity,
            || Err(io::Error::other("preflight rejected")),
            || {
                delete_calls.set(delete_calls.get() + 1);
                Ok(())
            },
            || {
                handoff_calls.set(handoff_calls.get() + 1);
                Ok(())
            },
            |_| {
                committed_calls.set(committed_calls.get() + 1);
                true
            },
        );

        assert!(!completed);
        assert_eq!(delete_calls.get(), 0);
        assert_eq!(handoff_calls.get(), 0);
        assert_eq!(committed_calls.get(), 0);
    }

    #[test]
    fn uninstall_task_delete_failure_never_relaxes_acl() {
        let identity = maintenance_identity_for_test();
        let handoff_calls = Cell::new(0usize);
        let committed_calls = Cell::new(0usize);

        let completed = complete_broker_uninstall_with(
            identity,
            || Ok(()),
            || Err(io::Error::other("Task deletion failed")),
            || {
                handoff_calls.set(handoff_calls.get() + 1);
                Ok(())
            },
            |_| {
                committed_calls.set(committed_calls.get() + 1);
                true
            },
        );

        assert!(!completed);
        assert_eq!(handoff_calls.get(), 0);
        assert_eq!(committed_calls.get(), 0);
    }

    #[test]
    fn uninstall_handoff_failure_after_task_delete_never_emits_committed() {
        let identity = maintenance_identity_for_test();
        let committed_calls = Cell::new(0usize);

        let completed = complete_broker_uninstall_with(
            identity,
            || Ok(()),
            || Ok(()),
            || Err(io::Error::other("ACL handoff failed")),
            |_| {
                committed_calls.set(committed_calls.get() + 1);
                true
            },
        );

        assert!(!completed);
        assert_eq!(committed_calls.get(), 0);
    }

    #[test]
    fn uninstall_lost_committed_ack_does_not_attempt_rollback() {
        let identity = maintenance_identity_for_test();
        let events = RefCell::new(Vec::new());

        let completed = complete_broker_uninstall_with(
            identity,
            || {
                events.borrow_mut().push("preflight");
                Ok(())
            },
            || {
                events.borrow_mut().push("delete_task");
                Ok(())
            },
            || {
                events.borrow_mut().push("handoff");
                Ok(())
            },
            |_| {
                events.borrow_mut().push("committed_write_failed");
                false
            },
        );

        assert!(!completed);
        assert_eq!(
            events.into_inner(),
            [
                "preflight",
                "delete_task",
                "handoff",
                "committed_write_failed"
            ]
        );
    }

    #[test]
    fn parent_uninstall_transport_sends_only_uninstall_mode_and_requires_exact_committed() {
        let identity = maintenance_identity_for_test();

        let committed = encode_control_frame(FRAME_UNINSTALL_COMMITTED, identity, 0).to_vec();

        let mut io = RecordingParentWireIo::with_reads([committed.clone()]);

        assert!(parent_send_uninstall_transport(&mut io, identity));

        assert_eq!(io.writes.len(), 1);

        assert_eq!(
            decode_session_mode(io.writes[0].as_slice().try_into().unwrap(), identity),
            Some(BrokerSessionMode::Uninstall)
        );

        let wrong_identity = HandshakeIdentity {
            key_vk: identity.key_vk + 1,
            ..identity
        };

        let mut wrong = RecordingParentWireIo::with_reads([encode_control_frame(
            FRAME_UNINSTALL_COMMITTED,
            wrong_identity,
            0,
        )
        .to_vec()]);

        assert!(!parent_send_uninstall_transport(&mut wrong, identity));

        let mut missing = RecordingParentWireIo::default();

        assert!(!parent_send_uninstall_transport(&mut missing, identity));
    }

    #[test]
    fn uninstall_persistent_readback_recovers_lost_committed() {
        let transport_calls = Cell::new(0usize);
        let readback_calls = Cell::new(0usize);

        let result = uninstall_then_persistent_readback(
            || {
                transport_calls.set(transport_calls.get() + 1);

                Err(io::Error::other("COMMITTED frame was lost"))
            },
            || {
                readback_calls.set(readback_calls.get() + 1);
                Ok(true)
            },
        );

        assert!(result.is_ok());
        assert_eq!(transport_calls.get(), 1);
        assert_eq!(readback_calls.get(), 1);
    }

    #[test]
    fn uninstall_transport_success_without_persistent_commit_fails_closed() {
        let result = uninstall_then_persistent_readback(|| Ok(()), || Ok(false));

        assert!(result.is_err());
    }

    #[test]
    fn uninstall_transport_failure_without_persistent_commit_remains_failure() {
        let result = uninstall_then_persistent_readback(
            || Err(io::Error::other("transport failed")),
            || Ok(false),
        );

        assert!(result.is_err());
    }

    #[test]
    fn uninstall_persistent_readback_error_is_never_hidden_by_transport_success() {
        let result = uninstall_then_persistent_readback(
            || Ok(()),
            || Err(io::Error::other("readback failed")),
        );

        assert!(result.is_err());
    }

    #[test]
    fn parent_input_session_still_sends_exact_session_input_frame() {
        let identity = maintenance_identity_for_test();
        let mut io = RecordingParentWireIo::default();

        assert_eq!(
            parent_send_session_mode(&mut io, BrokerSessionMode::Input, identity),
            PipeIoOutcome::Complete
        );

        assert_eq!(
            io.writes,
            vec![encode_frame(FRAME_SESSION_INPUT, identity).to_vec()]
        );
    }

    #[test]
    fn parent_maintenance_transport_orders_frames_and_bounds_payload_chunks() {
        let identity = maintenance_identity_for_test();
        let target_version = "0.0.5-beta";
        let manifest = b"authenticated-signed-manifest";
        let payload = (0..9_137usize)
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        let payload_length = payload.len() as u64;

        let receipt =
            encode_control_frame(FRAME_MAINTENANCE_RECEIVED, identity, payload_length).to_vec();
        let committed =
            encode_control_frame(FRAME_MAINTENANCE_COMMITTED, identity, payload_length).to_vec();
        let mut io = RecordingParentWireIo::with_reads([receipt, committed]);

        assert!(parent_send_maintenance_transport(
            &mut io,
            identity,
            target_version,
            manifest,
            &payload,
        ));

        assert_eq!(io.writes.len(), 9);
        assert_eq!(
            decode_session_mode(io.writes[0].as_slice().try_into().unwrap(), identity),
            Some(BrokerSessionMode::Maintenance)
        );
        assert_eq!(
            decode_control_frame(io.writes[1].as_slice().try_into().unwrap()),
            Some((
                FRAME_MAINTENANCE_VERSION_LEN,
                identity,
                target_version.len() as u64,
            ))
        );
        assert_eq!(io.writes[2], target_version.as_bytes());
        assert_eq!(
            decode_control_frame(io.writes[3].as_slice().try_into().unwrap()),
            Some((FRAME_MAINTENANCE_PAYLOAD_LEN, identity, payload_length))
        );
        assert_eq!(
            decode_control_frame(io.writes[4].as_slice().try_into().unwrap()),
            Some((
                FRAME_MAINTENANCE_MANIFEST_LEN,
                identity,
                manifest.len() as u64,
            ))
        );
        assert_eq!(io.writes[5], manifest);

        let payload_chunks = &io.writes[6..];
        assert_eq!(
            payload_chunks.iter().map(Vec::len).collect::<Vec<_>>(),
            [MAINTENANCE_CHUNK_BYTES, MAINTENANCE_CHUNK_BYTES, 945]
        );
        assert!(payload_chunks
            .iter()
            .all(|chunk| chunk.len() <= MAINTENANCE_CHUNK_BYTES));
        assert_eq!(payload_chunks.concat(), payload);
    }

    #[test]
    fn parent_maintenance_success_requires_received_then_committed_with_exact_identity() {
        let identity = maintenance_identity_for_test();
        let payload = b"maintenance-payload";
        let payload_length = payload.len() as u64;
        let received =
            encode_control_frame(FRAME_MAINTENANCE_RECEIVED, identity, payload_length).to_vec();
        let committed =
            encode_control_frame(FRAME_MAINTENANCE_COMMITTED, identity, payload_length).to_vec();

        let mut received_only = RecordingParentWireIo::with_reads([received.clone()]);
        assert!(!parent_send_maintenance_transport(
            &mut received_only,
            identity,
            "0.0.5-beta",
            b"signed-manifest",
            payload,
        ));

        let mut wrong_order =
            RecordingParentWireIo::with_reads([committed.clone(), received.clone()]);
        assert!(!parent_send_maintenance_transport(
            &mut wrong_order,
            identity,
            "0.0.5-beta",
            b"signed-manifest",
            payload,
        ));

        let wrong_identity = HandshakeIdentity {
            nonce: [0x7b; 16],
            ..identity
        };
        let wrong_received =
            encode_control_frame(FRAME_MAINTENANCE_RECEIVED, wrong_identity, payload_length)
                .to_vec();
        let mut wrong_identity_io =
            RecordingParentWireIo::with_reads([wrong_received, committed.clone()]);
        assert!(!parent_send_maintenance_transport(
            &mut wrong_identity_io,
            identity,
            "0.0.5-beta",
            b"signed-manifest",
            payload,
        ));

        let wrong_committed =
            encode_control_frame(FRAME_MAINTENANCE_RECEIVED, identity, payload_length).to_vec();
        let mut wrong_kind_io = RecordingParentWireIo::with_reads([received, wrong_committed]);
        assert!(!parent_send_maintenance_transport(
            &mut wrong_kind_io,
            identity,
            "0.0.5-beta",
            b"signed-manifest",
            payload,
        ));
    }

    #[test]
    fn authenticated_maintenance_metadata_binds_exact_outer_version_and_length() {
        let metadata = AuthenticatedMaintenanceMetadata {
            target_version: "0.0.5-beta".to_owned(),
            payload_length: 9_137,
            payload_sha256: [0x5a; 32],
        };

        assert!(maintenance_metadata_matches_outer(
            &metadata,
            "0.0.5-beta",
            9_137,
        ));

        assert!(!maintenance_metadata_matches_outer(
            &metadata,
            "0.0.6-beta",
            9_137,
        ));

        assert!(!maintenance_metadata_matches_outer(
            &metadata,
            "0.0.5-beta",
            9_138,
        ));
    }

    #[test]
    fn maintenance_transport_bounds_are_explicit() {
        assert_eq!(WIRE_VERSION, 2);

        assert_eq!(FRAME_MAINTENANCE_VERSION_LEN, 6);
        assert_eq!(FRAME_MAINTENANCE_PAYLOAD_LEN, 7);
        assert_eq!(FRAME_MAINTENANCE_RECEIVED, 8);
        assert_eq!(FRAME_MAINTENANCE_MANIFEST_LEN, 9);
        assert_eq!(FRAME_MAINTENANCE_COMMITTED, 10);

        assert_eq!(MAINTENANCE_MAX_VERSION_BYTES, 128);
        assert_eq!(MAINTENANCE_MAX_PAYLOAD_BYTES, 64 * 1024 * 1024);
        assert_eq!(MAINTENANCE_CHUNK_BYTES, 4096);
        assert_eq!(BROKER_SIGNED_MANIFEST_MAX_BYTES, 244);

        assert!(!maintenance_version_length_valid(0));
        assert!(maintenance_version_length_valid(1));
        assert!(maintenance_version_length_valid(
            MAINTENANCE_MAX_VERSION_BYTES as u64,
        ));
        assert!(!maintenance_version_length_valid(
            MAINTENANCE_MAX_VERSION_BYTES as u64 + 1,
        ));

        assert!(!maintenance_payload_length_valid(0));
        assert!(maintenance_payload_length_valid(1));
        assert!(maintenance_payload_length_valid(
            MAINTENANCE_MAX_PAYLOAD_BYTES,
        ));
        assert!(!maintenance_payload_length_valid(
            MAINTENANCE_MAX_PAYLOAD_BYTES + 1,
        ));

        assert!(!maintenance_manifest_length_valid(0));
        assert!(maintenance_manifest_length_valid(1));
        assert!(maintenance_manifest_length_valid(
            BROKER_SIGNED_MANIFEST_MAX_BYTES as u64,
        ));
        assert!(!maintenance_manifest_length_valid(
            BROKER_SIGNED_MANIFEST_MAX_BYTES as u64 + 1,
        ));
    }

    #[test]
    fn committed_control_frame_round_trip_preserves_identity_and_signed_length() {
        let identity = maintenance_identity_for_test();
        let signed_payload_length = 9_137;
        let frame =
            encode_control_frame(FRAME_MAINTENANCE_COMMITTED, identity, signed_payload_length);

        assert_eq!(
            decode_control_frame(&frame),
            Some((FRAME_MAINTENANCE_COMMITTED, identity, signed_payload_length,))
        );
    }

    #[test]
    fn authenticated_maintenance_sequence_streams_exact_payload_without_mutation() {
        let identity = HandshakeIdentity {
            parent_pid: 101,
            broker_pid: 202,
            session_id: 303,
            key_vk: 5,
            nonce: [0x3c; 16],
        };

        let target_version = "0.0.5-beta";

        let payload = (0..9_137usize)
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();

        let expected_payload = payload.clone();
        let sent_payload = payload.clone();
        let expected_payload_length = payload.len() as u64;
        let expected_payload_sha256 = [0x5a; 32];

        let manifest = b"test-authenticated-broker-manifest".to_vec();
        let expected_manifest = manifest.clone();
        let sent_manifest = manifest.clone();

        let result = with_duplex_pipe(
            move |pipe, parent| {
                let mut io = BrokerPipeIo::new(pipe, parent).expect("client I/O");

                let hello = encode_frame(FRAME_HELLO, identity);

                if !io.write(&hello) {
                    return false;
                }

                let mut ack = [0u8; FRAME_LEN];

                if !io.read(&mut ack) || !frame_matches(&ack, FRAME_ACK, identity) {
                    return false;
                }

                let mut session = [0u8; FRAME_LEN];

                if !io.read(&mut session)
                    || decode_session_mode(&session, identity)
                        != Some(BrokerSessionMode::Maintenance)
                {
                    return false;
                }

                let Some((authenticated, (received, chunk_lengths, prepared_before_body))) =
                    broker_receive_maintenance_transport_with_sink_and_verifier(
                        &mut io,
                        identity,
                        |candidate_manifest| {
                            if candidate_manifest != expected_manifest.as_slice() {
                                return None;
                            }

                            Some(AuthenticatedMaintenanceMetadata {
                                target_version: target_version.to_owned(),
                                payload_length: expected_payload_length,
                                payload_sha256: expected_payload_sha256,
                            })
                        },
                        |_| Some((Vec::new(), Vec::new(), true)),
                        |state, chunk| {
                            state.0.extend_from_slice(chunk);
                            state.1.push(chunk.len());
                            true
                        },
                    )
                else {
                    return false;
                };

                authenticated.target_version == target_version
                    && authenticated.payload_length == expected_payload_length
                    && authenticated.payload_sha256 == expected_payload_sha256
                    && received == expected_payload
                    && chunk_lengths == [4096, 4096, 945]
                    && prepared_before_body
            },
            move |server, event, stop, peer| {
                let mut hello = [0u8; FRAME_LEN];

                if read_exact_pipe_overlapped(server, &mut hello, event, stop, peer)
                    != PipeIoOutcome::Complete
                    || !frame_matches(&hello, FRAME_HELLO, identity)
                {
                    return false;
                }

                let ack = encode_frame(FRAME_ACK, identity);

                if write_all_pipe_overlapped(server, &ack, event, stop, peer)
                    != PipeIoOutcome::Complete
                {
                    return false;
                }

                let mode = encode_session_mode(BrokerSessionMode::Maintenance, identity);

                if write_all_pipe_overlapped(server, &mode, event, stop, peer)
                    != PipeIoOutcome::Complete
                {
                    return false;
                }

                let version_len = encode_control_frame(
                    FRAME_MAINTENANCE_VERSION_LEN,
                    identity,
                    target_version.len() as u64,
                );

                if write_all_pipe_overlapped(server, &version_len, event, stop, peer)
                    != PipeIoOutcome::Complete
                    || write_all_pipe_overlapped(
                        server,
                        target_version.as_bytes(),
                        event,
                        stop,
                        peer,
                    ) != PipeIoOutcome::Complete
                {
                    return false;
                }

                let payload_len = encode_control_frame(
                    FRAME_MAINTENANCE_PAYLOAD_LEN,
                    identity,
                    sent_payload.len() as u64,
                );

                if write_all_pipe_overlapped(server, &payload_len, event, stop, peer)
                    != PipeIoOutcome::Complete
                {
                    return false;
                }

                let manifest_len = encode_control_frame(
                    FRAME_MAINTENANCE_MANIFEST_LEN,
                    identity,
                    sent_manifest.len() as u64,
                );

                if write_all_pipe_overlapped(server, &manifest_len, event, stop, peer)
                    != PipeIoOutcome::Complete
                    || write_all_pipe_overlapped(server, &sent_manifest, event, stop, peer)
                        != PipeIoOutcome::Complete
                {
                    return false;
                }

                if write_all_pipe_overlapped(server, &sent_payload, event, stop, peer)
                    != PipeIoOutcome::Complete
                {
                    return false;
                }

                let mut receipt = [0u8; FRAME_LEN];

                if read_exact_pipe_overlapped(server, &mut receipt, event, stop, peer)
                    != PipeIoOutcome::Complete
                {
                    return false;
                }

                matches!(
                    decode_control_frame(&receipt),
                    Some((
                        FRAME_MAINTENANCE_RECEIVED,
                        receipt_identity,
                        receipt_bytes,
                    ))
                        if receipt_identity == identity
                            && receipt_bytes
                                == sent_payload.len() as u64
                )
            },
        );

        assert_eq!(result, (true, true));
    }

    #[test]
    fn pre_body_staging_failure_stops_before_payload_sink() {
        let identity = HandshakeIdentity {
            parent_pid: 321,
            broker_pid: 322,
            session_id: 323,
            key_vk: 5,
            nonce: [0x4d; 16],
        };
        let manifest = b"authenticated-test-manifest".to_vec();
        let sent_manifest = manifest.clone();

        let result = with_duplex_pipe(
            move |pipe, parent| {
                let mut io = BrokerPipeIo::new(pipe, parent).expect("client I/O");
                let mut sink_bytes = 0usize;

                broker_receive_maintenance_transport_with_sink_and_verifier(
                    &mut io,
                    identity,
                    |candidate_manifest| {
                        (candidate_manifest == manifest.as_slice()).then_some(
                            AuthenticatedMaintenanceMetadata {
                                target_version: "0.0.5-beta".to_owned(),
                                payload_length: 32,
                                payload_sha256: [0x7a; 32],
                            },
                        )
                    },
                    |_| None::<()>,
                    |_, chunk| {
                        sink_bytes += chunk.len();
                        true
                    },
                )
                .is_none()
                    && sink_bytes == 0
            },
            move |server, event, stop, peer| {
                let version = b"0.0.5-beta";
                let version_len = encode_control_frame(
                    FRAME_MAINTENANCE_VERSION_LEN,
                    identity,
                    version.len() as u64,
                );
                let payload_len = encode_control_frame(FRAME_MAINTENANCE_PAYLOAD_LEN, identity, 32);
                let manifest_len = encode_control_frame(
                    FRAME_MAINTENANCE_MANIFEST_LEN,
                    identity,
                    sent_manifest.len() as u64,
                );
                let mut manifest_and_body = sent_manifest;
                manifest_and_body.extend_from_slice(&[0x55; 32]);

                write_all_pipe_overlapped(server, &version_len, event, stop, peer)
                    == PipeIoOutcome::Complete
                    && write_all_pipe_overlapped(server, version, event, stop, peer)
                        == PipeIoOutcome::Complete
                    && write_all_pipe_overlapped(server, &payload_len, event, stop, peer)
                        == PipeIoOutcome::Complete
                    && write_all_pipe_overlapped(server, &manifest_len, event, stop, peer)
                        == PipeIoOutcome::Complete
                    && write_all_pipe_overlapped(server, &manifest_and_body, event, stop, peer)
                        == PipeIoOutcome::Complete
            },
        );

        assert_eq!(result, (true, true));
    }

    #[test]
    fn observed_payload_must_match_signed_length_and_digest() {
        let payload = [0x35; MAINTENANCE_CHUNK_BYTES + 29];
        let digest: [u8; 32] = Sha256::digest(payload.as_slice()).into();
        let metadata = AuthenticatedMaintenanceMetadata {
            target_version: "0.0.5-beta".to_owned(),
            payload_length: payload.len() as u64,
            payload_sha256: digest,
        };

        let mut exact = ObservedMaintenancePayload::new(&metadata);
        exact.observe(&payload[..MAINTENANCE_CHUNK_BYTES]).unwrap();
        exact.observe(&payload[MAINTENANCE_CHUNK_BYTES..]).unwrap();
        exact.finish().unwrap();

        let mut short = ObservedMaintenancePayload::new(&metadata);
        short.observe(&payload[..MAINTENANCE_CHUNK_BYTES]).unwrap();
        assert!(short.finish().is_err());

        let mut wrong_digest_metadata = metadata.clone();
        wrong_digest_metadata.payload_sha256 = [0x99; 32];
        let mut wrong_digest = ObservedMaintenancePayload::new(&wrong_digest_metadata);
        wrong_digest.observe(&payload).unwrap();
        assert!(wrong_digest.finish().is_err());

        let mut overflow = ObservedMaintenancePayload::new(&AuthenticatedMaintenanceMetadata {
            payload_length: 1,
            ..metadata
        });
        assert!(overflow.observe(&[1, 2]).is_err());
    }

    #[test]
    fn executable_identity_rejects_length_hash_and_product_version_mismatch_without_deletion() {
        let directory = std::env::temp_dir().join(format!(
            "lcr-broker-maintenance-identity-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();

        let target = directory.join("LCRInputBroker.exe");
        let payload = [0x6b; MAINTENANCE_CHUNK_BYTES + 17];
        let digest: [u8; 32] = Sha256::digest(payload.as_slice()).into();
        fs::write(&target, payload.as_slice()).unwrap();

        assert!(executable_identity_matches_with_version_reader(
            &target,
            payload.len() as u64,
            &digest,
            "0.0.5-beta",
            |_| Ok("0.0.5-beta".to_owned()),
        )
        .unwrap());
        assert!(!executable_identity_matches_with_version_reader(
            &target,
            payload.len() as u64 + 1,
            &digest,
            "0.0.5-beta",
            |_| Ok("0.0.5-beta".to_owned()),
        )
        .unwrap());
        assert!(!executable_identity_matches_with_version_reader(
            &target,
            payload.len() as u64,
            &[0x41; 32],
            "0.0.5-beta",
            |_| Ok("0.0.5-beta".to_owned()),
        )
        .unwrap());
        assert!(!executable_identity_matches_with_version_reader(
            &target,
            payload.len() as u64,
            &digest,
            "0.0.5-beta",
            |_| Ok("0.0.6-beta".to_owned()),
        )
        .unwrap());

        assert_eq!(fs::read(&target).unwrap(), payload.as_slice());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn maintenance_receiver_rejects_invalid_publisher_manifest_before_body() {
        let identity = HandshakeIdentity {
            parent_pid: 351,
            broker_pid: 352,
            session_id: 353,
            key_vk: 5,
            nonce: [0x61; 16],
        };

        let invalid_manifest = b"not-a-valid-signed-broker-manifest".to_vec();

        let sent_manifest = invalid_manifest.clone();

        let result = with_duplex_pipe(
            move |pipe, parent| {
                let mut io = BrokerPipeIo::new(pipe, parent).expect("client I/O");

                let mut received = 0usize;

                broker_receive_maintenance_transport_with_sink(&mut io, identity, |chunk| {
                    received += chunk.len();
                    true
                })
                .is_none()
                    && received == 0
            },
            move |server, event, stop, peer| {
                let target_version = "0.0.5-beta";

                let version_len = encode_control_frame(
                    FRAME_MAINTENANCE_VERSION_LEN,
                    identity,
                    target_version.len() as u64,
                );

                if write_all_pipe_overlapped(server, &version_len, event, stop, peer)
                    != PipeIoOutcome::Complete
                    || write_all_pipe_overlapped(
                        server,
                        target_version.as_bytes(),
                        event,
                        stop,
                        peer,
                    ) != PipeIoOutcome::Complete
                {
                    return false;
                }

                let payload_len = encode_control_frame(FRAME_MAINTENANCE_PAYLOAD_LEN, identity, 32);

                if write_all_pipe_overlapped(server, &payload_len, event, stop, peer)
                    != PipeIoOutcome::Complete
                {
                    return false;
                }

                let manifest_len = encode_control_frame(
                    FRAME_MAINTENANCE_MANIFEST_LEN,
                    identity,
                    sent_manifest.len() as u64,
                );

                write_all_pipe_overlapped(server, &manifest_len, event, stop, peer)
                    == PipeIoOutcome::Complete
                    && write_all_pipe_overlapped(server, &sent_manifest, event, stop, peer)
                        == PipeIoOutcome::Complete
            },
        );

        assert_eq!(result, (true, true));
    }

    #[test]
    fn maintenance_receiver_rejects_oversized_payload_before_body() {
        let identity = HandshakeIdentity {
            parent_pid: 401,
            broker_pid: 402,
            session_id: 403,
            key_vk: 5,
            nonce: [0x77; 16],
        };

        let result = with_duplex_pipe(
            move |pipe, parent| {
                let mut io = BrokerPipeIo::new(pipe, parent).expect("client I/O");

                let mut received = 0usize;

                broker_receive_maintenance_transport_with_sink(&mut io, identity, |chunk| {
                    received += chunk.len();
                    true
                })
                .is_none()
                    && received == 0
            },
            move |server, event, stop, peer| {
                let version_len = encode_control_frame(
                    FRAME_MAINTENANCE_VERSION_LEN,
                    identity,
                    "0.0.5-beta".len() as u64,
                );

                if write_all_pipe_overlapped(server, &version_len, event, stop, peer)
                    != PipeIoOutcome::Complete
                    || write_all_pipe_overlapped(server, b"0.0.5-beta", event, stop, peer)
                        != PipeIoOutcome::Complete
                {
                    return false;
                }

                let oversized = encode_control_frame(
                    FRAME_MAINTENANCE_PAYLOAD_LEN,
                    identity,
                    MAINTENANCE_MAX_PAYLOAD_BYTES + 1,
                );

                write_all_pipe_overlapped(server, &oversized, event, stop, peer)
                    == PipeIoOutcome::Complete
            },
        );

        assert_eq!(result, (true, true));
    }

    #[test]
    fn task_demand_run_arguments_are_exact_six_and_ordered() {
        let pipe_name = std::ffi::OsString::from(r"\\.\pipe\LCR.InputBroker.test");

        let arguments = broker_task_run_arguments(
            1234,
            pipe_name.as_os_str(),
            "00112233445566778899aabbccddeeff",
            0x54,
            77,
            88,
        );

        assert_eq!(arguments.len(), 6);
        assert_eq!(
            arguments,
            [
                "1234".to_owned(),
                r"\\.\pipe\LCR.InputBroker.test".to_owned(),
                "00112233445566778899aabbccddeeff".to_owned(),
                "84".to_owned(),
                "77".to_owned(),
                "88".to_owned(),
            ]
        );

        assert!(arguments.iter().all(|argument| argument != BROKER_ARG));
    }

    #[test]
    fn maintenance_task_boundary_is_demand_run_only_with_inert_existing_slots() {
        let (old_contract, _, _) = task_migration_test_contracts();
        let pipe_name = OsString::from(r"\\.\pipe\LCR.InputBroker.maintenance-test");
        let arguments = broker_task_run_arguments(
            1234,
            pipe_name.as_os_str(),
            "00112233445566778899aabbccddeeff",
            MAINTENANCE_INERT_KEY_VK,
            MAINTENANCE_INERT_AUTHORITY_EPOCH,
            MAINTENANCE_INERT_KEY_REVISION,
        );
        let events = RefCell::new(Vec::new());

        let engine_pid = demand_run_maintenance_task_with(
            &old_contract,
            &arguments,
            |observed_contract, observed_arguments| {
                assert_eq!(observed_contract, &old_contract);
                assert_eq!(observed_arguments, &arguments);
                events.borrow_mut().push("demand_run");
                Ok(9123)
            },
        )
        .expect("maintenance demand-run");

        assert_eq!(engine_pid, 9123);
        assert_eq!(events.into_inner(), ["demand_run"]);
        assert_eq!(arguments.len(), 6);
        assert_eq!(arguments[3], "1");
        assert_eq!(arguments[4], "0");
        assert_eq!(arguments[5], "0");
    }

    #[test]
    fn production_parent_maintenance_rejects_untrusted_bundle_before_task_demand_run() {
        let (old_contract, _, _) = task_migration_test_contracts();

        let error = run_broker_maintenance_client(
            &old_contract,
            distribution::CURRENT_VERSION,
            b"not-a-publisher-signed-manifest",
            b"MZ-untrusted-payload",
        )
        .expect_err("untrusted bundle must fail before Task demand-run");

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn canonical_helper_parent_projection_is_distinct_from_broker() {
        let local_app_data = std::path::Path::new(r"C:\Users\Test\AppData\Local");

        let helper = canonical_helper_path_from_local_app_data(local_app_data);

        assert_eq!(
            helper,
            std::path::Path::new(r"C:\Users\Test\AppData\Local\Programs\LCR\LCR.exe")
        );

        assert_eq!(helper.file_name(), Some(std::ffi::OsStr::new("LCR.exe")));

        assert_ne!(
            helper.file_name(),
            Some(std::ffi::OsStr::new("LCRInputBroker.exe"))
        );
    }
    #[test]
    fn observation_frame_round_trip_is_exact_and_reserved_bytes_are_zero() {
        let observation = BrokerWireObservation {
            epoch: 11,
            key_revision: 22,
            sequence: 33,
            observed_tick: 44,
            level: Edge::Down,
        };

        let frame = encode_observation(observation);

        assert_eq!(
            u16::from_le_bytes(frame[6..8].try_into().unwrap()),
            FRAME_OBSERVATION
        );
        assert!(frame[41..].iter().all(|byte| *byte == 0));

        assert_eq!(decode_observation(&frame), Some(observation));

        let mut bad_level = frame;
        bad_level[40] = 2;
        assert_eq!(decode_observation(&bad_level), None);

        let mut bad_reserved = frame;
        bad_reserved[47] = 1;
        assert_eq!(decode_observation(&bad_reserved), None);

        let mut bad_kind = frame;
        bad_kind[6..8].copy_from_slice(&FRAME_ACK.to_le_bytes());
        assert_eq!(decode_observation(&bad_kind), None);
    }

    #[test]
    fn sampler_emits_initial_up_edges_and_explicit_down_refreshes() {
        let mut sampler = BrokerSampler::new();

        let initial_up = sampler.initial_up(100, 7, 9).expect("initial up");

        assert_eq!(initial_up.sequence, 1);
        assert_eq!(initial_up.level, Edge::Up);
        assert!(!sampler.is_down());

        assert_eq!(sampler.edge(Edge::Up, 101, 7, 9), None);

        let down = sampler.edge(Edge::Down, 102, 7, 9).expect("down edge");

        assert_eq!(down.sequence, 2);
        assert_eq!(down.level, Edge::Down);
        assert!(sampler.is_down());

        assert_eq!(sampler.edge(Edge::Down, 103, 7, 9), None);

        let refresh = sampler
            .refresh_down(200, 7, 9)
            .expect("positive down refresh");

        assert_eq!(refresh.sequence, 3);
        assert_eq!(refresh.level, Edge::Down);

        let up = sampler.edge(Edge::Up, 201, 7, 9).expect("up edge");

        assert_eq!(up.sequence, 4);
        assert_eq!(up.level, Edge::Up);
        assert!(!sampler.is_down());

        assert_eq!(sampler.refresh_down(202, 7, 9), None);
        assert_eq!(sampler.edge(Edge::Up, 203, 7, 9), None);
    }

    #[test]
    fn sampler_sequence_exhaustion_fails_closed_without_aliasing() {
        let mut sampler = BrokerSampler::new();
        sampler.next_sequence = u64::MAX;

        assert_eq!(sampler.initial_up(100, 1, 1), None);
        assert_eq!(sampler.edge(Edge::Down, 101, 1, 1), None);
        assert!(!sampler.is_down());
    }

    #[test]
    fn raw_input_mouse_mapping_preserves_selected_xbutton_edges() {
        assert_eq!(
            mouse_button_edges(RI_MOUSE_BUTTON_4_DOWN, VK_XBUTTON1 as u32),
            [Some(Edge::Down), None]
        );
        assert_eq!(
            mouse_button_edges(RI_MOUSE_BUTTON_4_UP, VK_XBUTTON1 as u32),
            [None, Some(Edge::Up)]
        );
        assert_eq!(
            mouse_button_edges(
                RI_MOUSE_BUTTON_4_DOWN | RI_MOUSE_BUTTON_4_UP,
                VK_XBUTTON1 as u32,
            ),
            [Some(Edge::Down), Some(Edge::Up)]
        );
        assert_eq!(
            mouse_button_edges(RI_MOUSE_BUTTON_5_DOWN, VK_XBUTTON1 as u32),
            [None, None]
        );
        assert_eq!(
            mouse_button_edges(RI_MOUSE_BUTTON_5_DOWN, VK_XBUTTON2 as u32),
            [Some(Edge::Down), None]
        );
    }

    #[test]
    fn raw_input_keyboard_mapping_filters_vk_break_and_overrun() {
        assert_eq!(keyboard_edge(0x1e, 0x54, 0, 0x54), Some(Edge::Down));
        assert_eq!(
            keyboard_edge(0x1e, 0x54, RI_KEY_BREAK, 0x54),
            Some(Edge::Up)
        );
        assert_eq!(keyboard_edge(0x1e, 0x55, 0, 0x54), None);
        assert_eq!(keyboard_edge(0x1e, 0, 0, 0x54), None);
        assert_eq!(keyboard_edge(0x1e, 255, 0, 0x54), None);
        assert_eq!(
            keyboard_edge(KEYBOARD_OVERRUN_MAKE_CODE, 0x54, 0, 0x54),
            None
        );
    }

    #[test]
    fn raw_input_registration_selects_only_required_device_class() {
        let mouse = broker_raw_input_device(123, VK_XBUTTON1 as u32, RIDEV_INPUTSINK);
        assert_eq!(mouse.usage_page, HID_USAGE_PAGE_GENERIC);
        assert_eq!(mouse.usage, HID_USAGE_GENERIC_MOUSE);
        assert_eq!(mouse.flags, RIDEV_INPUTSINK);
        assert_eq!(mouse.target, 123);

        let keyboard = broker_raw_input_device(456, 0x54, RIDEV_INPUTSINK);
        assert_eq!(keyboard.usage, HID_USAGE_GENERIC_KEYBOARD);
        assert_eq!(keyboard.target, 456);

        let remove = broker_raw_input_device(999, 0x54, RIDEV_REMOVE);
        assert_eq!(remove.flags, RIDEV_REMOVE);
        assert_eq!(remove.target, 0);
    }

    #[test]
    fn down_refresh_timeout_is_idle_infinite_and_held_deadline_bounded() {
        assert_eq!(down_refresh_timeout(100, None, false), INFINITE);
        assert_eq!(down_refresh_timeout(100, Some(164), true), 64);
        assert_eq!(down_refresh_timeout(200, Some(164), true), 0);

        assert!(!down_refresh_due(100, None, true));
        assert!(!down_refresh_due(100, Some(100), false));
        assert!(!down_refresh_due(99, Some(100), true));
        assert!(down_refresh_due(100, Some(100), true));
        assert!(down_refresh_due(101, Some(100), true));

        assert_eq!(BROKER_DOWN_REFRESH_MS, 64);
    }

    #[test]
    fn pending_queue_coalesces_refreshes_and_preserves_transitions() {
        let mut pending = PendingObservations::new();

        let down1 = BrokerWireObservation {
            epoch: 1,
            key_revision: 1,
            sequence: 1,
            observed_tick: 10,
            level: Edge::Down,
        };

        let mut down2 = down1;
        down2.sequence = 2;
        down2.observed_tick = 20;

        pending.push(down1);
        pending.push(down2);

        assert_eq!(pending.items.len(), 1);
        assert_eq!(pending.items.front(), Some(&down2));

        let up = BrokerWireObservation {
            sequence: 3,
            observed_tick: 30,
            level: Edge::Up,
            ..down2
        };

        pending.push(up);

        assert_eq!(pending.items.len(), 2);
        assert_eq!(pending.items[0].level, Edge::Down);
        assert_eq!(pending.items[1].level, Edge::Up);
        assert!(!pending.overflowed);
    }

    #[test]
    fn pending_queue_overflow_is_sticky_and_bounded() {
        let mut pending = PendingObservations::new();

        for sequence in 1..=(BROKER_PENDING_CAPACITY as u64 + 2) {
            pending.push(BrokerWireObservation {
                epoch: 1,
                key_revision: 1,
                sequence,
                observed_tick: sequence,
                level: if sequence % 2 == 0 {
                    Edge::Up
                } else {
                    Edge::Down
                },
            });
        }

        assert!(pending.overflowed);
        assert!(pending.items.len() <= BROKER_PENDING_CAPACITY);
        assert!(!pending.items.is_empty());
    }

    #[test]
    fn pending_push_requests_wake_only_when_consumer_progress_is_needed() {
        let mut pending = PendingObservations::new();

        let down1 = BrokerWireObservation {
            epoch: 1,
            key_revision: 1,
            sequence: 1,
            observed_tick: 10,
            level: Edge::Down,
        };

        assert!(pending.push(down1));

        let down2 = BrokerWireObservation {
            sequence: 2,
            observed_tick: 20,
            ..down1
        };

        // Same-level refresh replaces the queued DOWN. The existing wake is
        // already enough to make the consumer observe the newest proof.
        assert!(!pending.push(down2));

        assert_eq!(pending.poll(), BrokerPoll::Observation(down2));
        assert_eq!(pending.poll(), BrokerPoll::Empty);

        let up = BrokerWireObservation {
            sequence: 3,
            observed_tick: 30,
            level: Edge::Up,
            ..down1
        };

        // Once the consumer drained to empty, new data needs a fresh wake.
        assert!(pending.push(up));
    }

    #[test]
    fn pending_overflow_always_requests_a_fail_closed_wake() {
        let mut pending = PendingObservations::new();

        for sequence in 1..=BROKER_PENDING_CAPACITY as u64 {
            let observation = BrokerWireObservation {
                epoch: 5,
                key_revision: 7,
                sequence,
                observed_tick: sequence,
                level: if sequence % 2 == 0 {
                    Edge::Up
                } else {
                    Edge::Down
                },
            };

            let wake = pending.push(observation);

            if sequence == 1 {
                assert!(wake);
            } else {
                assert!(!wake);
            }
        }

        let overflow = BrokerWireObservation {
            epoch: 5,
            key_revision: 7,
            sequence: BROKER_PENDING_CAPACITY as u64 + 1,
            observed_tick: 99,
            level: Edge::Down,
        };

        assert!(pending.push(overflow));
        assert_eq!(pending.poll(), BrokerPoll::Overflowed);
        assert_eq!(pending.poll(), BrokerPoll::Observation(overflow));
    }

    #[test]
    fn pending_poll_returns_overflow_before_post_overflow_observation() {
        let mut pending = PendingObservations::new();

        for sequence in 1..=(BROKER_PENDING_CAPACITY as u64 + 1) {
            pending.push(BrokerWireObservation {
                epoch: 1,
                key_revision: 1,
                sequence,
                observed_tick: sequence,
                level: if sequence % 2 == 0 {
                    Edge::Up
                } else {
                    Edge::Down
                },
            });
        }

        assert_eq!(pending.poll(), BrokerPoll::Overflowed);

        assert!(matches!(pending.poll(), BrokerPoll::Observation(_)));
    }

    #[test]
    fn pending_poll_does_not_consume_observation_when_reporting_overflow() {
        let mut pending = PendingObservations::new();

        for sequence in 1..=(BROKER_PENDING_CAPACITY as u64 + 1) {
            pending.push(BrokerWireObservation {
                epoch: 7,
                key_revision: 9,
                sequence,
                observed_tick: sequence,
                level: if sequence % 2 == 0 {
                    Edge::Up
                } else {
                    Edge::Down
                },
            });
        }

        let expected = *pending.items.front().expect("post-overflow observation");

        assert_eq!(pending.poll(), BrokerPoll::Overflowed);
        assert_eq!(pending.poll(), BrokerPoll::Observation(expected));
        assert_eq!(pending.poll(), BrokerPoll::Empty);
    }

    #[test]
    fn pending_poll_without_overflow_preserves_transition_order() {
        let mut pending = PendingObservations::new();

        let down = BrokerWireObservation {
            epoch: 3,
            key_revision: 4,
            sequence: 1,
            observed_tick: 10,
            level: Edge::Down,
        };

        let up = BrokerWireObservation {
            sequence: 2,
            observed_tick: 20,
            level: Edge::Up,
            ..down
        };

        let next_down = BrokerWireObservation {
            sequence: 3,
            observed_tick: 30,
            level: Edge::Down,
            ..down
        };

        pending.push(down);
        pending.push(up);
        pending.push(next_down);

        assert_eq!(pending.poll(), BrokerPoll::Observation(down));
        assert_eq!(pending.poll(), BrokerPoll::Observation(up));
        assert_eq!(pending.poll(), BrokerPoll::Observation(next_down));
        assert_eq!(pending.poll(), BrokerPoll::Empty);
    }

    #[test]
    fn broker_key_mapping_is_bounded_to_supported_physical_keys() {
        assert_eq!(key_to_vk(&RadioKey::Mouse4), Some(VK_XBUTTON1));
        assert_eq!(key_to_vk(&RadioKey::Mouse5), Some(VK_XBUTTON2));
        assert_eq!(key_to_vk(&RadioKey::Keyboard(0x54)), Some(0x54));
        assert_eq!(key_to_vk(&RadioKey::Keyboard(0)), None);
        assert_eq!(key_to_vk(&RadioKey::Keyboard(255)), None);
    }

    #[test]
    fn nonce_hex_round_trip_is_exact() {
        let nonce = [
            0x00, 0x01, 0x02, 0x03, 0x10, 0x11, 0x12, 0x13, 0xaa, 0xbb, 0xcc, 0xdd, 0xfc, 0xfd,
            0xfe, 0xff,
        ];

        let encoded = nonce_hex(&nonce);

        assert_eq!(encoded.len(), 32);
        assert_eq!(parse_nonce_hex(OsStr::new(&encoded)), Some(nonce));
        assert_eq!(parse_nonce_hex(OsStr::new("not-a-valid-nonce")), None);
    }

    #[test]
    fn handshake_frame_requires_exact_binding() {
        let expected = HandshakeIdentity {
            parent_pid: 11,
            broker_pid: 22,
            session_id: 33,
            key_vk: 0x54,
            nonce: [0x5a; 16],
        };

        let frame = encode_frame(FRAME_HELLO, expected);

        assert!(frame_matches(&frame, FRAME_HELLO, expected));

        let mut wrong_pid = expected;
        wrong_pid.broker_pid = 23;

        assert!(!frame_matches(&frame, FRAME_HELLO, wrong_pid));

        let mut wrong_nonce = expected;
        wrong_nonce.nonce[0] ^= 1;

        assert!(!frame_matches(&frame, FRAME_HELLO, wrong_nonce));

        assert!(!frame_matches(&frame, FRAME_ACK, expected));
    }

    #[test]
    fn malformed_version_or_reserved_bytes_are_rejected() {
        let identity = HandshakeIdentity {
            parent_pid: 1,
            broker_pid: 2,
            session_id: 3,
            key_vk: 4,
            nonce: [7; 16],
        };

        let mut bad_version = encode_frame(FRAME_HELLO, identity);

        let unsupported_version = WIRE_VERSION
            .checked_add(1)
            .expect("wire test version must have a successor");

        bad_version[4..6].copy_from_slice(&unsupported_version.to_le_bytes());

        assert_eq!(decode_frame(&bad_version), None);

        let mut bad_reserved = encode_frame(FRAME_HELLO, identity);
        bad_reserved[47] = 1;

        assert_eq!(decode_frame(&bad_reserved), None);
    }

    #[test]
    fn current_process_session_and_image_identity_are_exact() {
        let current_pid = unsafe { GetCurrentProcessId() };

        assert!(same_session(current_pid, current_pid));

        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, current_pid) };

        assert!(!process.is_null());

        let executable = std::env::current_exe().expect("current exe");

        assert!(process_image_matches(process, &executable));

        let wrong = executable.with_file_name("lcr-broker-definitely-not-current-image.exe");

        assert!(!process_image_matches(process, &wrong));

        unsafe {
            let _ = CloseHandle(process);
        }
    }

    #[test]
    fn pre_signaled_stop_cancels_pending_overlapped_connect_without_polling() {
        let nonce = random_nonce().expect("system RNG");
        let token = nonce_hex(&nonce);

        let name = OsString::from(format!(r"\\.\pipe\LCR.InputBroker.StopConnect.{token}"));

        let server = create_private_pipe(&name).expect("create private pipe");

        let io_event = unsafe { CreateEventW(core::ptr::null(), 1, 0, core::ptr::null()) };
        let stop_event = unsafe { CreateEventW(core::ptr::null(), 1, 1, core::ptr::null()) };
        let peer_exit = unsafe { CreateEventW(core::ptr::null(), 1, 0, core::ptr::null()) };

        assert!(!io_event.is_null());
        assert!(!stop_event.is_null());
        assert!(!peer_exit.is_null());

        assert_eq!(
            connect_pipe_server(server, io_event, stop_event, peer_exit),
            PipeIoOutcome::Stopped
        );

        unsafe {
            let _ = DisconnectNamedPipe(server);
            let _ = CloseHandle(peer_exit);
            let _ = CloseHandle(stop_event);
            let _ = CloseHandle(io_event);
            let _ = CloseHandle(server);
        }
    }

    #[test]
    fn pre_signaled_stop_cancels_pending_overlapped_read_without_polling() {
        let nonce = random_nonce().expect("system RNG");
        let token = nonce_hex(&nonce);

        let name = OsString::from(format!(r"\\.\pipe\LCR.InputBroker.StopRead.{token}"));

        let server = create_private_pipe(&name).expect("create private pipe");
        let client = open_pipe(&name).expect("open local client");

        let io_event = unsafe { CreateEventW(core::ptr::null(), 1, 0, core::ptr::null()) };
        let stop_event = unsafe { CreateEventW(core::ptr::null(), 1, 1, core::ptr::null()) };
        let peer_exit = unsafe { CreateEventW(core::ptr::null(), 1, 0, core::ptr::null()) };

        assert!(!io_event.is_null());
        assert!(!stop_event.is_null());
        assert!(!peer_exit.is_null());

        assert_eq!(
            connect_pipe_server(server, io_event, stop_event, peer_exit),
            PipeIoOutcome::Complete
        );

        let mut frame = [0u8; FRAME_LEN];

        assert_eq!(
            read_exact_pipe_overlapped(server, &mut frame, io_event, stop_event, peer_exit,),
            PipeIoOutcome::Stopped
        );

        unsafe {
            let _ = CloseHandle(client);
            let _ = DisconnectNamedPipe(server);
            let _ = CloseHandle(peer_exit);
            let _ = CloseHandle(stop_event);
            let _ = CloseHandle(io_event);
            let _ = CloseHandle(server);
        }
    }

    #[test]
    fn hidden_mode_parser_does_not_claim_normal_invocations() {
        assert_eq!(maybe_run_from_args(std::iter::empty::<OsString>()), None);

        assert_eq!(
            maybe_run_from_args(vec![OsString::from("--unrelated-mode")].into_iter()),
            None
        );

        // Broker marker is recognized, while malformed broker arguments are
        // rejected before any startup/updater/runtime path can run.
        assert_eq!(
            maybe_run_from_args(vec![OsString::from(BROKER_ARG)].into_iter()),
            Some(2)
        );
    }

    #[test]
    fn kernel_named_pipe_reports_exact_local_process_ids() {
        let nonce = random_nonce().expect("system RNG");
        let token = nonce_hex(&nonce);

        let name = OsString::from(format!(r"\\.\pipe\LCR.InputBroker.Test.{token}"));

        let server = create_private_pipe(&name).expect("create private pipe");

        // A client may connect before ConnectNamedPipe is called; the server
        // then observes ERROR_PIPE_CONNECTED, which is a successful state.
        let client = open_pipe(&name).expect("open local client");

        let io_event = unsafe { CreateEventW(core::ptr::null(), 1, 0, core::ptr::null()) };
        let stop_event = unsafe { CreateEventW(core::ptr::null(), 1, 0, core::ptr::null()) };
        let peer_exit = unsafe { CreateEventW(core::ptr::null(), 1, 0, core::ptr::null()) };

        assert!(!io_event.is_null());
        assert!(!stop_event.is_null());
        assert!(!peer_exit.is_null());

        assert_eq!(
            connect_pipe_server(server, io_event, stop_event, peer_exit),
            PipeIoOutcome::Complete
        );

        let current = unsafe { GetCurrentProcessId() };

        let mut client_pid = 0u32;
        assert_ne!(
            unsafe { GetNamedPipeClientProcessId(server, &mut client_pid,) },
            0
        );
        assert_eq!(client_pid, current);

        let mut server_pid = 0u32;
        assert_ne!(
            unsafe { GetNamedPipeServerProcessId(client, &mut server_pid,) },
            0
        );
        assert_eq!(server_pid, current);

        unsafe {
            let _ = CloseHandle(client);
            let _ = DisconnectNamedPipe(server);
            let _ = CloseHandle(peer_exit);
            let _ = CloseHandle(stop_event);
            let _ = CloseHandle(io_event);
            let _ = CloseHandle(server);
        }
    }
}
