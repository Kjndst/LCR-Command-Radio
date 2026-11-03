use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static INSTALL_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Build/package version authority.
///
/// This does not choose the next public release version. During development it
/// simply exposes the Cargo package version to distribution code so installer,
/// updater, uninstall registration, and future UI copy do not invent separate
/// version strings.
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Startup update checks are attempted at most once per 24 hours.
///
/// The timestamp is the time of the attempt, including failed attempts, so
/// restarting Helper cannot create GitHub request spam.
pub const STARTUP_UPDATE_INTERVAL_SECS: u64 = 24 * 60 * 60;

pub fn install_dir(local_app_data: &Path) -> PathBuf {
    local_app_data.join("Programs").join("LCR")
}

pub fn install_path(local_app_data: &Path) -> PathBuf {
    install_dir(local_app_data).join("LCR.exe")
}

pub fn update_state_path(app_data: &Path) -> PathBuf {
    app_data.join("LCR").join("update-state.json")
}

pub fn updates_dir(app_data: &Path) -> PathBuf {
    app_data.join("LCR").join("updates")
}

pub fn pending_update_path(app_data: &Path) -> PathBuf {
    updates_dir(app_data).join("LCR.pending.exe")
}

/// Relative HKCU uninstall registration path.
///
/// Keeping this relative to HKEY_CURRENT_USER is deliberate: Helper is a
/// per-user application and must not require elevation to install or remove.
pub const UNINSTALL_REGISTRY_SUBKEY: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Uninstall\LCR";

pub fn start_menu_programs_dir(app_data: &Path) -> PathBuf {
    app_data
        .join("Microsoft")
        .join("Windows")
        .join("Start Menu")
        .join("Programs")
}

pub fn start_menu_shortcut_path(app_data: &Path) -> PathBuf {
    start_menu_programs_dir(app_data).join("LCR.lnk")
}

pub fn uninstall_command(local_app_data: &Path) -> String {
    format!("\"{}\" --uninstall", install_path(local_app_data).display())
}

pub fn uninstall_cleanup_root(temp_dir: &Path) -> PathBuf {
    temp_dir.join("LCR-Uninstall")
}

pub fn uninstall_cleanup_stage_dir(
    temp_dir: &Path,
    parent_process_id: u32,
    nonce: u64,
) -> Option<PathBuf> {
    if parent_process_id == 0 || nonce == 0 {
        return None;
    }

    Some(uninstall_cleanup_root(temp_dir).join(format!("{parent_process_id}-{nonce:016x}")))
}

pub fn uninstall_cleanup_executable_path(
    temp_dir: &Path,
    parent_process_id: u32,
    nonce: u64,
) -> Option<PathBuf> {
    uninstall_cleanup_stage_dir(temp_dir, parent_process_id, nonce)
        .map(|directory| directory.join("LCR-Cleanup.exe"))
}

/// Bootstrap serialization and normal runtime ownership are intentionally
/// separate. A bootstrap process may launch the installed process before the
/// bootstrap itself has exited, so sharing one mutex would make that child
/// falsely identify itself as a duplicate.
pub const INSTALL_MUTEX_NAME: &str = r"Local\LinhLanBang.LCR.Helper.Install.v1";
pub const RUNTIME_MUTEX_NAME: &str = r"Local\LinhLanBang.LCR.Helper.Runtime.v1";

/// Serializes protected Broker lifecycle classification and signed maintenance
/// without colliding with Helper install/update or ordinary runtime ownership.
///
/// The guard is held only while startup reconciles persistent Broker authority.
/// It is deliberately dropped before updater U2 and before normal runtime.
pub const BROKER_LIFECYCLE_MUTEX_NAME: &str = r"Local\LinhLanBang.LCR.Helper.BrokerLifecycle.v1";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StartupRole {
    Bootstrap { target: PathBuf },
    Installed,
}

pub fn startup_role(current_executable: &Path, local_app_data: &Path) -> StartupRole {
    let target = install_path(local_app_data);

    if same_existing_file(current_executable, &target) {
        StartupRole::Installed
    } else {
        StartupRole::Bootstrap { target }
    }
}

