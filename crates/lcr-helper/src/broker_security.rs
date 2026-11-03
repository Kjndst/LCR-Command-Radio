use crate::{
    broker_install::{current_protected_broker_path, ProtectedBrokerPath},
    task_authority::valid_sid_text,
};
use std::{
    ffi::OsStr,
    fs, io,
    os::windows::{ffi::OsStrExt, fs::MetadataExt},
    path::{Path, PathBuf},
    ptr::null_mut,
    slice,
};

type Handle = *mut core::ffi::c_void;

const SECURITY_DESCRIPTOR_REVISION: u32 = 1;
const SE_FILE_OBJECT: i32 = 1;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;

const OWNER_SECURITY_INFORMATION: u32 = 0x0000_0001;
const DACL_SECURITY_INFORMATION: u32 = 0x0000_0004;
const PROTECTED_DACL_SECURITY_INFORMATION: u32 = 0x8000_0000;

const READBACK_SECURITY_INFORMATION: u32 = OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;

const WRITE_SECURITY_INFORMATION: u32 =
    READBACK_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION;

#[link(name = "kernel32")]
extern "system" {
    fn LocalFree(memory: Handle) -> Handle;
}

#[link(name = "advapi32")]
extern "system" {
    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        string_security_descriptor: *const u16,
        string_sd_revision: u32,
        security_descriptor: *mut Handle,
        security_descriptor_size: *mut u32,
    ) -> i32;

    fn ConvertSecurityDescriptorToStringSecurityDescriptorW(
        security_descriptor: Handle,
        requested_string_sd_revision: u32,
        security_information: u32,
        string_security_descriptor: *mut *mut u16,
        string_security_descriptor_length: *mut u32,
    ) -> i32;

    fn GetSecurityDescriptorOwner(
        security_descriptor: Handle,
        owner: *mut Handle,
        owner_defaulted: *mut i32,
    ) -> i32;

    fn GetSecurityDescriptorDacl(
        security_descriptor: Handle,
        dacl_present: *mut i32,
        dacl: *mut Handle,
        dacl_defaulted: *mut i32,
    ) -> i32;

    fn SetNamedSecurityInfoW(
        object_name: *mut u16,
        object_type: i32,
        security_information: u32,
        owner: Handle,
        group: Handle,
        dacl: Handle,
        sacl: Handle,
    ) -> u32;

    fn GetNamedSecurityInfoW(
        object_name: *mut u16,
        object_type: i32,
        security_information: u32,
        owner: *mut Handle,
        group: *mut Handle,
        dacl: *mut Handle,
        sacl: *mut Handle,
        security_descriptor: *mut Handle,
    ) -> u32;
}

fn io_other(message: impl Into<String>) -> io::Error {
    io::Error::other(message.into())
}

fn win32_error(code: u32) -> io::Error {
    io::Error::from_raw_os_error(code as i32)
}

struct LocalAllocation(Handle);

impl LocalAllocation {
    fn new(pointer: Handle) -> Option<Self> {
        if pointer.is_null() {
            None
        } else {
            Some(Self(pointer))
        }
    }

    fn as_ptr(&self) -> Handle {
        self.0
    }
}

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        unsafe {
            let _ = LocalFree(self.0);
        }
    }
}

fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

unsafe fn wide_ptr_to_string(pointer: *const u16) -> Option<String> {
    if pointer.is_null() {
        return None;
    }

    let mut len = 0usize;

    while len < 65_536 {
        if unsafe { *pointer.add(len) } == 0 {
            let units = unsafe { slice::from_raw_parts(pointer, len) };

            return String::from_utf16(units).ok();
        }

        len += 1;
    }

    None
}

fn directory_security_sddl(user_sid: &str) -> io::Result<String> {
    if !valid_sid_text(user_sid) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid Broker user SID",
        ));
    }

    Ok(format!(
        "O:BAD:P\
(A;OICI;FA;;;SY)\
(A;OICI;FA;;;BA)\
(A;OICI;FRFX;;;{user_sid})"
    ))
}

fn file_security_sddl(user_sid: &str) -> io::Result<String> {
    if !valid_sid_text(user_sid) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid Broker user SID",
        ));
    }

    Ok(format!(
        "O:BAD:P\
(A;;FA;;;SY)\
(A;;FA;;;BA)\
(A;;FRFX;;;{user_sid})"
    ))
}

fn descriptor_from_sddl(sddl: &str) -> io::Result<LocalAllocation> {
    let sddl = wide(OsStr::new(sddl));

    let mut descriptor: Handle = null_mut();

    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SECURITY_DESCRIPTOR_REVISION,
            &mut descriptor,
            null_mut(),
        )
    };

    if ok == 0 {
        return Err(io::Error::last_os_error());
    }

    LocalAllocation::new(descriptor).ok_or_else(|| io_other("SDDL conversion returned null"))
}

fn descriptor_to_sddl(descriptor: Handle) -> io::Result<String> {
    if descriptor.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "security descriptor is null",
        ));
    }

    let mut text: *mut u16 = null_mut();

    let ok = unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor,
            SECURITY_DESCRIPTOR_REVISION,
            READBACK_SECURITY_INFORMATION,
            &mut text,
            null_mut(),
        )
    };

    if ok == 0 {
        return Err(io::Error::last_os_error());
    }

    let allocation = LocalAllocation::new(text.cast())
        .ok_or_else(|| io_other("security descriptor text is null"))?;

    unsafe { wide_ptr_to_string(allocation.as_ptr().cast()) }
        .ok_or_else(|| io_other("security descriptor text is invalid UTF-16"))
}

fn descriptor_owner_and_dacl(descriptor: Handle) -> io::Result<(Handle, Handle)> {
    let mut owner: Handle = null_mut();
    let mut owner_defaulted = 0i32;

    let owner_ok =
        unsafe { GetSecurityDescriptorOwner(descriptor, &mut owner, &mut owner_defaulted) };

    if owner_ok == 0 || owner.is_null() {
        return Err(io::Error::last_os_error());
    }

    let mut dacl_present = 0i32;
    let mut dacl: Handle = null_mut();
    let mut dacl_defaulted = 0i32;

    let dacl_ok = unsafe {
        GetSecurityDescriptorDacl(
            descriptor,
            &mut dacl_present,
            &mut dacl,
            &mut dacl_defaulted,
        )
    };

    if dacl_ok == 0 {
        return Err(io::Error::last_os_error());
    }

    if dacl_present == 0 || dacl.is_null() {
        return Err(io_other("required Broker DACL is missing or null"));
    }

    Ok((owner, dacl))
}

