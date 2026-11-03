use crate::{
    distribution::{
        executable_product_version, install_executable, startup_role, StartupRole,
        BROKER_LIFECYCLE_MUTEX_NAME, CURRENT_VERSION, INSTALL_MUTEX_NAME, RUNTIME_MUTEX_NAME,
    },
    instance::{try_acquire_named_mutex, NamedMutexAcquire, NamedMutexGuard},
    registration::register_installed_app_version,
    shortcut::{create_start_menu_shortcut, start_menu_shortcut_spec},
    updater::{prepare_update_apply_parent, UpdateApplyParentDisposition},
};
use std::{
    env,
    ffi::{OsStr, OsString},
    fs, io,
    path::{Path, PathBuf},
    process::Command,
};

const BROKER_FIRST_INSTALL_ARG: &str = "--lcr-first-install-broker";

fn first_install_error(message: impl Into<String>) -> io::Error {
    io::Error::other(message.into())
}

fn current_user_sid() -> io::Result<String> {
    crate::input_broker::current_user_sid_string()
        .ok_or_else(|| first_install_error("cannot resolve current Windows user SID"))
}

fn broker_task_contract(
    user_sid: &str,
    target: &crate::broker_install::ProtectedBrokerPath,
) -> io::Result<crate::task_authority::BrokerTaskContract> {
    crate::task_authority::BrokerTaskContract::new(user_sid, target)
        .map_err(|_| first_install_error("current Windows user SID is not a valid Task contract"))
}

fn broker_persistent_authority_is_exact(user_sid: &str) -> io::Result<bool> {
    use crate::task_authority::BrokerTaskVerification::Exact;

    let target = crate::broker_install::current_protected_broker_path()?;

    let contract = broker_task_contract(user_sid, &target)?;

    if !crate::broker_security::protected_broker_filesystem_security_matches(user_sid, &target)? {
        return Ok(false);
    }

    let task =
        crate::task_authority::verify_broker_task_via_scheduler(&contract).map_err(|error| {
            first_install_error(format!(
                "Task Scheduler readback failed: \
                     {error}"
            ))
        })?;

    Ok(task == Exact)
}

