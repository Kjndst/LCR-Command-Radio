use crate::distribution::{
    install_dir, install_path, replace_same_directory_file_atomically, start_menu_shortcut_path,
};
use std::{
    ffi::{c_void, OsStr},
    fmt, fs,
    marker::PhantomData,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    ptr::null_mut,
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
};

#[cfg(test)]
use std::{ffi::OsString, os::windows::ffi::OsStringExt};

static SHORTCUT_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

type HResult = i32;
type VTableSlot = *const c_void;

const COINIT_APARTMENTTHREADED: u32 = 0x0000_0002;
const COINIT_DISABLE_OLE1DDE: u32 = 0x0000_0004;
const CLSCTX_INPROC_SERVER: u32 = 0x0000_0001;

#[cfg(test)]
const SLGP_RAWPATH: u32 = 0x0000_0004;
#[cfg(test)]
const READBACK_BUFFER_LEN: usize = 32_768;

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

static CLSID_SHELL_LINK: Guid = Guid {
    data1: 0x0002_1401,
    data2: 0x0000,
    data3: 0x0000,
    data4: [0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
};

static IID_ISHELL_LINK_W: Guid = Guid {
    data1: 0x0002_14F9,
    data2: 0x0000,
    data3: 0x0000,
    data4: [0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
};

static IID_IPERSIST_FILE: Guid = Guid {
    data1: 0x0000_010B,
    data2: 0x0000,
    data3: 0x0000,
    data4: [0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
};

#[link(name = "ole32")]
extern "system" {
    fn CoInitializeEx(reserved: *mut c_void, coinit: u32) -> HResult;
    fn CoUninitialize();
    fn CoCreateInstance(
        class_id: *const Guid,
        outer: *mut c_void,
        class_context: u32,
        interface_id: *const Guid,
        object: *mut *mut c_void,
    ) -> HResult;
}

#[repr(C)]
struct IUnknownVTable {
    query_interface: unsafe extern "system" fn(
        this: *mut c_void,
        interface_id: *const Guid,
        object: *mut *mut c_void,
    ) -> HResult,
    _add_ref: unsafe extern "system" fn(this: *mut c_void) -> u32,
    release: unsafe extern "system" fn(this: *mut c_void) -> u32,
}

#[repr(C)]
struct ShellLinkVTable {
    unknown: IUnknownVTable,
    get_path: unsafe extern "system" fn(
        this: *mut c_void,
        path: *mut u16,
        path_len: i32,
        find_data: *mut c_void,
        flags: u32,
    ) -> HResult,
    _get_id_list: VTableSlot,
    _set_id_list: VTableSlot,
    _get_description: VTableSlot,
    _set_description: VTableSlot,
    get_working_directory: unsafe extern "system" fn(
        this: *mut c_void,
        directory: *mut u16,
        directory_len: i32,
    ) -> HResult,
    set_working_directory:
        unsafe extern "system" fn(this: *mut c_void, directory: *const u16) -> HResult,
    _get_arguments: VTableSlot,
    _set_arguments: VTableSlot,
    _get_hotkey: VTableSlot,
    _set_hotkey: VTableSlot,
    _get_show_command: VTableSlot,
    _set_show_command: VTableSlot,
    get_icon_location: unsafe extern "system" fn(
        this: *mut c_void,
        icon_path: *mut u16,
        icon_path_len: i32,
        icon_index: *mut i32,
    ) -> HResult,
    set_icon_location: unsafe extern "system" fn(
        this: *mut c_void,
        icon_path: *const u16,
        icon_index: i32,
    ) -> HResult,
    _set_relative_path: VTableSlot,
    _resolve: VTableSlot,
    set_path: unsafe extern "system" fn(this: *mut c_void, path: *const u16) -> HResult,
}

#[repr(C)]
struct PersistFileVTable {
    unknown: IUnknownVTable,
    _get_class_id: VTableSlot,
    _is_dirty: VTableSlot,
    load: unsafe extern "system" fn(this: *mut c_void, path: *const u16, mode: u32) -> HResult,
    save: unsafe extern "system" fn(this: *mut c_void, path: *const u16, remember: i32) -> HResult,
    _save_completed: VTableSlot,
    _get_current_file: VTableSlot,
}

#[repr(C)]
struct ShellLinkRaw {
    vtable: *const ShellLinkVTable,
}

#[repr(C)]
struct PersistFileRaw {
    vtable: *const PersistFileVTable,
}

#[derive(Debug)]
pub enum ShortcutError {
    Io(std::io::Error),
    HResult {
        operation: &'static str,
        code: HResult,
    },
    NullComPointer {
        operation: &'static str,
    },
}

impl fmt::Display for ShortcutError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::HResult { operation, code } => {
                write!(
                    formatter,
                    "{operation} failed with HRESULT 0x{:08X}",
                    *code as u32
                )
            }
            Self::NullComPointer { operation } => {
                write!(formatter, "{operation} succeeded with a null COM pointer")
            }
        }
    }
}