fn normalized_sddl(sddl: &str) -> io::Result<String> {
    let descriptor = descriptor_from_sddl(sddl)?;

    descriptor_to_sddl(descriptor.as_ptr())
}

fn set_named_security(path: &Path, expected_sddl: &str) -> io::Result<()> {
    let descriptor = descriptor_from_sddl(expected_sddl)?;

    let (owner, dacl) = descriptor_owner_and_dacl(descriptor.as_ptr())?;

    let mut path_wide = wide(path.as_os_str());

    let status = unsafe {
        SetNamedSecurityInfoW(
            path_wide.as_mut_ptr(),
            SE_FILE_OBJECT,
            WRITE_SECURITY_INFORMATION,
            owner,
            null_mut(),
            dacl,
            null_mut(),
        )
    };

    if status != 0 {
        return Err(win32_error(status));
    }

    Ok(())
}

fn read_named_security_sddl(path: &Path) -> io::Result<String> {
    let mut path_wide = wide(path.as_os_str());

    let mut descriptor: Handle = null_mut();

    let status = unsafe {
        GetNamedSecurityInfoW(
            path_wide.as_mut_ptr(),
            SE_FILE_OBJECT,
            READBACK_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            null_mut(),
            null_mut(),
            &mut descriptor,
        )
    };

    if status != 0 {
        return Err(win32_error(status));
    }

    let descriptor = LocalAllocation::new(descriptor)
        .ok_or_else(|| io_other("security readback returned null descriptor"))?;

    descriptor_to_sddl(descriptor.as_ptr())
}

fn security_readback_matches(expected: &str, actual: &str) -> bool {
    if actual == expected {
        return true;
    }

    // Filesystem security providers may mark an otherwise exact protected
    // DACL as auto-inherited after applying it. AI is provider metadata; P
    // remains the authority boundary. Accept only this exact additional flag.
    let Some(dacl) = expected.find("D:P") else {
        return false;
    };

    let mut with_auto_inherited = String::with_capacity(expected.len() + 2);
    with_auto_inherited.push_str(&expected[..dacl]);
    with_auto_inherited.push_str("D:PAI");
    with_auto_inherited.push_str(&expected[dacl + 3..]);

    actual == with_auto_inherited
}

fn security_matches(path: &Path, expected_sddl: &str) -> io::Result<bool> {
    let expected = normalized_sddl(expected_sddl)?;

    let actual = read_named_security_sddl(path)?;

    Ok(security_readback_matches(&expected, &actual))
}

fn apply_and_verify_security(path: &Path, expected_sddl: &str) -> io::Result<()> {
    set_named_security(path, expected_sddl)?;

    if !security_matches(path, expected_sddl)? {
        return Err(io_other(
            "Broker filesystem security readback differs from required contract",
        ));
    }

    Ok(())
}

fn protected_directory_chain(target: &ProtectedBrokerPath) -> io::Result<[PathBuf; 3]> {
    let version = target
        .as_path()
        .parent()
        .ok_or_else(|| io_other("Broker target has no version directory"))?;

    let input_broker = version
        .parent()
        .ok_or_else(|| io_other("Broker target has no InputBroker directory"))?;

    let lcr = input_broker
        .parent()
        .ok_or_else(|| io_other("Broker target has no LCR directory"))?;

    if input_broker.file_name() != Some(OsStr::new("InputBroker"))
        || lcr.file_name() != Some(OsStr::new("LCR"))
    {
        return Err(io_other(
            "protected Broker directory shape differs from fixed contract",
        ));
    }

    Ok([
        lcr.to_path_buf(),
        input_broker.to_path_buf(),
        version.to_path_buf(),
    ])
}

fn is_reparse_attributes(attributes: u32) -> bool {
    attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

fn existing_metadata_without_reparse(path: &Path) -> io::Result<fs::Metadata> {
    let metadata = fs::symlink_metadata(path)?;

    if is_reparse_attributes(metadata.file_attributes()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "protected Broker path component is a reparse point",
        ));
    }

    Ok(metadata)
}

fn require_first_install_path_absent(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if is_reparse_attributes(metadata.file_attributes()) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "protected Broker first-install path is a reparse point",
                ));
            }

            Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "protected Broker first-install path already exists",
            ))
        }

        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),

        Err(error) => Err(error),
    }
}

fn remove_created_directory(path: &Path) -> io::Result<()> {
    match fs::remove_dir(path) {
        Ok(()) => Ok(()),

        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),

        Err(error) => Err(error),
    }
}

fn create_and_protect_directory_first_install(path: &Path, expected_sddl: &str) -> io::Result<()> {
    require_first_install_path_absent(path)?;

    // Parent authority is already trusted before this point:
    // Program Files for LCR, then the previously protected directory for
    // each subsequent child.
    fs::create_dir(path)?;

    let result = (|| {
        let metadata = existing_metadata_without_reparse(path)?;

        if !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "new protected Broker path is not a directory",
            ));
        }

        apply_and_verify_security(path, expected_sddl)
    })();

    if let Err(setup_error) = result {
        match remove_created_directory(path) {
            Ok(()) => return Err(setup_error),

            Err(cleanup_error) => {
                return Err(io_other(format!(
                    "protected Broker directory setup failed and cleanup also failed: \
setup={setup_error}; cleanup={cleanup_error}; path={}",
                    path.display()
                )));
            }
        }
    }

    Ok(())
}