fn broker_pristine_first_install_is_available(user_sid: &str) -> io::Result<bool> {
    use crate::task_authority::BrokerTaskVerification::Absent;

    let target = crate::broker_install::current_protected_broker_path()?;

    if !crate::broker_security::protected_broker_first_install_namespace_absent(&target)? {
        return Ok(false);
    }

    let contract = broker_task_contract(user_sid, &target)?;

    let task =
        crate::task_authority::verify_broker_task_via_scheduler(&contract).map_err(|error| {
            first_install_error(format!(
                "Task Scheduler preflight failed: \
                     {error}"
            ))
        })?;

    Ok(task == Absent)
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BrokerLifecycleObservation {
    Absent {
        pristine_namespace: bool,
    },
    Current {
        signed_bundle_valid: bool,
        current_target_exact: bool,
    },
    Older {
        signed_bundle_valid: bool,
        older_authority_exact: bool,
    },
    Mismatch,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BrokerLifecycleDecision {
    FirstInstall,
    Ready,
    Maintain,
    FailClosed,
}

fn broker_lifecycle_decision(observation: BrokerLifecycleObservation) -> BrokerLifecycleDecision {
    match observation {
        BrokerLifecycleObservation::Absent {
            pristine_namespace: true,
        } => BrokerLifecycleDecision::FirstInstall,

        BrokerLifecycleObservation::Current {
            signed_bundle_valid: true,
            current_target_exact: true,
        } => BrokerLifecycleDecision::Ready,

        BrokerLifecycleObservation::Older {
            signed_bundle_valid: true,
            older_authority_exact: true,
        } => BrokerLifecycleDecision::Maintain,

        BrokerLifecycleObservation::Absent {
            pristine_namespace: false,
        }
        | BrokerLifecycleObservation::Current { .. }
        | BrokerLifecycleObservation::Older { .. }
        | BrokerLifecycleObservation::Mismatch => BrokerLifecycleDecision::FailClosed,
    }
}

fn broker_lifecycle_guard_from_acquire(acquire: NamedMutexAcquire) -> Option<NamedMutexGuard> {
    match acquire {
        NamedMutexAcquire::Acquired(guard) => Some(guard),
        NamedMutexAcquire::AlreadyExists => None,
    }
}

struct VerifiedCurrentBrokerBundle {
    payload: Vec<u8>,
    signed_manifest: Vec<u8>,
    artifact: crate::broker_artifact::VerifiedBrokerArtifact,
}

fn load_verified_current_broker_bundle() -> io::Result<VerifiedCurrentBrokerBundle> {
    let payload = crate::broker_install::embedded_broker_payload()?;
    let signed_manifest = crate::broker_install::embedded_broker_signed_manifest()?;

    let artifact = crate::broker_artifact::verify_current_broker_bundle(&signed_manifest, &payload)
        .map_err(|error| {
            first_install_error(format!(
                "signed current Broker bundle verification failed: {error:?}"
            ))
        })?;

    Ok(VerifiedCurrentBrokerBundle {
        payload,
        signed_manifest,
        artifact,
    })
}

fn recognize_broker_task(
    user_sid: &str,
) -> io::Result<crate::task_authority::RecognizedBrokerTaskVerification> {
    crate::task_authority::recognize_broker_task_via_scheduler(user_sid).map_err(|error| {
        first_install_error(format!(
            "Task Scheduler Broker authority recognition failed: {error}"
        ))
    })
}

fn current_authority_is_exact_with_bundle(
    user_sid: &str,
    authority: &crate::task_authority::RecognizedBrokerTaskAuthority,
    bundle: &VerifiedCurrentBrokerBundle,
) -> io::Result<bool> {
    use crate::task_authority::BrokerTaskVersionRelation::Current;

    if authority.relation() != Current || authority.version() != CURRENT_VERSION {
        return Ok(false);
    }

    if bundle.artifact.target_version() != CURRENT_VERSION {
        return Ok(false);
    }

    let target = crate::broker_install::current_protected_broker_path()?;

    if authority.broker_path() != target.as_path() {
        return Ok(false);
    }

    crate::input_broker::verified_protected_broker_target_is_exact(
        user_sid,
        &target,
        &bundle.artifact,
    )
}

fn recognized_current_persistent_authority_is_exact(
    user_sid: &str,
    bundle: &VerifiedCurrentBrokerBundle,
) -> io::Result<bool> {
    use crate::task_authority::RecognizedBrokerTaskVerification::Exact;

    match recognize_broker_task(user_sid)? {
        Exact(authority) => current_authority_is_exact_with_bundle(user_sid, &authority, bundle),
        _ => Ok(false),
    }
}

fn older_authority_is_exact(
    user_sid: &str,
    authority: &crate::task_authority::RecognizedBrokerTaskAuthority,
) -> io::Result<bool> {
    use crate::task_authority::BrokerTaskVersionRelation::Older;

    if authority.relation() != Older {
        return Ok(false);
    }

    let target = crate::broker_install::protected_broker_path_for_version(authority.version())?;

    if authority.broker_path() != target.as_path() {
        return Ok(false);
    }

    if !crate::broker_security::protected_broker_filesystem_security_matches(user_sid, &target)? {
        return Ok(false);
    }

    Ok(executable_product_version(target.as_path())? == authority.version())
}

fn maintenance_postcheck_allows_ready(
    _maintenance_reached_committed: bool,
    current_persistent_authority_exact: bool,
) -> bool {
    // COMMITTED is an acknowledgement, not persistent authority. A pipe failure
    // after A3B3 durably migrated the Task is therefore successful iff the
    // independent post-maintenance persistent readback is exact CURRENT.
    current_persistent_authority_exact
}

fn maintenance_then_persistent_readback<M, P>(
    maintenance: M,
    persistent_readback: P,
) -> io::Result<bool>
where
    M: FnOnce() -> io::Result<()>,
    P: FnOnce() -> io::Result<bool>,
{
    let maintenance_reached_committed = maintenance().is_ok();

    // Deliberately runs after both maintenance Ok and Err. The maintenance
    // transport acknowledgement is never substituted for persistent authority.
    let current_persistent_authority_exact = persistent_readback()?;

    Ok(maintenance_postcheck_allows_ready(
        maintenance_reached_committed,
        current_persistent_authority_exact,
    ))
}

fn reconcile_broker_lifecycle(current_executable: &Path) -> io::Result<()> {
    use crate::task_authority::{
        BrokerTaskVersionRelation::{Current, Older},
        RecognizedBrokerTaskVerification::{Absent, Exact, Mismatch},
    };

    let user_sid = current_user_sid()?;

    match recognize_broker_task(&user_sid)? {
        Absent => {
            let pristine_namespace = broker_pristine_first_install_is_available(&user_sid)?;

            if broker_lifecycle_decision(BrokerLifecycleObservation::Absent { pristine_namespace })
                != BrokerLifecycleDecision::FirstInstall
            {
                return Err(first_install_error(
                    "Broker Task is absent but protected Broker state is not pristine",
                ));
            }

            // Preserve the existing first-install transaction byte-for-byte in
            // authority semantics. Signed-bundle enforcement belongs to the
            // Current/Older lifecycle paths, not pristine first install.
            ensure_broker_first_install_ready(current_executable)
        }

        Mismatch => {
            debug_assert_eq!(
                broker_lifecycle_decision(BrokerLifecycleObservation::Mismatch),
                BrokerLifecycleDecision::FailClosed
            );

            Err(first_install_error(
                "fixed Broker Task is mismatched, future, or untrusted",
            ))
        }

        Exact(authority) => match authority.relation() {
            Current => {
                let bundle = load_verified_current_broker_bundle()?;

                let current_target_exact =
                    current_authority_is_exact_with_bundle(&user_sid, &authority, &bundle)?;

                match broker_lifecycle_decision(BrokerLifecycleObservation::Current {
                    signed_bundle_valid: true,
                    current_target_exact,
                }) {
                    BrokerLifecycleDecision::Ready => Ok(()),

                    _ => Err(first_install_error(
                        "current Broker Task exists but signed protected Broker authority is not exact",
                    )),
                }
            }

            Older => {
                let older_authority_exact = older_authority_is_exact(&user_sid, &authority)?;

                if !older_authority_exact {
                    return Err(first_install_error(
                        "recognized older Broker Task does not have exact protected filesystem authority",
                    ));
                }

                // Both RCDATA resources are loaded once by this startup
                // reconciliation. The existing maintenance client performs its
                // own defensive verification again before demand-running the
                // privileged Broker.
                let bundle = load_verified_current_broker_bundle()?;

                if broker_lifecycle_decision(BrokerLifecycleObservation::Older {
                    signed_bundle_valid: true,
                    older_authority_exact,
                }) != BrokerLifecycleDecision::Maintain
                {
                    return Err(first_install_error(
                        "older Broker authority is not eligible for signed maintenance",
                    ));
                }

                let ready = maintenance_then_persistent_readback(
                    || {
                        crate::input_broker::run_broker_maintenance_client(
                            authority.contract(),
                            CURRENT_VERSION,
                            &bundle.signed_manifest,
                            &bundle.payload,
                        )
                    },
                    || recognized_current_persistent_authority_is_exact(&user_sid, &bundle),
                )?;

                if ready {
                    Ok(())
                } else {
                    Err(first_install_error(
                        "Broker maintenance did not leave exact current persistent authority",
                    ))
                }
            }
        },
    }
}

fn rollback_broker_first_install(
    directory_target: &crate::broker_install::ProtectedBrokerPath,
    broker_target: &crate::broker_install::ProtectedBrokerPath,
    broker_created: bool,
    task_created: bool,
) -> io::Result<()> {
    let mut first_error: Option<String> = None;

    if task_created {
        if let Err(error) =
            crate::task_authority::delete_broker_task_and_verify_absent_via_scheduler()
        {
            first_error = Some(format!("Task rollback failed: {error}"));
        }
    }

    if broker_created {
        match fs::remove_file(broker_target.as_path()) {
            Ok(()) => {}

            Err(error) if error.kind() == io::ErrorKind::NotFound => {}

            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(format!(
                        "Broker file rollback failed: \
                         {error}"
                    ));
                }
            }
        }
    }

    if let Err(error) = crate::broker_security::rollback_protected_broker_directories_first_install(
        directory_target,
    ) {
        if first_error.is_none() {
            first_error = Some(format!(
                "Broker directory rollback failed: \
                 {error}"
            ));
        }
    }

    match first_error {
        Some(error) => Err(first_install_error(error)),

        None => Ok(()),
    }
}

