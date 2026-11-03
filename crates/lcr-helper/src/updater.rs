//! Helper updater U1 deterministic core.
//!
//! This module intentionally has no executable-apply authority. It owns only:
//! - development-sentinel gating;
//! - release/version/asset selection;
//! - 24-hour attempt-state persistence;
//! - metadata for an already-verified pending candidate.
//!
//! U1 owns a separate bounded public WinHTTP discovery/download path and stages
//! only a hash-verified pending candidate. Executable replacement remains U2.

use crate::distribution::{
    pending_update_path, replace_same_directory_file_atomically, startup_update_due,
    update_state_path, CURRENT_VERSION,
};
use semver::Version;
use serde_json::{json, Value};
use std::{
    cmp::Ordering,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering as AtomicOrdering},
};

#[cfg(windows)]
use crate::{
    distribution::{
        executable_product_version, install_path, prepare_executable_install, INSTALL_MUTEX_NAME,
        RUNTIME_MUTEX_NAME,
    },
    instance::{try_acquire_named_mutex, NamedMutexAcquire, NamedMutexGuard},
    registration,
    uninstall::{
        cleanup_staged_child_after_parent_exit, generate_cleanup_nonce,
        launch_update_child_with_guards, prove_update_parent_exit_handoff,
        remove_unlaunched_cleanup_stage_best_effort, resolve_update_known_folder_authority,
        stage_cleanup_child, validate_uninstall_parent_executable_at, CleanupParentHandleValue,
    },
};

#[cfg(windows)]
use std::{
    env,
    io::Read,
    process::Command,
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

pub const DEVELOPMENT_VERSION_SENTINEL: &str = "0.0.0";
pub const RELEASE_OWNER: &str = "Kjndst";
pub const RELEASE_REPOSITORY: &str = "LCR-Command-Radio";
pub const RELEASE_ASSET_NAME: &str = "LCR.exe";
pub const RELEASE_PAGE_LIMIT: usize = 100;
pub const MAX_HELPER_ASSET_BYTES: u64 = 32 * 1024 * 1024;

#[cfg(windows)]
pub(crate) const RELEASE_API_HOST: &str = "api.github.com";
#[cfg(windows)]
pub(crate) const RELEASE_API_PATH: &str = "/repos/Kjndst/LCR-Command-Radio/releases?per_page=100";
#[cfg(windows)]
pub(crate) const RELEASE_DOWNLOAD_HOST: &str = "github.com";
#[cfg(windows)]
pub(crate) const MAX_RELEASE_API_BYTES: u64 = 2 * 1024 * 1024;

static UPDATE_STATE_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
#[cfg(windows)]
static PENDING_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleaseCandidate {
    pub version: String,
    pub sha256: String,
    pub size: u64,
    pub download_url: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedPending {
    pub version: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UpdateState {
    pub last_attempt_unix_secs: Option<u64>,
    pub pending: Option<VerifiedPending>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReleaseDiscoveryError {
    InvalidCurrentVersion,
    InvalidResponse,
    AmbiguousPage,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiscoveryAttempt {
    Disabled,
    NotDue,
    StatePersistFailed,
    FetchFailed,
    InvalidResponse,
    NoUpdate,
    Candidate(ReleaseCandidate),
}

// Non-Windows deterministic discovery needs only the Ready state; the
// durable payload is consumed by the Windows updater worker.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug)]
struct PreparedAttempt {
    state_path: PathBuf,
    state: UpdateState,
}

#[derive(Debug)]
enum AttemptPreparation {
    Disabled,
    NotDue,
    StatePersistFailed,
    Ready(PreparedAttempt),
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UpdateRunOutcome {
    FetchFailed,
    InvalidResponse,
    NoUpdate,
    ExistingPending,
    DownloadFailed,
    PendingStatePersistFailed,
    PendingPublished,
}

#[cfg(windows)]
pub enum UpdateApplyParentDisposition {
    NotApplicable,
    Exit,
    ContinueRuntime(NamedMutexGuard),
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpdateApplyChildOutcome {
    Committed,
    RecoveredOldRuntime,
    Incomplete,
}
#[cfg(windows)]
pub(crate) trait UpdateNetwork {
    fn fetch_releases(&mut self) -> io::Result<Vec<u8>>;

    fn download_candidate(
        &mut self,
        candidate: &ReleaseCandidate,
        sink: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    ) -> io::Result<()>;
}

/// Development builds are not members of the public updater channel.
///
/// `0.0.0` is a sentinel rather than "the oldest release". This prevents a
/// locally newer development binary from replacing itself with an older public
/// beta merely because `0.0.0 < 0.0.2-beta` in normal SemVer ordering.
pub fn updater_enabled(version: &str) -> bool {
    version != DEVELOPMENT_VERSION_SENTINEL && Version::parse(version).is_ok()
}

pub fn current_build_updater_enabled() -> bool {
    updater_enabled(CURRENT_VERSION)
}

/// Select the highest valid public Helper release for `current_version`.
///
/// This parser is deliberately strict and order-independent:
/// - body must be one JSON array with fewer than 100 entries;
/// - draft releases are ignored;
/// - tags are exact `v<semver>`;
/// - stable clients ignore prereleases;
/// - comparison uses SemVer precedence, disregarding build metadata;
/// - exactly one `LCR.exe` asset is required;
/// - size and GitHub-provided sha256 digest are mandatory;
/// - browser_download_url must bind to the fixed public repository/tag/asset.
pub fn select_release(
    current_version: &str,
    body: &[u8],
) -> Result<Option<ReleaseCandidate>, ReleaseDiscoveryError> {
    if current_version == DEVELOPMENT_VERSION_SENTINEL {
        return Ok(None);
    }

    let current = Version::parse(current_version)
        .map_err(|_| ReleaseDiscoveryError::InvalidCurrentVersion)?;

    let value = serde_json::from_slice::<Value>(body)
        .map_err(|_| ReleaseDiscoveryError::InvalidResponse)?;
    let releases = value
        .as_array()
        .ok_or(ReleaseDiscoveryError::InvalidResponse)?;

    if releases.len() >= RELEASE_PAGE_LIMIT {
        return Err(ReleaseDiscoveryError::AmbiguousPage);
    }

    let current_is_stable = current.pre.is_empty();
    let mut best: Option<(Version, ReleaseCandidate)> = None;

    for release in releases {
        if release.get("draft").and_then(Value::as_bool) != Some(false) {
            continue;
        }

        let Some(tag) = release.get("tag_name").and_then(Value::as_str) else {
            continue;
        };
        let Some(version_text) = tag.strip_prefix('v') else {
            continue;
        };
        if version_text.is_empty() {
            continue;
        }

        let Ok(version) = Version::parse(version_text) else {
            continue;
        };

        let Some(release_is_prerelease) = release.get("prerelease").and_then(Value::as_bool) else {
            continue;
        };
        let version_is_prerelease = !version.pre.is_empty();

        if release_is_prerelease != version_is_prerelease {
            continue;
        }

        if current_is_stable && version_is_prerelease {
            continue;
        }

        if version.cmp_precedence(&current) != Ordering::Greater {
            continue;
        }

        let Some(candidate) = candidate_from_release(release, tag, &version) else {
            continue;
        };

        let replace_best = match &best {
            None => true,
            Some((best_version, _)) => version.cmp_precedence(best_version) == Ordering::Greater,
        };

        if replace_best {
            best = Some((version, candidate));
        }
    }

    Ok(best.map(|(_, candidate)| candidate))
}

fn candidate_from_release(
    release: &Value,
    tag: &str,
    version: &Version,
) -> Option<ReleaseCandidate> {
    let assets = release.get("assets")?.as_array()?;
    let mut matching_assets = assets
        .iter()
        .filter(|asset| asset.get("name").and_then(Value::as_str) == Some(RELEASE_ASSET_NAME));

    let asset = matching_assets.next()?;
    if matching_assets.next().is_some() {
        return None;
    }

    let size = asset.get("size")?.as_u64()?;
    if size == 0 || size > MAX_HELPER_ASSET_BYTES {
        return None;
    }

    let digest = normalize_sha256(asset.get("digest")?.as_str()?)?;
    let download_url = asset.get("browser_download_url")?.as_str()?;
    let expected_url = expected_download_url(tag);

    if download_url != expected_url {
        return None;
    }

    Some(ReleaseCandidate {
        version: version.to_string(),
        sha256: digest,
        size,
        download_url: download_url.to_owned(),
    })
}

fn expected_download_url(tag: &str) -> String {
    format!(
        "https://github.com/{RELEASE_OWNER}/{RELEASE_REPOSITORY}/releases/download/{tag}/{RELEASE_ASSET_NAME}"
    )
}

/// Revalidates a selected candidate immediately before public download.
///
/// Production candidates come from `select_release`, but this second binding
/// keeps the WinHTTP seam incapable of accepting an arbitrary host/scheme/path.
#[cfg(any(windows, test))]
pub(crate) fn release_download_path(candidate: &ReleaseCandidate) -> Option<String> {
    if candidate.version == DEVELOPMENT_VERSION_SENTINEL
        || candidate.size == 0
        || candidate.size > MAX_HELPER_ASSET_BYTES
    {
        return None;
    }

    let version = Version::parse(&candidate.version).ok()?;
    if version.to_string() != candidate.version {
        return None;
    }

    let digest = normalize_sha256(&format!("sha256:{}", candidate.sha256))?;
    if digest != candidate.sha256 {
        return None;
    }

    let tag = format!("v{}", candidate.version);
    if candidate.download_url != expected_download_url(&tag) {
        return None;
    }

    Some(format!(
        "/{RELEASE_OWNER}/{RELEASE_REPOSITORY}/releases/download/{tag}/{RELEASE_ASSET_NAME}"
    ))
}

fn normalize_sha256(value: &str) -> Option<String> {
    let hex = value.strip_prefix("sha256:")?;

    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }

    Some(hex.to_ascii_lowercase())
}

fn load_update_state_unchecked(path: &Path) -> UpdateState {
    let Ok(bytes) = fs::read(path) else {
        return UpdateState::default();
    };
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        return UpdateState::default();
    };

    let last_attempt_unix_secs = value.get("last_attempt_unix_secs").and_then(Value::as_u64);

    let pending = value.get("pending").and_then(parse_pending);

    UpdateState {
        last_attempt_unix_secs,
        pending,
    }
}

/// Loads durable updater state while treating a non-newer pending candidate as
/// stale metadata.
///
/// Build metadata alone never makes a pending candidate newer because U1 uses
/// SemVer precedence rather than total textual ordering.
pub fn load_update_state(path: &Path, current_version: &str) -> UpdateState {
    let mut state = load_update_state_unchecked(path);

    let Ok(current) = Version::parse(current_version) else {
        state.pending = None;
        return state;
    };

    let pending_is_newer = state.pending.as_ref().is_some_and(|pending| {
        Version::parse(&pending.version)
            .map(|version| version.cmp_precedence(&current) == Ordering::Greater)
            .unwrap_or(false)
    });

    if !pending_is_newer {
        state.pending = None;
    }

    state
}

fn parse_pending(value: &Value) -> Option<VerifiedPending> {
    let version = value.get("version")?.as_str()?;
    let parsed_version = Version::parse(version).ok()?;

    if parsed_version.to_string() != version || version == DEVELOPMENT_VERSION_SENTINEL {
        return None;
    }

    let sha256 = normalize_sha256(&format!("sha256:{}", value.get("sha256")?.as_str()?))?;
    let size = value.get("size")?.as_u64()?;

    if size == 0 || size > MAX_HELPER_ASSET_BYTES {
        return None;
    }

    Some(VerifiedPending {
        version: version.to_owned(),
        sha256,
        size,
    })
}

pub fn save_update_state(path: &Path, state: &UpdateState) -> io::Result<()> {
    save_update_state_with(path, state, replace_same_directory_file_atomically)
}

fn save_update_state_with(
    path: &Path,
    state: &UpdateState,
    replace: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    let directory = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "update state path has no parent directory",
        )
    })?;

    fs::create_dir_all(directory)?;

    let (temporary, mut output) = create_state_temporary(directory, path)?;
    let bytes = serialize_state(state)?;

    let result = (|| -> io::Result<()> {
        output.write_all(&bytes)?;
        output.sync_all()?;
        drop(output);

        replace(&temporary, path)
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }

    result
}

fn serialize_state(state: &UpdateState) -> io::Result<Vec<u8>> {
    let pending = state.pending.as_ref().map(|pending| {
        json!({
            "version": pending.version,
            "sha256": pending.sha256,
            "size": pending.size
        })
    });

    serde_json::to_vec(&json!({
        "last_attempt_unix_secs": state.last_attempt_unix_secs,
        "pending": pending
    }))
    .map_err(io::Error::other)
}

fn create_state_temporary(directory: &Path, target: &Path) -> io::Result<(PathBuf, File)> {
    let name = target
        .file_name()
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "missing update-state filename")
        })?
        .to_string_lossy();

    for _ in 0..32 {
        let sequence = UPDATE_STATE_TEMP_SEQUENCE.fetch_add(1, AtomicOrdering::Relaxed);
        let temporary = directory.join(format!(".{name}.{}.{}.tmp", std::process::id(), sequence));

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
        "unable to allocate update-state temporary file",
    ))
}

