use crate::{
    broker_artifact::{BROKER_SIGNED_MANIFEST_MAX_BYTES, BROKER_SIGNED_MANIFEST_RESOURCE_ID},
    distribution::{executable_product_version, prepare_executable_bytes_install, CURRENT_VERSION},
    task_authority::BROKER_EXECUTABLE_NAME,
};
use semver::Version;
use std::{
    ffi::OsString,
    fs, io,
    os::windows::ffi::OsStringExt,
    path::{Path, PathBuf},
    ptr::{null, null_mut},
    slice,
};
use windows::core::GUID;

pub const BROKER_PAYLOAD_RESOURCE_ID: usize = 101;
const RT_RCDATA: usize = 10;

const FOLDERID_PROGRAM_FILES: GUID = GUID::from_u128(0x905e63b6_c1bf_494e_b29c_65b732d3d21a);

type Handle = *mut core::ffi::c_void;

/// Opaque authority for the one fixed protected Broker location.
///
/// Production callers cannot manufacture this from an arbitrary path. The
/// capability is created only from Windows Program Files Known Folder
/// authority plus the current canonical Helper/Broker version.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProtectedBrokerPath {
    path: PathBuf,
}

impl ProtectedBrokerPath {
    pub fn as_path(&self) -> &Path {
        &self.path
    }
}

#[link(name = "shell32")]
extern "system" {
    fn SHGetKnownFolderPath(
        rfid: *const GUID,
        flags: u32,
        token: Handle,
        path: *mut *mut u16,
    ) -> i32;
}

#[link(name = "ole32")]
extern "system" {
    fn CoTaskMemFree(memory: *mut core::ffi::c_void);
}

#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleW(module_name: *const u16) -> isize;
    fn FindResourceW(module: isize, name: *const u16, resource_type: *const u16) -> Handle;
    fn LoadResource(module: isize, resource: Handle) -> Handle;
    fn SizeofResource(module: isize, resource: Handle) -> u32;
    fn LockResource(resource_data: Handle) -> *mut core::ffi::c_void;
}

fn io_other(message: impl Into<String>) -> io::Error {
    io::Error::other(message.into())
}

unsafe fn wide_ptr_to_path(pointer: *const u16) -> Option<PathBuf> {
    if pointer.is_null() {
        return None;
    }

    let mut len = 0usize;

    while len < 32_768 {
        if unsafe { *pointer.add(len) } == 0 {
            let units = unsafe { slice::from_raw_parts(pointer, len) };

            return Some(PathBuf::from(OsString::from_wide(units)));
        }

        len += 1;
    }

    None
}

/// Resolves protected Program Files through Windows Known Folder authority
/// instead of trusting `%ProgramFiles%`.
pub fn program_files_dir() -> io::Result<PathBuf> {
    let mut raw_path = null_mut::<u16>();

    let status =
        unsafe { SHGetKnownFolderPath(&FOLDERID_PROGRAM_FILES, 0, null_mut(), &mut raw_path) };

    if status < 0 || raw_path.is_null() {
        return Err(io_other(format!(
            "SHGetKnownFolderPath(FOLDERID_ProgramFiles) failed: HRESULT=0x{:08x}",
            status as u32
        )));
    }

    let path = unsafe { wide_ptr_to_path(raw_path) };

    unsafe {
        CoTaskMemFree(raw_path.cast());
    }

    let path = path.ok_or_else(|| io_other("Program Files path is not valid UTF-16"))?;

    if !path.is_absolute() {
        return Err(io_other("Program Files Known Folder path is not absolute"));
    }

    Ok(path)
}