fn fail_with_broker_first_install_rollback(
    step_error: io::Error,
    directory_target: &crate::broker_install::ProtectedBrokerPath,
    broker_target: &crate::broker_install::ProtectedBrokerPath,
    broker_created: bool,
    task_created: bool,
) -> io::Error {
    match rollback_broker_first_install(
        directory_target,
        broker_target,
        broker_created,
        task_created,
    ) {
        Ok(()) => step_error,

        Err(rollback_error) => first_install_error(format!(
            "{step_error}; rollback incomplete: \
                 {rollback_error}"
        )),
    }
}

fn run_broker_first_install_transaction(expected_user_sid: &str) -> io::Result<()> {
    use crate::task_authority::BrokerTaskVerification::{Absent, Exact};

    let actual_user_sid = current_user_sid()?;

    // R1 supports the same local Administrators account
    // on both sides of UAC.
    if actual_user_sid != expected_user_sid {
        return Err(first_install_error(
            "elevated first-install identity differs \
             from the requesting user",
        ));
    }

    let candidate = crate::broker_install::current_protected_broker_path()?;

    let contract = broker_task_contract(expected_user_sid, &candidate)?;

    let task_before =
        crate::task_authority::verify_broker_task_via_scheduler(&contract).map_err(|error| {
            first_install_error(format!(
                "Task Scheduler preflight failed: \
                     {error}"
            ))
        })?;

    if task_before != Absent {
        return Err(first_install_error("fixed Broker task already exists"));
    }

    if !crate::broker_security::protected_broker_first_install_namespace_absent(&candidate)? {
        return Err(first_install_error(
            "protected Broker namespace already exists",
        ));
    }

    let directory_target =
        crate::broker_security::establish_protected_broker_directories(expected_user_sid)?;

    if directory_target != candidate {
        return Err(fail_with_broker_first_install_rollback(
            first_install_error("protected Broker path changed during install"),
            &directory_target,
            &directory_target,
            false,
            false,
        ));
    }

    let broker_target = match crate::broker_install::install_embedded_broker_first_install() {
        Ok(target) => target,

        Err(error) => {
            return Err(fail_with_broker_first_install_rollback(
                error,
                &directory_target,
                &directory_target,
                false,
                false,
            ));
        }
    };

    let broker_created = true;

    if broker_target != directory_target {
        return Err(fail_with_broker_first_install_rollback(
            first_install_error(
                "installed Broker path differs from \
                     protected directory path",
            ),
            &directory_target,
            &broker_target,
            broker_created,
            false,
        ));
    }

    if let Err(error) = crate::broker_security::protect_and_verify_installed_broker(
        expected_user_sid,
        &broker_target,
    ) {
        return Err(fail_with_broker_first_install_rollback(
            error,
            &directory_target,
            &broker_target,
            broker_created,
            false,
        ));
    }

    if let Err(error) =
        crate::task_authority::create_broker_task_first_install_via_scheduler(&contract)
    {
        return Err(fail_with_broker_first_install_rollback(
            first_install_error(format!(
                "create-only Broker task registration failed: \
                     {error}"
            )),
            &directory_target,
            &broker_target,
            broker_created,
            false,
        ));
    }

    let task_created = true;

    let task_after = match crate::task_authority::verify_broker_task_via_scheduler(&contract) {
        Ok(task) => task,

        Err(error) => {
            return Err(fail_with_broker_first_install_rollback(
                first_install_error(format!(
                    "post-create Task readback failed: \
                             {error}"
                )),
                &directory_target,
                &broker_target,
                broker_created,
                task_created,
            ));
        }
    };

    if task_after != Exact {
        return Err(fail_with_broker_first_install_rollback(
            first_install_error(
                "created Broker task does not match \
                     persistent authority",
            ),
            &directory_target,
            &broker_target,
            broker_created,
            task_created,
        ));
    }

    // Once persistent state is owned, both mismatch AND
    // readback failure must enter rollback.
    match crate::broker_security::protected_broker_filesystem_security_matches(
        expected_user_sid,
        &broker_target,
    ) {
        Ok(true) => {}

        Ok(false) => {
            return Err(fail_with_broker_first_install_rollback(
                first_install_error(
                    "final Broker filesystem authority \
                         does not match",
                ),
                &directory_target,
                &broker_target,
                broker_created,
                task_created,
            ));
        }

        Err(error) => {
            return Err(fail_with_broker_first_install_rollback(
                error,
                &directory_target,
                &broker_target,
                broker_created,
                task_created,
            ));
        }
    }

    Ok(())
}