fn rollback_created_directories(directories: &[PathBuf]) -> io::Result<()> {
    let mut first_failure: Option<String> = None;

    // Reverse order is required: child must be removed before its protected
    // parent can be removed.
    for directory in directories.iter().rev() {
        if let Err(error) = remove_created_directory(directory) {
            if first_failure.is_none() {
                first_failure = Some(format!(
                    "failed to rollback protected Broker directory {}: {error}",
                    directory.display()
                ));
            }
        }
    }

    match first_failure {
        Some(error) => Err(io_other(error)),
        None => Ok(()),
    }
}
/// Establishes the complete first-install namespace below trusted Program
/// Files.
///
/// Every path component must be absent before this transaction. Existing
/// directories, symlinks, junctions, or other reparse points are never adopted
/// or repaired here; repair belongs to P2B4.
///
/// Each child is created only after its parent is already trusted/protected.
/// A failure removes all directories created by this invocation in reverse
/// order before returning.
pub fn establish_protected_broker_directories(user_sid: &str) -> io::Result<ProtectedBrokerPath> {
    let target = current_protected_broker_path()?;

    let expected = directory_security_sddl(user_sid)?;

    let chain = protected_directory_chain(&target)?;

    // Prove the complete candidate namespace is absent before mutating any
    // component. This avoids discovering an existing path only after earlier
    // components were already created.
    for directory in &chain {
        require_first_install_path_absent(directory)?;
    }

    let mut created = Vec::<PathBuf>::with_capacity(chain.len());

    for directory in chain {
        if let Err(step_error) = create_and_protect_directory_first_install(&directory, &expected) {
            match rollback_created_directories(&created) {
                Ok(()) => return Err(step_error),

                Err(rollback_error) => {
                    return Err(io_other(
                        format!(
                            "protected Broker namespace establishment failed and rollback was incomplete: \
step={step_error}; rollback={rollback_error}"
                        )
                    ));
                }
            }
        }

        created.push(directory);
    }

    Ok(target)
}

fn require_existing_protected_directory(path: &Path, expected_sddl: &str) -> io::Result<()> {
    let metadata = existing_metadata_without_reparse(path)?;

    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "protected Broker update parent is not a directory",
        ));
    }

    if !security_matches(path, expected_sddl)? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "protected Broker update directory security differs from required contract",
        ));
    }

    Ok(())
}

/// Establishes only the signed target-version directory for a Broker update.
///
/// The persistent LCR and InputBroker parents must already be exact protected
/// authority. They are never created, adopted, or repaired here. An existing
/// target-version directory is reusable only when it independently satisfies
/// the same no-reparse, directory, and exact ACL contract.
pub(crate) fn establish_protected_broker_update_directory(
    user_sid: &str,
    target: &ProtectedBrokerPath,
) -> io::Result<()> {
    let expected = directory_security_sddl(user_sid)?;
    let [lcr, input_broker, version] = protected_directory_chain(target)?;

    require_existing_protected_directory(&lcr, &expected)?;
    require_existing_protected_directory(&input_broker, &expected)?;

    match fs::symlink_metadata(&version) {
        Ok(_) => require_existing_protected_directory(&version, &expected),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            create_and_protect_directory_first_install(&version, &expected)
        }
        Err(error) => Err(error),
    }
}
/// Read-only pristine-first-install check.
///
/// Every directory in the protected Broker namespace must still be absent.
/// Existing state is never adopted by the first-install transaction.
pub(crate) fn protected_broker_first_install_namespace_absent(
    target: &ProtectedBrokerPath,
) -> io::Result<bool> {
    for directory in protected_directory_chain(target)? {
        match std::fs::symlink_metadata(&directory) {
            Ok(_) => return Ok(false),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }

    Ok(true)
}

/// Rolls back only a namespace previously returned successfully by
/// `establish_protected_broker_directories` in this same transaction.
///
/// Successful establishment proves that the complete chain was absent before
/// this invocation created it.
pub(crate) fn rollback_protected_broker_directories_first_install(
    target: &ProtectedBrokerPath,
) -> io::Result<()> {
    let chain = protected_directory_chain(target)?;
    rollback_created_directories(&chain)
}
/// Applies and verifies an explicit protected owner/DACL to the installed
/// Broker executable after exact embedded-byte installation.
pub fn protect_and_verify_installed_broker(
    user_sid: &str,
    target: &ProtectedBrokerPath,
) -> io::Result<()> {
    let metadata = existing_metadata_without_reparse(target.as_path())?;

    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "protected Broker executable is not a regular file",
        ));
    }

    let expected = file_security_sddl(user_sid)?;

    apply_and_verify_security(target.as_path(), &expected)
}

/// Applies and verifies the fixed Broker file ACL on one create-new private
/// install candidate in the exact protected target directory.
pub(crate) fn protect_and_verify_broker_candidate(
    user_sid: &str,
    target: &ProtectedBrokerPath,
    candidate: &Path,
) -> io::Result<()> {
    let target_path = target.as_path();
    let target_name = target_path
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| io_other("protected Broker target filename is not Unicode"))?;
    let candidate_name = candidate
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| io_other("protected Broker candidate filename is not Unicode"))?;
    let expected_prefix = format!(".{target_name}.");

    if candidate.parent() != target_path.parent()
        || candidate == target_path
        || !candidate_name.starts_with(&expected_prefix)
        || !candidate_name.ends_with(".install.tmp")
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Broker candidate is not the private same-directory install temporary",
        ));
    }

    let metadata = existing_metadata_without_reparse(candidate)?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "protected Broker candidate is not a regular file",
        ));
    }

    let expected = file_security_sddl(user_sid)?;
    apply_and_verify_security(candidate, &expected)
}
/// Read-only persistent filesystem authority proof.
///
/// No directory, ACL, file, Task Scheduler object, process, or UAC state is
/// changed by this function.
pub fn protected_broker_filesystem_security_matches(
    user_sid: &str,
    target: &ProtectedBrokerPath,
) -> io::Result<bool> {
    let directory_expected = directory_security_sddl(user_sid)?;

    for directory in protected_directory_chain(target)? {
        let metadata = match existing_metadata_without_reparse(&directory) {
            Ok(metadata) => metadata,

            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(false);
            }

            Err(error) => return Err(error),
        };

        if !metadata.is_dir() {
            return Ok(false);
        }

        if !security_matches(&directory, &directory_expected)? {
            return Ok(false);
        }
    }

    let file_metadata = match existing_metadata_without_reparse(target.as_path()) {
        Ok(metadata) => metadata,

        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(false);
        }

        Err(error) => return Err(error),
    };

    if !file_metadata.is_file() {
        return Ok(false);
    }

    let file_expected = file_security_sddl(user_sid)?;

    security_matches(target.as_path(), &file_expected)
}