fn prepare_attempt(
    app_data: &Path,
    current_version: &str,
    now_unix_secs: u64,
) -> AttemptPreparation {
    if !updater_enabled(current_version) {
        return AttemptPreparation::Disabled;
    }

    let state_path = update_state_path(app_data);
    let mut state = load_update_state(&state_path, current_version);

    if !startup_update_due(state.last_attempt_unix_secs, now_unix_secs) {
        return AttemptPreparation::NotDue;
    }

    state.last_attempt_unix_secs = Some(now_unix_secs);

    if save_update_state(&state_path, &state).is_err() {
        return AttemptPreparation::StatePersistFailed;
    }

    AttemptPreparation::Ready(PreparedAttempt { state_path, state })
}
/// Deterministic once-per-start discovery shell used by U1 tests and later by
/// the isolated WinHTTP startup worker.
///
/// The attempt timestamp is persisted before `fetch` is invoked. A failed
/// fetch therefore still consumes the 24-hour window, and a state-persistence
/// failure performs zero network work.
pub fn discover_once_with(
    app_data: &Path,
    current_version: &str,
    now_unix_secs: u64,
    fetch: impl FnOnce() -> Result<Vec<u8>, ()>,
) -> DiscoveryAttempt {
    let _prepared = match prepare_attempt(app_data, current_version, now_unix_secs) {
        AttemptPreparation::Disabled => return DiscoveryAttempt::Disabled,
        AttemptPreparation::NotDue => return DiscoveryAttempt::NotDue,
        AttemptPreparation::StatePersistFailed => return DiscoveryAttempt::StatePersistFailed,
        AttemptPreparation::Ready(prepared) => prepared,
    };

    let body = match fetch() {
        Ok(body) => body,
        Err(()) => return DiscoveryAttempt::FetchFailed,
    };

    match select_release(current_version, &body) {
        Ok(Some(candidate)) => DiscoveryAttempt::Candidate(candidate),
        Ok(None) => DiscoveryAttempt::NoUpdate,
        Err(_) => DiscoveryAttempt::InvalidResponse,
    }
}

#[cfg(windows)]
type BCryptHandle = *mut core::ffi::c_void;

#[cfg(windows)]
#[link(name = "bcrypt")]
extern "system" {
    fn BCryptCreateHash(
        algorithm: BCryptHandle,
        hash: *mut BCryptHandle,
        hash_object: *mut u8,
        hash_object_len: u32,
        secret: *const u8,
        secret_len: u32,
        flags: u32,
    ) -> i32;
    fn BCryptHashData(hash: BCryptHandle, input: *const u8, input_len: u32, flags: u32) -> i32;
    fn BCryptFinishHash(hash: BCryptHandle, output: *mut u8, output_len: u32, flags: u32) -> i32;
    fn BCryptDestroyHash(hash: BCryptHandle) -> i32;
}

#[cfg(windows)]
struct Sha256Hash {
    handle: BCryptHandle,
}

#[cfg(windows)]
impl Sha256Hash {
    fn new() -> io::Result<Self> {
        // Windows 10+ SHA-256 CNG pseudo-handle. The product support floor is
        // Windows 10, so no algorithm-provider allocation is needed.
        const BCRYPT_SHA256_ALG_HANDLE: usize = 0x0000_0041;

        let mut handle = core::ptr::null_mut();
        let status = unsafe {
            BCryptCreateHash(
                BCRYPT_SHA256_ALG_HANDLE as BCryptHandle,
                &mut handle,
                core::ptr::null_mut(),
                0,
                core::ptr::null(),
                0,
                0,
            )
        };

        ntstatus_result(status, "BCryptCreateHash")?;

        if handle.is_null() {
            return Err(io::Error::other(
                "BCryptCreateHash returned a null SHA-256 handle",
            ));
        }

        Ok(Self { handle })
    }