/// Hidden elevated first-install child.
///
/// Accepts exactly one SID. No caller-selected path,
/// task, payload, URL, Discord credential or PTT state.
pub fn maybe_run_broker_first_install_from_args<I>(mut args: I) -> Option<i32>
where
    I: Iterator<Item = OsString>,
{
    let first = args.next()?;

    if first != OsStr::new(BROKER_FIRST_INSTALL_ARG) {
        return None;
    }

    let Some(user_sid) = args.next() else {
        return Some(2);
    };

    if args.next().is_some() {
        return Some(2);
    }

    let Ok(user_sid) = user_sid.into_string() else {
        return Some(2);
    };

    Some(if run_broker_first_install_transaction(&user_sid).is_ok() {
        0
    } else {
        1
    })
}

fn ensure_broker_first_install_ready(current_executable: &Path) -> io::Result<()> {
    let user_sid = current_user_sid()?;

    if broker_persistent_authority_is_exact(&user_sid)? {
        return Ok(());
    }

    if !broker_pristine_first_install_is_available(&user_sid)? {
        return Err(first_install_error(
            "Broker authority is partial or conflicting; \
             first install will not adopt or overwrite it",
        ));
    }

    let parameters = format!("{BROKER_FIRST_INSTALL_ARG} {user_sid}");

    let exit_code =
        crate::input_broker::runas_and_wait(current_executable, OsStr::new(&parameters))?;

    if exit_code != 0 {
        return Err(first_install_error(format!(
            "elevated Broker first install exited with \
             code {exit_code}"
        )));
    }

    if broker_persistent_authority_is_exact(&user_sid)? {
        Ok(())
    } else {
        Err(first_install_error(
            "elevated first install returned success \
             without exact persistent authority",
        ))
    }
}
pub enum StartupDisposition {
    Exit,
    ActivateExisting,
    Run(NamedMutexGuard),
}
fn prepare_installed_startup_with<G, A, R, U, Q>(
    current_executable: &Path,
    acquire_broker_lifecycle: A,
    reconcile: R,
    prepare_update: U,
    acquire_runtime_after_ready: Q,
) -> StartupDisposition
where
    A: FnOnce() -> io::Result<Option<G>>,
    R: FnOnce(&Path) -> io::Result<()>,
    U: FnOnce(&Path) -> UpdateApplyParentDisposition,
    Q: FnOnce() -> StartupDisposition,
{
    let broker_lifecycle_guard = match acquire_broker_lifecycle() {
        Ok(Some(guard)) => guard,
        Ok(None) | Err(_) => return StartupDisposition::Exit,
    };

    if reconcile(current_executable).is_err() {
        return StartupDisposition::Exit;
    }

    // This explicit drop is the serialization barrier: updater U2 and ordinary
    // runtime are not entered while Broker lifecycle ownership remains held.
    drop(broker_lifecycle_guard);

    match prepare_update(current_executable) {
        UpdateApplyParentDisposition::NotApplicable => acquire_runtime_after_ready(),
        UpdateApplyParentDisposition::Exit => StartupDisposition::Exit,
        UpdateApplyParentDisposition::ContinueRuntime(runtime_guard) => {
            StartupDisposition::Run(runtime_guard)
        }
    }
}

