use crate::{
    broker_install::current_protected_broker_path,
    broker_security::{
        protected_broker_filesystem_security_matches, remove_uninstall_handoff_filesystem,
        validate_uninstall_handoff_cleanup_residue,
    },
    distribution::{
        install_dir, install_executable, install_path, start_menu_shortcut_path,
        uninstall_cleanup_executable_path, uninstall_cleanup_root, uninstall_cleanup_stage_dir,
        BROKER_LIFECYCLE_MUTEX_NAME, CURRENT_VERSION, INSTALL_MUTEX_NAME, RUNTIME_MUTEX_NAME,
        UNINSTALL_REGISTRY_SUBKEY,
    },
    input_broker::{current_user_sid_string, run_broker_uninstall_client},
    instance::{try_acquire_named_mutex, NamedMutexAcquire, NamedMutexGuard},
    localization as l, registration, shortcut,
    task_authority::{
        recognize_broker_task_via_scheduler, BrokerTaskContract, BrokerTaskVersionRelation,
        RecognizedBrokerTaskVerification,
    },
    windows::quiesce_runtime_for_uninstall,
};
use std::{
    env,
    ffi::{OsStr, OsString},
    fs, io,
    iter::once,
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvocationMode {
    Normal,
    Uninstall,
    CleanupUninstall {
        parent_process_id: u32,
        nonce: u64,
        parent_handle: CleanupParentHandleValue,
    },
    CleanupUpdate {
        parent_process_id: u32,
        nonce: u64,
        parent_handle: CleanupParentHandleValue,
        install_handle: CleanupParentHandleValue,
        runtime_handle: CleanupParentHandleValue,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnsupportedInvocation;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UninstallParentOutcome {
    Cancelled,
    CleanupLaunched,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CleanupChildOutcome {
    Complete,
    Incomplete,
}

/// Opaque numeric representation of the exact parent process HANDLE that will
/// later be inherited into the cleanup child.
///
/// HANDLE values are pointer-width. Zero and the current-process pseudo-handle
/// representation `(HANDLE)-1` are never valid cleanup authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CleanupParentHandleValue(usize);

impl CleanupParentHandleValue {
    pub fn new(value: usize) -> Result<Self, UnsupportedInvocation> {
        if value == 0 || value == usize::MAX {
            Err(UnsupportedInvocation)
        } else {
            Ok(Self(value))
        }
    }

    pub fn get(self) -> usize {
        self.0
    }

    pub fn encode(self) -> String {
        format!(
            "{:0width$x}",
            self.0,
            width = cleanup_parent_handle_hex_width()
        )
    }
}

pub fn parse_cleanup_parent_handle(
    value: &OsStr,
) -> Result<CleanupParentHandleValue, UnsupportedInvocation> {
    let value = value.to_str().ok_or(UnsupportedInvocation)?;

    if value.len() != cleanup_parent_handle_hex_width()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(UnsupportedInvocation);
    }

    let value = usize::from_str_radix(value, 16).map_err(|_| UnsupportedInvocation)?;

    CleanupParentHandleValue::new(value)
}

fn cleanup_parent_handle_hex_width() -> usize {
    std::mem::size_of::<usize>() * 2
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CleanupChildAuthority {
    pub parent_process_id: u32,
    pub nonce: u64,
    pub staging_directory: PathBuf,
    pub executable_path: PathBuf,
}

pub fn cleanup_child_authority(
    temp_dir: &Path,
    parent_process_id: u32,
    nonce: u64,
) -> Option<CleanupChildAuthority> {
    let staging_directory = uninstall_cleanup_stage_dir(temp_dir, parent_process_id, nonce)?;
    let executable_path = uninstall_cleanup_executable_path(temp_dir, parent_process_id, nonce)?;

    Some(CleanupChildAuthority {
        parent_process_id,
        nonce,
        staging_directory,
        executable_path,
    })
}

/// Stages an exact copy of the trusted current Helper for the later cleanup
/// process.
///
/// The stage directory must be newly created. A collision fails closed rather
/// than reusing or overwriting a pre-existing cleanup directory. A failed copy
/// removes only the newly-created stage directory.
pub fn stage_cleanup_child(
    current_executable: &Path,
    temp_dir: &Path,
    parent_process_id: u32,
    nonce: u64,
) -> io::Result<CleanupChildAuthority> {
    let authority =
        cleanup_child_authority(temp_dir, parent_process_id, nonce).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid cleanup child identity",
            )
        })?;

    fs::create_dir_all(uninstall_cleanup_root(temp_dir))?;
    fs::create_dir(&authority.staging_directory)?;

    match install_executable(current_executable, &authority.executable_path) {
        Ok(()) => Ok(authority),
        Err(error) => {
            let _ = fs::remove_dir_all(&authority.staging_directory);
            Err(error)
        }
    }
}

type Handle = *mut core::ffi::c_void;
type ProcThreadAttributeList = core::ffi::c_void;

const SYNCHRONIZE: u32 = 0x0010_0000;
const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x0000_1000;
const TRUE: i32 = 1;

const HANDLE_FLAG_INHERIT: u32 = 0x0000_0001;
const WAIT_OBJECT_0: u32 = 0x0000_0000;
const WAIT_FAILED: u32 = 0xFFFF_FFFF;
const INFINITE: u32 = 0xFFFF_FFFF;

const PROC_THREAD_ATTRIBUTE_NUMBER_MASK: usize = 0x0000_FFFF;
const PROC_THREAD_ATTRIBUTE_INPUT: usize = 0x0002_0000;
const PROC_THREAD_ATTRIBUTE_HANDLE_LIST_NUMBER: usize = 2;
const PROC_THREAD_ATTRIBUTE_HANDLE_LIST: usize = (PROC_THREAD_ATTRIBUTE_HANDLE_LIST_NUMBER
    & PROC_THREAD_ATTRIBUTE_NUMBER_MASK)
    | PROC_THREAD_ATTRIBUTE_INPUT;

#[repr(C)]
struct StartupInfoW {
    cb: u32,
    _reserved: *mut u16,
    _desktop: *mut u16,
    _title: *mut u16,
    _x: u32,
    _y: u32,
    _x_size: u32,
    _y_size: u32,
    _x_count_chars: u32,
    _y_count_chars: u32,
    _fill_attribute: u32,
    _flags: u32,
    _show_window: u16,
    _reserved2_size: u16,
    _reserved2: *mut u8,
    _std_input: Handle,
    _std_output: Handle,
    _std_error: Handle,
}

#[repr(C)]
struct StartupInfoExW {
    startup_info: StartupInfoW,
    attribute_list: *mut ProcThreadAttributeList,
}

#[repr(C)]
struct ProcessInformation {
    process_handle: Handle,
    thread_handle: Handle,
    process_id: u32,
    _thread_id: u32,
}

#[link(name = "kernel32")]
extern "system" {
    fn OpenProcess(desired_access: u32, inherit_handle: i32, process_id: u32) -> Handle;
    fn CloseHandle(handle: Handle) -> i32;
    fn GetCurrentProcessId() -> u32;
    fn InitializeProcThreadAttributeList(
        attribute_list: *mut ProcThreadAttributeList,
        attribute_count: u32,
        flags: u32,
        size: *mut usize,
    ) -> i32;
    fn UpdateProcThreadAttribute(
        attribute_list: *mut ProcThreadAttributeList,
        flags: u32,
        attribute: usize,
        value: *mut core::ffi::c_void,
        value_size: usize,
        previous_value: *mut core::ffi::c_void,
        return_size: *mut usize,
    ) -> i32;
    fn DeleteProcThreadAttributeList(attribute_list: *mut ProcThreadAttributeList);
    fn CreateProcessW(
        application_name: *const u16,
        command_line: *mut u16,
        process_attributes: *const core::ffi::c_void,
        thread_attributes: *const core::ffi::c_void,
        inherit_handles: i32,
        creation_flags: u32,
        environment: *const core::ffi::c_void,
        current_directory: *const u16,
        startup_info: *mut StartupInfoW,
        process_information: *mut ProcessInformation,
    ) -> i32;
    fn SetHandleInformation(object: Handle, mask: u32, flags: u32) -> i32;
    fn GetProcessId(process: Handle) -> u32;
    fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32;
    fn GetHandleInformation(object: Handle, flags: *mut u32) -> i32;
}

/// Exact real process handle owned by the uninstall parent.
///
/// This is deliberately not Clone/Copy. It never wraps a pseudo-handle and is
/// closed exactly once on drop.
struct OwnedParentProcessHandle {
    handle: Handle,
}

impl OwnedParentProcessHandle {
    fn raw(&self) -> Handle {
        self.handle
    }

    fn value(&self) -> Result<CleanupParentHandleValue, UnsupportedInvocation> {
        CleanupParentHandleValue::new(self.handle as usize)
    }
}

impl Drop for OwnedParentProcessHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

fn open_inheritable_self_process_handle() -> io::Result<OwnedParentProcessHandle> {
    let process_id = unsafe { GetCurrentProcessId() };
    let desired_access = SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION;

    let handle = unsafe { OpenProcess(desired_access, TRUE, process_id) };

    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }

    if handle as usize == usize::MAX {
        unsafe {
            let _ = CloseHandle(handle);
        }

        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "OpenProcess returned a pseudo-handle",
        ));
    }

    Ok(OwnedParentProcessHandle { handle })
}

/// Owns the initialized extended-startup attribute-list storage and the exact
/// HANDLE array referenced by PROC_THREAD_ATTRIBUTE_HANDLE_LIST.
///
/// The production uninstall handoff uses this same primitive for either:
/// - the historical one-parent-HANDLE proof path; or
/// - parent + install mutex + runtime mutex during C4D2.
///
/// The HANDLE array is one attribute. `attribute_count` therefore remains one
/// regardless of how many handles are whitelisted inside that attribute.
struct ParentHandleAttributeList {
    _storage: Vec<usize>,
    list: *mut ProcThreadAttributeList,
    handle_values: Box<[Handle]>,
}

impl ParentHandleAttributeList {
    fn as_mut_ptr(&mut self) -> *mut ProcThreadAttributeList {
        self.list
    }

    #[cfg(test)]
    fn inherited_handle_value(&self) -> Result<CleanupParentHandleValue, UnsupportedInvocation> {
        CleanupParentHandleValue::new(self.handle_values[0] as usize)
    }
}

impl Drop for ParentHandleAttributeList {
    fn drop(&mut self) {
        unsafe {
            DeleteProcThreadAttributeList(self.list);
        }
    }
}

fn build_handle_attribute_list(
    handle_values: Box<[Handle]>,
) -> io::Result<ParentHandleAttributeList> {
    if handle_values.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cleanup inheritance list cannot be empty",
        ));
    }

    if handle_values
        .iter()
        .any(|handle| handle.is_null() || *handle as usize == usize::MAX)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cleanup inheritance list contains an invalid HANDLE",
        ));
    }

    let mut required_size = 0usize;

    unsafe {
        let _ = InitializeProcThreadAttributeList(core::ptr::null_mut(), 1, 0, &mut required_size);
    }

    if required_size == 0 {
        return Err(io::Error::last_os_error());
    }

    let word_size = std::mem::size_of::<usize>();
    let storage_bytes = required_size.checked_add(word_size - 1).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "attribute list size overflow")
    })?;
    let word_count = storage_bytes / word_size;

    let mut storage = vec![0usize; word_count];
    let list = storage.as_mut_ptr().cast::<ProcThreadAttributeList>();

    let initialized = unsafe { InitializeProcThreadAttributeList(list, 1, 0, &mut required_size) };

    if initialized == 0 {
        return Err(io::Error::last_os_error());
    }

    let handle_bytes = std::mem::size_of::<Handle>()
        .checked_mul(handle_values.len())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "cleanup inheritance list size overflow",
            )
        })?;

    let attribute_list = ParentHandleAttributeList {
        _storage: storage,
        list,
        handle_values,
    };

    let updated = unsafe {
        UpdateProcThreadAttribute(
            attribute_list.list,
            0,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
            attribute_list
                .handle_values
                .as_ptr()
                .cast_mut()
                .cast::<core::ffi::c_void>(),
            handle_bytes,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
        )
    };

    if updated == 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(attribute_list)
}

fn build_parent_handle_attribute_list(
    parent: &OwnedParentProcessHandle,
) -> io::Result<ParentHandleAttributeList> {
    build_handle_attribute_list(vec![parent.raw()].into_boxed_slice())
}

struct TemporaryInheritedHandleFlag {
    handle: Handle,
    original_inherit_flag: u32,
}

impl TemporaryInheritedHandleFlag {
    fn new(handle: Handle) -> io::Result<Self> {
        if handle.is_null() || handle as usize == usize::MAX {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot mark invalid HANDLE inheritable",
            ));
        }

        let mut flags = 0u32;
        let queried = unsafe { GetHandleInformation(handle, &mut flags) };

        if queried == 0 {
            return Err(io::Error::last_os_error());
        }

        let original_inherit_flag = flags & HANDLE_FLAG_INHERIT;

        if original_inherit_flag == 0 {
            let changed =
                unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };

            if changed == 0 {
                return Err(io::Error::last_os_error());
            }
        }

        Ok(Self {
            handle,
            original_inherit_flag,
        })
    }
}

impl Drop for TemporaryInheritedHandleFlag {
    fn drop(&mut self) {
        unsafe {
            let _ =
                SetHandleInformation(self.handle, HANDLE_FLAG_INHERIT, self.original_inherit_flag);
        }
    }
}
const EXTENDED_STARTUPINFO_PRESENT: u32 = 0x0008_0000;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn cleanup_child_temp_dir(authority: &CleanupChildAuthority) -> io::Result<&Path> {
    let staging_root = authority.staging_directory.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "cleanup staging directory has no root",
        )
    })?;
    let temp_dir = staging_root.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "cleanup staging root has no temp parent",
        )
    })?;

    let expected = cleanup_child_authority(temp_dir, authority.parent_process_id, authority.nonce)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid cleanup child authority",
            )
        })?;

    if &expected != authority {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cleanup child authority does not match canonical staging paths",
        ));
    }

    Ok(temp_dir)
}