    fn update(&mut self, bytes: &[u8]) -> io::Result<()> {
        let len = u32::try_from(bytes.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "SHA-256 chunk too large"))?;

        ntstatus_result(
            unsafe { BCryptHashData(self.handle, bytes.as_ptr(), len, 0) },
            "BCryptHashData",
        )
    }

    fn finish(&mut self) -> io::Result<String> {
        let mut digest = [0u8; 32];

        ntstatus_result(
            unsafe { BCryptFinishHash(self.handle, digest.as_mut_ptr(), digest.len() as u32, 0) },
            "BCryptFinishHash",
        )?;

        let mut hex = String::with_capacity(64);
        for byte in digest {
            use std::fmt::Write as _;
            let _ = write!(&mut hex, "{byte:02x}");
        }

        Ok(hex)
    }
}

#[cfg(windows)]
impl Drop for Sha256Hash {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe {
                let _ = BCryptDestroyHash(self.handle);
            }
        }
    }
}

#[cfg(windows)]
fn ntstatus_result(status: i32, operation: &'static str) -> io::Result<()> {
    if status >= 0 {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "{operation} failed with NTSTATUS {:#010x}",
            status as u32
        )))
    }
}

#[cfg(windows)]
fn create_pending_temporary(directory: &Path, target: &Path) -> io::Result<(PathBuf, File)> {
    let name = target
        .file_name()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "pending update path has no filename",
            )
        })?
        .to_string_lossy();

    for _ in 0..32 {
        let sequence = PENDING_TEMP_SEQUENCE.fetch_add(1, AtomicOrdering::Relaxed);
        let temporary = directory.join(format!(
            ".{name}.{}.{}.partial",
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
        "unable to allocate pending update temporary file",
    ))
}

#[cfg(windows)]
fn publish_verified_candidate_with<N>(
    app_data: &Path,
    candidate: &ReleaseCandidate,
    network: &mut N,
    replace: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> io::Result<VerifiedPending>
where
    N: UpdateNetwork,
{
    release_download_path(candidate).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "release candidate download authority is invalid",
        )
    })?;

    let target = pending_update_path(app_data);
    let directory = target.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "pending update path has no parent directory",
        )
    })?;

    fs::create_dir_all(directory)?;
    let (temporary, file) = create_pending_temporary(directory, &target)?;
    let mut output = Some(file);

    let result = (|| -> io::Result<VerifiedPending> {
        let mut hasher = Sha256Hash::new()?;
        let mut total = 0u64;

        network.download_candidate(candidate, &mut |chunk| {
            let chunk_len = u64::try_from(chunk.len()).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "download chunk too large")
            })?;
            let new_total = total.checked_add(chunk_len).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "download size overflow")
            })?;

            if new_total > candidate.size || new_total > MAX_HELPER_ASSET_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "download exceeded declared updater asset size",
                ));
            }

            output
                .as_mut()
                .ok_or_else(|| io::Error::other("pending output already closed"))?
                .write_all(chunk)?;
            hasher.update(chunk)?;
            total = new_total;

            Ok(())
        })?;

        if total != candidate.size {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "downloaded updater asset size does not match release metadata",
            ));
        }

        output
            .as_mut()
            .ok_or_else(|| io::Error::other("pending output already closed"))?
            .sync_all()?;

        let actual_sha256 = hasher.finish()?;
        if actual_sha256 != candidate.sha256 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "downloaded updater SHA-256 does not match release metadata",
            ));
        }

        drop(output.take());
        replace(&temporary, &target)?;

        Ok(VerifiedPending {
            version: candidate.version.clone(),
            sha256: candidate.sha256.clone(),
            size: candidate.size,
        })
    })();

    drop(output.take());

    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }

    result
}

#[cfg(windows)]
pub(crate) fn sha256_file(path: &Path, expected_size: u64) -> io::Result<String> {
    let metadata = fs::metadata(path)?;
    if !metadata.is_file() || metadata.len() != expected_size {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "pending updater file size does not match durable metadata",
        ));
    }

    let mut input = File::open(path)?;
    let mut hasher = Sha256Hash::new()?;
    let mut total = 0u64;
    let mut buffer = [0u8; 8192];

    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }

        total = total
            .checked_add(read as u64)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "pending size overflow"))?;

        if total > expected_size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "pending updater file exceeded durable size",
            ));
        }

        hasher.update(&buffer[..read])?;
    }

    if total != expected_size {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "pending updater file ended before durable size",
        ));
    }

    hasher.finish()
}

#[cfg(windows)]
fn verified_pending_file_matches(app_data: &Path, pending: &VerifiedPending) -> bool {
    sha256_file(&pending_update_path(app_data), pending.size)
        .map(|sha256| sha256 == pending.sha256)
        .unwrap_or(false)
}

#[cfg(windows)]
fn create_pending_rollback_copy(
    app_data: &Path,
    pending: Option<&VerifiedPending>,
) -> io::Result<Option<PathBuf>> {
    let Some(pending) = pending else {
        return Ok(None);
    };

    if !verified_pending_file_matches(app_data, pending) {
        return Ok(None);
    }

    let source = pending_update_path(app_data);
    let directory = source.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "pending update path has no parent directory",
        )
    })?;

    for _ in 0..32 {
        let sequence = PENDING_TEMP_SEQUENCE.fetch_add(1, AtomicOrdering::Relaxed);
        let backup = directory.join(format!(
            ".LCR.pending.exe.{}.{}.rollback",
            std::process::id(),
            sequence
        ));

        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&backup)
        {
            Ok(mut output) => {
                let mut input = File::open(&source)?;

                let copied = io::copy(&mut input, &mut output)?;
                if copied != pending.size {
                    let _ = fs::remove_file(&backup);
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "pending rollback copy size changed during copy",
                    ));
                }

                output.sync_all()?;
                drop(output);

                let copied_sha256 = sha256_file(&backup, pending.size)?;
                if copied_sha256 != pending.sha256 {
                    let _ = fs::remove_file(&backup);
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "pending rollback copy hash changed during copy",
                    ));
                }

                return Ok(Some(backup));
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "unable to allocate pending rollback file",
    ))
}

#[cfg(windows)]
fn restore_pending_rollback(backup: &mut Option<PathBuf>, target: &Path) -> io::Result<()> {
    let Some(path) = backup.take() else {
        return Ok(());
    };

    match replace_same_directory_file_atomically(&path, target) {
        Ok(()) => Ok(()),
        Err(error) => {
            *backup = Some(path);
            Err(error)
        }
    }
}
#[cfg(windows)]
fn existing_pending_covers_candidate(
    app_data: &Path,
    state: &UpdateState,
    candidate: &ReleaseCandidate,
) -> bool {
    let Some(pending) = state.pending.as_ref() else {
        return false;
    };

    let Ok(pending_version) = Version::parse(&pending.version) else {
        return false;
    };
    let Ok(candidate_version) = Version::parse(&candidate.version) else {
        return false;
    };

    if pending_version.cmp_precedence(&candidate_version) == Ordering::Less {
        return false;
    }

    verified_pending_file_matches(app_data, pending)
}