fn existing_runtime_observed(acquire: io::Result<NamedMutexAcquire>) -> io::Result<bool> {
    match acquire? {
        NamedMutexAcquire::Acquired(guard) => {
            drop(guard);
            Ok(false)
        }
        NamedMutexAcquire::AlreadyExists => Ok(true),
    }
}

fn prepare_installed_startup(current_executable: &Path) -> StartupDisposition {
    match existing_runtime_observed(try_acquire_named_mutex(RUNTIME_MUTEX_NAME)) {
        Ok(true) => return StartupDisposition::ActivateExisting,
        Ok(false) => {}
        Err(_) => return StartupDisposition::Exit,
    }

    prepare_installed_startup_with(
        current_executable,
        || {
            try_acquire_named_mutex(BROKER_LIFECYCLE_MUTEX_NAME)
                .map(broker_lifecycle_guard_from_acquire)
        },
        reconcile_broker_lifecycle,
        prepare_update_apply_parent,
        acquire_runtime,
    )
}

/// Resolves whether this process is a downloaded bootstrap or the canonical
/// installed Helper.
///
/// All startup failures fail closed by exiting before the Win32 Helper runtime
/// is created. The downloaded bootstrap never runs the PTT/control runtime.
pub fn prepare_startup() -> StartupDisposition {
    let current_executable = match env::current_exe() {
        Ok(path) => path,
        Err(_) => return StartupDisposition::Exit,
    };

    let local_app_data = match env::var_os("LOCALAPPDATA") {
        Some(value) => PathBuf::from(value),
        None => return StartupDisposition::Exit,
    };

    match startup_role(&current_executable, &local_app_data) {
        StartupRole::Bootstrap { target } => {
            bootstrap(&current_executable, &target, &local_app_data)
        }
        StartupRole::Installed => prepare_installed_startup(&current_executable),
    }
}