fn checked_wide_nul(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut encoded = value.encode_wide().collect::<Vec<_>>();

    if encoded.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Windows process argument contains an interior NUL",
        ));
    }

    encoded.push(0);
    Ok(encoded)
}

fn cleanup_child_command_line(
    authority: &CleanupChildAuthority,
    parent_handle: CleanupParentHandleValue,
) -> io::Result<Vec<u16>> {
    let _ = cleanup_child_temp_dir(authority)?;

    let process_id = authority.parent_process_id.to_string();
    let nonce = format!("{:016x}", authority.nonce);
    let parent_handle = parent_handle.encode();

    let mut command_line = OsString::from("\"");
    command_line.push(authority.executable_path.as_os_str());
    command_line.push("\" --cleanup-uninstall ");
    command_line.push(OsStr::new(&process_id));
    command_line.push(" ");
    command_line.push(OsStr::new(&nonce));
    command_line.push(" ");
    command_line.push(OsStr::new(&parent_handle));

    checked_wide_nul(&command_line)
}

fn update_child_command_line(
    authority: &CleanupChildAuthority,
    parent_handle: CleanupParentHandleValue,
    install_handle: CleanupParentHandleValue,
    runtime_handle: CleanupParentHandleValue,
) -> io::Result<Vec<u16>> {
    let _ = cleanup_child_temp_dir(authority)?;

    let process_id = authority.parent_process_id.to_string();
    let nonce = format!("{:016x}", authority.nonce);
    let parent_handle = parent_handle.encode();
    let install_handle = install_handle.encode();
    let runtime_handle = runtime_handle.encode();

    let mut command_line = OsString::from("\"");
    command_line.push(authority.executable_path.as_os_str());
    command_line.push("\" --cleanup-update ");
    command_line.push(OsStr::new(&process_id));
    command_line.push(" ");
    command_line.push(OsStr::new(&nonce));
    command_line.push(" ");
    command_line.push(OsStr::new(&parent_handle));
    command_line.push(" ");
    command_line.push(OsStr::new(&install_handle));
    command_line.push(" ");
    command_line.push(OsStr::new(&runtime_handle));

    checked_wide_nul(&command_line)
}
/// Launches one validated staged cleanup process with the supplied exact
/// inherited-HANDLE whitelist.
///
/// This helper owns no authority selection. Callers must prepare the exact
/// parent HANDLE and any temporary mutex inheritance flags before entering it.
fn create_staged_process(
    authority: &CleanupChildAuthority,
    mut command_line: Vec<u16>,
    attribute_list: &mut ParentHandleAttributeList,
) -> io::Result<u32> {
    let temp_dir = cleanup_child_temp_dir(authority)?;
    let current_process_id = unsafe { GetCurrentProcessId() };

    if authority.parent_process_id != current_process_id {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "staged child authority does not belong to the current process",
        ));
    }

    let application_name = checked_wide_nul(authority.executable_path.as_os_str())?;
    let current_directory = checked_wide_nul(temp_dir.as_os_str())?;

    let mut startup_info: StartupInfoExW = unsafe { std::mem::zeroed() };
    startup_info.startup_info.cb = std::mem::size_of::<StartupInfoExW>() as u32;
    startup_info.attribute_list = attribute_list.as_mut_ptr();

    let mut process_information: ProcessInformation = unsafe { std::mem::zeroed() };

    let created = unsafe {
        CreateProcessW(
            application_name.as_ptr(),
            command_line.as_mut_ptr(),
            core::ptr::null(),
            core::ptr::null(),
            TRUE,
            CREATE_NO_WINDOW | EXTENDED_STARTUPINFO_PRESENT,
            core::ptr::null(),
            current_directory.as_ptr(),
            &mut startup_info.startup_info,
            &mut process_information,
        )
    };

    if created == 0 {
        return Err(io::Error::last_os_error());
    }

    let child_process_id = process_information.process_id;

    unsafe {
        if !process_information.thread_handle.is_null() {
            let _ = CloseHandle(process_information.thread_handle);
        }
        if !process_information.process_handle.is_null() {
            let _ = CloseHandle(process_information.process_handle);
        }
    }

    Ok(child_process_id)
}

fn create_cleanup_process(
    authority: &CleanupChildAuthority,
    parent_handle: CleanupParentHandleValue,
    attribute_list: &mut ParentHandleAttributeList,
) -> io::Result<u32> {
    let command_line = cleanup_child_command_line(authority, parent_handle)?;
    create_staged_process(authority, command_line, attribute_list)
}

fn create_update_process(
    authority: &CleanupChildAuthority,
    parent_handle: CleanupParentHandleValue,
    install_handle: CleanupParentHandleValue,
    runtime_handle: CleanupParentHandleValue,
    attribute_list: &mut ParentHandleAttributeList,
) -> io::Result<u32> {
    let command_line =
        update_child_command_line(authority, parent_handle, install_handle, runtime_handle)?;
    create_staged_process(authority, command_line, attribute_list)
}
/// Historical C3 launch seam: inherit only the exact real parent-process HANDLE.
///
/// C4D2 deliberately preserves this public primitive and its existing tests.
/// Production uninstall orchestration uses the guarded variant below.
pub fn launch_cleanup_child(authority: &CleanupChildAuthority) -> io::Result<u32> {
    let parent_process = open_inheritable_self_process_handle()?;
    let parent_handle = parent_process.value().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid inherited parent process handle",
        )
    })?;
    let mut attribute_list = build_parent_handle_attribute_list(&parent_process)?;

    create_cleanup_process(authority, parent_handle, &mut attribute_list)
}

/// Production clean-uninstall launch seam.
///
/// In addition to the exact parent-process HANDLE, the cleanup child inherits
/// the already-owned INSTALL, RUNTIME, and BrokerLifecycle mutex HANDLEs.
/// Therefore all three named kernel objects remain alive after the uninstall
/// parent exits and throughout protected-Broker cleanup, Policy-B, and C4C.
///
/// No guard HANDLE is added to the command line. The BrokerLifecycle handle is
/// intentionally opaque to the child: simply keeping the inherited kernel
/// HANDLE open until child exit preserves lifecycle serialization.
///
/// The three mutex HANDLEs are made inheritable only for this CreateProcessW
/// call and their original HANDLE_FLAG_INHERIT state is restored on every
/// return path. PROC_THREAD_ATTRIBUTE_HANDLE_LIST remains the exact inheritance
/// whitelist, so unrelated inheritable handles are not passed to the child.
#[doc(hidden)]
pub fn launch_cleanup_child_with_guards(
    authority: &CleanupChildAuthority,
    install_guard: &NamedMutexGuard,
    runtime_guard: &NamedMutexGuard,
    broker_lifecycle_guard: &NamedMutexGuard,
) -> io::Result<u32> {
    let parent_process = open_inheritable_self_process_handle()?;
    let parent_handle = parent_process.value().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid inherited parent process handle",
        )
    })?;

    let install_handle = install_guard.raw_handle().cast::<core::ffi::c_void>();
    let runtime_handle = runtime_guard.raw_handle().cast::<core::ffi::c_void>();
    let broker_lifecycle_handle = broker_lifecycle_guard
        .raw_handle()
        .cast::<core::ffi::c_void>();

    let _install_inherit = TemporaryInheritedHandleFlag::new(install_handle)?;
    let _runtime_inherit = TemporaryInheritedHandleFlag::new(runtime_handle)?;
    let _broker_lifecycle_inherit = TemporaryInheritedHandleFlag::new(broker_lifecycle_handle)?;

    let mut attribute_list = build_handle_attribute_list(
        vec![
            parent_process.raw(),
            install_handle,
            runtime_handle,
            broker_lifecycle_handle,
        ]
        .into_boxed_slice(),
    )?;

    create_cleanup_process(authority, parent_handle, &mut attribute_list)
}
/// U2 launch seam. Reuses the exact C4D2 whitelist and temporary inherit flags,
/// but passes the two guard HANDLE values to the trusted staged child so it can
/// close RUNTIME before launching the newly committed canonical Helper.
#[doc(hidden)]
pub fn launch_update_child_with_guards(
    authority: &CleanupChildAuthority,
    install_guard: &NamedMutexGuard,
    runtime_guard: &NamedMutexGuard,
) -> io::Result<u32> {
    let parent_process = open_inheritable_self_process_handle()?;
    let parent_handle = parent_process.value().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid inherited update parent process handle",
        )
    })?;

    let install_raw = install_guard.raw_handle().cast::<core::ffi::c_void>();
    let runtime_raw = runtime_guard.raw_handle().cast::<core::ffi::c_void>();

    let install_handle = CleanupParentHandleValue::new(install_raw as usize)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid install guard HANDLE"))?;
    let runtime_handle = CleanupParentHandleValue::new(runtime_raw as usize)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid runtime guard HANDLE"))?;

    let _install_inherit = TemporaryInheritedHandleFlag::new(install_raw)?;
    let _runtime_inherit = TemporaryInheritedHandleFlag::new(runtime_raw)?;

    let mut attribute_list = build_handle_attribute_list(
        vec![parent_process.raw(), install_raw, runtime_raw].into_boxed_slice(),
    )?;

    create_update_process(
        authority,
        parent_handle,
        install_handle,
        runtime_handle,
        &mut attribute_list,
    )
}
/// Owns the exact process HANDLE inherited into the cleanup child.
///
/// Construction never reopens a PID. The numeric HANDLE value already refers
/// to the kernel process object inherited by CreateProcessW.
struct InheritedParentProcessHandle {
    handle: Handle,
}

impl InheritedParentProcessHandle {
    fn from_value(value: CleanupParentHandleValue) -> Self {
        Self {
            handle: value.get() as Handle,
        }
    }

    fn raw(&self) -> Handle {
        self.handle
    }
}

impl Drop for InheritedParentProcessHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

struct OwnedInheritedGuardHandle {
    handle: Handle,
}

impl OwnedInheritedGuardHandle {
    fn from_value(value: CleanupParentHandleValue) -> io::Result<Self> {
        let handle = value.get() as Handle;
        clear_inherited_handle_flag(handle)?;
        Ok(Self { handle })
    }
}

impl Drop for OwnedInheritedGuardHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}
fn validate_cleanup_child_execution_path(
    current_executable: &Path,
    parent_process_id: u32,
    nonce: u64,
) -> io::Result<CleanupChildAuthority> {
    let staging_directory = current_executable.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "cleanup executable has no staging directory",
        )
    })?;
    let staging_root = staging_directory.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "cleanup staging directory has no cleanup root",
        )
    })?;
    let temp_dir = staging_root.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "cleanup staging root has no temp parent",
        )
    })?;

    let expected =
        cleanup_child_authority(temp_dir, parent_process_id, nonce).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid cleanup child execution identity",
            )
        })?;

    if expected.executable_path != current_executable
        || expected.staging_directory != staging_directory
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cleanup process is not running from its exact staged authority",
        ));
    }

    Ok(expected)
}

fn clear_inherited_handle_flag(handle: Handle) -> io::Result<()> {
    let cleared = unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) };

    if cleared == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn process_id_from_handle(handle: Handle) -> io::Result<u32> {
    let process_id = unsafe { GetProcessId(handle) };

    if process_id == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(process_id)
    }
}

fn validate_inherited_parent_process(
    expected_parent_process_id: u32,
    value: CleanupParentHandleValue,
) -> io::Result<InheritedParentProcessHandle> {
    let current_process_id = unsafe { GetCurrentProcessId() };

    if expected_parent_process_id == current_process_id {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cleanup child cannot use itself as uninstall parent",
        ));
    }

    let handle = InheritedParentProcessHandle::from_value(value);

    clear_inherited_handle_flag(handle.raw())?;

    let actual_process_id = process_id_from_handle(handle.raw())?;

    if actual_process_id != expected_parent_process_id {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "inherited process handle does not identify the expected parent",
        ));
    }

    Ok(handle)
}

fn wait_for_parent_process_exit(handle: &InheritedParentProcessHandle) -> io::Result<()> {
    let result = unsafe { WaitForSingleObject(handle.raw(), INFINITE) };

    match result {
        WAIT_OBJECT_0 => Ok(()),
        WAIT_FAILED => Err(io::Error::last_os_error()),
        unexpected => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected process wait result: {unexpected:#010x}"),
        )),
    }
}

/// Opaque authority proving that the exact staged cleanup child validated the
/// inherited parent process object and observed that exact process terminate.
///
/// A later deletion gate may require this type rather than accepting PID/nonce
/// input directly.
#[derive(Debug, Eq, PartialEq)]
pub struct CleanupParentExitProof {
    authority: CleanupChildAuthority,
}

impl CleanupParentExitProof {
    pub fn authority(&self) -> &CleanupChildAuthority {
        &self.authority
    }
}

/// Validates child staging authority, clears further HANDLE inheritance,
/// verifies exact parent identity, and waits for that exact process object.
///
/// This performs no filesystem/registry/config deletion.
pub fn prove_cleanup_parent_exit(
    current_executable: &Path,
    parent_process_id: u32,
    nonce: u64,
    parent_handle: CleanupParentHandleValue,
) -> io::Result<CleanupParentExitProof> {
    let authority =
        validate_cleanup_child_execution_path(current_executable, parent_process_id, nonce)?;

    let parent_process = validate_inherited_parent_process(parent_process_id, parent_handle)?;

    wait_for_parent_process_exit(&parent_process)?;

    // Close the inherited process HANDLE before issuing the proof.
    drop(parent_process);

    Ok(CleanupParentExitProof { authority })
}

/// Exact U2 process handoff after the staged old Helper has observed the exact
/// apply parent exit. INSTALL remains held for this object's lifetime; RUNTIME
/// can be released exactly once before the new canonical Helper is launched.
pub struct UpdateParentExitHandoff {
    proof: CleanupParentExitProof,
    _install_guard: OwnedInheritedGuardHandle,
    runtime_guard: Option<OwnedInheritedGuardHandle>,
}

impl UpdateParentExitHandoff {
    pub fn proof(&self) -> &CleanupParentExitProof {
        &self.proof
    }

    pub fn release_runtime(&mut self) {
        drop(self.runtime_guard.take());
    }
}