impl std::error::Error for ShortcutError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::HResult { .. } | Self::NullComPointer { .. } => None,
        }
    }
}

impl From<std::io::Error> for ShortcutError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShortcutSpec {
    pub path: PathBuf,
    pub target: PathBuf,
    pub working_directory: PathBuf,
    pub icon_path: PathBuf,
}

pub fn start_menu_shortcut_spec(app_data: &Path, local_app_data: &Path) -> ShortcutSpec {
    let target = install_path(local_app_data);

    ShortcutSpec {
        path: start_menu_shortcut_path(app_data),
        target: target.clone(),
        working_directory: install_dir(local_app_data),
        icon_path: target,
    }
}

/// Creates a Shell Link through native COM, saves it to a same-directory
/// temporary file, then atomically replaces the final shortcut.
///
/// Production callers supply `start_menu_shortcut_spec`. Accepting a complete
/// specification keeps Windows CI isolated to its temporary directory.
pub fn remove_shortcut_artifact(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub fn create_start_menu_shortcut(spec: &ShortcutSpec) -> Result<(), ShortcutError> {
    validate_spec(spec)?;

    let parent = spec.path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "shortcut path has no parent directory",
        )
    })?;

    fs::create_dir_all(parent)?;

    let temporary = temporary_shortcut_path(&spec.path)?;

    let result = (|| -> Result<(), ShortcutError> {
        save_shell_link(spec, &temporary)?;

        fs::OpenOptions::new()
            .write(true)
            .open(&temporary)?
            .sync_all()?;

        replace_same_directory_file_atomically(&temporary, &spec.path)?;

        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }

    result
}

struct ComApartment {
    _thread_affinity: PhantomData<Rc<()>>,
}

impl ComApartment {
    fn initialize() -> Result<Self, ShortcutError> {
        let code = unsafe {
            CoInitializeEx(
                null_mut(),
                COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE,
            )
        };

        check_hresult("CoInitializeEx", code)?;

        Ok(Self {
            _thread_affinity: PhantomData,
        })
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

struct ShellLink<'apartment> {
    pointer: *mut ShellLinkRaw,
    _apartment: PhantomData<&'apartment ComApartment>,
    _thread_affinity: PhantomData<Rc<()>>,
}

impl<'apartment> ShellLink<'apartment> {
    fn create(_apartment: &'apartment ComApartment) -> Result<Self, ShortcutError> {
        let mut object: *mut c_void = null_mut();

        let code = unsafe {
            CoCreateInstance(
                &CLSID_SHELL_LINK,
                null_mut(),
                CLSCTX_INPROC_SERVER,
                &IID_ISHELL_LINK_W,
                &mut object,
            )
        };

        check_hresult("CoCreateInstance(CLSID_ShellLink)", code)?;

        if object.is_null() {
            return Err(ShortcutError::NullComPointer {
                operation: "CoCreateInstance(CLSID_ShellLink)",
            });
        }

        Ok(Self {
            pointer: object.cast::<ShellLinkRaw>(),
            _apartment: PhantomData,
            _thread_affinity: PhantomData,
        })
    }

    fn set_path(&self, path: &[u16]) -> Result<(), ShortcutError> {
        let code =
            unsafe { (self.vtable().set_path)(self.pointer.cast::<c_void>(), path.as_ptr()) };

        check_hresult("IShellLinkW::SetPath", code)
    }

    fn set_working_directory(&self, directory: &[u16]) -> Result<(), ShortcutError> {
        let code = unsafe {
            (self.vtable().set_working_directory)(self.pointer.cast::<c_void>(), directory.as_ptr())
        };

        check_hresult("IShellLinkW::SetWorkingDirectory", code)
    }

    fn set_icon_location(&self, path: &[u16]) -> Result<(), ShortcutError> {
        let code = unsafe {
            (self.vtable().set_icon_location)(self.pointer.cast::<c_void>(), path.as_ptr(), 0)
        };

        check_hresult("IShellLinkW::SetIconLocation", code)
    }