fn bootstrap(
    current_executable: &Path,
    target: &Path,
    local_app_data: &Path,
) -> StartupDisposition {
    let _install_guard = match try_acquire_named_mutex(INSTALL_MUTEX_NAME) {
        Ok(NamedMutexAcquire::Acquired(guard)) => guard,
        Ok(NamedMutexAcquire::AlreadyExists) | Err(_) => return StartupDisposition::Exit,
    };

    if ensure_bootstrap_target(current_executable, target).is_err() {
        return StartupDisposition::Exit;
    }

    let app_data = match env::var_os("APPDATA") {
        Some(value) => PathBuf::from(value),
        None => return StartupDisposition::Exit,
    };

    // Bind shell metadata to the executable that is actually installed. This is
    // especially important when a downloaded bootstrap finds an existing
    // canonical target and therefore must not advertise the bootstrap's version.
    let installed_version = match executable_product_version(target) {
        Ok(version) => version,
        Err(_) => return StartupDisposition::Exit,
    };

    let shortcut = start_menu_shortcut_spec(&app_data, local_app_data);
    if create_start_menu_shortcut(&shortcut).is_err() {
        return StartupDisposition::Exit;
    }

    if register_installed_app_version(local_app_data, &installed_version).is_err() {
        return StartupDisposition::Exit;
    }

    // The installed executable is a Windows GUI binary. Dropping Child does not
    // terminate it; the child owns its independent runtime mutex.
    let _ = Command::new(target).spawn();

    StartupDisposition::Exit
}

fn runtime_disposition_from_acquire(acquire: io::Result<NamedMutexAcquire>) -> StartupDisposition {
    match acquire {
        Ok(NamedMutexAcquire::Acquired(guard)) => StartupDisposition::Run(guard),
        Ok(NamedMutexAcquire::AlreadyExists) => StartupDisposition::ActivateExisting,
        Err(_) => StartupDisposition::Exit,
    }
}

fn acquire_runtime() -> StartupDisposition {
    runtime_disposition_from_acquire(try_acquire_named_mutex(RUNTIME_MUTEX_NAME))
}