/// Validates all inherited U2 HANDLEs, clears further inheritance on both guard
/// HANDLEs, waits for the exact parent process object, and keeps INSTALL/RUNTIME
/// alive across the handoff.
pub fn prove_update_parent_exit_handoff(
    current_executable: &Path,
    parent_process_id: u32,
    nonce: u64,
    parent_handle: CleanupParentHandleValue,
    install_handle: CleanupParentHandleValue,
    runtime_handle: CleanupParentHandleValue,
) -> io::Result<UpdateParentExitHandoff> {
    let parent_value = parent_handle.get();
    let install_value = install_handle.get();
    let runtime_value = runtime_handle.get();

    if parent_value == install_value
        || parent_value == runtime_value
        || install_value == runtime_value
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "update inherited HANDLE values must be distinct",
        ));
    }

    let install_guard = OwnedInheritedGuardHandle::from_value(install_handle)?;
    let runtime_guard = OwnedInheritedGuardHandle::from_value(runtime_handle)?;

    let proof =
        prove_cleanup_parent_exit(current_executable, parent_process_id, nonce, parent_handle)?;

    Ok(UpdateParentExitHandoff {
        proof,
        _install_guard: install_guard,
        runtime_guard: Some(runtime_guard),
    })
}
#[repr(C)]
#[derive(Clone, Copy)]
struct KnownFolderId {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

const FOLDERID_LOCAL_APP_DATA: KnownFolderId = KnownFolderId {
    data1: 0xF1B3_2785,
    data2: 0x6FBA,
    data3: 0x4FCF,
    data4: [0x9D, 0x55, 0x7B, 0x8E, 0x7F, 0x15, 0x70, 0x91],
};

const FOLDERID_ROAMING_APP_DATA: KnownFolderId = KnownFolderId {
    data1: 0x3EB6_85DB,
    data2: 0x65F9,
    data3: 0x4CF6,
    data4: [0xA0, 0x3A, 0xE3, 0xEF, 0x65, 0x72, 0x9F, 0x3D],
};

const FOLDERID_PROGRAMS: KnownFolderId = KnownFolderId {
    data1: 0xA77F_5D77,
    data2: 0x2E2B,
    data3: 0x44C3,
    data4: [0xA6, 0xA2, 0xAB, 0xA6, 0x01, 0x05, 0x4A, 0x51],
};

type HResult = i32;

const COINIT_MULTITHREADED: u32 = 0;
const COINIT_DISABLE_OLE1DDE: u32 = 0x0000_0004;
const RPC_E_CHANGED_MODE: HResult = 0x8001_0106u32 as i32;

#[link(name = "ole32")]
extern "system" {
    fn CoInitializeEx(reserved: *mut core::ffi::c_void, coinit: u32) -> HResult;
    fn CoUninitialize();
    fn CoTaskMemFree(memory: *mut core::ffi::c_void);
}

#[link(name = "shell32")]
extern "system" {
    fn SHGetKnownFolderPath(
        folder_id: *const KnownFolderId,
        flags: u32,
        token: Handle,
        path: *mut *mut u16,
    ) -> HResult;
}

struct ComInitialization {
    should_uninitialize: bool,
}

impl ComInitialization {
    fn current_thread() -> io::Result<Self> {
        let result = unsafe {
            CoInitializeEx(
                core::ptr::null_mut(),
                COINIT_MULTITHREADED | COINIT_DISABLE_OLE1DDE,
            )
        };

        if result >= 0 {
            Ok(Self {
                should_uninitialize: true,
            })
        } else if result == RPC_E_CHANGED_MODE {
            // The thread is already initialized in another valid COM
            // concurrency model. Do not attempt to change or uninitialize it.
            Ok(Self {
                should_uninitialize: false,
            })
        } else {
            Err(hresult_error("CoInitializeEx", result))
        }
    }
}

impl Drop for ComInitialization {
    fn drop(&mut self) {
        if self.should_uninitialize {
            unsafe {
                CoUninitialize();
            }
        }
    }
}

struct CoTaskMemWide(*mut u16);

impl CoTaskMemWide {
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

impl Drop for CoTaskMemWide {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                CoTaskMemFree(self.0.cast());
            }
        }
    }
}

fn hresult_error(operation: &'static str, result: HResult) -> io::Error {
    io::Error::other(format!(
        "{operation} failed with HRESULT 0x{:08X}",
        result as u32
    ))
}

fn known_folder_path(folder_id: &KnownFolderId) -> io::Result<PathBuf> {
    let mut raw = core::ptr::null_mut();

    let result = unsafe { SHGetKnownFolderPath(folder_id, 0, core::ptr::null_mut(), &mut raw) };

    // Microsoft requires the returned task-memory pointer to be freed even
    // when SHGetKnownFolderPath reports failure.
    let raw = CoTaskMemWide(raw);

    if result < 0 {
        return Err(hresult_error("SHGetKnownFolderPath", result));
    }

    raw.to_path_buf()
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DestructiveKnownFolderAuthority {
    local_app_data: PathBuf,
    roaming_app_data: PathBuf,
    programs: PathBuf,
}

impl DestructiveKnownFolderAuthority {
    fn validate(&self) -> io::Result<()> {
        for (name, path) in [
            ("LocalAppData", &self.local_app_data),
            ("RoamingAppData", &self.roaming_app_data),
            ("Programs", &self.programs),
        ] {
            if path.as_os_str().is_empty() || !path.is_absolute() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{name} known-folder authority is not an absolute path"),
                ));
            }

            if path.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::CurDir | std::path::Component::ParentDir
                )
            }) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{name} known-folder authority contains lexical traversal"),
                ));
            }
        }

        Ok(())
    }
}

/// Resolves and freezes all Windows Known Folder roots required by clean
/// uninstall before the first destructive operation.
///
/// These paths come from the current-user Windows Known Folder authority, not
/// process environment variables. Independent Programs resolution is
/// intentional because Windows can redirect known folders separately.
fn resolve_destructive_known_folder_authority() -> io::Result<DestructiveKnownFolderAuthority> {
    let _com = ComInitialization::current_thread()?;

    let authority = DestructiveKnownFolderAuthority {
        local_app_data: known_folder_path(&FOLDERID_LOCAL_APP_DATA)?,
        roaming_app_data: known_folder_path(&FOLDERID_ROAMING_APP_DATA)?,
        programs: known_folder_path(&FOLDERID_PROGRAMS)?,
    };

    authority.validate()?;

    Ok(authority)
}

/// Reuses the already-reviewed current-user Known Folder authority for updater
/// apply. Only the two roots required by U2 are exposed.
pub(crate) fn resolve_update_known_folder_authority() -> io::Result<(PathBuf, PathBuf)> {
    let authority = resolve_destructive_known_folder_authority()?;
    Ok((authority.local_app_data, authority.roaming_app_data))
}
/// Removes only the canonical per-user installed Helper executable after the
/// staged cleanup child has proven that its exact uninstall parent exited.
///
/// The target path is derived internally from the current user's Windows
/// `FOLDERID_LocalAppData` authority; callers cannot supply an arbitrary
/// deletion path and process environment variables cannot redirect it.
///
/// All destructive Known Folder roots required by the later Policy-B
/// composition are resolved and frozen before this first mutation. This does
/// not remove configuration, shortcuts, registry state, the install directory,
/// or the staged cleanup copy. Missing `LCR.exe` is idempotent success; every
/// other removal failure is returned to the caller.
pub fn remove_canonical_executable_after_parent_exit(
    proof: &CleanupParentExitProof,
) -> io::Result<()> {
    let current_executable = std::env::current_exe()?;
    let known_folders = resolve_destructive_known_folder_authority()?;

    remove_canonical_executable_after_parent_exit_at(
        proof,
        &current_executable,
        &known_folders.local_app_data,
    )
}

fn validate_cleanup_process_matches_proof(
    proof: &CleanupParentExitProof,
    current_executable: &Path,
) -> io::Result<()> {
    if current_executable != proof.authority().executable_path {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cleanup process does not match parent-exit proof authority",
        ))
    } else {
        Ok(())
    }
}

fn remove_canonical_executable_after_parent_exit_at(
    proof: &CleanupParentExitProof,
    current_executable: &Path,
    local_app_data: &Path,
) -> io::Result<()> {
    let target = install_path(local_app_data);

    remove_canonical_executable_after_parent_exit_target(proof, current_executable, &target)
}

fn remove_canonical_executable_after_parent_exit_target(
    proof: &CleanupParentExitProof,
    current_executable: &Path,
    target: &Path,
) -> io::Result<()> {
    validate_cleanup_process_matches_proof(proof, current_executable)?;
    remove_file_idempotent(target)
}

fn remove_file_idempotent(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn remove_directory_tree_idempotent(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PolicyBCleanupStep {
    RoamingAppData,
    StartMenuShortcut,
    InstalledAppsRegistration,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PolicyBCleanupOutcome {
    Complete,
    Incomplete {
        failed_steps: Vec<PolicyBCleanupStep>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PolicyBCleanupTargets {
    executable_path: PathBuf,
    app_data_directory: PathBuf,
    shortcut_path: PathBuf,
}

impl PolicyBCleanupTargets {
    fn from_known_folders(authority: &DestructiveKnownFolderAuthority) -> Self {
        Self {
            executable_path: install_path(&authority.local_app_data),
            app_data_directory: authority.roaming_app_data.join("LCR"),
            shortcut_path: authority.programs.join("LCR.lnk"),
        }
    }
}

trait PolicyBCleanupOperations {
    fn remove_executable(
        &mut self,
        proof: &CleanupParentExitProof,
        current_executable: &Path,
        target: &Path,
    ) -> io::Result<()>;

    fn remove_roaming_app_data(&mut self, target: &Path) -> io::Result<()>;

    fn remove_start_menu_shortcut(&mut self, target: &Path) -> io::Result<()>;

    fn unregister_installed_app(&mut self) -> io::Result<()>;
}

struct NativePolicyBCleanupOperations;

impl PolicyBCleanupOperations for NativePolicyBCleanupOperations {
    fn remove_executable(
        &mut self,
        proof: &CleanupParentExitProof,
        current_executable: &Path,
        target: &Path,
    ) -> io::Result<()> {
        remove_canonical_executable_after_parent_exit_target(proof, current_executable, target)
    }

    fn remove_roaming_app_data(&mut self, target: &Path) -> io::Result<()> {
        remove_directory_tree_idempotent(target)
    }

    fn remove_start_menu_shortcut(&mut self, target: &Path) -> io::Result<()> {
        shortcut::remove_shortcut_artifact(target)
    }

    fn unregister_installed_app(&mut self) -> io::Result<()> {
        registration::unregister_installed_app()
    }
}

/// Executes the reviewed Policy-B R1 clean-uninstall transaction after the
/// staged cleanup child has proven that its exact uninstall parent exited.
///
/// Preflight is zero-mutation: the current cleanup executable must match the
/// proof authority and all destructive Windows Known Folder roots are resolved
/// and frozen before deletion begins.
///
/// Exact canonical `LCR.exe` deletion is the commit barrier. If that deletion
/// fails, no later cleanup step runs. Once it succeeds, cleanup is monotonic:
/// roaming application state, Start Menu shortcut, and finally Installed Apps
/// registration are each attempted even when an earlier post-barrier step
/// fails. Post-barrier failures are reported as typed failed steps and are not
/// rolled back.
///
/// This R1 transaction deliberately does not remove the install directory,
/// staged cleanup child, or any updater artifact outside `%RoamingAppData%\LCR`.
pub fn execute_policy_b_cleanup_after_parent_exit(
    proof: &CleanupParentExitProof,
) -> io::Result<PolicyBCleanupOutcome> {
    let current_executable = std::env::current_exe()?;

    validate_cleanup_process_matches_proof(proof, &current_executable)?;

    let known_folders = resolve_destructive_known_folder_authority()?;
    let targets = PolicyBCleanupTargets::from_known_folders(&known_folders);
    let mut operations = NativePolicyBCleanupOperations;

    execute_policy_b_cleanup_at(proof, &current_executable, &targets, &mut operations)
}

fn execute_policy_b_cleanup_at<O: PolicyBCleanupOperations>(
    proof: &CleanupParentExitProof,
    current_executable: &Path,
    targets: &PolicyBCleanupTargets,
    operations: &mut O,
) -> io::Result<PolicyBCleanupOutcome> {
    validate_cleanup_process_matches_proof(proof, current_executable)?;

    operations.remove_executable(proof, current_executable, &targets.executable_path)?;

    let mut failed_steps = Vec::new();

    if operations
        .remove_roaming_app_data(&targets.app_data_directory)
        .is_err()
    {
        failed_steps.push(PolicyBCleanupStep::RoamingAppData);
    }

    if operations
        .remove_start_menu_shortcut(&targets.shortcut_path)
        .is_err()
    {
        failed_steps.push(PolicyBCleanupStep::StartMenuShortcut);
    }

    if operations.unregister_installed_app().is_err() {
        failed_steps.push(PolicyBCleanupStep::InstalledAppsRegistration);
    }

    if failed_steps.is_empty() {
        Ok(PolicyBCleanupOutcome::Complete)
    } else {
        Ok(PolicyBCleanupOutcome::Incomplete { failed_steps })
    }
}

type FileInfoByHandleClass = i32;
type NtStatus = i32;

const DELETE_ACCESS: u32 = 0x0001_0000;
const FILE_SHARE_READ: u32 = 0x0000_0001;
const FILE_SHARE_WRITE: u32 = 0x0000_0002;
const FILE_SHARE_DELETE: u32 = 0x0000_0004;
const OPEN_EXISTING: u32 = 3;
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

// FILE_INFO_BY_HANDLE_CLASS::FileRenameInfo.
const FILE_RENAME_INFO_CLASS: FileInfoByHandleClass = 3;

// FILE_INFORMATION_CLASS::FileDispositionInformationEx.
const FILE_DISPOSITION_INFORMATION_EX_CLASS: u32 = 64;
const FILE_DISPOSITION_DELETE: u32 = 0x0000_0001;
const FILE_DISPOSITION_POSIX_SEMANTICS: u32 = 0x0000_0002;

#[repr(C)]
struct FileDispositionInformationEx {
    flags: u32,
}

#[repr(C)]
struct IoStatusBlock {
    status_or_pointer: usize,
    information: usize,
}

#[repr(C)]
struct FileRenameInfoLayout {
    replace_if_exists: u32,
    root_directory: Handle,
    file_name_length: u32,
    file_name: [u16; 1],
}

#[link(name = "kernel32")]
extern "system" {
    fn CreateFileW(
        file_name: *const u16,
        desired_access: u32,
        share_mode: u32,
        security_attributes: *const core::ffi::c_void,
        creation_disposition: u32,
        flags_and_attributes: u32,
        template_file: Handle,
    ) -> Handle;

    fn SetFileInformationByHandle(
        file: Handle,
        file_information_class: FileInfoByHandleClass,
        file_information: *mut core::ffi::c_void,
        buffer_size: u32,
    ) -> i32;
}

#[link(name = "ntdll")]
extern "system" {
    fn NtSetInformationFile(
        file_handle: Handle,
        io_status_block: *mut IoStatusBlock,
        file_information: *mut core::ffi::c_void,
        length: u32,
        file_information_class: u32,
    ) -> NtStatus;

    fn RtlNtStatusToDosError(status: NtStatus) -> u32;
}

struct OwnedFileHandle {
    handle: Handle,
}

impl OwnedFileHandle {
    fn open_for_delete_without_following_reparse(path: &Path) -> io::Result<Self> {
        let path = checked_wide_nul(path.as_os_str())?;

        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                DELETE_ACCESS,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                core::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OPEN_REPARSE_POINT,
                core::ptr::null_mut(),
            )
        };

        if handle as isize == -1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self { handle })
        }
    }
}

impl Drop for OwnedFileHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

fn rename_default_stream_for_self_delete(file: &OwnedFileHandle, nonce: u64) -> io::Result<()> {
    let stream_name = format!(":lcr-cleanup-{nonce:016x}");
    let stream_wide = OsStr::new(&stream_name).encode_wide().collect::<Vec<_>>();

    let file_name_bytes = stream_wide
        .len()
        .checked_mul(std::mem::size_of::<u16>())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "cleanup stream name length overflow",
            )
        })?;

    let file_name_offset = std::mem::offset_of!(FileRenameInfoLayout, file_name);
    let buffer_size = file_name_offset
        .checked_add(file_name_bytes)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "cleanup rename buffer size overflow",
            )
        })?;

    let word_size = std::mem::size_of::<usize>();
    let allocation_size = buffer_size.checked_add(word_size - 1).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "cleanup rename allocation overflow",
        )
    })?;
    let word_count = allocation_size / word_size;
    let mut storage = vec![0usize; word_count];

    let base = storage.as_mut_ptr().cast::<u8>();
    let layout = base.cast::<FileRenameInfoLayout>();

    unsafe {
        (*layout).replace_if_exists = 0;
        (*layout).root_directory = core::ptr::null_mut();
        (*layout).file_name_length = u32::try_from(file_name_bytes).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "cleanup stream name exceeds Windows DWORD length",
            )
        })?;

        core::ptr::copy_nonoverlapping(
            stream_wide.as_ptr().cast::<u8>(),
            base.add(file_name_offset),
            file_name_bytes,
        );
    }

    let changed = unsafe {
        SetFileInformationByHandle(
            file.handle,
            FILE_RENAME_INFO_CLASS,
            base.cast(),
            u32::try_from(buffer_size).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "cleanup rename buffer exceeds Windows DWORD length",
                )
            })?,
        )
    };

    if changed == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn ntstatus_error(status: NtStatus) -> io::Error {
    let windows_error = unsafe { RtlNtStatusToDosError(status) };

    io::Error::from_raw_os_error(windows_error as i32)
}