    #[cfg(test)]
    fn get_path(&self) -> Result<PathBuf, ShortcutError> {
        let mut buffer = vec![0; READBACK_BUFFER_LEN];

        let code = unsafe {
            (self.vtable().get_path)(
                self.pointer.cast::<c_void>(),
                buffer.as_mut_ptr(),
                READBACK_BUFFER_LEN as i32,
                null_mut(),
                SLGP_RAWPATH,
            )
        };

        check_hresult("IShellLinkW::GetPath", code)?;
        read_wide_buffer_path(&buffer, "IShellLinkW::GetPath")
    }

    #[cfg(test)]
    fn get_working_directory(&self) -> Result<PathBuf, ShortcutError> {
        let mut buffer = vec![0; READBACK_BUFFER_LEN];

        let code = unsafe {
            (self.vtable().get_working_directory)(
                self.pointer.cast::<c_void>(),
                buffer.as_mut_ptr(),
                READBACK_BUFFER_LEN as i32,
            )
        };

        check_hresult("IShellLinkW::GetWorkingDirectory", code)?;
        read_wide_buffer_path(&buffer, "IShellLinkW::GetWorkingDirectory")
    }

    #[cfg(test)]
    fn get_icon_location(&self) -> Result<(PathBuf, i32), ShortcutError> {
        let mut buffer = vec![0; READBACK_BUFFER_LEN];
        let mut icon_index = i32::MIN;

        let code = unsafe {
            (self.vtable().get_icon_location)(
                self.pointer.cast::<c_void>(),
                buffer.as_mut_ptr(),
                READBACK_BUFFER_LEN as i32,
                &mut icon_index,
            )
        };

        check_hresult("IShellLinkW::GetIconLocation", code)?;

        Ok((
            read_wide_buffer_path(&buffer, "IShellLinkW::GetIconLocation")?,
            icon_index,
        ))
    }

    fn persist_file(&self) -> Result<PersistFile<'apartment>, ShortcutError> {
        let mut object: *mut c_void = null_mut();

        let code = unsafe {
            (self.vtable().unknown.query_interface)(
                self.pointer.cast::<c_void>(),
                &IID_IPERSIST_FILE,
                &mut object,
            )
        };

        check_hresult("IUnknown::QueryInterface(IID_IPersistFile)", code)?;

        if object.is_null() {
            return Err(ShortcutError::NullComPointer {
                operation: "IUnknown::QueryInterface(IID_IPersistFile)",
            });
        }

        Ok(PersistFile {
            pointer: object.cast::<PersistFileRaw>(),
            _apartment: PhantomData,
            _thread_affinity: PhantomData,
        })
    }

    unsafe fn vtable(&self) -> &ShellLinkVTable {
        &*(*self.pointer).vtable
    }
}

impl Drop for ShellLink<'_> {
    fn drop(&mut self) {
        unsafe {
            let _ = (self.vtable().unknown.release)(self.pointer.cast::<c_void>());
        }
    }
}

struct PersistFile<'apartment> {
    pointer: *mut PersistFileRaw,
    _apartment: PhantomData<&'apartment ComApartment>,
    _thread_affinity: PhantomData<Rc<()>>,
}

impl PersistFile<'_> {
    #[cfg(test)]
    fn load(&self, path: &[u16]) -> Result<(), ShortcutError> {
        let code = unsafe { (self.vtable().load)(self.pointer.cast::<c_void>(), path.as_ptr(), 0) };

        check_hresult("IPersistFile::Load", code)
    }

    fn save(&self, path: &[u16]) -> Result<(), ShortcutError> {
        let code = unsafe { (self.vtable().save)(self.pointer.cast::<c_void>(), path.as_ptr(), 1) };

        check_hresult("IPersistFile::Save", code)
    }

    unsafe fn vtable(&self) -> &PersistFileVTable {
        &*(*self.pointer).vtable
    }
}

impl Drop for PersistFile<'_> {
    fn drop(&mut self) {
        unsafe {
            let _ = (self.vtable().unknown.release)(self.pointer.cast::<c_void>());
        }
    }
}

fn save_shell_link(spec: &ShortcutSpec, temporary: &Path) -> Result<(), ShortcutError> {
    let target = wide_path(&spec.target, "shortcut target")?;
    let working_directory = wide_path(&spec.working_directory, "shortcut working directory")?;
    let icon_path = wide_path(&spec.icon_path, "shortcut icon path")?;
    let temporary = wide_path(temporary, "shortcut temporary path")?;

    let apartment = ComApartment::initialize()?;

    {
        let shell_link = ShellLink::create(&apartment)?;

        shell_link.set_path(&target)?;
        shell_link.set_working_directory(&working_directory)?;
        shell_link.set_icon_location(&icon_path)?;

        let persist_file = shell_link.persist_file()?;
        persist_file.save(&temporary)?;
    }

    Ok(())
}