/// Complete same-directory executable candidate prepared for one later atomic
/// commit.
///
/// Merely preparing an install never mutates the target. Dropping an uncommitted
/// value removes only its private temporary candidate.
pub struct PreparedExecutableInstall {
    temporary: PathBuf,
    target: PathBuf,
    committed: bool,
}

impl PreparedExecutableInstall {
    pub fn path(&self) -> &Path {
        &self.temporary
    }

    /// Atomically replaces the target with the exact already-prepared bytes.
    ///
    /// This is the executable-update commit barrier. No second source copy is
    /// performed here, so callers can verify `path()` immediately beforehand.
    pub fn commit(mut self) -> io::Result<()> {
        replace_same_directory_file_atomically(&self.temporary, &self.target)?;
        self.committed = true;
        Ok(())
    }

    /// Atomically installs the prepared executable only while the target is
    /// still absent.
    ///
    /// This is the first-install commit barrier. Unlike `commit`, it must
    /// never replace a target that appears after preparation.
    #[cfg(windows)]
    pub fn commit_new(mut self) -> io::Result<()> {
        move_same_directory_file_new_atomically(&self.temporary, &self.target)?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for PreparedExecutableInstall {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_file(&self.temporary);
        }
    }
}

/// Maximum write accepted by one streaming executable-candidate operation.
///
/// This matches the Broker maintenance transport quantum so the privileged
/// receiver never needs to accumulate payload bytes between pipe and disk.
#[cfg(windows)]
pub(crate) const STREAMING_EXECUTABLE_MAX_CHUNK_BYTES: usize = 4096;

/// Create-new same-directory executable candidate written in bounded chunks.
///
/// The target parent must already exist. Until `finish` succeeds, dropping this
/// value closes the output and removes only its private temporary candidate.
#[cfg(windows)]
pub(crate) struct StreamingExecutableInstall {
    output: Option<File>,
    prepared: Option<PreparedExecutableInstall>,
}

#[cfg(windows)]
impl StreamingExecutableInstall {
    pub(crate) fn write_chunk(&mut self, chunk: &[u8]) -> io::Result<()> {
        if chunk.is_empty() || chunk.len() > STREAMING_EXECUTABLE_MAX_CHUNK_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "streaming executable chunk is empty or exceeds the fixed bound",
            ));
        }

        self.output
            .as_mut()
            .ok_or_else(|| io::Error::other("streaming executable candidate is already finished"))?
            .write_all(chunk)
    }

    /// Flushes, durably synchronizes, and closes the candidate before exposing
    /// the existing prepared-install commit authority.
    pub(crate) fn finish(mut self) -> io::Result<PreparedExecutableInstall> {
        let mut output = self.output.take().ok_or_else(|| {
            io::Error::other("streaming executable candidate is already finished")
        })?;

        output.flush()?;
        output.sync_all()?;
        drop(output);

        self.prepared
            .take()
            .ok_or_else(|| io::Error::other("streaming executable candidate is unavailable"))
    }
}

#[cfg(windows)]
impl Drop for StreamingExecutableInstall {
    fn drop(&mut self) {
        // Windows cannot remove an open file. Close first; the uncommitted
        // PreparedExecutableInstall then removes only this private temporary.
        drop(self.output.take());
    }
}

/// Starts a bounded streaming executable candidate beside an exact target.
///
/// Unlike the existing first-install and update helpers, this primitive never
/// creates parent directories. Protected namespace authority must already have
/// established the exact target directory.
#[cfg(windows)]
pub(crate) fn prepare_streaming_executable_install(
    target: &Path,
) -> io::Result<StreamingExecutableInstall> {
    let directory = target
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing install directory"))?;

    let metadata = fs::metadata(directory)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "streaming executable target parent is not a directory",
        ));
    }

    let (temporary, output) = create_install_temporary(directory, target)?;
    let prepared = PreparedExecutableInstall {
        temporary,
        target: target.to_path_buf(),
        committed: false,
    };

    Ok(StreamingExecutableInstall {
        output: Some(output),
        prepared: Some(prepared),
    })
}

