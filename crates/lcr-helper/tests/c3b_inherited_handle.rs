#[cfg(windows)]
mod windows_proof {
    use lcr_helper::{
        instance::{try_acquire_named_mutex, NamedMutexAcquire, NamedMutexGuard},
        uninstall::{
            cleanup_child_authority, launch_cleanup_child, launch_cleanup_child_with_guards,
            parse_invocation_mode, prove_cleanup_parent_exit, stage_cleanup_child, InvocationMode,
        },
    };
    use std::{
        env,
        ffi::{OsStr, OsString},
        fs,
        io::Read as _,
        path::{Path, PathBuf},
        process::{self, Child, Command, Stdio},
        thread,
        time::{Duration, Instant},
    };

    const HOLD_NONCE: u64 = 0x3d;
    const EXITED_NONCE: u64 = 0x3e;
    const C4D2_GUARD_NONCE: u64 = 0x3f;

    const READY_MARKER: &str = "c3b-child-ready";
    const GO_MARKER: &str = "c3b-proof-go";
    const CALL_MARKER: &str = "c3b-proof-call";
    const PROOF_MARKER: &str = "c3b-parent-exit-proof.ok";
    const FAILURE_MARKER: &str = "c3b-child-failure.txt";

    const C4D2_GUARD_READY_MARKER: &str = "c4d2-guard-child-ready";
    const C4D2_GUARD_PROOF_MARKER: &str = "c4d2-guard-parent-exit-proof.ok";
    const C4D2_GUARD_RELEASE_MARKER: &str = "c4d2-guard-child-release";

    pub fn run() -> Result<(), String> {
        let args = env::args_os().skip(1).collect::<Vec<_>>();

        match args.first().map(|argument| argument.as_os_str()) {
            None => orchestrate(),
            Some(argument) if argument == OsStr::new("--c3b-parent-hold") => {
                if args.len() != 2 {
                    return Err("c3b hold parent requires exactly one temp directory".to_owned());
                }

                parent_mode(Path::new(args[1].as_os_str()), HOLD_NONCE, true)
            }
            Some(argument) if argument == OsStr::new("--c3b-parent-exit") => {
                if args.len() != 2 {
                    return Err("c3b exit parent requires exactly one temp directory".to_owned());
                }

                parent_mode(Path::new(args[1].as_os_str()), EXITED_NONCE, false)
            }
            Some(argument) if argument == OsStr::new("--c4d2-guard-parent") => {
                if args.len() != 2 {
                    return Err("c4d2 guard parent requires exactly one temp directory".to_owned());
                }

                c4d2_guard_parent_mode(Path::new(args[1].as_os_str()))
            }
            Some(argument) if argument == OsStr::new("--cleanup-uninstall") => {
                cleanup_child_mode(args)
            }
            Some(argument) => Err(format!("unexpected c3b invocation: {argument:?}")),
        }
    }

    fn orchestrate() -> Result<(), String> {
        let current_executable =
            env::current_exe().map_err(|error| format!("current_exe failed: {error}"))?;

        let root =
            env::temp_dir().join(format!("lcr-helper-c3b-inherited-handle-{}", process::id()));
        let temp_dir = root.join("Temp");

        let _ = fs::remove_dir_all(&root);

        fs::create_dir_all(&temp_dir)
            .map_err(|error| format!("create c3b temp root failed: {error}"))?;

        let result = run_parent_alive_scenario(&current_executable, &temp_dir)
            .and_then(|()| run_parent_already_exited_scenario(&current_executable, &temp_dir))
            .and_then(|()| run_c4d2_guard_inheritance_scenario(&current_executable, &temp_dir));

        if result.is_ok() {
            println!("C3B_REAL_INHERITED_HANDLE_CROSS_PROCESS_PROOF=PASS");
            println!("C4D2_GUARD_MUTEX_CROSS_PROCESS_PROOF=PASS");
        }

        // Test-owned temporary evidence only. A still-exiting staged child may
        // transiently keep its image open, so cleanup is deliberately best-effort.
        let _ = fs::remove_dir_all(&root);

        result
    }