/// LCR-parent ACL used only after the fixed privileged Broker Task has already
/// been deleted and its absence has been read back.
///
/// SYSTEM and Administrators retain full control. The exact owning user gains
/// only generic read/execute plus DELETE: enough to enumerate the LCR parent,
/// traverse it, and remove it when empty, without create/write/DACL/owner
/// authority at the Program Files\LCR root.
fn uninstall_handoff_lcr_root_security_sddl(user_sid: &str) -> io::Result<String> {
    if !valid_sid_text(user_sid) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid Broker uninstall user SID",
        ));
    }
    Ok(format!(
        "O:BAD:P\
(A;OICI;FA;;;SY)\
(A;OICI;FA;;;BA)\
(A;;FRFXSD;;;{user_sid})"
    ))
}

fn uninstall_handoff_directory_security_sddl(user_sid: &str) -> io::Result<String> {
    if !valid_sid_text(user_sid) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid Broker uninstall user SID",
        ));
    }
    Ok(format!(
        "O:BAD:P\
(A;OICI;FA;;;SY)\
(A;OICI;FA;;;BA)\
(A;;FRFXSD;;;{user_sid})"
    ))
}

fn uninstall_handoff_file_security_sddl(user_sid: &str) -> io::Result<String> {
    if !valid_sid_text(user_sid) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid Broker uninstall user SID",
        ));
    }
    Ok(format!(
        "O:BAD:P\
(A;;FA;;;SY)\
(A;;FA;;;BA)\
(A;;FRSD;;;{user_sid})"
    ))
}
struct UninstallHandoffLayout {
    lcr_root: PathBuf,
    input_broker_root: PathBuf,
    directories: Vec<PathBuf>,
    files: Vec<PathBuf>,
}

fn collect_uninstall_handoff_subtree(
    directory: &Path,
    directories: &mut Vec<PathBuf>,
    files: &mut Vec<PathBuf>,
) -> io::Result<()> {
    let metadata = existing_metadata_without_reparse(directory)?;

    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Broker uninstall subtree contains a non-directory root",
        ));
    }

    directories.push(directory.to_path_buf());

    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = existing_metadata_without_reparse(&path)?;

        if metadata.is_dir() {
            collect_uninstall_handoff_subtree(&path, directories, files)?;
        } else if metadata.is_file() {
            files.push(path);
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Broker uninstall subtree contains an unsupported filesystem object",
            ));
        }
    }

    Ok(())
}

/// Performs the complete read-only shape/reparse preflight before any uninstall
/// ACL is relaxed.
///
/// The current recognized Broker target must still exist inside the exact
/// Program Files\LCR\InputBroker subtree. Every existing descendant must be a
/// regular directory or file and no reparse point is accepted.
fn uninstall_handoff_layout(target: &ProtectedBrokerPath) -> io::Result<UninstallHandoffLayout> {
    let [lcr_root, input_broker_root, version_root] = protected_directory_chain(target)?;

    for directory in [&lcr_root, &input_broker_root] {
        let metadata = existing_metadata_without_reparse(directory)?;

        if !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Broker uninstall authority root is not a directory",
            ));
        }
    }

    let mut directories = Vec::new();
    let mut files = Vec::new();

    collect_uninstall_handoff_subtree(&input_broker_root, &mut directories, &mut files)?;

    if !directories.iter().any(|path| path == &version_root) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "recognized Broker version directory is absent from uninstall subtree",
        ));
    }

    if !files.iter().any(|path| path == target.as_path()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "recognized Broker executable is absent from uninstall subtree",
        ));
    }

    Ok(UninstallHandoffLayout {
        lcr_root,
        input_broker_root,
        directories,
        files,
    })
}

/// Performs the complete read-only shape/reparse proof required before the
/// privileged Task-deletion commit barrier.
///
/// No ACL, file, directory, Task Scheduler object, or process is changed.
pub(crate) fn validate_uninstall_handoff_filesystem_shape(
    target: &ProtectedBrokerPath,
) -> io::Result<()> {
    uninstall_handoff_layout(target).map(|_| ())
}

/// Returns only the canonical protected roots that the ordinary staged cleanup
/// child may remove after the elevated Broker has committed uninstall handoff.
///
/// This function performs no mutation.
pub(crate) fn protected_broker_uninstall_cleanup_roots(
    target: &ProtectedBrokerPath,
) -> io::Result<(PathBuf, PathBuf)> {
    let [lcr_root, input_broker_root, _] = protected_directory_chain(target)?;

    Ok((lcr_root, input_broker_root))
}

fn uninstall_handoff_layout_security_matches(
    user_sid: &str,
    layout: &UninstallHandoffLayout,
) -> io::Result<bool> {
    let lcr_root_expected = uninstall_handoff_lcr_root_security_sddl(user_sid)?;
    let directory_expected = uninstall_handoff_directory_security_sddl(user_sid)?;
    let file_expected = uninstall_handoff_file_security_sddl(user_sid)?;

    if !security_matches(&layout.lcr_root, &lcr_root_expected)? {
        return Ok(false);
    }
    for directory in &layout.directories {
        if !security_matches(directory, &directory_expected)? {
            return Ok(false);
        }
    }
    for file in &layout.files {
        if !security_matches(file, &file_expected)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Read-only proof that the complete protected Broker subtree is in the exact
/// post-Task-deletion uninstall handoff ACL state.
///
/// This remains the strict COMMITTED readback immediately after the elevated
/// Broker transaction: the recognized version directory and Broker executable
/// must still exist.
pub(crate) fn uninstall_handoff_filesystem_security_matches(
    user_sid: &str,
    target: &ProtectedBrokerPath,
) -> io::Result<bool> {
    let layout = uninstall_handoff_layout(target)?;
    uninstall_handoff_layout_security_matches(user_sid, &layout)
}

/// Snapshots only cleanup residue that still exists after an already committed
/// handoff. Missing descendants are allowed; reparse or wrong-type descendants
/// are never accepted.
fn uninstall_handoff_cleanup_residue_layout(
    target: &ProtectedBrokerPath,
) -> io::Result<Option<UninstallHandoffLayout>> {
    let [lcr_root, input_broker_root, _] = protected_directory_chain(target)?;

    let lcr_metadata = match existing_metadata_without_reparse(&lcr_root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !lcr_metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Broker uninstall cleanup LCR root is not a directory",
        ));
    }

    let mut directories = Vec::new();
    let mut files = Vec::new();
    match existing_metadata_without_reparse(&input_broker_root) {
        Ok(metadata) => {
            if !metadata.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Broker uninstall cleanup InputBroker root is not a directory",
                ));
            }
            collect_uninstall_handoff_subtree(&input_broker_root, &mut directories, &mut files)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    Ok(Some(UninstallHandoffLayout {
        lcr_root,
        input_broker_root,
        directories,
        files,
    }))
}