fn set_native_posix_delete(file: &OwnedFileHandle) -> io::Result<()> {
    let mut disposition = FileDispositionInformationEx {
        flags: FILE_DISPOSITION_DELETE | FILE_DISPOSITION_POSIX_SEMANTICS,
    };
    let mut io_status = IoStatusBlock {
        status_or_pointer: 0,
        information: 0,
    };

    let status = unsafe {
        NtSetInformationFile(
            file.handle,
            &mut io_status,
            (&mut disposition as *mut FileDispositionInformationEx).cast(),
            std::mem::size_of::<FileDispositionInformationEx>() as u32,
            FILE_DISPOSITION_INFORMATION_EX_CLASS,
        )
    };

    if status < 0 {
        Err(ntstatus_error(status))
    } else {
        Ok(())
    }
}

/// Self-unlinks the exact running staged cleanup executable.
///
/// Modern Windows, including Windows 11 24H2, can reject disposition directly
/// against the mapped unnamed executable stream. The bounded sequence is:
///
/// 1. open the exact staged file with DELETE access without following a
///    replacement reparse point;
/// 2. rename its unnamed NTFS data stream to a nonce-bound private stream;
/// 3. close and reopen the exact file path;
/// 4. issue native FileDispositionInformationEx with DELETE + POSIX semantics;
/// 5. close the disposition handle so the visible file link disappears.
///
/// No shell process, polling, retry heuristic, reboot-delete, or recursive
/// stage deletion is used. Failure is reported to the caller and does not
/// roll back already-completed Policy-B cleanup.
fn self_unlink_running_file_without_following_reparse(path: &Path, nonce: u64) -> io::Result<()> {
    let file = OwnedFileHandle::open_for_delete_without_following_reparse(path)?;

    rename_default_stream_for_self_delete(&file, nonce)?;

    drop(file);

    let file = OwnedFileHandle::open_for_delete_without_following_reparse(path)?;

    set_native_posix_delete(&file)?;

    drop(file);

    Ok(())
}
fn remove_empty_directory_idempotent(path: &Path) -> io::Result<()> {
    match fs::remove_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Removes the staged cleanup executable and then its now-empty staging
/// directory after Policy-B work has completed.
///
/// The caller remains responsible for ordering this primitive after
/// `execute_policy_b_cleanup_after_parent_exit`. This function does not
/// repeat Policy-B and does not provide rollback.
///
/// Safety boundaries:
/// - requires the opaque parent-exit proof;
/// - current process path must exactly match the proof's staged executable;
/// - canonical staging authority is revalidated before mutation;
/// - only the exact staged executable is stream-renamed and POSIX-unlinked;
/// - the staging directory is removed non-recursively, so unexpected siblings
///   survive and cause an error instead of being recursively erased;
/// - no cmd/PowerShell janitor, reboot-delete, polling, or retry fallback.
///
/// Neutral U2 alias for the already-proven C4C staged-self-cleanup primitive.
pub fn cleanup_staged_child_after_parent_exit(proof: &CleanupParentExitProof) -> io::Result<()> {
    cleanup_staged_child_after_policy_b(proof)
}
pub fn cleanup_staged_child_after_policy_b(proof: &CleanupParentExitProof) -> io::Result<()> {
    let current_executable = std::env::current_exe()?;

    cleanup_staged_child_after_policy_b_at(proof, &current_executable)
}

fn cleanup_staged_child_after_policy_b_at(
    proof: &CleanupParentExitProof,
    current_executable: &Path,
) -> io::Result<()> {
    validate_cleanup_process_matches_proof(proof, current_executable)?;

    // Revalidate that the opaque proof still describes the canonical
    // Temp\LCR-Uninstall\<pid>-<nonce>\LCR-Cleanup.exe authority before
    // issuing the irreversible self-unlink.
    let _ = cleanup_child_temp_dir(proof.authority())?;

    self_unlink_running_file_without_following_reparse(
        &proof.authority().executable_path,
        proof.authority().nonce,
    )?;

    remove_empty_directory_idempotent(&proof.authority().staging_directory)
}

const BCRYPT_USE_SYSTEM_PREFERRED_RNG: u32 = 0x0000_0002;

#[link(name = "bcrypt")]
extern "system" {
    fn BCryptGenRandom(
        algorithm: Handle,
        buffer: *mut u8,
        buffer_size: u32,
        flags: u32,
    ) -> NtStatus;
}

pub(crate) fn generate_cleanup_nonce() -> io::Result<u64> {
    let mut nonce = 0u64;

    let status = unsafe {
        BCryptGenRandom(
            core::ptr::null_mut(),
            (&mut nonce as *mut u64).cast::<u8>(),
            std::mem::size_of::<u64>() as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };

    if status < 0 {
        return Err(ntstatus_error(status));
    }

    if nonce == 0 {
        return Err(io::Error::other(
            "system RNG returned the reserved zero cleanup nonce",
        ));
    }

    Ok(nonce)
}

pub(crate) fn validate_uninstall_parent_executable_at(
    current_executable: &Path,
    local_app_data: &Path,
) -> io::Result<PathBuf> {
    if !current_executable.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "uninstall parent executable path must be absolute",
        ));
    }

    let expected = install_path(local_app_data);
    let current_resolved = fs::canonicalize(current_executable)?;
    let expected_resolved = fs::canonicalize(&expected)?;

    if current_resolved != expected_resolved {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "clean uninstall must run from the canonical installed Helper",
        ));
    }

    Ok(expected)
}

fn resolve_uninstall_parent_authority(current_executable: &Path) -> io::Result<PathBuf> {
    let known_folders = resolve_destructive_known_folder_authority()?;

    validate_uninstall_parent_executable_at(current_executable, &known_folders.local_app_data)
}

fn acquire_uninstall_named_guard(name: &str, purpose: &'static str) -> io::Result<NamedMutexGuard> {
    match try_acquire_named_mutex(name)? {
        NamedMutexAcquire::Acquired(guard) => Ok(guard),
        NamedMutexAcquire::AlreadyExists => Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            format!("{purpose} mutex already exists"),
        )),
    }
}

fn uninstall_task_version_is_current(relation: BrokerTaskVersionRelation, version: &str) -> bool {
    relation == BrokerTaskVersionRelation::Current && version == CURRENT_VERSION
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum BrokerUninstallState {
    Armed(BrokerTaskContract),
    Committed,
}

fn current_broker_uninstall_state() -> io::Result<BrokerUninstallState> {
    let user_sid = current_user_sid_string().ok_or_else(|| {
        io::Error::other("cannot resolve current Windows user SID for Broker uninstall")
    })?;
    let verification = recognize_broker_task_via_scheduler(&user_sid).map_err(|error| {
        io::Error::other(format!(
            "cannot recognize fixed Broker Task for uninstall: {error}"
        ))
    })?;

    match verification {
        RecognizedBrokerTaskVerification::Exact(authority)
            if uninstall_task_version_is_current(authority.relation(), authority.version()) =>
        {
            let contract = authority.contract().clone();
            if !protected_broker_filesystem_security_matches(
                &user_sid,
                contract.protected_broker_path(),
            )? {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "clean uninstall requires exact protected Broker filesystem authority",
                ));
            }
            Ok(BrokerUninstallState::Armed(contract))
        }
        RecognizedBrokerTaskVerification::Exact(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "clean uninstall requires exact CURRENT Broker Task authority",
        )),
        RecognizedBrokerTaskVerification::Absent => {
            let target = current_protected_broker_path()?;
            validate_uninstall_handoff_cleanup_residue(&user_sid, &target).map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!(
                        "fixed Broker Task is absent but uninstall residue is not resumable: {error}"
                    ),
                )
            })?;
            Ok(BrokerUninstallState::Committed)
        }
        RecognizedBrokerTaskVerification::Mismatch => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "clean uninstall rejected mismatched Broker Task authority",
        )),
    }
}

fn run_broker_uninstall_if_armed<F>(state: &BrokerUninstallState, run: F) -> io::Result<()>
where
    F: FnOnce(&BrokerTaskContract) -> io::Result<()>,
{
    match state {
        BrokerUninstallState::Armed(contract) => run(contract),
        BrokerUninstallState::Committed => Ok(()),
    }
}
fn cleanup_protected_broker_after_parent_exit(proof: &CleanupParentExitProof) -> io::Result<()> {
    let current_executable = env::current_exe()?;

    validate_cleanup_process_matches_proof(proof, &current_executable)?;

    let user_sid = current_user_sid_string().ok_or_else(|| {
        io::Error::other("cannot resolve current Windows user SID in uninstall cleanup child")
    })?;

    match recognize_broker_task_via_scheduler(&user_sid).map_err(|error| {
        io::Error::other(format!(
            "Broker Task cleanup-child post-readback failed: {error}"
        ))
    })? {
        RecognizedBrokerTaskVerification::Absent => {}

        RecognizedBrokerTaskVerification::Exact(_) | RecognizedBrokerTaskVerification::Mismatch => {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "protected Broker cleanup requires fixed Task authority to remain absent",
            ));
        }
    }

    let target = current_protected_broker_path()?;

    remove_uninstall_handoff_filesystem(&user_sid, &target)
}

fn broker_then_policy_cleanup<B, P>(
    broker_cleanup: B,
    policy_cleanup: P,
) -> io::Result<PolicyBCleanupOutcome>
where
    B: FnOnce() -> io::Result<()>,
    P: FnOnce() -> io::Result<PolicyBCleanupOutcome>,
{
    broker_cleanup()?;
    policy_cleanup()
}

pub(crate) fn remove_unlaunched_cleanup_stage_best_effort(authority: &CleanupChildAuthority) {
    // The child was not created, so only this exact newly-staged executable is
    // eligible for cleanup. Directory removal is deliberately non-recursive:
    // an unexpected sibling survives rather than being erased.
    let _ = remove_file_idempotent(&authority.executable_path);
    let _ = fs::remove_dir(&authority.staging_directory);
}