/// Derives the single protected, versioned Broker path from already-resolved
/// Program Files authority.
///
/// This function remains private so production code cannot turn arbitrary
/// caller-controlled roots into a `ProtectedBrokerPath`.
fn protected_broker_path_from_root(
    program_files: &Path,
    version: &str,
) -> io::Result<ProtectedBrokerPath> {
    if !program_files.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Program Files path must be absolute",
        ));
    }

    let parsed = Version::parse(version).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidInput, "Broker version must be SemVer")
    })?;

    if parsed.to_string() != version {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Broker version must use canonical SemVer text",
        ));
    }

    let path = program_files
        .join("LCR")
        .join("InputBroker")
        .join(version)
        .join(BROKER_EXECUTABLE_NAME);

    if path.to_str().is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "protected Broker path must be Unicode",
        ));
    }

    Ok(ProtectedBrokerPath { path })
}

/// Returns the one production Broker-path capability.
///
/// Both the root and version are fixed authority: Program Files comes from the
/// Windows Known Folder API and the version is the Helper package version.
pub fn current_protected_broker_path() -> io::Result<ProtectedBrokerPath> {
    protected_broker_path_from_root(&program_files_dir()?, CURRENT_VERSION)
}

/// Returns the protected Broker-path capability for one canonical historical
/// or current Broker version under the real Program Files Known Folder.
///
/// The caller supplies only canonical SemVer text. It cannot supply a root,
/// executable name, or arbitrary filesystem path. This is the narrow P2B4
/// bridge required to recognize an already-installed older LCR Broker without
/// weakening `ProtectedBrokerPath` authority.
pub(crate) fn protected_broker_path_for_version(version: &str) -> io::Result<ProtectedBrokerPath> {
    protected_broker_path_from_root(&program_files_dir()?, version)
}

/// Test-only constructor for exercising path and Task Scheduler contracts
/// without depending on the local machine's physical Program Files location.
#[cfg(test)]
pub(crate) fn protected_broker_path_for_test(
    program_files: &Path,
    version: &str,
) -> io::Result<ProtectedBrokerPath> {
    protected_broker_path_from_root(program_files, version)
}
fn embedded_rcdata(
    resource_id: usize,
    description: &str,
    maximum_bytes: Option<usize>,
) -> io::Result<Vec<u8>> {
    let module = unsafe { GetModuleHandleW(null()) };

    if module == 0 {
        return Err(io::Error::last_os_error());
    }

    let resource =
        unsafe { FindResourceW(module, resource_id as *const u16, RT_RCDATA as *const u16) };

    if resource.is_null() {
        return Err(io::Error::last_os_error());
    }

    let size = unsafe { SizeofResource(module, resource) };

    if size == 0 {
        return Err(io_other(format!(
            "embedded {description} RCDATA resource is empty"
        )));
    }

    let size = size as usize;

    if maximum_bytes.is_some_and(|maximum| size > maximum) {
        return Err(io_other(format!(
            "embedded {description} RCDATA exceeds its size bound"
        )));
    }

    let loaded = unsafe { LoadResource(module, resource) };

    if loaded.is_null() {
        return Err(io::Error::last_os_error());
    }

    let data = unsafe { LockResource(loaded) };

    if data.is_null() {
        return Err(io_other(format!(
            "LockResource returned null for embedded {description}"
        )));
    }

    let bytes = unsafe { slice::from_raw_parts(data.cast::<u8>(), size) };

    Ok(bytes.to_vec())
}

/// Reads exact dedicated Broker bytes embedded in the running `LCR.exe`.
///
/// Standard development builds intentionally contain no resource 101 and
/// therefore fail closed here.
pub fn embedded_broker_payload() -> io::Result<Vec<u8>> {
    embedded_rcdata(BROKER_PAYLOAD_RESOURCE_ID, "Broker", None)
}

/// Reads the bounded signed Broker manifest embedded in the running `LCR.exe`.
///
/// Standard unsigned development builds may legitimately contain no resource
/// 102; absence is an ordinary fail-closed error.
pub(crate) fn embedded_broker_signed_manifest() -> io::Result<Vec<u8>> {
    embedded_rcdata(
        usize::from(BROKER_SIGNED_MANIFEST_RESOURCE_ID),
        "signed Broker manifest",
        Some(BROKER_SIGNED_MANIFEST_MAX_BYTES),
    )
}