/// First-install bridge only.
///
/// An existing canonical Helper is never replaced by an arbitrary downloaded
/// bootstrap. Versioned replacement belongs to the verified updater path.
fn ensure_bootstrap_target(current_executable: &Path, target: &Path) -> io::Result<()> {
    if target.is_file() {
        return Ok(());
    }

    install_executable(current_executable, target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::{Cell, RefCell},
        fs,
        rc::Rc,
    };

    fn test_dir(case: &str) -> PathBuf {
        let directory =
            env::temp_dir().join(format!("lcr-helper-startup-{case}-{}", std::process::id()));

        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();

        directory
    }

    #[test]
    fn existing_runtime_requests_activation_instead_of_silent_exit() {
        let disposition = runtime_disposition_from_acquire(Ok(NamedMutexAcquire::AlreadyExists));

        assert!(matches!(disposition, StartupDisposition::ActivateExisting));
    }

    #[test]
    fn runtime_acquire_error_still_fails_closed() {
        let disposition = runtime_disposition_from_acquire(Err(io::Error::other(
            "synthetic runtime mutex failure",
        )));

        assert!(matches!(disposition, StartupDisposition::Exit));
    }

    #[test]
    fn existing_runtime_fast_path_is_detected_before_lifecycle_work() {
        for _ in 0..10_000 {
            assert!(existing_runtime_observed(Ok(NamedMutexAcquire::AlreadyExists)).unwrap());
        }
    }

    #[test]
    fn existing_runtime_fast_path_error_fails_closed() {
        assert!(existing_runtime_observed(Err(io::Error::other(
            "synthetic runtime observation failure",
        )))
        .is_err());
    }

    #[test]
    fn missing_bootstrap_target_is_installed() {
        let directory = test_dir("missing");
        let source = directory.join("downloaded.exe");
        let target = directory.join("Programs").join("LCR").join("LCR.exe");

        fs::write(&source, b"bridge-candidate").unwrap();

        ensure_bootstrap_target(&source, &target).unwrap();

        assert_eq!(fs::read(&target).unwrap(), b"bridge-candidate");

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn existing_canonical_target_is_never_overwritten_by_bootstrap() {
        let directory = test_dir("existing");
        let source = directory.join("downloaded.exe");
        let target_dir = directory.join("Programs").join("LCR");
        let target = target_dir.join("LCR.exe");

        fs::create_dir_all(&target_dir).unwrap();
        fs::write(&source, b"arbitrary-downloaded-copy").unwrap();
        fs::write(&target, b"trusted-canonical-copy").unwrap();

        ensure_bootstrap_target(&source, &target).unwrap();

        assert_eq!(fs::read(&target).unwrap(), b"trusted-canonical-copy");

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn lifecycle_reducer_allows_only_pristine_current_or_exact_older_paths() {
        assert_eq!(
            broker_lifecycle_decision(BrokerLifecycleObservation::Absent {
                pristine_namespace: true,
            }),
            BrokerLifecycleDecision::FirstInstall
        );

        assert_eq!(
            broker_lifecycle_decision(BrokerLifecycleObservation::Absent {
                pristine_namespace: false,
            }),
            BrokerLifecycleDecision::FailClosed
        );

        assert_eq!(
            broker_lifecycle_decision(BrokerLifecycleObservation::Current {
                signed_bundle_valid: true,
                current_target_exact: true,
            }),
            BrokerLifecycleDecision::Ready
        );

        assert_eq!(
            broker_lifecycle_decision(BrokerLifecycleObservation::Current {
                signed_bundle_valid: false,
                current_target_exact: true,
            }),
            BrokerLifecycleDecision::FailClosed
        );

        assert_eq!(
            broker_lifecycle_decision(BrokerLifecycleObservation::Current {
                signed_bundle_valid: true,
                current_target_exact: false,
            }),
            BrokerLifecycleDecision::FailClosed
        );

        assert_eq!(
            broker_lifecycle_decision(BrokerLifecycleObservation::Older {
                signed_bundle_valid: true,
                older_authority_exact: true,
            }),
            BrokerLifecycleDecision::Maintain
        );

        assert_eq!(
            broker_lifecycle_decision(BrokerLifecycleObservation::Older {
                signed_bundle_valid: false,
                older_authority_exact: true,
            }),
            BrokerLifecycleDecision::FailClosed
        );

        assert_eq!(
            broker_lifecycle_decision(BrokerLifecycleObservation::Older {
                signed_bundle_valid: true,
                older_authority_exact: false,
            }),
            BrokerLifecycleDecision::FailClosed
        );

        assert_eq!(
            broker_lifecycle_decision(BrokerLifecycleObservation::Mismatch),
            BrokerLifecycleDecision::FailClosed
        );
    }

    #[test]
    fn lost_committed_ack_is_resolved_only_from_persistent_current_readback() {
        assert!(maintenance_postcheck_allows_ready(true, true));
        assert!(maintenance_postcheck_allows_ready(false, true));

        assert!(!maintenance_postcheck_allows_ready(true, false));
        assert!(!maintenance_postcheck_allows_ready(false, false));
    }

    #[test]
    fn lifecycle_mutex_contention_never_yields_a_reconciliation_guard() {
        assert!(broker_lifecycle_guard_from_acquire(NamedMutexAcquire::AlreadyExists).is_none());
    }

    #[test]
    fn maintenance_attempt_is_single_and_readback_runs_after_success() {
        let maintenance_calls = Cell::new(0u32);
        let readback_calls = Cell::new(0u32);

        let ready = maintenance_then_persistent_readback(
            || {
                maintenance_calls.set(maintenance_calls.get() + 1);
                Ok(())
            },
            || {
                readback_calls.set(readback_calls.get() + 1);
                Ok(true)
            },
        )
        .unwrap();

        assert!(ready);
        assert_eq!(maintenance_calls.get(), 1);
        assert_eq!(readback_calls.get(), 1);
    }

    #[test]
    fn lost_committed_error_still_performs_one_persistent_readback() {
        let maintenance_calls = Cell::new(0u32);
        let readback_calls = Cell::new(0u32);

        let ready = maintenance_then_persistent_readback(
            || {
                maintenance_calls.set(maintenance_calls.get() + 1);
                Err(io::Error::other("COMMITTED acknowledgement lost"))
            },
            || {
                readback_calls.set(readback_calls.get() + 1);
                Ok(true)
            },
        )
        .unwrap();

        assert!(ready);
        assert_eq!(maintenance_calls.get(), 1);
        assert_eq!(readback_calls.get(), 1);
    }

    #[test]
    fn failed_maintenance_without_current_persistent_state_fails_closed() {
        let maintenance_calls = Cell::new(0u32);
        let readback_calls = Cell::new(0u32);

        let ready = maintenance_then_persistent_readback(
            || {
                maintenance_calls.set(maintenance_calls.get() + 1);
                Err(io::Error::other("maintenance failed before commit"))
            },
            || {
                readback_calls.set(readback_calls.get() + 1);
                Ok(false)
            },
        )
        .unwrap();

        assert!(!ready);
        assert_eq!(maintenance_calls.get(), 1);
        assert_eq!(readback_calls.get(), 1);
    }

    #[test]
    fn installed_startup_contention_performs_zero_reconciliation_work() {
        let reconciles = Cell::new(0u32);
        let updater_calls = Cell::new(0u32);
        let runtime_calls = Cell::new(0u32);

        let disposition = prepare_installed_startup_with(
            Path::new("LCR.exe"),
            || Ok::<Option<()>, io::Error>(None),
            |_| {
                reconciles.set(reconciles.get() + 1);
                Ok(())
            },
            |_| {
                updater_calls.set(updater_calls.get() + 1);
                UpdateApplyParentDisposition::NotApplicable
            },
            || {
                runtime_calls.set(runtime_calls.get() + 1);
                StartupDisposition::Exit
            },
        );

        assert!(matches!(disposition, StartupDisposition::Exit));
        assert_eq!(reconciles.get(), 0);
        assert_eq!(updater_calls.get(), 0);
        assert_eq!(runtime_calls.get(), 0);
    }

    #[test]
    fn installed_startup_never_reaches_updater_or_runtime_when_reconcile_fails() {
        let updater_calls = Cell::new(0u32);
        let runtime_calls = Cell::new(0u32);

        let disposition = prepare_installed_startup_with(
            Path::new("LCR.exe"),
            || Ok::<Option<()>, io::Error>(Some(())),
            |_| Err(io::Error::other("Broker lifecycle not ready")),
            |_| {
                updater_calls.set(updater_calls.get() + 1);
                UpdateApplyParentDisposition::NotApplicable
            },
            || {
                runtime_calls.set(runtime_calls.get() + 1);
                StartupDisposition::Exit
            },
        );

        assert!(matches!(disposition, StartupDisposition::Exit));
        assert_eq!(updater_calls.get(), 0);
        assert_eq!(runtime_calls.get(), 0);
    }

    #[test]
    fn lifecycle_guard_drops_before_updater_and_runtime() {
        struct DropProbe {
            events: Rc<RefCell<Vec<&'static str>>>,
        }

        impl Drop for DropProbe {
            fn drop(&mut self) {
                self.events.borrow_mut().push("drop-lifecycle");
            }
        }

        let events = Rc::new(RefCell::new(Vec::new()));

        let acquire_events = Rc::clone(&events);
        let reconcile_events = Rc::clone(&events);
        let updater_events = Rc::clone(&events);
        let runtime_events = Rc::clone(&events);

        let disposition = prepare_installed_startup_with(
            Path::new("LCR.exe"),
            move || {
                acquire_events.borrow_mut().push("acquire-lifecycle");

                Ok::<Option<DropProbe>, io::Error>(Some(DropProbe {
                    events: Rc::clone(&acquire_events),
                }))
            },
            move |_| {
                reconcile_events.borrow_mut().push("reconcile");
                Ok(())
            },
            move |_| {
                updater_events.borrow_mut().push("updater");
                UpdateApplyParentDisposition::NotApplicable
            },
            move || {
                runtime_events.borrow_mut().push("runtime");
                StartupDisposition::Exit
            },
        );

        assert!(matches!(disposition, StartupDisposition::Exit));

        assert_eq!(
            &*events.borrow(),
            &[
                "acquire-lifecycle",
                "reconcile",
                "drop-lifecycle",
                "updater",
                "runtime",
            ]
        );
    }

    #[test]
    fn pristine_is_the_only_reducer_state_that_requests_first_install() {
        let decisions = [
            broker_lifecycle_decision(BrokerLifecycleObservation::Absent {
                pristine_namespace: true,
            }),
            broker_lifecycle_decision(BrokerLifecycleObservation::Absent {
                pristine_namespace: false,
            }),
            broker_lifecycle_decision(BrokerLifecycleObservation::Current {
                signed_bundle_valid: true,
                current_target_exact: true,
            }),
            broker_lifecycle_decision(BrokerLifecycleObservation::Current {
                signed_bundle_valid: false,
                current_target_exact: true,
            }),
            broker_lifecycle_decision(BrokerLifecycleObservation::Current {
                signed_bundle_valid: true,
                current_target_exact: false,
            }),
            broker_lifecycle_decision(BrokerLifecycleObservation::Older {
                signed_bundle_valid: true,
                older_authority_exact: true,
            }),
            broker_lifecycle_decision(BrokerLifecycleObservation::Older {
                signed_bundle_valid: false,
                older_authority_exact: true,
            }),
            broker_lifecycle_decision(BrokerLifecycleObservation::Older {
                signed_bundle_valid: true,
                older_authority_exact: false,
            }),
            broker_lifecycle_decision(BrokerLifecycleObservation::Mismatch),
        ];

        assert_eq!(
            decisions
                .into_iter()
                .filter(|decision| *decision == BrokerLifecycleDecision::FirstInstall)
                .count(),
            1
        );
    }
}