    fn run_parent_alive_scenario(current_executable: &Path, temp_dir: &Path) -> Result<(), String> {
        let mut parent = Command::new(current_executable)
            .arg("--c3b-parent-hold")
            .arg(temp_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| format!("spawn held c3b parent failed: {error}"))?;

        let parent_process_id = parent.id();

        let authority = cleanup_child_authority(temp_dir, parent_process_id, HOLD_NONCE)
            .ok_or_else(|| "derive held-parent cleanup authority failed".to_owned())?;

        let ready = marker(&authority.staging_directory, READY_MARKER);
        let go = marker(&authority.staging_directory, GO_MARKER);
        let call = marker(&authority.staging_directory, CALL_MARKER);
        let proof = marker(&authority.staging_directory, PROOF_MARKER);
        let failure = marker(&authority.staging_directory, FAILURE_MARKER);

        wait_for_marker_while_parent_alive(&ready, &failure, &mut parent, "held child ready")?;

        if proof.exists() {
            return Err("parent-exit proof appeared before held child was released".to_owned());
        }

        publish_marker(&go, "go\n")?;

        wait_for_marker_while_parent_alive(&call, &failure, &mut parent, "held child proof call")?;

        observe_no_proof_while_parent_alive(&proof, &failure, &mut parent)?;

        // This is the only release point. Until this pipe closes, parent_mode
        // remains alive and production prove_cleanup_parent_exit must not return.
        drop(parent.stdin.take());

        let status = parent
            .wait()
            .map_err(|error| format!("wait held c3b parent failed: {error}"))?;

        if !status.success() {
            return Err(format!("held c3b parent failed with status {status}"));
        }

        wait_for_marker(&proof, &failure, "held parent-exit proof")?;

        verify_proof_marker(&proof, parent_process_id, HOLD_NONCE)?;

        Ok(())
    }

    fn run_parent_already_exited_scenario(
        current_executable: &Path,
        temp_dir: &Path,
    ) -> Result<(), String> {
        let mut parent = Command::new(current_executable)
            .arg("--c3b-parent-exit")
            .arg(temp_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| format!("spawn already-exiting c3b parent failed: {error}"))?;

        let parent_process_id = parent.id();

        let authority = cleanup_child_authority(temp_dir, parent_process_id, EXITED_NONCE)
            .ok_or_else(|| "derive exited-parent cleanup authority failed".to_owned())?;

        let ready = marker(&authority.staging_directory, READY_MARKER);
        let go = marker(&authority.staging_directory, GO_MARKER);
        let call = marker(&authority.staging_directory, CALL_MARKER);
        let proof = marker(&authority.staging_directory, PROOF_MARKER);
        let failure = marker(&authority.staging_directory, FAILURE_MARKER);

        let status = parent
            .wait()
            .map_err(|error| format!("wait already-exiting c3b parent failed: {error}"))?;

        if !status.success() {
            return Err(format!(
                "already-exiting c3b parent failed with status {status}"
            ));
        }

        // Child cannot call production proof until GO exists. Therefore this
        // wait establishes that the parent is already terminated first.
        wait_for_marker(&ready, &failure, "already-exited child ready")?;

        if proof.exists() {
            return Err("already-exited proof appeared before GO authority".to_owned());
        }

        publish_marker(&go, "go\n")?;

        wait_for_marker(&call, &failure, "already-exited child proof call")?;

        wait_for_marker(&proof, &failure, "already-exited parent proof")?;

        verify_proof_marker(&proof, parent_process_id, EXITED_NONCE)?;

        Ok(())
    }

    fn c4d2_guard_mutex_name(parent_process_id: u32, role: &str) -> String {
        format!(r"Local\LinhLanBang.LCR.Helper.C4D2.Guard.{parent_process_id}.{role}")
    }

    fn acquire_exact_named_mutex(name: &str) -> Result<NamedMutexGuard, String> {
        match try_acquire_named_mutex(name)
            .map_err(|error| format!("acquire {name} failed: {error}"))?
        {
            NamedMutexAcquire::Acquired(guard) => Ok(guard),
            NamedMutexAcquire::AlreadyExists => {
                Err(format!("fresh C4D2 guard mutex already exists: {name}"))
            }
        }
    }

    fn require_named_mutex_exists(name: &str, label: &str) -> Result<(), String> {
        match try_acquire_named_mutex(name)
            .map_err(|error| format!("probe {label} mutex failed: {error}"))?
        {
            NamedMutexAcquire::AlreadyExists => Ok(()),
            NamedMutexAcquire::Acquired(guard) => {
                drop(guard);
                Err(format!(
                    "{label} mutex disappeared after uninstall parent exit while cleanup child was alive"
                ))
            }
        }
    }

    fn wait_for_named_mutex_release(name: &str, label: &str) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(20);