// A3C1 deliberately has no startup caller. Keep the staged crate-private seam
// type-checked and lint-reachable without introducing runtime wiring.
const _: fn() -> io::Result<Vec<u8>> = embedded_broker_signed_manifest;

/// First-install protected Broker byte transaction only.
///
/// This function deliberately does not request elevation and does not mutate
/// Task Scheduler. P2B2B2 may call it only from its fixed elevated install
/// child after proving persistent authority is absent.
///
/// Existing targets fail closed; update/repair belongs to P2B4.
pub fn install_embedded_broker_first_install() -> io::Result<ProtectedBrokerPath> {
    let target = current_protected_broker_path()?;
    let target_path = target.as_path();

    if target_path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "protected Broker target already exists",
        ));
    }

    let payload = embedded_broker_payload()?;

    let prepared = prepare_executable_bytes_install(&payload, target_path)?;

    if fs::read(prepared.path())? != payload {
        return Err(io_other(
            "prepared protected Broker bytes differ from embedded payload",
        ));
    }

    prepared.commit_new()?;

    let installed = fs::read(target_path)?;

    if installed != payload {
        let _ = fs::remove_file(target_path);

        return Err(io_other(
            "committed protected Broker bytes differ from embedded payload",
        ));
    }

    match executable_product_version(target_path) {
        Ok(version) if version == CURRENT_VERSION => Ok(target),

        Ok(_) | Err(_) => {
            let _ = fs::remove_file(target_path);

            Err(io_other(
                "protected Broker ProductVersion does not match Helper",
            ))
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::{env, process};

    fn test_dir(case: &str) -> PathBuf {
        let directory =
            env::temp_dir().join(format!("lcr-broker-install-{case}-{}", process::id()));

        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn protected_path_is_versioned_and_exact() {
        let path =
            protected_broker_path_for_test(Path::new(r"C:\Program Files"), "0.0.4-beta").unwrap();

        assert_eq!(
            path.as_path(),
            Path::new(r"C:\Program Files\LCR\InputBroker\0.0.4-beta\LCRInputBroker.exe")
        );
    }

    #[test]
    fn protected_path_rejects_relative_root_and_non_semver_version() {
        assert!(protected_broker_path_for_test(Path::new("Program Files"), "0.0.4-beta",).is_err());

        assert!(
            protected_broker_path_for_test(Path::new(r"C:\Program Files"), r"..\escape",).is_err()
        );
    }

    #[test]
    fn embedded_resource_identity_is_fixed() {
        assert_eq!(BROKER_PAYLOAD_RESOURCE_ID, 101);

        assert_eq!(BROKER_SIGNED_MANIFEST_RESOURCE_ID, 102);
        assert_eq!(BROKER_SIGNED_MANIFEST_MAX_BYTES, 244);

        assert_eq!(RT_RCDATA, 10);
    }

    #[test]
    fn standard_development_test_artifact_lacks_signed_manifest_fail_closed() {
        assert!(embedded_broker_signed_manifest().is_err());
    }

    #[test]
    fn in_memory_executable_prepare_is_exact_and_atomic() {
        let directory = test_dir("bytes");

        let target = directory
            .join("LCR")
            .join("InputBroker")
            .join("test")
            .join(BROKER_EXECUTABLE_NAME);

        let payload = b"exact-broker-payload";

        let prepared = prepare_executable_bytes_install(payload, &target).unwrap();

        assert!(!target.exists());

        assert_eq!(fs::read(prepared.path()).unwrap(), payload);

        prepared.commit_new().unwrap();

        assert_eq!(fs::read(&target).unwrap(), payload);

        fs::remove_dir_all(directory).unwrap();
    }
}