#[cfg(test)]
#[derive(Debug, Eq, PartialEq)]
struct ShortcutPayload {
    target: PathBuf,
    working_directory: PathBuf,
    icon_path: PathBuf,
    icon_index: i32,
}

#[cfg(test)]
fn read_shell_link_payload(path: &Path) -> Result<ShortcutPayload, ShortcutError> {
    validate_absolute_path(path, "shortcut readback path")?;
    let path = wide_path(path, "shortcut readback path")?;
    let apartment = ComApartment::initialize()?;

    let shell_link = ShellLink::create(&apartment)?;
    let persist_file = shell_link.persist_file()?;

    persist_file.load(&path)?;

    let target = shell_link.get_path()?;
    let working_directory = shell_link.get_working_directory()?;
    let (icon_path, icon_index) = shell_link.get_icon_location()?;

    Ok(ShortcutPayload {
        target,
        working_directory,
        icon_path,
        icon_index,
    })
}

#[cfg(test)]
fn read_wide_buffer_path(buffer: &[u16], field: &'static str) -> Result<PathBuf, ShortcutError> {
    let end = buffer.iter().position(|value| *value == 0).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{field} did not return a NUL-terminated path"),
        )
    })?;

    if end == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{field} returned an empty path"),
        )
        .into());
    }

    Ok(PathBuf::from(OsString::from_wide(&buffer[..end])))
}

fn validate_spec(spec: &ShortcutSpec) -> Result<(), ShortcutError> {
    validate_absolute_path(&spec.path, "shortcut path")?;
    validate_absolute_path(&spec.target, "shortcut target")?;
    validate_absolute_path(&spec.working_directory, "shortcut working directory")?;
    validate_absolute_path(&spec.icon_path, "shortcut icon path")
}

fn validate_absolute_path(path: &Path, field: &'static str) -> Result<(), ShortcutError> {
    if path.as_os_str().is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{field} is empty"),
        )
        .into());
    }

    if !path.is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{field} must be absolute"),
        )
        .into());
    }

    Ok(())
}

fn wide_path(path: &Path, field: &'static str) -> Result<Vec<u16>, ShortcutError> {
    wide_os_str(path.as_os_str(), field)
}

fn wide_os_str(value: &OsStr, field: &'static str) -> Result<Vec<u16>, ShortcutError> {
    let mut wide = value.encode_wide().collect::<Vec<_>>();

    if wide.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{field} is empty"),
        )
        .into());
    }

    if wide.contains(&0) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{field} contains an interior NUL"),
        )
        .into());
    }

    wide.push(0);
    Ok(wide)
}

fn temporary_shortcut_path(final_path: &Path) -> Result<PathBuf, ShortcutError> {
    let parent = final_path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "shortcut path has no parent directory",
        )
    })?;

    for _ in 0..32 {
        let sequence = SHORTCUT_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);

        let candidate = parent.join(format!(".LCR.{}.{}.tmp.lnk", std::process::id(), sequence));

        if !candidate.exists() {
            return Ok(candidate);
        }
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "unable to allocate shortcut temporary path",
    )
    .into())
}