/// Copies an executable into a complete, flushed, create-new temporary file in
/// the target directory without modifying the target itself.
pub fn prepare_executable_install(
    source: &Path,
    target: &Path,
) -> io::Result<PreparedExecutableInstall> {
    if same_existing_file(source, target) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cannot prepare executable install from the target itself",
        ));
    }

    let directory = target
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing install directory"))?;

    fs::create_dir_all(directory)?;

    let (temporary, output) = create_install_temporary(directory, target)?;

    let result = (|| -> io::Result<()> {
        let mut input = File::open(source)?;
        let mut output = output;

        io::copy(&mut input, &mut output)?;
        output.sync_all()?;
        drop(output);

        Ok(())
    })();

    if let Err(error) = result {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }

    Ok(PreparedExecutableInstall {
        temporary,
        target: target.to_path_buf(),
        committed: false,
    })
}

/// Prepares exact executable bytes in a complete, flushed, create-new
/// temporary file beside the final target.
///
/// This is the in-memory sibling of `prepare_executable_install`. It exists so
/// an embedded PE resource can be verified before one atomic commit without
/// first publishing an attacker-writable sidecar executable.
pub fn prepare_executable_bytes_install(
    bytes: &[u8],
    target: &Path,
) -> io::Result<PreparedExecutableInstall> {
    if bytes.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "executable payload cannot be empty",
        ));
    }

    let directory = target
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing install directory"))?;

    fs::create_dir_all(directory)?;

    let (temporary, mut output) = create_install_temporary(directory, target)?;

    let result = (|| -> io::Result<()> {
        output.write_all(bytes)?;
        output.sync_all()?;
        drop(output);
        Ok(())
    })();

    if let Err(error) = result {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }

    Ok(PreparedExecutableInstall {
        temporary,
        target: target.to_path_buf(),
        committed: false,
    })
}
/// Atomically places a complete executable at the target path.
///
/// Historical callers retain exactly the previous one-call behavior while U2
/// can split copy/verification from the final atomic commit.
pub fn install_executable(source: &Path, target: &Path) -> io::Result<()> {
    if same_existing_file(source, target) {
        return Ok(());
    }

    prepare_executable_install(source, target)?.commit()
}

fn same_existing_file(source: &Path, target: &Path) -> bool {
    match (fs::canonicalize(source), fs::canonicalize(target)) {
        (Ok(source), Ok(target)) => source == target,
        _ => false,
    }
}

fn create_install_temporary(directory: &Path, target: &Path) -> io::Result<(PathBuf, File)> {
    let name = target
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing executable filename"))?
        .to_string_lossy();

    for _ in 0..32 {
        let sequence = INSTALL_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary = directory.join(format!(
            ".{name}.{}.{}.install.tmp",
            std::process::id(),
            sequence
        ));

        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => return Ok((temporary, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "unable to allocate executable install temporary file",
    ))
}

pub(crate) fn replace_same_directory_file_atomically(
    source: &Path,
    target: &Path,
) -> io::Result<()> {
    let source_parent = source.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "atomic replacement source has no parent directory",
        )
    })?;

    let target_parent = target.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "atomic replacement target has no parent directory",
        )
    })?;

    if source_parent != target_parent {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "atomic replacement requires source and target in the same directory",
        ));
    }

    replace_file(source, target)?;
    sync_replace_parent(target_parent)
}

#[cfg(windows)]
fn move_same_directory_file_new_atomically(source: &Path, target: &Path) -> io::Result<()> {
    let source_parent = source.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "atomic no-replace source has no parent directory",
        )
    })?;

    let target_parent = target.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "atomic no-replace target has no parent directory",
        )
    })?;

    if source_parent != target_parent {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "atomic no-replace requires source and target in the same directory",
        ));
    }

    move_file_new(source, target)?;
    sync_replace_parent(target_parent)
}