#[cfg(windows)]
fn run_prepared_attempt_with<N, S>(
    app_data: &Path,
    current_version: &str,
    mut prepared: PreparedAttempt,
    network: &mut N,
    replace: impl FnOnce(&Path, &Path) -> io::Result<()>,
    mut save_state: S,
) -> UpdateRunOutcome
where
    N: UpdateNetwork,
    S: FnMut(&Path, &UpdateState) -> io::Result<()>,
{
    let body = match network.fetch_releases() {
        Ok(body) => body,
        Err(_) => return UpdateRunOutcome::FetchFailed,
    };

    let candidate = match select_release(current_version, &body) {
        Ok(Some(candidate)) => candidate,
        Ok(None) => return UpdateRunOutcome::NoUpdate,
        Err(_) => return UpdateRunOutcome::InvalidResponse,
    };

    if existing_pending_covers_candidate(app_data, &prepared.state, &candidate) {
        return UpdateRunOutcome::ExistingPending;
    }

    // A valid previous pending candidate is copied to a same-directory rollback
    // file before publishing the new verified candidate. Network/hash/publish
    // failure never touches the old target. If durable metadata persistence
    // fails after publish, restore the previous exact bytes before returning.
    let mut rollback = match create_pending_rollback_copy(app_data, prepared.state.pending.as_ref())
    {
        Ok(rollback) => rollback,
        Err(_) => return UpdateRunOutcome::DownloadFailed,
    };

    let pending = match publish_verified_candidate_with(app_data, &candidate, network, replace) {
        Ok(pending) => pending,
        Err(_) => {
            if let Some(path) = rollback.take() {
                let _ = fs::remove_file(path);
            }
            return UpdateRunOutcome::DownloadFailed;
        }
    };

    prepared.state.pending = Some(pending);

    if save_state(&prepared.state_path, &prepared.state).is_err() {
        let target = pending_update_path(app_data);
        let had_rollback = rollback.is_some();

        if had_rollback {
            // Best effort: restore the previous verified pending at the
            // deterministic target. If restore fails, keep the rollback file
            // itself as the last recoverable trusted copy.
            let _ = restore_pending_rollback(&mut rollback, &target);
        } else {
            // With no valid previous pending, the newly verified but unrecorded
            // target has no durable state authority.
            let _ = fs::remove_file(&target);
        }

        if let Some(_path) = rollback.take() {
            // Deliberately retain a failed-restore rollback file. Dropping a
            // PathBuf does not delete the file.
        }

        return UpdateRunOutcome::PendingStatePersistFailed;
    }

    if let Some(path) = rollback.take() {
        let _ = fs::remove_file(path);
    }

    UpdateRunOutcome::PendingPublished
}

#[cfg(windows)]
fn verify_apply_candidate_file(path: &Path, pending: &VerifiedPending) -> io::Result<()> {
    let observed_sha256 = sha256_file(path, pending.size)?;

    if observed_sha256 != pending.sha256 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "update candidate SHA-256 does not match durable pending metadata",
        ));
    }

    let product_version = executable_product_version(path)?;
    if product_version != pending.version {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "update candidate ProductVersion does not match durable pending version",
        ));
    }

    Ok(())
}

#[cfg(windows)]
fn suppress_failed_pending(
    roaming_app_data: &Path,
    state_path: &Path,
    state: &UpdateState,
) -> bool {
    let pending_path = pending_update_path(roaming_app_data);

    let file_suppressed = match fs::remove_file(&pending_path) {
        Ok(()) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => true,
        Err(_) => false,
    };

    let mut cleared = state.clone();
    cleared.pending = None;
    let state_suppressed = save_update_state(state_path, &cleared).is_ok();

    file_suppressed || state_suppressed
}

#[cfg(windows)]
fn acquire_apply_guard(name: &str) -> io::Result<NamedMutexGuard> {
    match try_acquire_named_mutex(name)? {
        NamedMutexAcquire::Acquired(guard) => Ok(guard),
        NamedMutexAcquire::AlreadyExists => Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "updater apply guard already exists",
        )),
    }
}

/// Evaluates and, when safe, hands a pending update from the canonical old
/// Helper to a staged copy of that same trusted old Helper.
///
/// This runs before normal runtime acquisition. It never quiesces an existing
/// runtime: if INSTALL or RUNTIME already exists, this launch simply exits.
#[cfg(windows)]
pub fn prepare_update_apply_parent(current_executable: &Path) -> UpdateApplyParentDisposition {
    // Hard development sentinel: zero U2 Known Folder/state/mutex/process work.
    if !current_build_updater_enabled() {
        return UpdateApplyParentDisposition::NotApplicable;
    }

    let (local_app_data, roaming_app_data) = match resolve_update_known_folder_authority() {
        Ok(authority) => authority,
        Err(_) => return UpdateApplyParentDisposition::NotApplicable,
    };

    if validate_uninstall_parent_executable_at(current_executable, &local_app_data).is_err() {
        return UpdateApplyParentDisposition::NotApplicable;
    }

    let state_path = update_state_path(&roaming_app_data);
    let state = load_update_state(&state_path, CURRENT_VERSION);
    let Some(pending) = state.pending.as_ref() else {
        return UpdateApplyParentDisposition::NotApplicable;
    };

    let pending_path = pending_update_path(&roaming_app_data);

    // Catch corrupt/mis-versioned cache while the old canonical process is
    // still alive. U2 child repeats this proof after parent exit.
    if verify_apply_candidate_file(&pending_path, pending).is_err() {
        let _ = suppress_failed_pending(&roaming_app_data, &state_path, &state);
        return UpdateApplyParentDisposition::NotApplicable;
    }

    let install_guard = match acquire_apply_guard(INSTALL_MUTEX_NAME) {
        Ok(guard) => guard,
        Err(_) => return UpdateApplyParentDisposition::Exit,
    };

    let runtime_guard = match acquire_apply_guard(RUNTIME_MUTEX_NAME) {
        Ok(guard) => guard,
        Err(_) => {
            drop(install_guard);
            return UpdateApplyParentDisposition::Exit;
        }
    };

    let nonce = match generate_cleanup_nonce() {
        Ok(nonce) => nonce,
        Err(_) => {
            drop(install_guard);
            return UpdateApplyParentDisposition::ContinueRuntime(runtime_guard);
        }
    };

    let authority = match stage_cleanup_child(
        current_executable,
        &env::temp_dir(),
        std::process::id(),
        nonce,
    ) {
        Ok(authority) => authority,
        Err(_) => {
            drop(install_guard);
            return UpdateApplyParentDisposition::ContinueRuntime(runtime_guard);
        }
    };

    match launch_update_child_with_guards(&authority, &install_guard, &runtime_guard) {
        Ok(child_process_id) if child_process_id != 0 => UpdateApplyParentDisposition::Exit,
        Ok(_) | Err(_) => {
            remove_unlaunched_cleanup_stage_best_effort(&authority);
            drop(install_guard);
            UpdateApplyParentDisposition::ContinueRuntime(runtime_guard)
        }
    }
}

#[cfg(windows)]
fn restart_canonical_after_precommit_failure(
    canonical_executable: &Path,
    handoff: &mut crate::uninstall::UpdateParentExitHandoff,
) -> bool {
    handoff.release_runtime();
    Command::new(canonical_executable).spawn().is_ok()
}