        loop {
            match try_acquire_named_mutex(name)
                .map_err(|error| format!("probe released {label} mutex failed: {error}"))?
            {
                NamedMutexAcquire::Acquired(guard) => {
                    drop(guard);
                    return Ok(());
                }
                NamedMutexAcquire::AlreadyExists => {}
            }

            if Instant::now() >= deadline {
                return Err(format!(
                    "timed out waiting for inherited {label} mutex to disappear"
                ));
            }

            thread::sleep(Duration::from_millis(10));
        }
    }

    fn run_c4d2_guard_inheritance_scenario(
        current_executable: &Path,
        temp_dir: &Path,
    ) -> Result<(), String> {
        let mut parent = Command::new(current_executable)
            .arg("--c4d2-guard-parent")
            .arg(temp_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| format!("spawn C4D2 guard parent failed: {error}"))?;

        let parent_process_id = parent.id();
        let authority = cleanup_child_authority(temp_dir, parent_process_id, C4D2_GUARD_NONCE)
            .ok_or_else(|| "derive C4D2 guard cleanup authority failed".to_owned())?;

        let ready = marker(&authority.staging_directory, C4D2_GUARD_READY_MARKER);
        let proof = marker(&authority.staging_directory, C4D2_GUARD_PROOF_MARKER);
        let release = marker(&authority.staging_directory, C4D2_GUARD_RELEASE_MARKER);
        let failure = marker(&authority.staging_directory, FAILURE_MARKER);

        wait_for_marker_while_parent_alive(
            &ready,
            &failure,
            &mut parent,
            "C4D2 guard child ready",
        )?;

        let status = parent
            .wait()
            .map_err(|error| format!("wait C4D2 guard parent failed: {error}"))?;

        if !status.success() {
            return Err(format!("C4D2 guard parent failed with status {status}"));
        }

        // The child publishes this only after production
        // prove_cleanup_parent_exit observed the exact parent process terminate.
        wait_for_marker(&proof, &failure, "C4D2 guard child parent-exit proof")?;

        let install_name = c4d2_guard_mutex_name(parent_process_id, "Install");
        let runtime_name = c4d2_guard_mutex_name(parent_process_id, "Runtime");
        let broker_lifecycle_name = c4d2_guard_mutex_name(parent_process_id, "BrokerLifecycle");

        // Parent is already dead. The only reason these named objects can still
        // exist is that the production cleanup-child CreateProcessW inherited
        // their exact HANDLEs.
        require_named_mutex_exists(&install_name, "install")?;
        require_named_mutex_exists(&runtime_name, "runtime")?;
        require_named_mutex_exists(&broker_lifecycle_name, "broker lifecycle")?;

        publish_marker(&release, "release\n")?;

        // Child returns only after RELEASE; process teardown then closes the
        // otherwise-unowned inherited HANDLEs. All three names must disappear.
        wait_for_named_mutex_release(&install_name, "install")?;
        wait_for_named_mutex_release(&runtime_name, "runtime")?;
        wait_for_named_mutex_release(&broker_lifecycle_name, "broker lifecycle")?;

        Ok(())
    }

    fn c4d2_guard_parent_mode(temp_dir: &Path) -> Result<(), String> {
        let current_executable = env::current_exe()
            .map_err(|error| format!("C4D2 guard parent current_exe failed: {error}"))?;

        let parent_process_id = process::id();
        let install_name = c4d2_guard_mutex_name(parent_process_id, "Install");
        let runtime_name = c4d2_guard_mutex_name(parent_process_id, "Runtime");
        let broker_lifecycle_name = c4d2_guard_mutex_name(parent_process_id, "BrokerLifecycle");

        let install_guard = acquire_exact_named_mutex(&install_name)?;
        let runtime_guard = acquire_exact_named_mutex(&runtime_name)?;
        let broker_lifecycle_guard = acquire_exact_named_mutex(&broker_lifecycle_name)?;

        let authority = stage_cleanup_child(
            &current_executable,
            temp_dir,
            parent_process_id,
            C4D2_GUARD_NONCE,
        )
        .map_err(|error| format!("stage C4D2 guard cleanup child failed: {error}"))?;

        let child_process_id = launch_cleanup_child_with_guards(
            &authority,
            &install_guard,
            &runtime_guard,
            &broker_lifecycle_guard,
        )
        .map_err(|error| format!("launch C4D2 guarded cleanup child failed: {error}"))?;

        if child_process_id == 0 {
            return Err("C4D2 guarded cleanup launch returned zero child PID".to_owned());
        }

        let ready = marker(&authority.staging_directory, C4D2_GUARD_READY_MARKER);
        let failure = marker(&authority.staging_directory, FAILURE_MARKER);

        // Do not let the parent exit until the child has parsed the production
        // cleanup argv and is demonstrably alive with its inherited handles.
        wait_for_marker(&ready, &failure, "C4D2 guard child ready")?;

        Ok(())
    }
    fn parent_mode(temp_dir: &Path, nonce: u64, hold_open: bool) -> Result<(), String> {
        let current_executable =
            env::current_exe().map_err(|error| format!("parent current_exe failed: {error}"))?;

        let authority = stage_cleanup_child(&current_executable, temp_dir, process::id(), nonce)
            .map_err(|error| format!("stage c3b cleanup child failed: {error}"))?;

        let child_process_id = launch_cleanup_child(&authority)
            .map_err(|error| format!("launch c3b cleanup child failed: {error}"))?;

        if child_process_id == 0 {
            return Err("native cleanup launch returned zero child PID".to_owned());
        }

        if hold_open {
            let mut stdin = std::io::stdin().lock();
            let mut sink = Vec::new();

            stdin
                .read_to_end(&mut sink)
                .map_err(|error| format!("held parent stdin wait failed: {error}"))?;
        }

        Ok(())
    }

    fn cleanup_child_mode(args: Vec<OsString>) -> Result<(), String> {
        let current_executable = env::current_exe()
            .map_err(|error| format!("staged child current_exe failed: {error}"))?;

        let staging_directory = current_executable
            .parent()
            .ok_or_else(|| "staged child has no staging directory".to_owned())?
            .to_path_buf();

        match cleanup_child_inner(args, &current_executable, &staging_directory) {
            Ok(()) => Ok(()),
            Err(error) => {
                let failure = marker(&staging_directory, FAILURE_MARKER);

                let _ = fs::write(&failure, format!("{error}\n"));

                Err(error)
            }
        }
    }

    fn cleanup_child_inner(
        args: Vec<OsString>,
        current_executable: &Path,
        staging_directory: &Path,
    ) -> Result<(), String> {
        let invocation = parse_invocation_mode(args.iter().map(|argument| argument.as_os_str()))
            .map_err(|_| "staged child rejected production cleanup argv".to_owned())?;

        let (parent_process_id, nonce, parent_handle) = match invocation {
            InvocationMode::CleanupUninstall {
                parent_process_id,
                nonce,
                parent_handle,
            } => (parent_process_id, nonce, parent_handle),
            _ => return Err("staged child did not resolve CleanupUninstall mode".to_owned()),
        };

        if nonce == C4D2_GUARD_NONCE {
            return c4d2_guard_cleanup_child_inner(
                current_executable,
                staging_directory,
                parent_process_id,
                nonce,
                parent_handle,
            );
        }

        let ready = marker(staging_directory, READY_MARKER);
        let go = marker(staging_directory, GO_MARKER);
        let call = marker(staging_directory, CALL_MARKER);
        let proof_marker = marker(staging_directory, PROOF_MARKER);

        // Production argv has already been parsed. GO is test-only scheduling
        // authority so the orchestrator controls whether parent is alive or
        // already exited when the production proof call begins.
        publish_marker(&ready, "ready\n")?;
        wait_for_go(&go)?;

        // This marker is deliberately the final test operation before entering
        // production prove_cleanup_parent_exit.
        publish_marker(&call, "call\n")?;

        let proof =
            prove_cleanup_parent_exit(current_executable, parent_process_id, nonce, parent_handle)
                .map_err(|error| format!("staged child parent-exit proof failed: {error}"))?;

        if proof.authority().parent_process_id != parent_process_id
            || proof.authority().nonce != nonce
            || proof.authority().executable_path != current_executable
        {
            return Err(
                "parent-exit proof authority drifted from staged child identity".to_owned(),
            );
        }

        let body = proof_marker_body(parent_process_id, nonce);

        // Publication is impossible until production returned an opaque
        // CleanupParentExitProof.
        publish_marker(&proof_marker, &body)?;

        Ok(())
    }

    fn c4d2_guard_cleanup_child_inner(
        current_executable: &Path,
        staging_directory: &Path,
        parent_process_id: u32,
        nonce: u64,
        parent_handle: lcr_helper::uninstall::CleanupParentHandleValue,
    ) -> Result<(), String> {
        let ready = marker(staging_directory, C4D2_GUARD_READY_MARKER);
        let proof_marker = marker(staging_directory, C4D2_GUARD_PROOF_MARKER);
        let release = marker(staging_directory, C4D2_GUARD_RELEASE_MARKER);
        let failure = marker(staging_directory, FAILURE_MARKER);

        publish_marker(&ready, "ready\n")?;

        let proof =
            prove_cleanup_parent_exit(current_executable, parent_process_id, nonce, parent_handle)
                .map_err(|error| format!("C4D2 guard child parent-exit proof failed: {error}"))?;

        if proof.authority().parent_process_id != parent_process_id
            || proof.authority().nonce != nonce
            || proof.authority().executable_path != current_executable
        {
            return Err("C4D2 guard parent-exit proof authority drifted".to_owned());
        }

        publish_marker(&proof_marker, "proof\n")?;

        // Stay alive after the exact parent process is gone so the orchestrator
        // can prove that only the child's inherited mutex HANDLEs keep both
        // named kernel objects present.
        wait_for_marker(&release, &failure, "C4D2 guard child release")?;

        Ok(())
    }
    fn wait_for_go(go: &Path) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(20);

        loop {
            if go.is_file() {
                return Ok(());
            }

            if Instant::now() >= deadline {
                return Err("timed out waiting for c3b GO authority".to_owned());
            }

            thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_for_marker(target: &Path, failure: &Path, label: &str) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(20);

        loop {
            if target.is_file() {
                return Ok(());
            }

            if failure.is_file() {
                return Err(read_failure_marker(failure));
            }

            if Instant::now() >= deadline {
                return Err(format!(
                    "timed out waiting for {label}: {}",
                    target.display()
                ));
            }

            thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_for_marker_while_parent_alive(
        target: &Path,
        failure: &Path,
        parent: &mut Child,
        label: &str,
    ) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(20);

        loop {
            if target.is_file() {
                return Ok(());
            }

            if failure.is_file() {
                return Err(read_failure_marker(failure));
            }

            if let Some(status) = parent
                .try_wait()
                .map_err(|error| format!("query held c3b parent failed: {error}"))?
            {
                return Err(format!("held parent exited before {label}: {status}"));
            }

            if Instant::now() >= deadline {
                return Err(format!(
                    "timed out waiting for {label}: {}",
                    target.display()
                ));
            }

            thread::sleep(Duration::from_millis(10));
        }
    }

    fn observe_no_proof_while_parent_alive(
        proof: &Path,
        failure: &Path,
        parent: &mut Child,
    ) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(1);

        loop {
            if proof.is_file() {
                return Err(
                    "production parent-exit proof returned while parent was alive".to_owned(),
                );
            }

            if failure.is_file() {
                return Err(read_failure_marker(failure));
            }

            if let Some(status) = parent.try_wait().map_err(|error| {
                format!("query held c3b parent during block proof failed: {error}")
            })? {
                return Err(format!(
                    "held parent exited before explicit release: {status}"
                ));
            }

            if Instant::now() >= deadline {
                return Ok(());
            }

            thread::sleep(Duration::from_millis(10));
        }
    }

    fn publish_marker(target: &Path, body: &str) -> Result<(), String> {
        let pending = target.with_extension("pending");

        fs::write(&pending, body).map_err(|error| {
            format!(
                "write pending c3b marker {} failed: {error}",
                pending.display()
            )
        })?;

        fs::rename(&pending, target)
            .map_err(|error| format!("publish c3b marker {} failed: {error}", target.display()))?;

        Ok(())
    }

    fn verify_proof_marker(
        marker_path: &Path,
        parent_process_id: u32,
        nonce: u64,
    ) -> Result<(), String> {
        let observed = fs::read_to_string(marker_path)
            .map_err(|error| format!("read c3b proof marker failed: {error}"))?;

        let expected = proof_marker_body(parent_process_id, nonce);

        if observed != expected {
            return Err(format!(
                "unexpected c3b proof marker: expected {expected:?}, got {observed:?}"
            ));
        }

        Ok(())
    }

    fn proof_marker_body(parent_process_id: u32, nonce: u64) -> String {
        format!(
            "C3B_PARENT_EXIT_PROOF=PASS\n\
             PARENT_PROCESS_ID={parent_process_id}\n\
             NONCE={nonce:016x}\n"
        )
    }

    fn marker(staging_directory: &Path, name: &str) -> PathBuf {
        staging_directory.join(name)
    }

    fn read_failure_marker(path: &Path) -> String {
        fs::read_to_string(path)
            .unwrap_or_else(|error| format!("c3b child failure marker unreadable: {error}"))
    }
}

#[cfg(windows)]
fn main() {
    if let Err(error) = windows_proof::run() {
        eprintln!("C3B_REAL_INHERITED_HANDLE_CROSS_PROCESS_PROOF=FAIL {error}");
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {}