/// End-user uninstall parent orchestration.
///
/// This function runs only after `--uninstall` has been selected by the exact
/// invocation parser. It never enters normal PTT runtime.
///
/// Order is safety-critical:
/// 1. prove this process is the canonical installed Helper;
/// 2. obtain explicit user confirmation;
/// 3. hold INSTALL mutex;
/// 4. hold BrokerLifecycle mutex before any persistent Broker classification;
/// 5. gracefully quiesce any active runtime via C4D1;
/// 6. acquire and hold RUNTIME mutex, closing the post-quiesce start race;
/// 7. classify persistent uninstall state as ARMED or already COMMITTED;
/// 8. create and stage the cleanup child before any new privileged commit;
/// 9. only when ARMED, run one authenticated Broker Uninstall session and
///    require persistent Task-absent + exact handoff ACL readback;
/// 10. when already COMMITTED, skip Broker execution entirely;
/// 11. launch the staged child while inheriting parent + INSTALL + RUNTIME +
///     BrokerLifecycle HANDLEs.
///
/// On successful launch the parent returns and exits normally. The cleanup
/// child keeps all three named mutex kernel objects alive while it waits for
/// this exact parent and throughout protected-Broker + Policy-B + C4C cleanup.
pub fn run_clean_uninstall_parent() -> io::Result<UninstallParentOutcome> {
    let current_executable = env::current_exe()?;
    let canonical_executable = resolve_uninstall_parent_authority(&current_executable)?;

    match confirm_clean_uninstall() {
        UninstallDecision::Cancel => return Ok(UninstallParentOutcome::Cancelled),
        UninstallDecision::Failed => {
            return Err(io::Error::other(
                "native clean-uninstall confirmation failed",
            ));
        }
        UninstallDecision::Proceed => {}
    }

    let install_guard = acquire_uninstall_named_guard(INSTALL_MUTEX_NAME, "install")?;

    let broker_lifecycle_guard =
        acquire_uninstall_named_guard(BROKER_LIFECYCLE_MUTEX_NAME, "Broker lifecycle")?;

    quiesce_runtime_for_uninstall(&canonical_executable)?;

    let runtime_guard = acquire_uninstall_named_guard(RUNTIME_MUTEX_NAME, "runtime")?;

    let broker_state = current_broker_uninstall_state()?;

    let nonce = generate_cleanup_nonce()?;

    let authority = stage_cleanup_child(
        &current_executable,
        &env::temp_dir(),
        std::process::id(),
        nonce,
    )?;

    if let Err(error) = run_broker_uninstall_if_armed(&broker_state, run_broker_uninstall_client) {
        remove_unlaunched_cleanup_stage_best_effort(&authority);

        return Err(error);
    }

    match launch_cleanup_child_with_guards(
        &authority,
        &install_guard,
        &runtime_guard,
        &broker_lifecycle_guard,
    ) {
        Ok(child_process_id) if child_process_id != 0 => {
            Ok(UninstallParentOutcome::CleanupLaunched)
        }
        Ok(_) => {
            remove_unlaunched_cleanup_stage_best_effort(&authority);
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "cleanup child launch returned zero process id",
            ))
        }
        Err(error) => {
            remove_unlaunched_cleanup_stage_best_effort(&authority);
            Err(error)
        }
    }
}

fn classify_cleanup_child_outcome(
    policy_result: io::Result<PolicyBCleanupOutcome>,
    staged_cleanup_result: io::Result<()>,
) -> CleanupChildOutcome {
    match (policy_result, staged_cleanup_result) {
        (Ok(PolicyBCleanupOutcome::Complete), Ok(())) => CleanupChildOutcome::Complete,
        _ => CleanupChildOutcome::Incomplete,
    }
}

/// Hidden cleanup-child orchestration selected only by the exact internal
/// `--cleanup-uninstall <pid> <nonce> <handle>` invocation.
///
/// No destructive call is possible until C3 mints an opaque proof that this
/// exact staged child validated the inherited parent HANDLE and observed the
/// exact parent process exit.
///
/// Once proof exists, protected-Broker cleanup must succeed before Policy-B is
/// attempted. C4C is still attempted regardless of cleanup/policy outcome so
/// the staged child does not become unnecessary residue. There is no rollback
/// after either durable commit barrier. Any non-complete combination maps to
/// `Incomplete`, allowing main to return process exit code 1 while preserving
/// the monotonic transaction.
pub fn run_cleanup_uninstall_child(
    parent_process_id: u32,
    nonce: u64,
    parent_handle: CleanupParentHandleValue,
) -> io::Result<CleanupChildOutcome> {
    let current_executable = env::current_exe()?;

    let proof =
        prove_cleanup_parent_exit(&current_executable, parent_process_id, nonce, parent_handle)?;

    let policy_result = broker_then_policy_cleanup(
        || cleanup_protected_broker_after_parent_exit(&proof),
        || execute_policy_b_cleanup_after_parent_exit(&proof),
    );

    let staged_cleanup_result = cleanup_staged_child_after_policy_b(&proof);

    Ok(classify_cleanup_child_outcome(
        policy_result,
        staged_cleanup_result,
    ))
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UninstallDecision {
    Proceed,
    Cancel,
    Failed,
}

type Hwnd = isize;

const MB_YESNO: u32 = 0x0000_0004;
const MB_ICONWARNING: u32 = 0x0000_0030;
const MB_DEFBUTTON2: u32 = 0x0000_0100;
const IDYES: i32 = 6;
const IDNO: i32 = 7;

#[link(name = "user32")]
extern "system" {
    fn MessageBoxW(window: Hwnd, text: *const u16, caption: *const u16, kind: u32) -> i32;
}

/// Shows the minimal native clean-uninstall confirmation.
///
/// This is decision-only. It performs no filesystem, registry, process, or
/// configuration mutation. `No`, API failure, and unexpected return values all
/// fail closed.
pub fn confirm_clean_uninstall() -> UninstallDecision {
    let title = wide_nul(OsStr::new(l::UNINSTALL_CONFIRM_TITLE));
    let body = wide_nul(OsStr::new(l::UNINSTALL_CONFIRM_BODY));

    let result = unsafe {
        MessageBoxW(
            0,
            body.as_ptr(),
            title.as_ptr(),
            MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2,
        )
    };

    uninstall_decision_from_message_box_result(result)
}

fn uninstall_decision_from_message_box_result(result: i32) -> UninstallDecision {
    match result {
        IDYES => UninstallDecision::Proceed,
        IDNO => UninstallDecision::Cancel,
        _ => UninstallDecision::Failed,
    }
}

fn wide_nul(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(once(0)).collect()
}

/// Resolves the Helper process mode without performing any side effect.
///
/// The canonical uninstall command is exactly `--uninstall`. Unknown or
/// additional arguments fail closed rather than falling through into the
/// normal PTT/control runtime.
pub fn parse_invocation_mode<I, S>(args: I) -> Result<InvocationMode, UnsupportedInvocation>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut args = args.into_iter();

    match args.next() {
        None => Ok(InvocationMode::Normal),
        Some(argument) if argument.as_ref() == OsStr::new("--uninstall") => {
            if args.next().is_some() {
                Err(UnsupportedInvocation)
            } else {
                Ok(InvocationMode::Uninstall)
            }
        }
        Some(argument) if argument.as_ref() == OsStr::new("--cleanup-uninstall") => {
            let parent_process_id = match args.next() {
                Some(value) => parse_parent_process_id(value.as_ref())?,
                None => return Err(UnsupportedInvocation),
            };
            let nonce = match args.next() {
                Some(value) => parse_cleanup_nonce(value.as_ref())?,
                None => return Err(UnsupportedInvocation),
            };
            let parent_handle = match args.next() {
                Some(value) => parse_cleanup_parent_handle(value.as_ref())?,
                None => return Err(UnsupportedInvocation),
            };

            if args.next().is_some() {
                return Err(UnsupportedInvocation);
            }

            Ok(InvocationMode::CleanupUninstall {
                parent_process_id,
                nonce,
                parent_handle,
            })
        }
        Some(argument) if argument.as_ref() == OsStr::new("--cleanup-update") => {
            let parent_process_id = match args.next() {
                Some(value) => parse_parent_process_id(value.as_ref())?,
                None => return Err(UnsupportedInvocation),
            };
            let nonce = match args.next() {
                Some(value) => parse_cleanup_nonce(value.as_ref())?,
                None => return Err(UnsupportedInvocation),
            };
            let parent_handle = match args.next() {
                Some(value) => parse_cleanup_parent_handle(value.as_ref())?,
                None => return Err(UnsupportedInvocation),
            };
            let install_handle = match args.next() {
                Some(value) => parse_cleanup_parent_handle(value.as_ref())?,
                None => return Err(UnsupportedInvocation),
            };
            let runtime_handle = match args.next() {
                Some(value) => parse_cleanup_parent_handle(value.as_ref())?,
                None => return Err(UnsupportedInvocation),
            };

            if args.next().is_some() {
                return Err(UnsupportedInvocation);
            }

            Ok(InvocationMode::CleanupUpdate {
                parent_process_id,
                nonce,
                parent_handle,
                install_handle,
                runtime_handle,
            })
        }
        Some(_) => Err(UnsupportedInvocation),
    }
}

fn parse_parent_process_id(value: &OsStr) -> Result<u32, UnsupportedInvocation> {
    let value = value.to_str().ok_or(UnsupportedInvocation)?;
    let value = value.parse::<u32>().map_err(|_| UnsupportedInvocation)?;

    if value == 0 {
        Err(UnsupportedInvocation)
    } else {
        Ok(value)
    }
}

fn parse_cleanup_nonce(value: &OsStr) -> Result<u64, UnsupportedInvocation> {
    let value = value.to_str().ok_or(UnsupportedInvocation)?;

    if value.len() != 16
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(UnsupportedInvocation);
    }

    let value = u64::from_str_radix(value, 16).map_err(|_| UnsupportedInvocation)?;

    if value == 0 {
        Err(UnsupportedInvocation)
    } else {
        Ok(value)
    }
}

/// Policy B: uninstall means a clean per-user removal.
///
/// `app_data_directory` is deliberately the entire `%APPDATA%\LCR` authority,
/// so config, pairing credential, PTT preferences, generation state, and
/// updater state are all removed together. A later reinstall starts fresh.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CleanUninstallPlan {
    pub executable_path: PathBuf,
    pub install_directory: PathBuf,
    pub app_data_directory: PathBuf,
    pub shortcut_path: PathBuf,
    pub uninstall_registry_subkey: &'static str,
}