/// Applies one verified pending candidate from the trusted staged OLD Helper.
///
/// Canonical replacement is the single commit barrier. No executable rollback
/// exists after it.
#[cfg(windows)]
pub fn run_update_apply_child(
    parent_process_id: u32,
    nonce: u64,
    parent_handle: CleanupParentHandleValue,
    install_handle: CleanupParentHandleValue,
    runtime_handle: CleanupParentHandleValue,
) -> io::Result<UpdateApplyChildOutcome> {
    let current_executable = env::current_exe()?;

    let mut handoff = prove_update_parent_exit_handoff(
        &current_executable,
        parent_process_id,
        nonce,
        parent_handle,
        install_handle,
        runtime_handle,
    )?;

    let (local_app_data, roaming_app_data) = match resolve_update_known_folder_authority() {
        Ok(authority) => authority,
        Err(_) => {
            let _ = cleanup_staged_child_after_parent_exit(handoff.proof());
            return Ok(UpdateApplyChildOutcome::Incomplete);
        }
    };

    let canonical_executable = install_path(&local_app_data);
    let state_path = update_state_path(&roaming_app_data);
    let mut state = load_update_state(&state_path, CURRENT_VERSION);

    let Some(pending) = state.pending.clone() else {
        let restarted =
            restart_canonical_after_precommit_failure(&canonical_executable, &mut handoff);
        let _ = cleanup_staged_child_after_parent_exit(handoff.proof());

        return Ok(if restarted {
            UpdateApplyChildOutcome::RecoveredOldRuntime
        } else {
            UpdateApplyChildOutcome::Incomplete
        });
    };

    let pending_path = pending_update_path(&roaming_app_data);

    let prepared = (|| -> io::Result<_> {
        // Revalidate deterministic pending after exact parent exit.
        verify_apply_candidate_file(&pending_path, &pending)?;

        // Copy into the canonical directory, then bind verification to the exact
        // bytes that will cross the commit barrier.
        let prepared = prepare_executable_install(&pending_path, &canonical_executable)?;
        verify_apply_candidate_file(prepared.path(), &pending)?;
        Ok(prepared)
    })();

    let prepared = match prepared {
        Ok(prepared) => prepared,
        Err(_) => {
            let suppressed = suppress_failed_pending(&roaming_app_data, &state_path, &state);
            let restarted = if suppressed {
                restart_canonical_after_precommit_failure(&canonical_executable, &mut handoff)
            } else {
                false
            };
            let _ = cleanup_staged_child_after_parent_exit(handoff.proof());

            return Ok(if restarted {
                UpdateApplyChildOutcome::RecoveredOldRuntime
            } else {
                UpdateApplyChildOutcome::Incomplete
            });
        }
    };

    if prepared.commit().is_err() {
        let suppressed = suppress_failed_pending(&roaming_app_data, &state_path, &state);
        let restarted = if suppressed {
            restart_canonical_after_precommit_failure(&canonical_executable, &mut handoff)
        } else {
            false
        };
        let _ = cleanup_staged_child_after_parent_exit(handoff.proof());

        return Ok(if restarted {
            UpdateApplyChildOutcome::RecoveredOldRuntime
        } else {
            UpdateApplyChildOutcome::Incomplete
        });
    }

    // COMMIT BARRIER crossed. Never restore the old executable after here.
    let _ = registration::register_installed_app_version(&local_app_data, &pending.version);

    state.pending = None;
    let _ = save_update_state(&state_path, &state);
    let _ = fs::remove_file(&pending_path);

    // New installed startup only needs RUNTIME. INSTALL intentionally remains
    // alive in this staged child until its final exit.
    handoff.release_runtime();

    let launched = Command::new(&canonical_executable).spawn().is_ok();
    let _ = cleanup_staged_child_after_parent_exit(handoff.proof());

    Ok(if launched {
        UpdateApplyChildOutcome::Committed
    } else {
        UpdateApplyChildOutcome::Incomplete
    })
}
/// Starts at most one bounded U1 updater worker for a real release build.
///
/// The hard development sentinel check is deliberately first: `0.0.0` performs
/// no APPDATA lookup, state I/O, thread creation, or GitHub request.
///
/// For release builds, the tiny attempt-state transaction is completed before
/// spawning the network worker. Therefore a restart loop cannot create request
/// spam even if thread creation or GitHub access later fails.
#[cfg(windows)]
pub fn spawn_startup_update_check() {
    if !current_build_updater_enabled() {
        return;
    }

    let Some(app_data) = env::var_os("APPDATA").map(PathBuf::from) else {
        return;
    };

    let Ok(now_unix_secs) = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
    else {
        return;
    };

    let prepared = match prepare_attempt(&app_data, CURRENT_VERSION, now_unix_secs) {
        AttemptPreparation::Ready(prepared) => prepared,
        AttemptPreparation::Disabled
        | AttemptPreparation::NotDue
        | AttemptPreparation::StatePersistFailed => return,
    };

    let _ = thread::Builder::new()
        .name("lcr-updater-u1".into())
        .spawn(move || {
            let Ok(mut network) = crate::winhttp::PublicUpdateHttp::open() else {
                return;
            };

            let _ = run_prepared_attempt_with(
                &app_data,
                CURRENT_VERSION,
                prepared,
                &mut network,
                replace_same_directory_file_atomically,
                save_update_state,
            );
        });
}
/// U1's deterministic persistent candidate location.
///
/// The path is inside `%APPDATA%\LCR`, already owned by Policy-B clean uninstall.
/// U1 only publishes an already-verified file here; it never executes it.
pub fn verified_pending_path(app_data: &Path) -> PathBuf {
    pending_update_path(app_data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::Cell,
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    const HASH_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const HASH_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn test_root(case: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();

        let root = std::env::temp_dir().join(format!(
            "lcr-helper-updater-u1a-{case}-{}-{stamp}",
            std::process::id()
        ));

        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn asset(tag: &str, hash: &str, size: u64) -> Value {
        json!({
            "name": RELEASE_ASSET_NAME,
            "size": size,
            "digest": format!("sha256:{hash}"),
            "browser_download_url": expected_download_url(tag)
        })
    }

    fn release(tag: &str, draft: bool, assets: Vec<Value>, body: &str) -> Value {
        let prerelease = tag
            .strip_prefix('v')
            .and_then(|version| Version::parse(version).ok())
            .is_some_and(|version| !version.pre.is_empty());

        json!({
            "tag_name": tag,
            "draft": draft,
            "prerelease": prerelease,
            "body": body,
            "assets": assets
        })
    }

    fn releases_json(releases: Vec<Value>) -> Vec<u8> {
        serde_json::to_vec(&Value::Array(releases)).unwrap()
    }

    fn candidate_version(current: &str, releases: Vec<Value>) -> Option<String> {
        select_release(current, &releases_json(releases))
            .unwrap()
            .map(|candidate| candidate.version)
    }

    #[test]
    fn current_build_updater_gate_matches_package_version() {
        assert_eq!(
            current_build_updater_enabled(),
            updater_enabled(CURRENT_VERSION)
        );
        assert_eq!(
            current_build_updater_enabled(),
            CURRENT_VERSION != DEVELOPMENT_VERSION_SENTINEL
        );
        assert!(!updater_enabled(DEVELOPMENT_VERSION_SENTINEL));
    }

    #[test]
    fn development_sentinel_invokes_no_fetch_and_writes_no_state() {
        let root = test_root("dev-disabled");
        let called = Cell::new(false);

        let outcome = discover_once_with(&root, DEVELOPMENT_VERSION_SENTINEL, 1_000, || {
            called.set(true);
            Ok(Vec::new())
        });

        assert_eq!(outcome, DiscoveryAttempt::Disabled);
        assert!(!called.get());
        assert!(!update_state_path(&root).exists());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn prerelease_current_selects_higher_prerelease() {
        assert_eq!(
            candidate_version(
                "0.0.2-beta",
                vec![release(
                    "v0.0.3-beta",
                    false,
                    vec![asset("v0.0.3-beta", HASH_A, 100)],
                    ""
                )]
            ),
            Some("0.0.3-beta".into())
        );
    }

    #[test]
    fn prerelease_current_may_select_higher_stable() {
        assert_eq!(
            candidate_version(
                "0.0.3-beta.2",
                vec![release(
                    "v0.0.3",
                    false,
                    vec![asset("v0.0.3", HASH_A, 100)],
                    ""
                )]
            ),
            Some("0.0.3".into())
        );
    }

    #[test]
    fn stable_current_ignores_prerelease() {
        assert_eq!(
            candidate_version(
                "0.0.3",
                vec![
                    release(
                        "v0.0.4-beta",
                        false,
                        vec![asset("v0.0.4-beta", HASH_A, 100)],
                        ""
                    ),
                    release("v0.0.4", false, vec![asset("v0.0.4", HASH_B, 100)], ""),
                ]
            ),
            Some("0.0.4".into())
        );
    }

    #[test]
    fn same_lower_and_build_metadata_only_versions_are_ignored() {
        assert_eq!(
            candidate_version(
                "1.2.3+local",
                vec![
                    release(
                        "v1.2.3+public",
                        false,
                        vec![asset("v1.2.3+public", HASH_A, 100)],
                        ""
                    ),
                    release("v1.2.2", false, vec![asset("v1.2.2", HASH_B, 100)], ""),
                ]
            ),
            None
        );
    }

    #[test]
    fn malformed_fuzzy_and_draft_releases_are_ignored() {
        assert_eq!(
            candidate_version(
                "1.0.0-beta",
                vec![
                    release("1.1.0", false, vec![asset("1.1.0", HASH_A, 100)], ""),
                    release("vv1.2.0", false, vec![asset("vv1.2.0", HASH_A, 100)], ""),
                    release("v1.3.0", true, vec![asset("v1.3.0", HASH_A, 100)], ""),
                ]
            ),
            None
        );
    }

    #[test]
    fn github_prerelease_flag_must_match_semver_channel() {
        let mut stable_marked_prerelease =
            release("v1.2.0", false, vec![asset("v1.2.0", HASH_A, 100)], "");
        stable_marked_prerelease["prerelease"] = json!(true);

        let mut beta_marked_stable = release(
            "v1.3.0-beta.1",
            false,
            vec![asset("v1.3.0-beta.1", HASH_B, 100)],
            "",
        );
        beta_marked_stable["prerelease"] = json!(false);

        assert_eq!(
            candidate_version(
                "1.0.0-beta",
                vec![stable_marked_prerelease, beta_marked_stable]
            ),
            None
        );
    }

    #[test]
    fn stale_pending_is_ignored_for_running_version() {
        let root = test_root("stale-pending");
        let state_path = update_state_path(&root);

        save_update_state(
            &state_path,
            &UpdateState {
                last_attempt_unix_secs: Some(5),
                pending: Some(VerifiedPending {
                    version: "1.0.0+older-build".into(),
                    sha256: HASH_A.into(),
                    size: 123,
                }),
            },
        )
        .unwrap();

        let state = load_update_state(&state_path, "1.0.0+current-build");

        assert_eq!(state.last_attempt_unix_secs, Some(5));
        assert_eq!(state.pending, None);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn strictly_newer_pending_survives_version_aware_load() {
        let root = test_root("newer-pending");
        let state_path = update_state_path(&root);
        let pending = VerifiedPending {
            version: "1.1.0-beta.1".into(),
            sha256: HASH_B.into(),
            size: 456,
        };

        save_update_state(
            &state_path,
            &UpdateState {
                last_attempt_unix_secs: Some(5),
                pending: Some(pending.clone()),
            },
        )
        .unwrap();

        let state = load_update_state(&state_path, "1.0.0");

        assert_eq!(state.pending, Some(pending));

        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn duplicate_or_missing_helper_asset_is_rejected() {
        let duplicate = release(
            "v1.1.0",
            false,
            vec![asset("v1.1.0", HASH_A, 100), asset("v1.1.0", HASH_A, 100)],
            "",
        );

        let missing = release(
            "v1.2.0",
            false,
            vec![json!({
                "name": "other.exe",
                "size": 100,
                "digest": format!("sha256:{HASH_A}"),
                "browser_download_url": "https://example.invalid/other.exe"
            })],
            "",
        );

        assert_eq!(candidate_version("1.0.0", vec![duplicate, missing]), None);
    }

    #[test]
    fn malformed_digest_oversize_and_wrong_download_url_are_rejected() {
        let malformed_digest = release("v1.1.0", false, vec![asset("v1.1.0", "abcd", 100)], "");

        let oversize = release(
            "v1.2.0",
            false,
            vec![asset("v1.2.0", HASH_A, MAX_HELPER_ASSET_BYTES + 1)],
            "",
        );

        let wrong_url = release(
            "v1.3.0",
            false,
            vec![json!({
                "name": RELEASE_ASSET_NAME,
                "size": 100,
                "digest": format!("sha256:{HASH_A}"),
                "browser_download_url": "https://example.invalid/LCR.exe"
            })],
            "",
        );

        assert_eq!(
            candidate_version("1.0.0", vec![malformed_digest, oversize, wrong_url]),
            None
        );
    }

    #[test]
    fn release_body_is_never_version_or_digest_authority() {
        let selected = select_release(
            "1.0.0",
            &releases_json(vec![release(
                "v1.1.0",
                false,
                vec![asset("v1.1.0", HASH_A, 123)],
                "LCR.exe: sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
            )]),
        )
        .unwrap()
        .unwrap();

        assert_eq!(selected.version, "1.1.0");
        assert_eq!(selected.sha256, HASH_A);
        assert_eq!(selected.size, 123);
    }

    #[test]
    fn highest_semver_wins_independent_of_json_order() {
        let selected = candidate_version(
            "1.0.0-beta",
            vec![
                release(
                    "v1.1.0-beta.2",
                    false,
                    vec![asset("v1.1.0-beta.2", HASH_A, 100)],
                    "",
                ),
                release(
                    "v1.3.0-beta.1",
                    false,
                    vec![asset("v1.3.0-beta.1", HASH_B, 100)],
                    "",
                ),
                release("v1.2.0", false, vec![asset("v1.2.0", HASH_A, 100)], ""),
            ],
        );

        assert_eq!(selected, Some("1.3.0-beta.1".into()));
    }

    #[test]
    fn full_page_is_ambiguous_and_fails_closed() {
        let releases = (0..RELEASE_PAGE_LIMIT)
            .map(|index| {
                let tag = format!("v1.0.{index}");
                release(&tag, false, vec![asset(&tag, HASH_A, 100)], "")
            })
            .collect::<Vec<_>>();

        assert_eq!(
            select_release("0.9.0", &releases_json(releases)),
            Err(ReleaseDiscoveryError::AmbiguousPage)
        );
    }

    #[test]
    fn candidate_download_path_is_rebound_to_fixed_https_repository() {
        let candidate = ReleaseCandidate {
            version: "1.2.3-beta.1".into(),
            sha256: HASH_A.into(),
            size: 123,
            download_url: expected_download_url("v1.2.3-beta.1"),
        };

        assert_eq!(
            release_download_path(&candidate).as_deref(),
            Some("/Kjndst/LCR-Command-Radio/releases/download/v1.2.3-beta.1/LCR.exe")
        );

        let mut wrong_host = candidate.clone();
        wrong_host.download_url =
            "https://example.invalid/Kjndst/LCR-Command-Radio/releases/download/v1.2.3-beta.1/LCR.exe"
                .into();
        assert_eq!(release_download_path(&wrong_host), None);

        let mut wrong_scheme = candidate.clone();
        wrong_scheme.download_url =
            "http://github.com/Kjndst/LCR-Command-Radio/releases/download/v1.2.3-beta.1/LCR.exe"
                .into();
        assert_eq!(release_download_path(&wrong_scheme), None);

        let mut query = candidate;
        query.download_url.push_str("?unexpected=1");
        assert_eq!(release_download_path(&query), None);
    }
    #[test]
    fn pending_path_stays_inside_policy_b_owned_appdata_tree() {
        let app_data = Path::new(r"C:\Users\Test\AppData\Roaming");
        assert_eq!(
            verified_pending_path(app_data),
            app_data.join("LCR").join("updates").join("LCR.pending.exe")
        );
    }

    #[cfg(windows)]
    struct FixtureNetwork {
        release_body: Vec<u8>,
        chunks: Vec<Vec<u8>>,
        fail_before_chunk: Option<usize>,
        fetch_calls: usize,
        download_calls: usize,
    }

    #[cfg(windows)]
    impl FixtureNetwork {
        fn download_only(chunks: Vec<Vec<u8>>) -> Self {
            Self {
                release_body: releases_json(Vec::new()),
                chunks,
                fail_before_chunk: None,
                fetch_calls: 0,
                download_calls: 0,
            }
        }
    }

    #[cfg(windows)]
    impl UpdateNetwork for FixtureNetwork {
        fn fetch_releases(&mut self) -> io::Result<Vec<u8>> {
            self.fetch_calls += 1;
            Ok(self.release_body.clone())
        }

        fn download_candidate(
            &mut self,
            _candidate: &ReleaseCandidate,
            sink: &mut dyn FnMut(&[u8]) -> io::Result<()>,
        ) -> io::Result<()> {
            self.download_calls += 1;

            for (index, chunk) in self.chunks.iter().enumerate() {
                if self.fail_before_chunk == Some(index) {
                    return Err(io::Error::other("injected mid-download failure"));
                }
                sink(chunk)?;
            }

            if self.fail_before_chunk == Some(self.chunks.len()) {
                return Err(io::Error::other("injected end-download failure"));
            }

            Ok(())
        }
    }

    #[cfg(windows)]
    fn abc_candidate(version: &str) -> ReleaseCandidate {
        const ABC_SHA256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

        ReleaseCandidate {
            version: version.into(),
            sha256: ABC_SHA256.into(),
            size: 3,
            download_url: expected_download_url(&format!("v{version}")),
        }
    }

    #[cfg(windows)]
    #[test]
    fn suppress_failed_pending_blocks_immediate_reapply_by_state_and_file() {
        let root = test_root("u2-suppress-pending");
        let state_path = update_state_path(&root);
        let pending_path = pending_update_path(&root);

        fs::create_dir_all(pending_path.parent().unwrap()).unwrap();
        fs::write(&pending_path, b"bad").unwrap();

        let state = UpdateState {
            last_attempt_unix_secs: Some(10),
            pending: Some(VerifiedPending {
                version: "1.1.0".into(),
                sha256: HASH_A.into(),
                size: 3,
            }),
        };
        save_update_state(&state_path, &state).unwrap();

        assert!(suppress_failed_pending(&root, &state_path, &state));
        assert!(!pending_path.exists());
        assert_eq!(load_update_state(&state_path, "1.0.0").pending, None);

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires the separately built product LCR.exe"]
    fn u2_prepared_candidate_binds_exact_hash_size_and_product_version() {
        let executable = std::env::var_os("LCR_U2A_BUILT_EXE")
            .expect("LCR_U2A_BUILT_EXE must point to the built product binary");
        let executable = Path::new(&executable);
        let size = fs::metadata(executable).unwrap().len();
        let sha256 = sha256_file(executable, size).unwrap();

        let pending = VerifiedPending {
            version: CURRENT_VERSION.into(),
            sha256,
            size,
        };

        let root = test_root("u2-prepared-built-product");
        let target = root.join("Programs").join("LCR").join("LCR.exe");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, b"trusted-old-placeholder").unwrap();

        let prepared = prepare_executable_install(executable, &target).unwrap();
        verify_apply_candidate_file(prepared.path(), &pending).unwrap();

        let mut wrong_version = pending.clone();
        wrong_version.version = "9.9.9".into();
        assert!(verify_apply_candidate_file(prepared.path(), &wrong_version).is_err());

        // Verification itself never crosses the commit barrier.
        assert_eq!(fs::read(&target).unwrap(), b"trusted-old-placeholder");

        drop(prepared);
        fs::remove_dir_all(root).unwrap();
    }
    #[cfg(windows)]
    #[test]
    fn bcrypt_sha256_matches_known_vector() {
        let mut hasher = Sha256Hash::new().unwrap();
        hasher.update(b"a").unwrap();
        hasher.update(b"bc").unwrap();

        assert_eq!(
            hasher.finish().unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[cfg(windows)]
    #[test]
    fn verified_download_publishes_exact_pending_bytes() {
        let root = test_root("u1b-publish");
        let candidate = abc_candidate("1.1.0");
        let mut network = FixtureNetwork::download_only(vec![b"a".to_vec(), b"bc".to_vec()]);

        let pending = publish_verified_candidate_with(
            &root,
            &candidate,
            &mut network,
            replace_same_directory_file_atomically,
        )
        .unwrap();

        assert_eq!(pending.version, "1.1.0");
        assert_eq!(pending.sha256, candidate.sha256);
        assert_eq!(pending.size, 3);
        assert_eq!(fs::read(pending_update_path(&root)).unwrap(), b"abc");
        assert_eq!(network.download_calls, 1);

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn hash_mismatch_preserves_previous_verified_pending() {
        let root = test_root("u1b-hash-mismatch");
        let pending_path = pending_update_path(&root);
        fs::create_dir_all(pending_path.parent().unwrap()).unwrap();
        fs::write(&pending_path, b"old-pending").unwrap();

        let mut candidate = abc_candidate("1.1.0");
        candidate.sha256 = HASH_A.into();

        let mut network = FixtureNetwork::download_only(vec![b"abc".to_vec()]);

        assert!(publish_verified_candidate_with(
            &root,
            &candidate,
            &mut network,
            replace_same_directory_file_atomically,
        )
        .is_err());

        assert_eq!(fs::read(&pending_path).unwrap(), b"old-pending");

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn size_mismatch_preserves_previous_verified_pending() {
        let root = test_root("u1b-size-mismatch");
        let pending_path = pending_update_path(&root);
        fs::create_dir_all(pending_path.parent().unwrap()).unwrap();
        fs::write(&pending_path, b"old-pending").unwrap();

        let mut candidate = abc_candidate("1.1.0");
        candidate.size = 4;

        let mut network = FixtureNetwork::download_only(vec![b"abc".to_vec()]);

        assert!(publish_verified_candidate_with(
            &root,
            &candidate,
            &mut network,
            replace_same_directory_file_atomically,
        )
        .is_err());

        assert_eq!(fs::read(&pending_path).unwrap(), b"old-pending");

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn mid_download_failure_preserves_previous_verified_pending() {
        let root = test_root("u1b-mid-download");
        let pending_path = pending_update_path(&root);
        fs::create_dir_all(pending_path.parent().unwrap()).unwrap();
        fs::write(&pending_path, b"old-pending").unwrap();

        let candidate = abc_candidate("1.1.0");
        let mut network = FixtureNetwork::download_only(vec![b"a".to_vec(), b"bc".to_vec()]);
        network.fail_before_chunk = Some(1);

        assert!(publish_verified_candidate_with(
            &root,
            &candidate,
            &mut network,
            replace_same_directory_file_atomically,
        )
        .is_err());

        assert_eq!(fs::read(&pending_path).unwrap(), b"old-pending");

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn atomic_publish_failure_preserves_previous_verified_pending() {
        let root = test_root("u1b-publish-failure");
        let pending_path = pending_update_path(&root);
        fs::create_dir_all(pending_path.parent().unwrap()).unwrap();
        fs::write(&pending_path, b"old-pending").unwrap();

        let candidate = abc_candidate("1.1.0");
        let mut network = FixtureNetwork::download_only(vec![b"abc".to_vec()]);

        assert!(
            publish_verified_candidate_with(&root, &candidate, &mut network, |_, _| Err(
                io::Error::other("injected atomic publish failure")
            ),)
            .is_err()
        );

        assert_eq!(fs::read(&pending_path).unwrap(), b"old-pending");

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn newer_existing_pending_avoids_lower_candidate_download() {
        let root = test_root("u1b-existing-newer");
        let state_path = update_state_path(&root);
        let pending_path = pending_update_path(&root);
        fs::create_dir_all(pending_path.parent().unwrap()).unwrap();
        fs::write(&pending_path, b"abc").unwrap();

        let state = UpdateState {
            last_attempt_unix_secs: Some(50),
            pending: Some(VerifiedPending {
                version: "1.2.0".into(),
                sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".into(),
                size: 3,
            }),
        };
        save_update_state(&state_path, &state).unwrap();

        let candidate = abc_candidate("1.1.0");
        let mut network = FixtureNetwork {
            release_body: releases_json(vec![release(
                "v1.1.0",
                false,
                vec![asset("v1.1.0", &candidate.sha256, candidate.size)],
                "",
            )]),
            chunks: vec![b"abc".to_vec()],
            fail_before_chunk: None,
            fetch_calls: 0,
            download_calls: 0,
        };

        let prepared = PreparedAttempt { state_path, state };

        assert_eq!(
            run_prepared_attempt_with(
                &root,
                "1.0.0",
                prepared,
                &mut network,
                replace_same_directory_file_atomically,
                save_update_state,
            ),
            UpdateRunOutcome::ExistingPending
        );

        assert_eq!(network.fetch_calls, 1);
        assert_eq!(network.download_calls, 0);
        assert_eq!(fs::read(&pending_path).unwrap(), b"abc");

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn same_size_corrupt_pending_is_rehashed_and_redownloaded() {
        let root = test_root("u1b-corrupt-same-size");
        let state_path = update_state_path(&root);
        let pending_path = pending_update_path(&root);
        fs::create_dir_all(pending_path.parent().unwrap()).unwrap();

        // Three bytes, same as the durable metadata, but wrong SHA-256.
        fs::write(&pending_path, b"bad").unwrap();

        let candidate = abc_candidate("1.1.0");
        let state = UpdateState {
            last_attempt_unix_secs: Some(70),
            pending: Some(VerifiedPending {
                version: "1.1.0".into(),
                sha256: candidate.sha256.clone(),
                size: 3,
            }),
        };
        save_update_state(&state_path, &state).unwrap();

        let mut network = FixtureNetwork {
            release_body: releases_json(vec![release(
                "v1.1.0",
                false,
                vec![asset("v1.1.0", &candidate.sha256, candidate.size)],
                "",
            )]),
            chunks: vec![b"abc".to_vec()],
            fail_before_chunk: None,
            fetch_calls: 0,
            download_calls: 0,
        };

        let prepared = PreparedAttempt { state_path, state };

        assert_eq!(
            run_prepared_attempt_with(
                &root,
                "1.0.0",
                prepared,
                &mut network,
                replace_same_directory_file_atomically,
                save_update_state,
            ),
            UpdateRunOutcome::PendingPublished
        );

        assert_eq!(network.download_calls, 1);
        assert_eq!(fs::read(&pending_path).unwrap(), b"abc");

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn post_publish_state_failure_restores_previous_verified_pending() {
        let root = test_root("u1b-state-fail-rollback");
        let state_path = update_state_path(&root);
        let pending_path = pending_update_path(&root);
        fs::create_dir_all(pending_path.parent().unwrap()).unwrap();

        const OLD_BYTES: &[u8] = b"old";
        const OLD_SHA256: &str = "cba06b5736faf67e54b07b561eae94395e774c517a7d910a54369e1263ccfbd4";

        fs::write(&pending_path, OLD_BYTES).unwrap();

        let old_state = UpdateState {
            last_attempt_unix_secs: Some(70),
            pending: Some(VerifiedPending {
                version: "1.0.5".into(),
                sha256: OLD_SHA256.into(),
                size: OLD_BYTES.len() as u64,
            }),
        };
        save_update_state(&state_path, &old_state).unwrap();

        let candidate = abc_candidate("1.1.0");
        let mut network = FixtureNetwork {
            release_body: releases_json(vec![release(
                "v1.1.0",
                false,
                vec![asset("v1.1.0", &candidate.sha256, candidate.size)],
                "",
            )]),
            chunks: vec![b"abc".to_vec()],
            fail_before_chunk: None,
            fetch_calls: 0,
            download_calls: 0,
        };

        let prepared = PreparedAttempt {
            state_path: state_path.clone(),
            state: old_state.clone(),
        };

        assert_eq!(
            run_prepared_attempt_with(
                &root,
                "1.0.0",
                prepared,
                &mut network,
                replace_same_directory_file_atomically,
                |_, _| Err(io::Error::other("injected post-publish state failure")),
            ),
            UpdateRunOutcome::PendingStatePersistFailed
        );

        assert_eq!(fs::read(&pending_path).unwrap(), OLD_BYTES);
        assert_eq!(load_update_state(&state_path, "1.0.0"), old_state);

        let rollback_residue = fs::read_dir(pending_path.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".rollback"))
            .count();
        assert_eq!(rollback_residue, 0);

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn post_publish_state_failure_without_previous_pending_removes_unrecorded_target() {
        let root = test_root("u1b-state-fail-no-prior");
        let state_path = update_state_path(&root);
        let state = UpdateState {
            last_attempt_unix_secs: Some(70),
            pending: None,
        };
        save_update_state(&state_path, &state).unwrap();

        let candidate = abc_candidate("1.1.0");
        let mut network = FixtureNetwork {
            release_body: releases_json(vec![release(
                "v1.1.0",
                false,
                vec![asset("v1.1.0", &candidate.sha256, candidate.size)],
                "",
            )]),
            chunks: vec![b"abc".to_vec()],
            fail_before_chunk: None,
            fetch_calls: 0,
            download_calls: 0,
        };

        let prepared = PreparedAttempt {
            state_path: state_path.clone(),
            state: state.clone(),
        };

        assert_eq!(
            run_prepared_attempt_with(
                &root,
                "1.0.0",
                prepared,
                &mut network,
                replace_same_directory_file_atomically,
                |_, _| Err(io::Error::other("injected post-publish state failure")),
            ),
            UpdateRunOutcome::PendingStatePersistFailed
        );

        assert!(!pending_update_path(&root).exists());
        assert_eq!(load_update_state(&state_path, "1.0.0"), state);

        fs::remove_dir_all(root).unwrap();
    }
    #[cfg(windows)]
    #[test]
    fn successful_network_attempt_persists_verified_pending_metadata() {
        let root = test_root("u1b-state-success");
        let state_path = update_state_path(&root);
        let state = UpdateState {
            last_attempt_unix_secs: Some(70),
            pending: None,
        };
        save_update_state(&state_path, &state).unwrap();

        let candidate = abc_candidate("1.1.0");
        let mut network = FixtureNetwork {
            release_body: releases_json(vec![release(
                "v1.1.0",
                false,
                vec![asset("v1.1.0", &candidate.sha256, candidate.size)],
                "",
            )]),
            chunks: vec![b"a".to_vec(), b"bc".to_vec()],
            fail_before_chunk: None,
            fetch_calls: 0,
            download_calls: 0,
        };

        let prepared = PreparedAttempt {
            state_path: state_path.clone(),
            state,
        };

        assert_eq!(
            run_prepared_attempt_with(
                &root,
                "1.0.0",
                prepared,
                &mut network,
                replace_same_directory_file_atomically,
                save_update_state,
            ),
            UpdateRunOutcome::PendingPublished
        );

        let after = load_update_state(&state_path, "1.0.0");
        assert_eq!(
            after.pending,
            Some(VerifiedPending {
                version: "1.1.0".into(),
                sha256: candidate.sha256,
                size: 3,
            })
        );
        assert_eq!(fs::read(pending_update_path(&root)).unwrap(), b"abc");

        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn attempt_timestamp_is_persisted_before_fetch() {
        let root = test_root("persist-before-fetch");
        let state_path = update_state_path(&root);
        let called = Cell::new(false);

        let outcome = discover_once_with(&root, "1.0.0", 9_000, || {
            called.set(true);
            let state = load_update_state(&state_path, "1.0.0");
            assert_eq!(state.last_attempt_unix_secs, Some(9_000));
            Ok(releases_json(Vec::new()))
        });

        assert_eq!(outcome, DiscoveryAttempt::NoUpdate);
        assert!(called.get());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn state_persistence_failure_invokes_no_fetch() {
        let root = test_root("persist-fail");
        fs::write(root.join("LCR"), b"blocks-directory").unwrap();

        let called = Cell::new(false);
        let outcome = discover_once_with(&root, "1.0.0", 9_000, || {
            called.set(true);
            Ok(releases_json(Vec::new()))
        });

        assert_eq!(outcome, DiscoveryAttempt::StatePersistFailed);
        assert!(!called.get());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_fetch_consumes_window_and_preserves_verified_pending() {
        let root = test_root("fetch-fail-preserve");
        let state_path = update_state_path(&root);
        let prior = UpdateState {
            last_attempt_unix_secs: Some(1),
            pending: Some(VerifiedPending {
                version: "1.1.0".into(),
                sha256: HASH_A.into(),
                size: 123,
            }),
        };
        save_update_state(&state_path, &prior).unwrap();

        let outcome = discover_once_with(&root, "1.0.0", 90_000, || Err(()));

        assert_eq!(outcome, DiscoveryAttempt::FetchFailed);

        let after = load_update_state(&state_path, "1.0.0");
        assert_eq!(after.last_attempt_unix_secs, Some(90_000));
        assert_eq!(after.pending, prior.pending);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn backward_clock_performs_no_fetch() {
        let root = test_root("backward-clock");
        let state_path = update_state_path(&root);
        save_update_state(
            &state_path,
            &UpdateState {
                last_attempt_unix_secs: Some(10_000),
                pending: None,
            },
        )
        .unwrap();

        let called = Cell::new(false);
        let outcome = discover_once_with(&root, "1.0.0", 9_000, || {
            called.set(true);
            Ok(releases_json(Vec::new()))
        });

        assert_eq!(outcome, DiscoveryAttempt::NotDue);
        assert!(!called.get());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn corrupt_state_is_replaced_before_network() {
        let root = test_root("corrupt-state");
        let state_path = update_state_path(&root);
        fs::create_dir_all(state_path.parent().unwrap()).unwrap();
        fs::write(&state_path, b"{not-json").unwrap();

        let outcome = discover_once_with(&root, "1.0.0", 42_000, || {
            let persisted = load_update_state(&state_path, "1.0.0");
            assert_eq!(persisted.last_attempt_unix_secs, Some(42_000));
            Ok(releases_json(Vec::new()))
        });

        assert_eq!(outcome, DiscoveryAttempt::NoUpdate);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_atomic_state_replace_preserves_existing_state() {
        let root = test_root("state-atomic");
        let state_path = update_state_path(&root);
        fs::create_dir_all(state_path.parent().unwrap()).unwrap();
        fs::write(&state_path, b"existing-state").unwrap();

        let result = save_update_state_with(
            &state_path,
            &UpdateState {
                last_attempt_unix_secs: Some(77),
                pending: None,
            },
            |_, _| Err(io::Error::other("injected replace failure")),
        );

        assert!(result.is_err());
        assert_eq!(fs::read(&state_path).unwrap(), b"existing-state");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn persisted_pending_validation_rejects_invalid_metadata() {
        let root = test_root("invalid-pending");
        let state_path = update_state_path(&root);
        fs::create_dir_all(state_path.parent().unwrap()).unwrap();

        fs::write(
            &state_path,
            serde_json::to_vec(&json!({
                "last_attempt_unix_secs": 5,
                "pending": {
                    "version": "0.0.0",
                    "sha256": HASH_A,
                    "size": 100
                }
            }))
            .unwrap(),
        )
        .unwrap();

        assert_eq!(
            load_update_state(&state_path, "1.0.0"),
            UpdateState {
                last_attempt_unix_secs: Some(5),
                pending: None,
            }
        );

        fs::remove_dir_all(root).unwrap();
    }
}