#[cfg(windows)]
fn move_file_new(source: &Path, target: &Path) -> io::Result<()> {
    use std::{iter::once, os::windows::ffi::OsStrExt};

    const MOVEFILE_WRITE_THROUGH: u32 = 0x0000_0008;

    #[link(name = "kernel32")]
    extern "system" {
        fn MoveFileExW(
            existing_file_name: *const u16,
            new_file_name: *const u16,
            flags: u32,
        ) -> i32;
    }

    let source = source
        .as_os_str()
        .encode_wide()
        .chain(once(0))
        .collect::<Vec<_>>();

    let target = target
        .as_os_str()
        .encode_wide()
        .chain(once(0))
        .collect::<Vec<_>>();

    let result = unsafe { MoveFileExW(source.as_ptr(), target.as_ptr(), MOVEFILE_WRITE_THROUGH) };

    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
#[cfg(windows)]
fn replace_file(source: &Path, target: &Path) -> io::Result<()> {
    use std::{iter::once, os::windows::ffi::OsStrExt};

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x0000_0001;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x0000_0008;

    #[link(name = "kernel32")]
    extern "system" {
        fn MoveFileExW(
            existing_file_name: *const u16,
            new_file_name: *const u16,
            flags: u32,
        ) -> i32;
    }

    let source = source
        .as_os_str()
        .encode_wide()
        .chain(once(0))
        .collect::<Vec<_>>();

    let target = target
        .as_os_str()
        .encode_wide()
        .chain(once(0))
        .collect::<Vec<_>>();

    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            target.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };

    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file(source: &Path, target: &Path) -> io::Result<()> {
    fs::rename(source, target)
}

#[cfg(unix)]
fn sync_replace_parent(directory: &Path) -> io::Result<()> {
    File::open(directory)?.sync_all()
}

#[cfg(not(unix))]
fn sync_replace_parent(_directory: &Path) -> io::Result<()> {
    // Windows replacement uses MOVEFILE_WRITE_THROUGH.
    Ok(())
}

/// Reads the exact Unicode ProductVersion string embedded in an LCR Windows
/// executable.
///
/// U2 uses this only as byte-bound self-version identity. It is not a signature
/// or authenticity primitive; download authenticity remains the U1 trust model.
#[cfg(windows)]
pub fn executable_product_version(path: &Path) -> io::Result<String> {
    use std::{iter::once, mem::size_of, os::windows::ffi::OsStrExt, ptr::null_mut};

    #[link(name = "version")]
    extern "system" {
        fn GetFileVersionInfoSizeW(filename: *const u16, handle: *mut u32) -> u32;
        fn GetFileVersionInfoW(
            filename: *const u16,
            handle: u32,
            len: u32,
            data: *mut core::ffi::c_void,
        ) -> i32;
        fn VerQueryValueW(
            block: *const core::ffi::c_void,
            sub_block: *const u16,
            buffer: *mut *mut core::ffi::c_void,
            len: *mut u32,
        ) -> i32;
    }

    let filename = path
        .as_os_str()
        .encode_wide()
        .chain(once(0))
        .collect::<Vec<_>>();

    let mut ignored_handle = 0u32;
    let byte_len = unsafe { GetFileVersionInfoSizeW(filename.as_ptr(), &mut ignored_handle) };
    if byte_len == 0 {
        return Err(io::Error::last_os_error());
    }

    let word_count = (byte_len as usize)
        .checked_add(size_of::<u32>() - 1)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "VERSIONINFO size overflow"))?
        / size_of::<u32>();
    let mut version_info = vec![0u32; word_count];

    let loaded = unsafe {
        GetFileVersionInfoW(
            filename.as_ptr(),
            0,
            byte_len,
            version_info.as_mut_ptr().cast(),
        )
    };
    if loaded == 0 {
        return Err(io::Error::last_os_error());
    }

    let query = r"\StringFileInfo\040904B0\ProductVersion"
        .encode_utf16()
        .chain(once(0))
        .collect::<Vec<_>>();

    let mut value = null_mut::<core::ffi::c_void>();
    let mut value_len = 0u32;
    let queried = unsafe {
        VerQueryValueW(
            version_info.as_ptr().cast(),
            query.as_ptr(),
            &mut value,
            &mut value_len,
        )
    };
    if queried == 0 || value.is_null() || value_len == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "missing LCR ProductVersion resource",
        ));
    }

    let mut units = unsafe { std::slice::from_raw_parts(value.cast::<u16>(), value_len as usize) };

    while units.last() == Some(&0) {
        units = &units[..units.len() - 1];
    }

    if units.is_empty() || units.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid LCR ProductVersion resource",
        ));
    }

    String::from_utf16(units).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "LCR ProductVersion is not valid UTF-16",
        )
    })
}
/// Returns true only when a startup update attempt is due.
///
/// A future stored timestamp is treated as not due. That intentionally avoids
/// restart/network spam if the wall clock moves backwards.
pub fn startup_update_due(last_attempt_unix_secs: Option<u64>, now_unix_secs: u64) -> bool {
    match last_attempt_unix_secs {
        None => true,
        Some(last) => now_unix_secs.saturating_sub(last) >= STARTUP_UPDATE_INTERVAL_SECS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn executable_test_dir(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("lcr-helper-install-{name}-{}", std::process::id()));

        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn executable_install_creates_parent_and_preserves_exact_bytes() {
        let directory = executable_test_dir("fresh");
        let source = directory.join("downloaded.exe");
        let target = directory
            .join("LocalAppData")
            .join("Programs")
            .join("LCR")
            .join("LCR.exe");

        fs::write(&source, b"candidate-executable").unwrap();

        install_executable(&source, &target).unwrap();

        assert_eq!(fs::read(&target).unwrap(), b"candidate-executable");
        assert_eq!(fs::read(&source).unwrap(), b"candidate-executable");

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn executable_install_atomically_replaces_existing_target() {
        let directory = executable_test_dir("replace");
        let source = directory.join("downloaded.exe");
        let target_dir = directory.join("Programs").join("LCR");
        let target = target_dir.join("LCR.exe");

        fs::create_dir_all(&target_dir).unwrap();
        fs::write(&source, b"new-executable").unwrap();
        fs::write(&target, b"old-executable").unwrap();

        install_executable(&source, &target).unwrap();

        assert_eq!(fs::read(&target).unwrap(), b"new-executable");

        let temporary_count = fs::read_dir(&target_dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                name.starts_with(".LCR.exe.") && name.ends_with(".install.tmp")
            })
            .count();

        assert_eq!(temporary_count, 0);

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn prepared_executable_install_does_not_mutate_target_before_commit() {
        let directory = executable_test_dir("prepared-no-commit");
        let source = directory.join("candidate.exe");
        let target_dir = directory.join("Programs").join("LCR");
        let target = target_dir.join("LCR.exe");

        fs::create_dir_all(&target_dir).unwrap();
        fs::write(&source, b"new-candidate").unwrap();
        fs::write(&target, b"trusted-old").unwrap();

        let prepared = prepare_executable_install(&source, &target).unwrap();

        assert_eq!(fs::read(prepared.path()).unwrap(), b"new-candidate");
        assert_eq!(fs::read(&target).unwrap(), b"trusted-old");

        drop(prepared);

        assert_eq!(fs::read(&target).unwrap(), b"trusted-old");

        let temporary_count = fs::read_dir(&target_dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                name.starts_with(".LCR.exe.") && name.ends_with(".install.tmp")
            })
            .count();
        assert_eq!(temporary_count, 0);

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn prepared_executable_commit_replaces_exact_target() {
        let directory = executable_test_dir("prepared-commit");
        let source = directory.join("candidate.exe");
        let target_dir = directory.join("Programs").join("LCR");
        let target = target_dir.join("LCR.exe");

        fs::create_dir_all(&target_dir).unwrap();
        fs::write(&source, b"new-candidate").unwrap();
        fs::write(&target, b"trusted-old").unwrap();

        let prepared = prepare_executable_install(&source, &target).unwrap();
        prepared.commit().unwrap();

        assert_eq!(fs::read(&target).unwrap(), b"new-candidate");

        fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn failed_executable_copy_preserves_existing_target_and_cleans_temporary() {
        let directory = executable_test_dir("failure");
        let missing_source = directory.join("missing.exe");
        let target_dir = directory.join("Programs").join("LCR");
        let target = target_dir.join("LCR.exe");

        fs::create_dir_all(&target_dir).unwrap();
        fs::write(&target, b"trusted-existing-executable").unwrap();

        assert!(install_executable(&missing_source, &target).is_err());

        assert_eq!(fs::read(&target).unwrap(), b"trusted-existing-executable");

        let temporary_count = fs::read_dir(&target_dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                name.starts_with(".LCR.exe.") && name.ends_with(".install.tmp")
            })
            .count();

        assert_eq!(temporary_count, 0);

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn installing_existing_file_onto_itself_is_a_noop() {
        let directory = executable_test_dir("same");
        let executable = directory.join("LCR.exe");

        fs::write(&executable, b"same-executable").unwrap();

        install_executable(&executable, &executable).unwrap();

        assert_eq!(fs::read(&executable).unwrap(), b"same-executable");

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn atomic_replace_replaces_existing_file_in_same_directory() {
        let directory = executable_test_dir("artifact-replace");
        let source = directory.join(".LCR.lnk.test.tmp");
        let target = directory.join("LCR.lnk");

        fs::write(&source, b"new-shortcut-artifact").unwrap();
        fs::write(&target, b"old-shortcut-artifact").unwrap();

        replace_same_directory_file_atomically(&source, &target).unwrap();

        assert_eq!(fs::read(&target).unwrap(), b"new-shortcut-artifact");
        assert!(!source.exists());

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn atomic_replace_rejects_cross_directory_move_without_touching_files() {
        let directory = executable_test_dir("artifact-cross-directory");
        let source_dir = directory.join("source");
        let target_dir = directory.join("target");
        let source = source_dir.join(".LCR.lnk.test.tmp");
        let target = target_dir.join("LCR.lnk");

        fs::create_dir_all(&source_dir).unwrap();
        fs::create_dir_all(&target_dir).unwrap();
        fs::write(&source, b"candidate-shortcut").unwrap();
        fs::write(&target, b"trusted-shortcut").unwrap();

        let error = replace_same_directory_file_atomically(&source, &target).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(fs::read(&source).unwrap(), b"candidate-shortcut");
        assert_eq!(fs::read(&target).unwrap(), b"trusted-shortcut");

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn failed_atomic_replace_preserves_existing_target() {
        let directory = executable_test_dir("artifact-failure");
        let source = directory.join(".missing.LCR.lnk.tmp");
        let target = directory.join("LCR.lnk");

        fs::write(&target, b"trusted-shortcut").unwrap();

        assert!(replace_same_directory_file_atomically(&source, &target).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"trusted-shortcut");

        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn prepared_commit_new_installs_when_target_remains_absent() {
        let directory = executable_test_dir("prepared-commit-new");

        let target_dir = directory.join("Programs").join("LCR");

        let target = target_dir.join("LCR.exe");

        fs::create_dir_all(&target_dir).unwrap();

        let prepared =
            prepare_executable_bytes_install(b"first-install-candidate", &target).unwrap();

        assert!(!target.exists());

        prepared.commit_new().unwrap();

        assert_eq!(fs::read(&target).unwrap(), b"first-install-candidate");

        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn prepared_commit_new_never_overwrites_target_created_after_prepare() {
        let directory = executable_test_dir("prepared-commit-new-race");

        let target_dir = directory.join("Programs").join("LCR");

        let target = target_dir.join("LCR.exe");

        fs::create_dir_all(&target_dir).unwrap();

        let prepared =
            prepare_executable_bytes_install(b"first-install-candidate", &target).unwrap();

        let temporary = prepared.path().to_path_buf();

        assert!(!target.exists());

        // Simulates another creator winning the race after preparation but
        // before the first-install commit barrier.
        fs::write(&target, b"race-winner-sentinel").unwrap();

        assert!(prepared.commit_new().is_err());

        assert_eq!(fs::read(&target).unwrap(), b"race-winner-sentinel");

        // Failed commit_new leaves committed=false, so Drop must clean only
        // the private prepared candidate.
        assert!(!temporary.exists());

        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn streaming_executable_candidate_writes_chunks_and_commits_exact_bytes() {
        let directory = executable_test_dir("streaming-commit-new");
        let target_dir = directory.join("Programs").join("LCR");
        let target = target_dir.join("LCRInputBroker.exe");
        fs::create_dir_all(&target_dir).unwrap();

        let first = [0x31; STREAMING_EXECUTABLE_MAX_CHUNK_BYTES];
        let second = [0x52; STREAMING_EXECUTABLE_MAX_CHUNK_BYTES];
        let final_chunk = [0x73; 947];
        let mut expected = Vec::new();
        expected.extend_from_slice(&first);
        expected.extend_from_slice(&second);
        expected.extend_from_slice(&final_chunk);

        let mut streaming = prepare_streaming_executable_install(&target).unwrap();
        streaming.write_chunk(&first).unwrap();
        streaming.write_chunk(&second).unwrap();
        streaming.write_chunk(&final_chunk).unwrap();

        assert!(!target.exists());

        let prepared = streaming.finish().unwrap();
        assert_eq!(fs::read(prepared.path()).unwrap(), expected);
        assert!(!target.exists());

        prepared.commit_new().unwrap();
        assert_eq!(fs::read(&target).unwrap(), expected);

        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn unfinished_streaming_candidate_cleans_only_its_private_temporary() {
        let directory = executable_test_dir("streaming-drop");
        let target_dir = directory.join("Programs").join("LCR");
        let target = target_dir.join("LCRInputBroker.exe");
        let unrelated = target_dir.join("keep.txt");
        fs::create_dir_all(&target_dir).unwrap();
        fs::write(&unrelated, b"keep").unwrap();

        let mut streaming = prepare_streaming_executable_install(&target).unwrap();
        streaming.write_chunk(b"partial-candidate").unwrap();
        let temporary = streaming.prepared.as_ref().unwrap().path().to_path_buf();
        assert!(temporary.exists());

        drop(streaming);

        assert!(!target.exists());
        assert!(!temporary.exists());
        assert_eq!(fs::read(&unrelated).unwrap(), b"keep");

        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn streaming_candidate_requires_existing_parent_and_bounded_chunks() {
        let directory = executable_test_dir("streaming-bounds");
        let missing_target = directory.join("missing").join("LCRInputBroker.exe");
        assert!(prepare_streaming_executable_install(&missing_target).is_err());
        assert!(!missing_target.parent().unwrap().exists());

        let target = directory.join("LCRInputBroker.exe");
        let mut streaming = prepare_streaming_executable_install(&target).unwrap();
        assert!(streaming.write_chunk(&[]).is_err());
        let oversized = [0; STREAMING_EXECUTABLE_MAX_CHUNK_BYTES + 1];
        assert!(streaming.write_chunk(&oversized).is_err());

        drop(streaming);
        assert!(!target.exists());
        fs::remove_dir_all(directory).unwrap();
    }
    #[cfg(windows)]
    #[test]
    fn product_version_reader_fails_closed_for_non_pe_file() {
        let directory = executable_test_dir("u2a-non-pe-version");
        let fake = directory.join("not-an-executable.bin");
        fs::write(&fake, b"not-a-pe-image").unwrap();

        assert!(executable_product_version(&fake).is_err());

        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires the separately built product LCR.exe"]
    fn u2a_reads_built_helper_product_version() {
        let executable = std::env::var_os("LCR_U2A_BUILT_EXE")
            .expect("LCR_U2A_BUILT_EXE must point to the built product binary");

        assert_eq!(
            executable_product_version(Path::new(&executable)).unwrap(),
            CURRENT_VERSION
        );
    }
    #[test]
    fn uninstall_cleanup_staging_authority_is_deterministic() {
        let temp = Path::new("Temp");
        let parent_process_id = 4242;
        let nonce = 0x2a;

        assert_eq!(uninstall_cleanup_root(temp), temp.join("LCR-Uninstall"));
        assert_eq!(
            uninstall_cleanup_stage_dir(temp, parent_process_id, nonce),
            Some(temp.join("LCR-Uninstall").join("4242-000000000000002a"))
        );
        assert_eq!(
            uninstall_cleanup_executable_path(temp, parent_process_id, nonce),
            Some(
                temp.join("LCR-Uninstall")
                    .join("4242-000000000000002a")
                    .join("LCR-Cleanup.exe")
            )
        );
    }

    #[test]
    fn uninstall_cleanup_staging_rejects_zero_identity_components() {
        let temp = Path::new("Temp");

        assert_eq!(uninstall_cleanup_stage_dir(temp, 0, 1), None);
        assert_eq!(uninstall_cleanup_stage_dir(temp, 1, 0), None);
        assert_eq!(uninstall_cleanup_executable_path(temp, 0, 1), None);
        assert_eq!(uninstall_cleanup_executable_path(temp, 1, 0), None);
    }

    #[test]
    fn lifecycle_mutex_names_are_distinct_and_session_local() {
        assert_ne!(INSTALL_MUTEX_NAME, RUNTIME_MUTEX_NAME);
        assert_ne!(INSTALL_MUTEX_NAME, BROKER_LIFECYCLE_MUTEX_NAME);
        assert_ne!(RUNTIME_MUTEX_NAME, BROKER_LIFECYCLE_MUTEX_NAME);

        assert!(INSTALL_MUTEX_NAME.starts_with(r"Local\"));
        assert!(RUNTIME_MUTEX_NAME.starts_with(r"Local\"));
        assert!(BROKER_LIFECYCLE_MUTEX_NAME.starts_with(r"Local\"));
    }

    #[test]
    fn startup_role_distinguishes_downloaded_and_installed_copies() {
        let directory = executable_test_dir("startup-role");
        let local_app_data = directory.join("LocalAppData");
        let downloads = directory.join("Downloads");
        let downloaded = downloads.join("LCR.exe");
        let installed = install_path(&local_app_data);

        fs::create_dir_all(&downloads).unwrap();
        fs::write(&downloaded, b"downloaded-helper").unwrap();

        assert_eq!(
            startup_role(&downloaded, &local_app_data),
            StartupRole::Bootstrap {
                target: installed.clone()
            }
        );

        fs::create_dir_all(installed.parent().unwrap()).unwrap();
        fs::write(&installed, b"installed-helper").unwrap();

        assert_eq!(
            startup_role(&installed, &local_app_data),
            StartupRole::Installed
        );

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn canonical_install_location_is_local_app_data_programs_lcr() {
        let base = Path::new("LocalAppData");

        assert_eq!(
            install_path(base),
            base.join("Programs").join("LCR").join("LCR.exe")
        );
    }

    #[test]
    fn update_state_is_separate_from_pairing_config() {
        let base = Path::new("AppData");

        assert_eq!(
            update_state_path(base),
            base.join("LCR").join("update-state.json")
        );
    }

    #[test]
    fn first_startup_check_is_due() {
        assert!(startup_update_due(None, 1));
    }

    #[test]
    fn startup_check_is_not_due_before_24_hours() {
        assert!(!startup_update_due(
            Some(100),
            100 + STARTUP_UPDATE_INTERVAL_SECS - 1
        ));
    }

    #[test]
    fn startup_check_is_due_at_24_hours() {
        assert!(startup_update_due(
            Some(100),
            100 + STARTUP_UPDATE_INTERVAL_SECS
        ));
    }

    #[test]
    fn backwards_clock_does_not_create_restart_spam() {
        assert!(!startup_update_due(Some(10_000), 9_000));
    }

    #[test]
    fn cargo_package_version_is_the_distribution_version_authority() {
        assert_eq!(CURRENT_VERSION, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn start_menu_shortcut_is_per_user_and_not_desktop() {
        let app_data = Path::new("AppData");

        assert_eq!(
            start_menu_shortcut_path(app_data),
            app_data
                .join("Microsoft")
                .join("Windows")
                .join("Start Menu")
                .join("Programs")
                .join("LCR.lnk")
        );

        assert!(!start_menu_shortcut_path(app_data)
            .to_string_lossy()
            .to_ascii_lowercase()
            .contains("desktop"));
    }

    #[test]
    fn uninstall_registration_is_hkcu_relative() {
        assert!(UNINSTALL_REGISTRY_SUBKEY.starts_with("Software\\"));
        assert!(!UNINSTALL_REGISTRY_SUBKEY
            .to_ascii_uppercase()
            .contains("HKLM"));
        assert!(!UNINSTALL_REGISTRY_SUBKEY
            .to_ascii_uppercase()
            .contains("HKEY_LOCAL_MACHINE"));
    }

    #[test]
    fn uninstall_command_targets_canonical_installed_helper() {
        let command = uninstall_command(Path::new("Local App Data"));

        assert!(command.starts_with('"'));
        assert!(command.ends_with("\" --uninstall"));
        assert!(command.contains("Programs"));
        assert!(command.contains("LCR"));
        assert!(command.contains("LCR.exe"));
    }
}