/// Validates a crash-resumable, shrink-only uninstall residue. Success means
/// the LCR protected namespace is already gone, or every remaining InputBroker
/// object still has the exact minimal post-commit handoff ACL.
pub(crate) fn validate_uninstall_handoff_cleanup_residue(
    user_sid: &str,
    target: &ProtectedBrokerPath,
) -> io::Result<()> {
    let Some(layout) = uninstall_handoff_cleanup_residue_layout(target)? else {
        return Ok(());
    };
    if !uninstall_handoff_layout_security_matches(user_sid, &layout)? {
        return Err(io_other(
            "Broker uninstall cleanup residue differs from required handoff security",
        ));
    }
    Ok(())
}
/// Relaxes only the already-validated protected Broker namespace for the final
/// ordinary cleanup child.
///
/// SECURITY ORDERING CONTRACT:
/// callers must delete the fixed Broker Task and prove its absence BEFORE
/// invoking this function. This function does not inspect or mutate Task
/// Scheduler authority itself.
///
/// All filesystem shape/reparse validation completes before the first ACL
/// mutation. Files are changed before directories. Directories are changed
/// deepest-first, with the LCR parent changed last, so ordinary-user cleanup
/// authority is exposed as late as possible.
pub(crate) fn establish_uninstall_handoff_filesystem_security(
    user_sid: &str,
    target: &ProtectedBrokerPath,
) -> io::Result<()> {
    let mut layout = uninstall_handoff_layout(target)?;

    let lcr_root_expected = uninstall_handoff_lcr_root_security_sddl(user_sid)?;
    let directory_expected = uninstall_handoff_directory_security_sddl(user_sid)?;
    let file_expected = uninstall_handoff_file_security_sddl(user_sid)?;

    layout.files.sort();

    for file in &layout.files {
        apply_and_verify_security(file, &file_expected)?;
    }

    layout.directories.sort_by(|left, right| {
        right
            .components()
            .count()
            .cmp(&left.components().count())
            .then_with(|| left.cmp(right))
    });

    for directory in &layout.directories {
        apply_and_verify_security(directory, &directory_expected)?;
    }

    // LCR is intentionally last. The fixed Task must already be absent before
    // this function is reachable.
    apply_and_verify_security(&layout.lcr_root, &lcr_root_expected)?;

    if !uninstall_handoff_filesystem_security_matches(user_sid, target)? {
        return Err(io_other(
            "Broker uninstall filesystem handoff readback differs from required contract",
        ));
    }

    Ok(())
}

fn validate_uninstall_cleanup_descendant(
    input_broker_root: &Path,
    path: &Path,
    expected_directory: bool,
) -> io::Result<bool> {
    let relative = path.strip_prefix(input_broker_root).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "Broker uninstall cleanup path escaped InputBroker root",
        )
    })?;

    let root_metadata = match existing_metadata_without_reparse(input_broker_root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if !root_metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Broker uninstall cleanup root is not a directory",
        ));
    }

    if relative.as_os_str().is_empty() {
        if !expected_directory {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Broker uninstall cleanup root was classified as a file",
            ));
        }
        return Ok(true);
    }

    let mut current = input_broker_root.to_path_buf();
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Broker uninstall cleanup path contains lexical traversal",
            ));
        };
        current.push(name);
        let metadata = match existing_metadata_without_reparse(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        let final_component = current == path;
        if final_component {
            let type_matches = if expected_directory {
                metadata.is_dir()
            } else {
                metadata.is_file()
            };
            if !type_matches {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Broker uninstall cleanup object type changed after handoff",
                ));
            }
        } else if !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Broker uninstall cleanup ancestor is no longer a directory",
            ));
        }
    }
    if current != path {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Broker uninstall cleanup path reconstruction mismatch",
        ));
    }
    Ok(true)
}
fn remove_uninstall_handoff_layout(
    mut layout: UninstallHandoffLayout,
    input_broker_root: &Path,
) -> io::Result<()> {
    if layout.input_broker_root != input_broker_root {
        return Err(io_other(
            "Broker uninstall cleanup root differs from residue snapshot",
        ));
    }

    layout.files.sort();
    for file in &layout.files {
        if !validate_uninstall_cleanup_descendant(input_broker_root, file, false)? {
            continue;
        }
        match fs::remove_file(file) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }

    layout.directories.sort_by(|left, right| {
        right
            .components()
            .count()
            .cmp(&left.components().count())
            .then_with(|| left.cmp(right))
    });
    for directory in &layout.directories {
        if !validate_uninstall_cleanup_descendant(input_broker_root, directory, true)? {
            continue;
        }
        match fs::remove_dir(directory) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }

    let lcr_metadata = match existing_metadata_without_reparse(&layout.lcr_root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !lcr_metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "LCR uninstall parent is not a directory",
        ));
    }

    let mut remaining = match fs::read_dir(&layout.lcr_root) {
        Ok(remaining) => remaining,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    match remaining.next() {
        None => match fs::remove_dir(&layout.lcr_root) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        },
        Some(Ok(entry)) => Err(io_other(format!(
            "LCR uninstall parent contains unrelated residue: {}",
            entry.path().display()
        ))),
        Some(Err(error)) => Err(error),
    }
}
/// Removes only the exact post-commit protected Broker namespace.
///
/// Preconditions are re-proven in this ordinary-user process:
/// - fixed Task authority must already have been destroyed by the elevated
///   Broker before the caller reaches this function;
/// - every remaining InputBroker residue object must still have the exact
///   minimal uninstall-handoff ACL state;
/// - all remaining descendants must still be ordinary files/directories with no
///   reparse points. Missing descendants are monotonic success.
///
/// Deletion is snapshot-bounded and non-recursive at every directory level.
/// Unexpected additions or replacement reparse points therefore fail closed
/// and remain as residue.
pub(crate) fn remove_uninstall_handoff_filesystem(
    user_sid: &str,
    target: &ProtectedBrokerPath,
) -> io::Result<()> {
    let Some(layout) = uninstall_handoff_cleanup_residue_layout(target)? else {
        return Ok(());
    };
    if !uninstall_handoff_layout_security_matches(user_sid, &layout)? {
        return Err(io_other(
            "Broker uninstall cleanup requires exact handoff residue security",
        ));
    }

    let (lcr_root, input_broker_root) = protected_broker_uninstall_cleanup_roots(target)?;
    if layout.lcr_root != lcr_root || layout.input_broker_root != input_broker_root {
        return Err(io_other(
            "Broker uninstall cleanup roots differ from protected authority",
        ));
    }

    remove_uninstall_handoff_layout(layout, &input_broker_root)
}
#[cfg(test)]
mod uninstall_handoff_tests {
    use super::*;
    use crate::broker_install::protected_broker_path_for_test;