pub fn clean_uninstall_plan(app_data: &Path, local_app_data: &Path) -> CleanUninstallPlan {
    CleanUninstallPlan {
        executable_path: install_path(local_app_data),
        install_directory: install_dir(local_app_data),
        app_data_directory: app_data.join("LCR"),
        shortcut_path: start_menu_shortcut_path(app_data),
        uninstall_registry_subkey: UNINSTALL_REGISTRY_SUBKEY,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{broker_install::protected_broker_path_for_test, distribution::update_state_path};

    fn c4d2_test_mutex_name(case: &str) -> String {
        format!(
            r"Local\LinhLanBang.LCR.Helper.C4D2.Test.{}.{}",
            std::process::id(),
            case
        )
    }

    fn acquire_c4d2_test_mutex(case: &str) -> NamedMutexGuard {
        match try_acquire_named_mutex(&c4d2_test_mutex_name(case)).unwrap() {
            NamedMutexAcquire::Acquired(guard) => guard,
            NamedMutexAcquire::AlreadyExists => {
                panic!("fresh C4D2 test mutex already exists: {case}")
            }
        }
    }

    #[test]
    fn c4d2_uninstall_parent_requires_canonical_installed_executable() {
        let directory = cleanup_stage_test_dir("c4d2-parent-canonical-authority");
        let local_app_data = directory.join("LocalAppData");
        let canonical = install_path(&local_app_data);
        let downloaded = directory.join("downloaded.exe");

        fs::create_dir_all(canonical.parent().unwrap()).unwrap();
        fs::write(&canonical, b"canonical-helper").unwrap();
        fs::write(&downloaded, b"downloaded-helper").unwrap();

        assert_eq!(
            validate_uninstall_parent_executable_at(&canonical, &local_app_data).unwrap(),
            canonical
        );

        let error =
            validate_uninstall_parent_executable_at(&downloaded, &local_app_data).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn uninstall_task_version_gate_requires_exact_current_text() {
        assert!(uninstall_task_version_is_current(
            BrokerTaskVersionRelation::Current,
            CURRENT_VERSION,
        ));

        assert!(!uninstall_task_version_is_current(
            BrokerTaskVersionRelation::Older,
            CURRENT_VERSION,
        ));

        assert!(!uninstall_task_version_is_current(
            BrokerTaskVersionRelation::Current,
            "0.0.0",
        ));
    }

    #[test]
    fn committed_uninstall_state_never_invokes_privileged_broker() {
        let calls = std::cell::Cell::new(0usize);
        run_broker_uninstall_if_armed(&BrokerUninstallState::Committed, |_| {
            calls.set(calls.get() + 1);
            Ok(())
        })
        .unwrap();
        assert_eq!(calls.get(), 0);
    }

    #[test]
    fn armed_uninstall_state_invokes_privileged_broker_once() {
        const USER_SID: &str = "S-1-5-21-111111111-222222222-333333333-1001";
        let target =
            protected_broker_path_for_test(Path::new(r"C:\Program Files"), CURRENT_VERSION)
                .unwrap();
        let contract = BrokerTaskContract::new(USER_SID, &target).unwrap();
        let state = BrokerUninstallState::Armed(contract);
        let calls = std::cell::Cell::new(0usize);
        run_broker_uninstall_if_armed(&state, |observed| {
            calls.set(calls.get() + 1);
            assert_eq!(observed.user_sid(), USER_SID);
            assert_eq!(observed.broker_path(), target.as_path());
            Ok(())
        })
        .unwrap();
        assert_eq!(calls.get(), 1);
    }
    #[test]
    fn c4d2_system_cleanup_nonce_is_nonzero() {
        assert_ne!(generate_cleanup_nonce().unwrap(), 0);
    }

    #[test]
    fn c4d2_four_handle_whitelist_keeps_broker_lifecycle_alive() {
        let install_guard = acquire_c4d2_test_mutex("install-guard");
        let runtime_guard = acquire_c4d2_test_mutex("runtime-guard");
        let broker_lifecycle_guard = acquire_c4d2_test_mutex("broker-lifecycle-guard");

        let parent = open_inheritable_self_process_handle().unwrap();

        let install_handle = install_guard.raw_handle().cast::<core::ffi::c_void>();

        let runtime_handle = runtime_guard.raw_handle().cast::<core::ffi::c_void>();

        let broker_lifecycle_handle = broker_lifecycle_guard
            .raw_handle()
            .cast::<core::ffi::c_void>();

        let mut install_flags_before = 0u32;
        let mut runtime_flags_before = 0u32;
        let mut broker_flags_before = 0u32;

        assert_ne!(
            unsafe { GetHandleInformation(install_handle, &mut install_flags_before,) },
            0
        );

        assert_ne!(
            unsafe { GetHandleInformation(runtime_handle, &mut runtime_flags_before,) },
            0
        );

        assert_ne!(
            unsafe { GetHandleInformation(broker_lifecycle_handle, &mut broker_flags_before,) },
            0
        );

        assert_eq!(install_flags_before & HANDLE_FLAG_INHERIT, 0);

        assert_eq!(runtime_flags_before & HANDLE_FLAG_INHERIT, 0);

        assert_eq!(broker_flags_before & HANDLE_FLAG_INHERIT, 0);

        {
            let _install_inherit = TemporaryInheritedHandleFlag::new(install_handle).unwrap();

            let _runtime_inherit = TemporaryInheritedHandleFlag::new(runtime_handle).unwrap();

            let _broker_lifecycle_inherit =
                TemporaryInheritedHandleFlag::new(broker_lifecycle_handle).unwrap();

            let attribute_list = build_handle_attribute_list(
                vec![
                    parent.raw(),
                    install_handle,
                    runtime_handle,
                    broker_lifecycle_handle,
                ]
                .into_boxed_slice(),
            )
            .unwrap();

            assert_eq!(attribute_list.handle_values.len(), 4);

            assert_eq!(attribute_list.handle_values[0], parent.raw());

            assert_eq!(attribute_list.handle_values[1], install_handle);

            assert_eq!(attribute_list.handle_values[2], runtime_handle);

            assert_eq!(attribute_list.handle_values[3], broker_lifecycle_handle);

            let mut install_flags_during = 0u32;
            let mut runtime_flags_during = 0u32;
            let mut broker_flags_during = 0u32;

            assert_ne!(
                unsafe { GetHandleInformation(install_handle, &mut install_flags_during,) },
                0
            );

            assert_ne!(
                unsafe { GetHandleInformation(runtime_handle, &mut runtime_flags_during,) },
                0
            );

            assert_ne!(
                unsafe { GetHandleInformation(broker_lifecycle_handle, &mut broker_flags_during,) },
                0
            );

            assert_ne!(install_flags_during & HANDLE_FLAG_INHERIT, 0);

            assert_ne!(runtime_flags_during & HANDLE_FLAG_INHERIT, 0);

            assert_ne!(broker_flags_during & HANDLE_FLAG_INHERIT, 0);
        }

        let mut install_flags_after = 0u32;
        let mut runtime_flags_after = 0u32;
        let mut broker_flags_after = 0u32;

        assert_ne!(
            unsafe { GetHandleInformation(install_handle, &mut install_flags_after,) },
            0
        );

        assert_ne!(
            unsafe { GetHandleInformation(runtime_handle, &mut runtime_flags_after,) },
            0
        );

        assert_ne!(
            unsafe { GetHandleInformation(broker_lifecycle_handle, &mut broker_flags_after,) },
            0
        );

        assert_eq!(install_flags_after & HANDLE_FLAG_INHERIT, 0);

        assert_eq!(runtime_flags_after & HANDLE_FLAG_INHERIT, 0);

        assert_eq!(broker_flags_after & HANDLE_FLAG_INHERIT, 0);
    }
    #[test]
    fn protected_broker_cleanup_failure_blocks_policy_b() {
        let policy_calls = std::cell::Cell::new(0usize);

        let result = broker_then_policy_cleanup(
            || Err(io::Error::other("protected Broker cleanup failed")),
            || {
                policy_calls.set(policy_calls.get() + 1);
                Ok(PolicyBCleanupOutcome::Complete)
            },
        );

        assert!(result.is_err());
        assert_eq!(policy_calls.get(), 0);
    }

    #[test]
    fn protected_broker_cleanup_success_allows_policy_b() {
        let policy_calls = std::cell::Cell::new(0usize);

        let result = broker_then_policy_cleanup(
            || Ok(()),
            || {
                policy_calls.set(policy_calls.get() + 1);
                Ok(PolicyBCleanupOutcome::Complete)
            },
        )
        .unwrap();

        assert_eq!(result, PolicyBCleanupOutcome::Complete);

        assert_eq!(policy_calls.get(), 1);
    }

    #[test]
    fn c4d2_cleanup_child_exit_is_complete_only_when_policy_and_stage_complete() {
        assert_eq!(
            classify_cleanup_child_outcome(Ok(PolicyBCleanupOutcome::Complete), Ok(())),
            CleanupChildOutcome::Complete
        );

        assert_eq!(
            classify_cleanup_child_outcome(
                Ok(PolicyBCleanupOutcome::Incomplete {
                    failed_steps: vec![PolicyBCleanupStep::RoamingAppData],
                }),
                Ok(())
            ),
            CleanupChildOutcome::Incomplete
        );

        assert_eq!(
            classify_cleanup_child_outcome(Err(io::Error::other("policy failed")), Ok(())),
            CleanupChildOutcome::Incomplete
        );

        assert_eq!(
            classify_cleanup_child_outcome(
                Ok(PolicyBCleanupOutcome::Complete),
                Err(io::Error::other("staged cleanup failed"))
            ),
            CleanupChildOutcome::Incomplete
        );
    }

    #[test]
    fn exact_internal_update_invocation_selects_hidden_update_mode() {
        let width = cleanup_parent_handle_hex_width();
        let parent = format!("{:0width$x}", 0x11usize, width = width);
        let install = format!("{:0width$x}", 0x22usize, width = width);
        let runtime = format!("{:0width$x}", 0x33usize, width = width);

        assert_eq!(
            parse_invocation_mode([
                OsStr::new("--cleanup-update"),
                OsStr::new("123"),
                OsStr::new("0000000000000044"),
                OsStr::new(&parent),
                OsStr::new(&install),
                OsStr::new(&runtime),
            ])
            .unwrap(),
            InvocationMode::CleanupUpdate {
                parent_process_id: 123,
                nonce: 0x44,
                parent_handle: CleanupParentHandleValue::new(0x11).unwrap(),
                install_handle: CleanupParentHandleValue::new(0x22).unwrap(),
                runtime_handle: CleanupParentHandleValue::new(0x33).unwrap(),
            }
        );
    }

    #[test]
    fn malformed_internal_update_invocation_fails_closed() {
        let width = cleanup_parent_handle_hex_width();
        let handle = format!("{:0width$x}", 0x11usize, width = width);

        for args in [
            vec![OsString::from("--cleanup-update")],
            vec![
                OsString::from("--cleanup-update"),
                OsString::from("123"),
                OsString::from("0000000000000044"),
                OsString::from(&handle),
            ],
            vec![
                OsString::from("--cleanup-update"),
                OsString::from("123"),
                OsString::from("0000000000000044"),
                OsString::from(&handle),
                OsString::from(&handle),
                OsString::from(&handle),
                OsString::from("extra"),
            ],
        ] {
            assert!(parse_invocation_mode(args).is_err());
        }
    }
    #[test]
    fn empty_invocation_is_normal_runtime() {
        assert_eq!(
            parse_invocation_mode(Vec::<&str>::new()),
            Ok(InvocationMode::Normal)
        );
    }

    #[test]
    fn exact_uninstall_switch_selects_uninstall_mode() {
        assert_eq!(
            parse_invocation_mode(["--uninstall"]),
            Ok(InvocationMode::Uninstall)
        );
    }

    #[test]
    fn unknown_switch_fails_closed() {
        assert_eq!(
            parse_invocation_mode(["--unknown"]),
            Err(UnsupportedInvocation)
        );
    }

    #[test]
    fn additional_arguments_fail_closed() {
        assert_eq!(
            parse_invocation_mode(["--uninstall", "unexpected"]),
            Err(UnsupportedInvocation)
        );
    }

    #[test]
    fn uninstall_switch_is_canonical_and_case_sensitive() {
        assert_eq!(
            parse_invocation_mode(["--UNINSTALL"]),
            Err(UnsupportedInvocation)
        );
    }

    #[test]
    fn inherited_parent_handle_codec_is_pointer_width_lower_hex() {
        let handle = CleanupParentHandleValue::new(0x2a).unwrap();
        let encoded = handle.encode();

        assert_eq!(encoded.len(), std::mem::size_of::<usize>() * 2);
        assert!(encoded
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)));
        assert!(encoded.ends_with("2a"));
        assert_eq!(handle.get(), 0x2a);
        assert_eq!(
            parse_cleanup_parent_handle(OsStr::new(&encoded)),
            Ok(handle)
        );
    }

    #[test]
    fn inherited_parent_handle_rejects_zero_and_process_pseudo_handle() {
        let width = std::mem::size_of::<usize>() * 2;
        let zero = "0".repeat(width);
        let pseudo_process = "f".repeat(width);

        assert_eq!(CleanupParentHandleValue::new(0), Err(UnsupportedInvocation));
        assert_eq!(
            CleanupParentHandleValue::new(usize::MAX),
            Err(UnsupportedInvocation)
        );
        assert_eq!(
            parse_cleanup_parent_handle(OsStr::new(&zero)),
            Err(UnsupportedInvocation)
        );
        assert_eq!(
            parse_cleanup_parent_handle(OsStr::new(&pseudo_process)),
            Err(UnsupportedInvocation)
        );
    }

    #[test]
    fn inherited_parent_handle_rejects_wrong_width_uppercase_and_non_hex() {
        let width = std::mem::size_of::<usize>() * 2;
        let valid = CleanupParentHandleValue::new(0x2a).unwrap().encode();
        let uppercase = valid.replace("2a", "2A");
        let short = valid[1..].to_owned();
        let long = format!("0{valid}");
        let mut non_hex = "0".repeat(width - 1);
        non_hex.push('g');

        for value in [uppercase, short, long, non_hex] {
            assert_eq!(
                parse_cleanup_parent_handle(OsStr::new(&value)),
                Err(UnsupportedInvocation)
            );
        }
    }

    #[test]
    fn exact_internal_cleanup_invocation_selects_hidden_cleanup_mode() {
        let parent_handle = CleanupParentHandleValue::new(0x2a).unwrap();
        let encoded_handle = parent_handle.encode();

        assert_eq!(
            parse_invocation_mode([
                "--cleanup-uninstall",
                "4242",
                "000000000000002a",
                encoded_handle.as_str(),
            ]),
            Ok(InvocationMode::CleanupUninstall {
                parent_process_id: 4242,
                nonce: 0x2a,
                parent_handle,
            })
        );
    }

    #[test]
    fn malformed_internal_cleanup_invocations_fail_closed() {
        let valid_handle = CleanupParentHandleValue::new(0x2a).unwrap().encode();
        let zero_handle = "0".repeat(cleanup_parent_handle_hex_width());
        let pseudo_handle = "f".repeat(cleanup_parent_handle_hex_width());
        let uppercase_handle = valid_handle.replace("2a", "2A");
        let short_handle = valid_handle[1..].to_owned();

        for invocation in [
            vec!["--cleanup-uninstall"],
            vec!["--cleanup-uninstall", "4242"],
            vec!["--cleanup-uninstall", "4242", "000000000000002a"],
            vec![
                "--cleanup-uninstall",
                "0",
                "000000000000002a",
                valid_handle.as_str(),
            ],
            vec![
                "--cleanup-uninstall",
                "abc",
                "000000000000002a",
                valid_handle.as_str(),
            ],
            vec![
                "--cleanup-uninstall",
                "4242",
                "0000000000000000",
                valid_handle.as_str(),
            ],
            vec!["--cleanup-uninstall", "4242", "2a", valid_handle.as_str()],
            vec![
                "--cleanup-uninstall",
                "4242",
                "000000000000002A",
                valid_handle.as_str(),
            ],
            vec![
                "--cleanup-uninstall",
                "4242",
                "00000000000000zz",
                valid_handle.as_str(),
            ],
            vec![
                "--cleanup-uninstall",
                "4242",
                "000000000000002a",
                zero_handle.as_str(),
            ],
            vec![
                "--cleanup-uninstall",
                "4242",
                "000000000000002a",
                pseudo_handle.as_str(),
            ],
            vec![
                "--cleanup-uninstall",
                "4242",
                "000000000000002a",
                uppercase_handle.as_str(),
            ],
            vec![
                "--cleanup-uninstall",
                "4242",
                "000000000000002a",
                short_handle.as_str(),
            ],
            vec![
                "--cleanup-uninstall",
                "4242",
                "000000000000002a",
                valid_handle.as_str(),
                "unexpected",
            ],
        ] {
            assert_eq!(
                parse_invocation_mode(invocation),
                Err(UnsupportedInvocation)
            );
        }
    }

    #[test]
    fn cleanup_child_authority_reuses_distribution_paths() {
        let authority = cleanup_child_authority(Path::new("Temp"), 4242, 0x2a).unwrap();

        assert_eq!(authority.parent_process_id, 4242);
        assert_eq!(authority.nonce, 0x2a);
        assert_eq!(
            authority.staging_directory,
            Path::new("Temp")
                .join("LCR-Uninstall")
                .join("4242-000000000000002a")
        );
        assert_eq!(
            authority.executable_path,
            authority.staging_directory.join("LCR-Cleanup.exe")
        );
    }

    fn cleanup_stage_test_dir(case: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "lcr-helper-uninstall-stage-{case}-{}",
            std::process::id()
        ));

        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    fn synthetic_parent_exit_proof(directory: &Path, nonce: u64) -> CleanupParentExitProof {
        let authority = cleanup_child_authority(&directory.join("Temp"), 4242, nonce).unwrap();

        CleanupParentExitProof { authority }
    }

    #[test]
    #[ignore = "subprocess target for Known Folder environment isolation"]
    fn known_folder_environment_probe() {
        let authority = resolve_destructive_known_folder_authority().unwrap();

        let poisoned_local =
            PathBuf::from(std::env::var_os("LCR_TEST_POISON_LOCALAPPDATA").unwrap());
        let poisoned_roaming = PathBuf::from(std::env::var_os("LCR_TEST_POISON_APPDATA").unwrap());

        assert_ne!(authority.local_app_data, poisoned_local);
        assert_ne!(authority.roaming_app_data, poisoned_roaming);
        assert_ne!(
            authority.programs,
            poisoned_roaming
                .join("Microsoft")
                .join("Windows")
                .join("Start Menu")
                .join("Programs")
        );
    }

    #[test]
    fn destructive_known_folder_authority_ignores_environment_poisoning() {
        let directory = cleanup_stage_test_dir("known-folder-environment");
        let poisoned_local = directory.join("poison-local-app-data");
        let poisoned_roaming = directory.join("poison-roaming-app-data");
        let current_test_executable = std::env::current_exe().unwrap();

        let status = std::process::Command::new(current_test_executable)
            .args([
                "--ignored",
                "--exact",
                "uninstall::tests::known_folder_environment_probe",
                "--nocapture",
            ])
            .env("LOCALAPPDATA", &poisoned_local)
            .env("APPDATA", &poisoned_roaming)
            .env("LCR_TEST_POISON_LOCALAPPDATA", &poisoned_local)
            .env("LCR_TEST_POISON_APPDATA", &poisoned_roaming)
            .status()
            .unwrap();

        assert!(status.success());

        fs::remove_dir_all(directory).unwrap();
    }

    #[derive(Default)]
    struct RecordingPolicyBCleanupOperations {
        calls: Vec<&'static str>,
        fail_executable: bool,
        fail_roaming: bool,
        fail_shortcut: bool,
        fail_registration: bool,
    }

    fn synthetic_policy_b_failure() -> io::Error {
        io::Error::other("synthetic Policy-B cleanup failure")
    }

    impl PolicyBCleanupOperations for RecordingPolicyBCleanupOperations {
        fn remove_executable(
            &mut self,
            _proof: &CleanupParentExitProof,
            _current_executable: &Path,
            _target: &Path,
        ) -> io::Result<()> {
            self.calls.push("executable");

            if self.fail_executable {
                Err(synthetic_policy_b_failure())
            } else {
                Ok(())
            }
        }

        fn remove_roaming_app_data(&mut self, _target: &Path) -> io::Result<()> {
            self.calls.push("roaming");

            if self.fail_roaming {
                Err(synthetic_policy_b_failure())
            } else {
                Ok(())
            }
        }

        fn remove_start_menu_shortcut(&mut self, _target: &Path) -> io::Result<()> {
            self.calls.push("shortcut");

            if self.fail_shortcut {
                Err(synthetic_policy_b_failure())
            } else {
                Ok(())
            }
        }

        fn unregister_installed_app(&mut self) -> io::Result<()> {
            self.calls.push("registration");

            if self.fail_registration {
                Err(synthetic_policy_b_failure())
            } else {
                Ok(())
            }
        }
    }

    fn synthetic_policy_b_targets(directory: &Path) -> PolicyBCleanupTargets {
        PolicyBCleanupTargets {
            executable_path: directory
                .join("LocalAppData")
                .join("Programs")
                .join("LCR")
                .join("LCR.exe"),
            app_data_directory: directory.join("RoamingAppData").join("LCR"),
            shortcut_path: directory.join("Programs").join("LCR.lnk"),
        }
    }

    #[test]
    fn policy_b_targets_use_independent_known_folder_roots() {
        let authority = DestructiveKnownFolderAuthority {
            local_app_data: PathBuf::from(r"C:\LCR-Test\Local"),
            roaming_app_data: PathBuf::from(r"D:\LCR-Test\Roaming"),
            programs: PathBuf::from(r"E:\LCR-Test\Programs"),
        };

        authority.validate().unwrap();

        let targets = PolicyBCleanupTargets::from_known_folders(&authority);

        assert_eq!(
            targets.executable_path,
            PathBuf::from(r"C:\LCR-Test\Local\Programs\LCR\LCR.exe")
        );
        assert_eq!(
            targets.app_data_directory,
            PathBuf::from(r"D:\LCR-Test\Roaming\LCR")
        );
        assert_eq!(
            targets.shortcut_path,
            PathBuf::from(r"E:\LCR-Test\Programs\LCR.lnk")
        );
    }

    #[test]
    fn policy_b_preflight_mismatch_performs_zero_operations() {
        let directory = cleanup_stage_test_dir("policy-b-preflight-mismatch");
        let proof = synthetic_parent_exit_proof(&directory, 0x61);
        let wrong_current_executable = proof
            .authority()
            .staging_directory
            .join("wrong-cleanup.exe");
        let targets = synthetic_policy_b_targets(&directory);
        let mut operations = RecordingPolicyBCleanupOperations::default();

        let error = execute_policy_b_cleanup_at(
            &proof,
            &wrong_current_executable,
            &targets,
            &mut operations,
        )
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(operations.calls.is_empty());

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn policy_b_executable_barrier_failure_blocks_all_later_cleanup() {
        let directory = cleanup_stage_test_dir("policy-b-barrier-failure");
        let proof = synthetic_parent_exit_proof(&directory, 0x62);
        let current_executable = proof.authority().executable_path.clone();
        let targets = synthetic_policy_b_targets(&directory);
        let mut operations = RecordingPolicyBCleanupOperations {
            fail_executable: true,
            ..Default::default()
        };

        let error =
            execute_policy_b_cleanup_at(&proof, &current_executable, &targets, &mut operations)
                .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(operations.calls, ["executable"]);

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn policy_b_success_runs_exact_monotonic_order() {
        let directory = cleanup_stage_test_dir("policy-b-success-order");
        let proof = synthetic_parent_exit_proof(&directory, 0x63);
        let current_executable = proof.authority().executable_path.clone();
        let targets = synthetic_policy_b_targets(&directory);
        let mut operations = RecordingPolicyBCleanupOperations::default();

        let outcome =
            execute_policy_b_cleanup_at(&proof, &current_executable, &targets, &mut operations)
                .unwrap();

        assert_eq!(outcome, PolicyBCleanupOutcome::Complete);
        assert_eq!(
            operations.calls,
            ["executable", "roaming", "shortcut", "registration"]
        );

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn policy_b_post_barrier_failures_continue_and_aggregate_in_order() {
        let directory = cleanup_stage_test_dir("policy-b-partial-failures");
        let proof = synthetic_parent_exit_proof(&directory, 0x64);
        let current_executable = proof.authority().executable_path.clone();
        let targets = synthetic_policy_b_targets(&directory);
        let mut operations = RecordingPolicyBCleanupOperations {
            fail_roaming: true,
            fail_shortcut: true,
            fail_registration: true,
            ..Default::default()
        };

        let outcome =
            execute_policy_b_cleanup_at(&proof, &current_executable, &targets, &mut operations)
                .unwrap();

        assert_eq!(
            outcome,
            PolicyBCleanupOutcome::Incomplete {
                failed_steps: vec![
                    PolicyBCleanupStep::RoamingAppData,
                    PolicyBCleanupStep::StartMenuShortcut,
                    PolicyBCleanupStep::InstalledAppsRegistration,
                ],
            }
        );
        assert_eq!(
            operations.calls,
            ["executable", "roaming", "shortcut", "registration"]
        );

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn policy_b_missing_owned_filesystem_resources_are_idempotent() {
        let directory = cleanup_stage_test_dir("policy-b-missing-resources");
        let missing_roaming = directory.join("missing-roaming").join("LCR");
        let missing_shortcut = directory.join("missing-programs").join("LCR.lnk");

        remove_directory_tree_idempotent(&missing_roaming).unwrap();
        shortcut::remove_shortcut_artifact(&missing_shortcut).unwrap();

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn policy_b_roaming_recursive_removal_does_not_follow_directory_junction() {
        let directory = cleanup_stage_test_dir("policy-b-junction-boundary");
        let owned_roaming = directory.join("RoamingAppData").join("LCR");
        let external_target = directory.join("external-target");
        let external_sentinel = external_target.join("must-survive.txt");
        let junction = owned_roaming.join("external-junction");

        fs::create_dir_all(&owned_roaming).unwrap();
        fs::create_dir_all(&external_target).unwrap();
        fs::write(&external_sentinel, b"must-survive").unwrap();

        let status = std::process::Command::new("cmd")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&external_target)
            .status()
            .unwrap();

        assert!(
            status.success(),
            "failed to create real Windows directory junction"
        );

        remove_directory_tree_idempotent(&owned_roaming).unwrap();

        assert!(!owned_roaming.exists());
        assert!(external_target.is_dir());
        assert_eq!(fs::read(&external_sentinel).unwrap(), b"must-survive");

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    #[ignore = "subprocess target for real staged cleanup self-delete"]
    fn c4c_cleanup_stage_self_unlink_probe() {
        let temp_dir = PathBuf::from(std::env::var_os("LCR_TEST_C4C_TEMP_DIR").unwrap());
        let parent_process_id = std::env::var("LCR_TEST_C4C_PARENT_PID")
            .unwrap()
            .parse::<u32>()
            .unwrap();
        let nonce = u64::from_str_radix(&std::env::var("LCR_TEST_C4C_NONCE").unwrap(), 16).unwrap();

        let authority = cleanup_child_authority(&temp_dir, parent_process_id, nonce).unwrap();
        let executable_path = authority.executable_path.clone();
        let staging_directory = authority.staging_directory.clone();
        let proof = CleanupParentExitProof { authority };

        assert_eq!(
            std::env::current_exe().unwrap(),
            proof.authority().executable_path
        );

        cleanup_staged_child_after_policy_b(&proof).unwrap();

        assert!(!executable_path.exists());
        assert!(!staging_directory.exists());
    }

    #[test]
    fn c4c_real_running_cleanup_copy_unlinks_itself_and_removes_stage() {
        let directory = cleanup_stage_test_dir("c4c-real-self-delete");
        let temp_dir = directory.join("Temp");
        let current_test_executable = std::env::current_exe().unwrap();
        let parent_process_id = std::process::id();
        let nonce = 0x71_u64;

        let authority = stage_cleanup_child(
            &current_test_executable,
            &temp_dir,
            parent_process_id,
            nonce,
        )
        .unwrap();

        let staged_executable = authority.executable_path.clone();
        let staging_directory = authority.staging_directory.clone();
        let outside_sentinel = directory.join("outside-must-survive.txt");

        fs::write(&outside_sentinel, b"outside-must-survive").unwrap();

        let status = std::process::Command::new(&staged_executable)
            .args([
                "--ignored",
                "--exact",
                "uninstall::tests::c4c_cleanup_stage_self_unlink_probe",
                "--nocapture",
            ])
            .env("LCR_TEST_C4C_TEMP_DIR", &temp_dir)
            .env("LCR_TEST_C4C_PARENT_PID", parent_process_id.to_string())
            .env("LCR_TEST_C4C_NONCE", format!("{nonce:016x}"))
            .status()
            .unwrap();

        assert!(status.success(), "real copied cleanup test process failed");
        assert!(!staged_executable.exists());
        assert!(!staging_directory.exists());
        assert_eq!(
            fs::read(&outside_sentinel).unwrap(),
            b"outside-must-survive"
        );

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn c4c_wrong_current_process_path_fails_before_mutation() {
        let directory = cleanup_stage_test_dir("c4c-wrong-current-path");
        let proof = synthetic_parent_exit_proof(&directory, 0x72);
        let executable_path = proof.authority().executable_path.clone();
        let wrong_current_executable = proof.authority().staging_directory.join("wrong.exe");

        fs::create_dir_all(&proof.authority().staging_directory).unwrap();
        fs::write(&executable_path, b"must-survive").unwrap();

        let error =
            cleanup_staged_child_after_policy_b_at(&proof, &wrong_current_executable).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(fs::read(&executable_path).unwrap(), b"must-survive");

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn c4c_unexpected_stage_sibling_survives_and_blocks_directory_removal() {
        let directory = cleanup_stage_test_dir("c4c-stage-sibling");
        let proof = synthetic_parent_exit_proof(&directory, 0x73);
        let executable_path = proof.authority().executable_path.clone();
        let staging_directory = proof.authority().staging_directory.clone();
        let sibling = staging_directory.join("unexpected-sibling.txt");

        fs::create_dir_all(&staging_directory).unwrap();
        fs::write(&executable_path, b"synthetic-cleanup").unwrap();
        fs::write(&sibling, b"must-survive").unwrap();

        let error = cleanup_staged_child_after_policy_b_at(&proof, &executable_path).unwrap_err();

        assert!(!executable_path.exists());
        assert_eq!(fs::read(&sibling).unwrap(), b"must-survive");
        assert!(staging_directory.is_dir());
        assert_ne!(error.kind(), io::ErrorKind::NotFound);

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn c4c_reparse_replacement_is_not_followed_to_external_target() {
        let directory = cleanup_stage_test_dir("c4c-reparse-replacement");
        let proof = synthetic_parent_exit_proof(&directory, 0x74);
        let executable_path = proof.authority().executable_path.clone();
        let staging_directory = proof.authority().staging_directory.clone();
        let external_target = directory.join("external-target");
        let external_sentinel = external_target.join("must-survive.txt");

        fs::create_dir_all(&staging_directory).unwrap();
        fs::create_dir_all(&external_target).unwrap();
        fs::write(&external_sentinel, b"must-survive").unwrap();

        let status = std::process::Command::new("cmd")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(&executable_path)
            .arg(&external_target)
            .status()
            .unwrap();

        assert!(
            status.success(),
            "failed to create replacement reparse point"
        );

        let result = cleanup_staged_child_after_policy_b_at(&proof, &executable_path);

        assert!(result.is_err());
        assert!(external_target.is_dir());
        assert_eq!(fs::read(&external_sentinel).unwrap(), b"must-survive");

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn canonical_executable_removal_deletes_only_exact_file_and_preserves_sibling() {
        let directory = cleanup_stage_test_dir("canonical-delete-exact");
        let local_app_data = directory.join("LocalAppData");
        let target = install_path(&local_app_data);
        let sibling = target.parent().unwrap().join("keep.txt");
        let proof = synthetic_parent_exit_proof(&directory, 0x51);
        let current_executable = proof.authority().executable_path.clone();

        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, b"canonical-helper").unwrap();
        fs::write(&sibling, b"keep-me").unwrap();

        remove_canonical_executable_after_parent_exit_at(
            &proof,
            &current_executable,
            &local_app_data,
        )
        .unwrap();

        assert!(!target.exists());
        assert_eq!(fs::read(&sibling).unwrap(), b"keep-me");

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn canonical_executable_removal_is_idempotent_when_target_is_missing() {
        let directory = cleanup_stage_test_dir("canonical-delete-missing");
        let local_app_data = directory.join("LocalAppData");
        let proof = synthetic_parent_exit_proof(&directory, 0x52);
        let current_executable = proof.authority().executable_path.clone();

        remove_canonical_executable_after_parent_exit_at(
            &proof,
            &current_executable,
            &local_app_data,
        )
        .unwrap();

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn canonical_executable_removal_rejects_process_proof_mismatch_before_delete() {
        let directory = cleanup_stage_test_dir("canonical-delete-mismatch");
        let local_app_data = directory.join("LocalAppData");
        let target = install_path(&local_app_data);
        let proof = synthetic_parent_exit_proof(&directory, 0x53);
        let wrong_current_executable = proof.authority().staging_directory.join("unexpected.exe");

        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, b"must-survive").unwrap();

        let error = remove_canonical_executable_after_parent_exit_at(
            &proof,
            &wrong_current_executable,
            &local_app_data,
        )
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(fs::read(&target).unwrap(), b"must-survive");

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn canonical_executable_removal_never_recursively_deletes_directory_target() {
        let directory = cleanup_stage_test_dir("canonical-delete-directory");
        let local_app_data = directory.join("LocalAppData");
        let target = install_path(&local_app_data);
        let sentinel = target.join("sentinel.txt");
        let proof = synthetic_parent_exit_proof(&directory, 0x54);
        let current_executable = proof.authority().executable_path.clone();

        fs::create_dir_all(&target).unwrap();
        fs::write(&sentinel, b"must-survive").unwrap();

        let error = remove_canonical_executable_after_parent_exit_at(
            &proof,
            &current_executable,
            &local_app_data,
        )
        .unwrap_err();

        assert!(target.is_dir());
        assert_eq!(fs::read(&sentinel).unwrap(), b"must-survive");
        assert_ne!(error.kind(), io::ErrorKind::NotFound);

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn cleanup_child_stage_copies_exact_bytes_to_derived_temp_authority() {
        let directory = cleanup_stage_test_dir("copy");
        let source = directory.join("current-LCR.exe");
        let temp = directory.join("Temp");

        fs::write(&source, b"trusted-helper-executable").unwrap();

        let authority = stage_cleanup_child(&source, &temp, 4242, 0x2a).unwrap();

        assert_eq!(
            authority,
            cleanup_child_authority(&temp, 4242, 0x2a).unwrap()
        );
        assert_eq!(
            fs::read(&authority.executable_path).unwrap(),
            b"trusted-helper-executable"
        );
        assert_eq!(
            fs::read_dir(&authority.staging_directory).unwrap().count(),
            1
        );

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn cleanup_child_stage_collision_fails_without_overwriting_existing_copy() {
        let directory = cleanup_stage_test_dir("collision");
        let source = directory.join("current-LCR.exe");
        let temp = directory.join("Temp");

        fs::write(&source, b"first-trusted-copy").unwrap();

        let authority = stage_cleanup_child(&source, &temp, 4242, 0x2a).unwrap();

        fs::write(&source, b"second-copy-must-not-replace").unwrap();

        let error = stage_cleanup_child(&source, &temp, 4242, 0x2a).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(
            fs::read(&authority.executable_path).unwrap(),
            b"first-trusted-copy"
        );

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn failed_cleanup_child_copy_removes_only_new_stage_directory() {
        let directory = cleanup_stage_test_dir("copy-failure");
        let missing_source = directory.join("missing-LCR.exe");
        let temp = directory.join("Temp");
        let authority = cleanup_child_authority(&temp, 4242, 0x2a).unwrap();

        let error = stage_cleanup_child(&missing_source, &temp, 4242, 0x2a).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(!authority.staging_directory.exists());
        assert!(uninstall_cleanup_root(&temp).is_dir());

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn cleanup_child_stage_rejects_invalid_identity_before_filesystem_mutation() {
        let directory = cleanup_stage_test_dir("invalid");
        let source = directory.join("current-LCR.exe");
        let temp = directory.join("Temp");

        fs::write(&source, b"trusted-helper-executable").unwrap();

        let error = stage_cleanup_child(&source, &temp, 0, 1).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(!uninstall_cleanup_root(&temp).exists());

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn sdk_handle_list_attribute_value_matches_windows_header_macro() {
        assert_eq!(PROC_THREAD_ATTRIBUTE_HANDLE_LIST, 0x0002_0002);
    }

    #[test]
    fn inheritable_self_process_handle_is_real_pointer_width_authority() {
        let handle = open_inheritable_self_process_handle().unwrap();
        let value = handle.value().unwrap();

        assert_ne!(value.get(), 0);
        assert_ne!(value.get(), usize::MAX);
        assert_eq!(value.encode().len(), std::mem::size_of::<usize>() * 2);
    }

    #[test]
    fn single_parent_handle_attribute_list_initializes_with_exact_handle() {
        let parent = open_inheritable_self_process_handle().unwrap();
        let expected = parent.value().unwrap();
        let mut attribute_list = build_parent_handle_attribute_list(&parent).unwrap();

        assert!(!attribute_list.as_mut_ptr().is_null());
        assert_eq!(attribute_list.inherited_handle_value().unwrap(), expected);
    }

    #[test]
    fn cleanup_child_command_line_carries_exact_handle_authority() {
        let temp = Path::new("Temp");
        let authority = cleanup_child_authority(temp, 4242, 0x2a).unwrap();
        let parent_handle = CleanupParentHandleValue::new(0x3b).unwrap();

        let command_line = cleanup_child_command_line(&authority, parent_handle).unwrap();
        let command_line = String::from_utf16(&command_line[..command_line.len() - 1]).unwrap();

        assert_eq!(
            command_line,
            format!(
                "\"{}\" --cleanup-uninstall 4242 000000000000002a {}",
                authority.executable_path.display(),
                parent_handle.encode()
            )
        );
        assert_eq!(cleanup_child_temp_dir(&authority).unwrap(), temp);
    }

    #[test]
    fn cleanup_child_command_line_rejects_tampered_staging_authority() {
        let temp = Path::new("Temp");
        let mut authority = cleanup_child_authority(temp, 4242, 0x2a).unwrap();
        let parent_handle = CleanupParentHandleValue::new(0x3b).unwrap();

        authority.executable_path = temp.join("unexpected.exe");

        let error = cleanup_child_command_line(&authority, parent_handle).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn native_cleanup_launch_rejects_authority_for_another_parent_process() {
        let current_process_id = std::process::id();
        let wrong_process_id = if current_process_id == u32::MAX {
            current_process_id - 1
        } else {
            current_process_id + 1
        };
        let authority = cleanup_child_authority(Path::new("Temp"), wrong_process_id, 0x2a).unwrap();

        let error = launch_cleanup_child(&authority).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn staged_cleanup_copy_can_be_launched_with_restricted_parent_handle() {
        let directory = cleanup_stage_test_dir("native-launch");
        let current_executable = std::env::current_exe().unwrap();
        let temp = directory.join("Temp");

        let authority =
            stage_cleanup_child(&current_executable, &temp, std::process::id(), 0x2b).unwrap();

        let child_process_id = launch_cleanup_child(&authority).unwrap();

        assert_ne!(child_process_id, 0);
        assert!(authority.executable_path.is_file());

        // C2B deliberately does not wait. The copied Rust test binary receives
        // an unsupported internal switch and exits independently. Cleanup here
        // is best-effort only; the ephemeral CI workspace is authoritative.
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn cleanup_child_execution_path_requires_exact_staged_authority() {
        let temp = Path::new("Temp");
        let authority = cleanup_child_authority(temp, 4242, 0x2a).unwrap();

        assert_eq!(
            validate_cleanup_child_execution_path(
                &authority.executable_path,
                authority.parent_process_id,
                authority.nonce,
            )
            .unwrap(),
            authority
        );

        let wrong_executable = authority.staging_directory.join("unexpected.exe");
        let error = validate_cleanup_child_execution_path(
            &wrong_executable,
            authority.parent_process_id,
            authority.nonce,
        )
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn inherited_parent_handle_flag_is_cleared_before_identity_use() {
        let parent = open_inheritable_self_process_handle().unwrap();

        let mut before = 0u32;
        let queried = unsafe { GetHandleInformation(parent.raw(), &mut before as *mut u32) };
        assert_ne!(queried, 0);
        assert_ne!(before & HANDLE_FLAG_INHERIT, 0);

        clear_inherited_handle_flag(parent.raw()).unwrap();

        let mut after = 0u32;
        let queried = unsafe { GetHandleInformation(parent.raw(), &mut after as *mut u32) };
        assert_ne!(queried, 0);
        assert_eq!(after & HANDLE_FLAG_INHERIT, 0);
    }

    #[test]
    fn inherited_parent_handle_self_pid_fails_closed_before_handle_adoption() {
        let parent = open_inheritable_self_process_handle().unwrap();
        let value = parent.value().unwrap();
        let current_process_id = unsafe { GetCurrentProcessId() };

        let error = validate_inherited_parent_process(current_process_id, value)
            .err()
            .unwrap();

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);

        // The rejection happens before the numeric HANDLE is adopted by the
        // cleanup-child RAII owner or has HANDLE_FLAG_INHERIT cleared.
        let mut flags = 0u32;
        let queried = unsafe { GetHandleInformation(parent.raw(), &mut flags as *mut u32) };

        assert_ne!(queried, 0);
        assert_ne!(flags & HANDLE_FLAG_INHERIT, 0);
    }

    #[test]
    fn inherited_parent_handle_pid_mismatch_fails_closed() {
        let current_process_id = unsafe { GetCurrentProcessId() };
        let raw = unsafe {
            OpenProcess(
                SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                TRUE,
                current_process_id,
            )
        };

        assert!(!raw.is_null());

        let value = CleanupParentHandleValue::new(raw as usize).unwrap();
        let wrong_process_id = if current_process_id == u32::MAX {
            current_process_id - 1
        } else {
            current_process_id + 1
        };

        let error = validate_inherited_parent_process(wrong_process_id, value)
            .err()
            .unwrap();

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    #[ignore = "subprocess target for parent-exit proof"]
    fn parent_wait_target_process() {
        use std::io::Read as _;

        let mut stdin = std::io::stdin().lock();
        let mut sink = Vec::new();

        stdin.read_to_end(&mut sink).unwrap();
    }

    #[test]
    fn cleanup_parent_exit_proof_waits_exact_real_process_object() {
        let current_test_executable = std::env::current_exe().unwrap();

        let mut child = std::process::Command::new(&current_test_executable)
            .args([
                "--ignored",
                "--exact",
                "uninstall::tests::parent_wait_target_process",
                "--nocapture",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();

        let target_process_id = child.id();

        let raw = unsafe {
            OpenProcess(
                SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                TRUE,
                target_process_id,
            )
        };

        if raw.is_null() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("failed to open deterministic target process");
        }

        let parent_handle = CleanupParentHandleValue::new(raw as usize).unwrap();
        let temp = Path::new("Temp");
        let authority = cleanup_child_authority(temp, target_process_id, 0x2c).unwrap();

        // Closing the pipe deterministically allows the ignored child test to
        // finish. prove_cleanup_parent_exit must then observe that exact process
        // HANDLE becoming signaled.
        drop(child.stdin.take());

        let proof = prove_cleanup_parent_exit(
            &authority.executable_path,
            target_process_id,
            authority.nonce,
            parent_handle,
        )
        .unwrap();

        assert_eq!(proof.authority(), &authority);

        let status = child.wait().unwrap();
        assert!(status.success());
    }

    #[test]
    fn cleanup_child_authority_rejects_zero_identity_components() {
        assert_eq!(cleanup_child_authority(Path::new("Temp"), 0, 1), None);
        assert_eq!(cleanup_child_authority(Path::new("Temp"), 1, 0), None);
    }

    #[test]
    fn uninstall_message_box_yes_proceeds() {
        assert_eq!(
            uninstall_decision_from_message_box_result(IDYES),
            UninstallDecision::Proceed
        );
    }

    #[test]
    fn uninstall_message_box_no_cancels() {
        assert_eq!(
            uninstall_decision_from_message_box_result(IDNO),
            UninstallDecision::Cancel
        );
    }

    #[test]
    fn uninstall_message_box_failure_and_unknown_values_fail_closed() {
        assert_eq!(
            uninstall_decision_from_message_box_result(0),
            UninstallDecision::Failed
        );
        assert_eq!(
            uninstall_decision_from_message_box_result(999),
            UninstallDecision::Failed
        );
    }

    #[test]
    fn uninstall_dialog_copy_encodes_as_single_nul_terminated_wide_strings() {
        for value in [l::UNINSTALL_CONFIRM_TITLE, l::UNINSTALL_CONFIRM_BODY] {
            let encoded = wide_nul(OsStr::new(value));

            assert_eq!(encoded.last(), Some(&0));
            assert!(!encoded[..encoded.len() - 1].contains(&0));
        }
    }

    #[test]
    fn clean_uninstall_plan_removes_all_per_user_lcr_state() {
        let app_data = Path::new("AppData");
        let local_app_data = Path::new("LocalAppData");

        let plan = clean_uninstall_plan(app_data, local_app_data);

        assert_eq!(
            plan.executable_path,
            local_app_data.join("Programs").join("LCR").join("LCR.exe")
        );
        assert_eq!(
            plan.install_directory,
            local_app_data.join("Programs").join("LCR")
        );
        assert_eq!(plan.app_data_directory, app_data.join("LCR"));
        assert_eq!(
            plan.shortcut_path,
            app_data
                .join("Microsoft")
                .join("Windows")
                .join("Start Menu")
                .join("Programs")
                .join("LCR.lnk")
        );
        assert_eq!(plan.uninstall_registry_subkey, UNINSTALL_REGISTRY_SUBKEY);

        assert_eq!(
            plan.app_data_directory.join("config.json"),
            app_data.join("LCR").join("config.json")
        );
        assert_eq!(
            update_state_path(app_data),
            plan.app_data_directory.join("update-state.json")
        );
    }
}