fn check_hresult(operation: &'static str, code: HResult) -> Result<(), ShortcutError> {
    if code >= 0 {
        Ok(())
    } else {
        Err(ShortcutError::HResult { operation, code })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::size_of;

    fn shortcut_test_dir(case: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("lcr-helper-shortcut-{case}-{}", std::process::id()));

        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();

        directory
    }

    #[test]
    fn start_menu_shortcut_reuses_canonical_distribution_authority() {
        let app_data = Path::new("App Data");
        let local_app_data = Path::new("Local App Data");
        let spec = start_menu_shortcut_spec(app_data, local_app_data);

        let expected_shortcut = app_data
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs")
            .join("LCR.lnk");

        let expected_target = local_app_data.join("Programs").join("LCR").join("LCR.exe");
        let expected_working_directory = local_app_data.join("Programs").join("LCR");

        assert_eq!(spec.path, expected_shortcut);
        assert_eq!(spec.target, expected_target);
        assert_eq!(spec.working_directory, expected_working_directory);
        assert_eq!(spec.icon_path, spec.target);
    }

    #[test]
    fn start_menu_shortcut_contract_never_targets_desktop() {
        let spec = start_menu_shortcut_spec(Path::new("App Data"), Path::new("Local App Data"));

        assert!(!spec
            .path
            .to_string_lossy()
            .to_ascii_lowercase()
            .contains("desktop"));
    }

    #[test]
    fn shell_link_guids_match_windows_contract() {
        assert_eq!(CLSID_SHELL_LINK.data1, 0x0002_1401);
        assert_eq!(IID_ISHELL_LINK_W.data1, 0x0002_14F9);
        assert_eq!(IID_IPERSIST_FILE.data1, 0x0000_010B);

        for guid in [CLSID_SHELL_LINK, IID_ISHELL_LINK_W, IID_IPERSIST_FILE] {
            assert_eq!(guid.data2, 0);
            assert_eq!(guid.data3, 0);
            assert_eq!(guid.data4, [0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46]);
        }
    }

    #[test]
    fn vtable_layouts_preserve_required_slot_counts() {
        assert_eq!(size_of::<ShellLinkVTable>(), 21 * size_of::<VTableSlot>());
        assert_eq!(size_of::<PersistFileVTable>(), 9 * size_of::<VTableSlot>());
    }

    #[test]
    fn wide_windows_path_rejects_interior_nul() {
        let error = wide_os_str(OsStr::new("C:\\LCR\0invalid.exe"), "test path").unwrap_err();

        match error {
            ShortcutError::Io(error) => {
                assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn relative_shortcut_spec_fails_before_any_filesystem_write() {
        let spec = ShortcutSpec {
            path: PathBuf::from("relative/LCR.lnk"),
            target: PathBuf::from("relative/LCR.exe"),
            working_directory: PathBuf::from("relative"),
            icon_path: PathBuf::from("relative/LCR.exe"),
        };

        let error = create_start_menu_shortcut(&spec).unwrap_err();

        match error {
            ShortcutError::Io(error) => {
                assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
            }
            other => panic!("unexpected error: {other:?}"),
        }

        assert!(!spec.path.exists());
    }

    #[test]
    fn com_apartment_accepts_balanced_repeated_same_mode_initialization() {
        let first = ComApartment::initialize().unwrap();
        let second = ComApartment::initialize().unwrap();

        drop(second);
        drop(first);

        let third = ComApartment::initialize().unwrap();
        drop(third);
    }

    #[test]
    fn shortcut_artifact_removal_is_idempotent() {
        let directory = shortcut_test_dir("remove");
        let shortcut = directory.join("LCR.lnk");

        fs::write(&shortcut, b"shortcut-artifact").unwrap();

        remove_shortcut_artifact(&shortcut).unwrap();
        assert!(!shortcut.exists());

        remove_shortcut_artifact(&shortcut).unwrap();
        assert!(!shortcut.exists());

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn shortcut_artifact_removal_propagates_non_not_found_errors() {
        let directory = shortcut_test_dir("remove-error");
        let shortcut = directory.join("LCR.lnk");

        fs::create_dir_all(&shortcut).unwrap();

        let error = remove_shortcut_artifact(&shortcut).unwrap_err();

        assert_ne!(error.kind(), std::io::ErrorKind::NotFound);
        assert!(shortcut.is_dir());

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn native_shell_link_writer_smoke_creates_atomic_nonempty_artifact() {
        let directory = shortcut_test_dir("native-smoke");
        let executable = std::env::current_exe().unwrap();
        let working_directory = executable.parent().unwrap().to_path_buf();

        let spec = ShortcutSpec {
            path: directory.join("LCR.lnk"),
            target: executable.clone(),
            working_directory,
            icon_path: executable,
        };

        create_start_menu_shortcut(&spec).unwrap();

        let metadata = fs::metadata(&spec.path).unwrap();

        assert!(metadata.is_file());
        assert!(metadata.len() > 0);

        let temporary_count = fs::read_dir(&directory)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                name.starts_with(".LCR.") && name.ends_with(".tmp.lnk")
            })
            .count();

        assert_eq!(temporary_count, 0);

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn native_shell_link_payload_round_trips_exact_fields() {
        let directory = shortcut_test_dir("payload-readback");
        let executable = std::env::current_exe().unwrap();
        let working_directory = executable.parent().unwrap().to_path_buf();

        let spec = ShortcutSpec {
            path: directory.join("LCR.lnk"),
            target: executable.clone(),
            working_directory: working_directory.clone(),
            icon_path: executable.clone(),
        };

        create_start_menu_shortcut(&spec).unwrap();

        let payload = read_shell_link_payload(&spec.path).unwrap();

        assert_eq!(payload.target, spec.target);
        assert_eq!(payload.working_directory, working_directory);
        assert_eq!(payload.icon_path, executable);
        assert_eq!(payload.icon_index, 0);

        fs::remove_dir_all(directory).unwrap();
    }
}