    const USER_SID: &str = "S-1-5-21-111111111-222222222-333333333-1001";

    fn handoff_test_root(case: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "lcr-broker-uninstall-handoff-{case}-{}",
            std::process::id()
        ));

        let _ = fs::remove_dir_all(&root);
        root
    }

    #[test]
    fn uninstall_handoff_security_contract_is_least_privilege_for_cleanup() {
        let lcr_root = uninstall_handoff_lcr_root_security_sddl(USER_SID).unwrap();
        let directory = uninstall_handoff_directory_security_sddl(USER_SID).unwrap();
        let file = uninstall_handoff_file_security_sddl(USER_SID).unwrap();

        assert_eq!(
            lcr_root,
            format!(
                "O:BAD:P\
(A;OICI;FA;;;SY)\
(A;OICI;FA;;;BA)\
(A;;FRFXSD;;;{USER_SID})"
            )
        );
        assert_eq!(
            directory,
            format!(
                "O:BAD:P\
(A;OICI;FA;;;SY)\
(A;OICI;FA;;;BA)\
(A;;FRFXSD;;;{USER_SID})"
            )
        );
        assert_eq!(
            file,
            format!(
                "O:BAD:P\
(A;;FA;;;SY)\
(A;;FA;;;BA)\
(A;;FRSD;;;{USER_SID})"
            )
        );
        assert!(normalized_sddl(&lcr_root).is_ok());
        assert!(normalized_sddl(&directory).is_ok());
        assert!(normalized_sddl(&file).is_ok());
        for sddl in [&lcr_root, &directory, &file] {
            let user_ace = sddl
                .split('(')
                .find(|ace| ace.contains(USER_SID))
                .expect("user cleanup ACE");
            assert!(!user_ace.contains(";FA;"));
            assert!(!user_ace.contains(";FW;"));
            assert!(!user_ace.contains("WD"));
            assert!(!user_ace.contains("WO"));
        }
    }
    #[test]
    fn uninstall_handoff_security_contract_rejects_sid_injection() {
        let malicious = "S-1-5-21-1-2-3-1001)(A;;GA;;;WD";

        assert!(uninstall_handoff_lcr_root_security_sddl(malicious).is_err());
        assert!(uninstall_handoff_directory_security_sddl(malicious).is_err());
        assert!(uninstall_handoff_file_security_sddl(malicious).is_err());
    }

    #[test]
    fn uninstall_handoff_layout_covers_complete_input_broker_subtree() {
        let root = handoff_test_root("layout");
        let target = protected_broker_path_for_test(&root, "0.0.4-beta").unwrap();

        fs::create_dir_all(target.as_path().parent().unwrap()).unwrap();
        fs::write(target.as_path(), b"broker").unwrap();

        let older = root.join("LCR").join("InputBroker").join("0.0.3-beta");

        fs::create_dir_all(&older).unwrap();
        fs::write(older.join("LCRInputBroker.exe"), b"older").unwrap();

        let layout = uninstall_handoff_layout(&target).unwrap();

        assert_eq!(layout.lcr_root, root.join("LCR"));

        assert!(layout
            .directories
            .iter()
            .any(|path| path == target.as_path().parent().unwrap()));

        assert!(layout.files.iter().any(|path| path == target.as_path()));

        assert!(layout
            .files
            .iter()
            .any(|path| path == &older.join("LCRInputBroker.exe")));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn uninstall_handoff_layout_rejects_reparse_descendant() {
        let root = handoff_test_root("reparse-descendant");
        let target = protected_broker_path_for_test(&root, "0.0.4-beta").unwrap();

        fs::create_dir_all(target.as_path().parent().unwrap()).unwrap();
        fs::write(target.as_path(), b"broker").unwrap();

        let external = root.join("external-target");
        fs::create_dir_all(&external).unwrap();

        let junction = root
            .join("LCR")
            .join("InputBroker")
            .join("unexpected-junction");

        let status = std::process::Command::new("cmd")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&external)
            .status()
            .unwrap();

        assert!(
            status.success(),
            "failed to create real Windows directory junction"
        );

        let error = match uninstall_handoff_layout(&target) {
            Ok(_) => panic!("reparse descendant must fail closed"),
            Err(error) => error,
        };

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);

        fs::remove_dir(&junction).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn uninstall_cleanup_roots_are_bounded_to_lcr_input_broker_namespace() {
        let target =
            protected_broker_path_for_test(Path::new(r"C:\Program Files"), "0.0.4-beta").unwrap();

        assert_eq!(
            protected_broker_uninstall_cleanup_roots(&target).unwrap(),
            (
                PathBuf::from(r"C:\Program Files\LCR"),
                PathBuf::from(r"C:\Program Files\LCR\InputBroker"),
            )
        );
    }

    #[test]
    fn uninstall_cleanup_removes_complete_input_broker_tree_but_preserves_unrelated_lcr_sibling() {
        let root = handoff_test_root("cleanup-preserve-parent");
        let target = protected_broker_path_for_test(&root, "0.0.4-beta").unwrap();
        fs::create_dir_all(target.as_path().parent().unwrap()).unwrap();
        fs::write(target.as_path(), b"current").unwrap();
        let older = root.join("LCR").join("InputBroker").join("0.0.3-beta");
        fs::create_dir_all(&older).unwrap();
        fs::write(older.join("LCRInputBroker.exe"), b"older").unwrap();
        let lcr_root = root.join("LCR");
        let keep = lcr_root.join("unrelated.keep");
        fs::write(&keep, b"keep").unwrap();
        let layout = uninstall_handoff_layout(&target).unwrap();
        let (_, input_broker_root) = protected_broker_uninstall_cleanup_roots(&target).unwrap();
        let error = remove_uninstall_handoff_layout(layout, &input_broker_root)
            .expect_err("unrelated LCR sibling must be preserved and report incomplete");
        assert_ne!(error.kind(), io::ErrorKind::NotFound);
        assert!(!input_broker_root.exists());
        assert!(lcr_root.exists());
        assert!(keep.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn uninstall_cleanup_residue_accepts_partial_shrink_without_current_target() {
        let root = handoff_test_root("resume-partial");
        let target = protected_broker_path_for_test(&root, "0.0.4-beta").unwrap();
        fs::create_dir_all(target.as_path().parent().unwrap()).unwrap();
        fs::write(target.as_path(), b"current").unwrap();
        let older = root.join("LCR").join("InputBroker").join("0.0.3-beta");
        fs::create_dir_all(&older).unwrap();
        fs::write(older.join("LCRInputBroker.exe"), b"older").unwrap();
        fs::remove_file(target.as_path()).unwrap();
        fs::remove_dir(target.as_path().parent().unwrap()).unwrap();
        let residue = uninstall_handoff_cleanup_residue_layout(&target)
            .unwrap()
            .expect("LCR residue remains");
        assert_eq!(residue.lcr_root, root.join("LCR"));
        assert_eq!(
            residue.input_broker_root,
            root.join("LCR").join("InputBroker")
        );
        assert!(!residue
            .directories
            .iter()
            .any(|path| path == target.as_path().parent().unwrap()));
        assert!(residue
            .files
            .iter()
            .any(|path| path == &older.join("LCRInputBroker.exe")));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn uninstall_cleanup_residue_accepts_missing_input_broker_and_missing_lcr() {
        let root = handoff_test_root("resume-gone");
        let target = protected_broker_path_for_test(&root, "0.0.4-beta").unwrap();
        fs::create_dir_all(target.as_path().parent().unwrap()).unwrap();
        fs::write(target.as_path(), b"current").unwrap();
        let lcr_root = root.join("LCR");
        let input_broker_root = lcr_root.join("InputBroker");
        fs::remove_dir_all(&input_broker_root).unwrap();
        let residue = uninstall_handoff_cleanup_residue_layout(&target)
            .unwrap()
            .expect("LCR root still remains");
        assert!(residue.directories.is_empty());
        assert!(residue.files.is_empty());
        fs::remove_dir(&lcr_root).unwrap();
        assert!(uninstall_handoff_cleanup_residue_layout(&target)
            .unwrap()
            .is_none());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn uninstall_cleanup_removes_lcr_parent_only_when_empty() {
        let root = handoff_test_root("cleanup-empty-parent");
        let target = protected_broker_path_for_test(&root, "0.0.4-beta").unwrap();

        fs::create_dir_all(target.as_path().parent().unwrap()).unwrap();
        fs::write(target.as_path(), b"current").unwrap();

        let layout = uninstall_handoff_layout(&target).unwrap();
        let (lcr_root, input_broker_root) =
            protected_broker_uninstall_cleanup_roots(&target).unwrap();

        remove_uninstall_handoff_layout(layout, &input_broker_root).unwrap();

        assert!(!input_broker_root.exists());
        assert!(!lcr_root.exists());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn uninstall_cleanup_snapshot_never_recursively_erases_late_addition() {
        let root = handoff_test_root("cleanup-late-addition");
        let target = protected_broker_path_for_test(&root, "0.0.4-beta").unwrap();

        fs::create_dir_all(target.as_path().parent().unwrap()).unwrap();
        fs::write(target.as_path(), b"current").unwrap();

        let layout = uninstall_handoff_layout(&target).unwrap();
        let (_, input_broker_root) = protected_broker_uninstall_cleanup_roots(&target).unwrap();

        let late = input_broker_root.join("late-added.keep");
        fs::write(&late, b"late").unwrap();

        let error = remove_uninstall_handoff_layout(layout, &input_broker_root)
            .expect_err("late object must keep InputBroker non-empty and fail closed");

        assert_ne!(error.kind(), io::ErrorKind::NotFound);
        assert!(input_broker_root.exists());
        assert!(late.exists());

        fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker_install::protected_broker_path_for_test;

    const USER_SID: &str = "S-1-5-21-111111111-222222222-333333333-1001";

    #[test]
    fn security_readback_accepts_only_auto_inherited_control_metadata() {
        let expected =
            "O:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1200a9;;;S-1-5-21-1-2-3-1001)";

        let actual =
            "O:BAD:PAI(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1200a9;;;S-1-5-21-1-2-3-1001)";

        assert!(security_readback_matches(expected, expected));
        assert!(security_readback_matches(expected, actual));
    }

    #[test]
    fn security_readback_rejects_other_control_or_acl_changes() {
        let expected =
            "O:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1200a9;;;S-1-5-21-1-2-3-1001)";

        assert!(!security_readback_matches(
            expected,
            "O:BAD:PAR(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1200a9;;;S-1-5-21-1-2-3-1001)"
        ));

        assert!(!security_readback_matches(
            expected,
            "O:BAD:PAI(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;S-1-5-21-1-2-3-1001)"
        ));

        assert!(!security_readback_matches(
            expected,
            "O:BAD:AI(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1200a9;;;S-1-5-21-1-2-3-1001)"
        ));
    }

    #[test]
    fn security_contract_is_owner_admin_protected_and_user_read_execute_only() {
        let directory = directory_security_sddl(USER_SID).unwrap();

        let file = file_security_sddl(USER_SID).unwrap();

        assert_eq!(
            directory,
            format!(
                "O:BAD:P\
(A;OICI;FA;;;SY)\
(A;OICI;FA;;;BA)\
(A;OICI;FRFX;;;{USER_SID})"
            )
        );

        assert_eq!(
            file,
            format!(
                "O:BAD:P\
(A;;FA;;;SY)\
(A;;FA;;;BA)\
(A;;FRFX;;;{USER_SID})"
            )
        );

        assert!(normalized_sddl(&directory).is_ok());

        assert!(normalized_sddl(&file).is_ok());
    }

    #[test]
    fn security_contract_rejects_sid_injection() {
        let malicious = "S-1-5-21-1-2-3-1001)(A;;GA;;;WD";

        assert!(directory_security_sddl(malicious).is_err());

        assert!(file_security_sddl(malicious).is_err());
    }

    #[test]
    fn protected_chain_covers_every_new_directory_below_program_files() {
        let target =
            protected_broker_path_for_test(Path::new(r"C:\Program Files"), "0.0.4-beta").unwrap();

        assert_eq!(
            protected_directory_chain(&target).unwrap(),
            [
                PathBuf::from(r"C:\Program Files\LCR"),
                PathBuf::from(r"C:\Program Files\LCR\InputBroker"),
                PathBuf::from(r"C:\Program Files\LCR\InputBroker\0.0.4-beta"),
            ]
        );
    }
    #[test]
    fn reparse_attribute_is_always_rejected_by_classifier() {
        assert!(is_reparse_attributes(FILE_ATTRIBUTE_REPARSE_POINT));

        assert!(is_reparse_attributes(FILE_ATTRIBUTE_REPARSE_POINT | 0x10));

        assert!(!is_reparse_attributes(0));
    }

    #[test]
    fn preexisting_directory_is_not_adopted_by_first_install() {
        let directory = std::env::temp_dir().join(format!(
            "lcr-broker-security-existing-{}",
            std::process::id()
        ));

        let _ = fs::remove_dir_all(&directory);

        fs::create_dir(&directory).unwrap();

        let error = require_first_install_path_absent(&directory)
            .expect_err("pre-existing directory must hard-stop first install");

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);

        fs::remove_dir(directory).unwrap();
    }
    #[test]
    fn rollback_removes_nested_directories_in_reverse_order() {
        let root = std::env::temp_dir().join(format!(
            "lcr-broker-security-rollback-{}",
            std::process::id()
        ));

        let _ = fs::remove_dir_all(&root);

        let lcr = root.join("LCR");

        let input_broker = lcr.join("InputBroker");

        let version = input_broker.join("0.0.4-beta");

        fs::create_dir_all(&version).unwrap();

        let created = vec![lcr.clone(), input_broker.clone(), version.clone()];

        rollback_created_directories(&created).unwrap();

        assert!(!lcr.exists());
        assert!(!input_broker.exists());
        assert!(!version.exists());

        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn rollback_failure_is_returned_to_caller() {
        let root = std::env::temp_dir().join(format!(
            "lcr-broker-security-rollback-fail-{}",
            std::process::id()
        ));

        let _ = fs::remove_dir_all(&root);

        fs::create_dir_all(&root).unwrap();

        let blocker = root.join("blocker.txt");

        fs::write(&blocker, b"keep directory non-empty").unwrap();

        let error = rollback_created_directories(std::slice::from_ref(&root))
            .expect_err("non-empty directory rollback must be surfaced");

        assert!(error
            .to_string()
            .contains("failed to rollback protected Broker directory"));

        assert!(root.exists());

        fs::remove_file(blocker).unwrap();

        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn update_namespace_rejects_unprotected_existing_parents_without_mutation() {
        let root = std::env::temp_dir().join(format!(
            "lcr-broker-security-update-parent-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);

        let target = protected_broker_path_for_test(&root, "0.0.5-beta").unwrap();
        let [lcr, input_broker, version] = protected_directory_chain(&target).unwrap();
        fs::create_dir_all(&input_broker).unwrap();

        assert!(establish_protected_broker_update_directory(USER_SID, &target).is_err());
        assert!(lcr.is_dir());
        assert!(input_broker.is_dir());
        assert!(!version.exists());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn candidate_protection_rejects_non_private_or_cross_directory_paths() {
        let root = std::env::temp_dir().join(format!(
            "lcr-broker-security-candidate-shape-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);

        let target = protected_broker_path_for_test(&root, "0.0.5-beta").unwrap();
        let version = target.as_path().parent().unwrap();
        let elsewhere = root.join("elsewhere");
        fs::create_dir_all(version).unwrap();
        fs::create_dir_all(&elsewhere).unwrap();

        let target_error = protect_and_verify_broker_candidate(USER_SID, &target, target.as_path())
            .expect_err("committed target must never be accepted as a candidate");
        assert_eq!(target_error.kind(), io::ErrorKind::InvalidInput);

        let cross_directory = elsewhere.join(".LCRInputBroker.exe.1.2.install.tmp");
        fs::write(&cross_directory, b"candidate").unwrap();
        let cross_error = protect_and_verify_broker_candidate(USER_SID, &target, &cross_directory)
            .expect_err("cross-directory candidate must fail closed");
        assert_eq!(cross_error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(fs::read(&cross_directory).unwrap(), b"candidate");

        fs::remove_dir_all(root).unwrap();
    }
}
