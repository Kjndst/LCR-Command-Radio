#[cfg(windows)]
mod windows_proof {
    use lcr_helper::{
        instance::{try_acquire_named_mutex, NamedMutexAcquire, NamedMutexGuard},
        uninstall::{
            cleanup_child_authority, launch_update_child_with_guards, parse_invocation_mode,
            prove_update_parent_exit_handoff, stage_cleanup_child, InvocationMode,
        },
    };
    use std::{
        env,
        ffi::OsStr,
        fs,
        path::{Path, PathBuf},
        process::{self, Child, Command, Stdio},
        thread,
        time::{Duration, Instant},
    };

    const NONCE: u64 = 0x51;
    const READY: &str = "u2-child-ready";
    const PROOF: &str = "u2-parent-exit-proof";
    const RELEASE_RUNTIME: &str = "u2-release-runtime";
    const RUNTIME_RELEASED: &str = "u2-runtime-released";
    const SPAWN_PROBE: &str = "u2-spawn-probe";
    const PROBE_READY: &str = "u2-probe-ready";
    const PROBE_RELEASE: &str = "u2-probe-release";
    const FAILURE: &str = "u2-failure.txt";

    pub fn run() -> Result<(), String> {
        let args = env::args_os().skip(1).collect::<Vec<_>>();

        match args.first().map(|arg| arg.as_os_str()) {
            None => orchestrate(),
            Some(arg) if arg == OsStr::new("--u2-guard-parent") => {
                if args.len() != 2 {
                    return Err("u2 guard parent requires temp directory".into());
                }
                parent_mode(Path::new(args[1].as_os_str()))
            }
            Some(arg) if arg == OsStr::new("--cleanup-update") => child_mode(args),
            Some(arg) if arg == OsStr::new("--u2-noninherit-probe") => {
                if args.len() != 2 {
                    return Err("u2 probe requires staging directory".into());
                }
                probe_mode(Path::new(args[1].as_os_str()))
            }
            Some(arg) => Err(format!("unexpected u2 guard invocation: {arg:?}")),
        }
    }

    fn mutex_name(parent_pid: u32, role: &str) -> String {
        format!(r"Local\LinhLanBang.LCR.Helper.U2.Guard.{parent_pid}.{role}")
    }

    fn marker(dir: &Path, name: &str) -> PathBuf {
        dir.join(name)
    }

    fn acquire(name: &str) -> Result<NamedMutexGuard, String> {
        match try_acquire_named_mutex(name).map_err(|e| format!("acquire {name}: {e}"))? {
            NamedMutexAcquire::Acquired(guard) => Ok(guard),
            NamedMutexAcquire::AlreadyExists => Err(format!("fresh mutex exists: {name}")),
        }
    }

    fn require_exists(name: &str, label: &str) -> Result<(), String> {
        match try_acquire_named_mutex(name).map_err(|e| format!("probe {label}: {e}"))? {
            NamedMutexAcquire::AlreadyExists => Ok(()),
            NamedMutexAcquire::Acquired(guard) => {
                drop(guard);
                Err(format!("{label} mutex unexpectedly acquirable"))
            }
        }
    }

    fn wait_acquirable(name: &str, label: &str) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            match try_acquire_named_mutex(name).map_err(|e| format!("probe {label}: {e}"))? {
                NamedMutexAcquire::Acquired(guard) => {
                    drop(guard);
                    return Ok(());
                }
                NamedMutexAcquire::AlreadyExists => {}
            }

            if Instant::now() >= deadline {
                return Err(format!("timed out waiting for {label} mutex release"));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn publish(path: &Path) -> Result<(), String> {
        fs::write(path, b"ok\n").map_err(|e| format!("write {}: {e}", path.display()))
    }

    fn wait_marker(path: &Path, failure: &Path, label: &str) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if path.is_file() {
                return Ok(());
            }
            if failure.is_file() {
                return Err(
                    fs::read_to_string(failure).unwrap_or_else(|_| "u2 child failed".into())
                );
            }
            if Instant::now() >= deadline {
                return Err(format!("timed out waiting for {label}: {}", path.display()));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_marker_while_parent_alive(
        path: &Path,
        failure: &Path,
        parent: &mut Child,
    ) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if path.is_file() {
                return Ok(());
            }
            if failure.is_file() {
                return Err(
                    fs::read_to_string(failure).unwrap_or_else(|_| "u2 child failed".into())
                );
            }
            if let Some(status) = parent
                .try_wait()
                .map_err(|e| format!("parent try_wait: {e}"))?
            {
                return Err(format!("u2 parent exited before child ready: {status}"));
            }
            if Instant::now() >= deadline {
                return Err("timed out waiting for u2 child ready".into());
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn orchestrate() -> Result<(), String> {
        let exe = env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
        let root = env::temp_dir().join(format!("lcr-helper-u2-handoff-{}", process::id()));
        let temp = root.join("Temp");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&temp).map_err(|e| format!("create root: {e}"))?;

        let mut parent = Command::new(&exe)
            .arg("--u2-guard-parent")
            .arg(&temp)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("spawn u2 parent: {e}"))?;

        let parent_pid = parent.id();
        let authority = cleanup_child_authority(&temp, parent_pid, NONCE)
            .ok_or_else(|| "derive u2 staged authority failed".to_owned())?;
        let ready = marker(&authority.staging_directory, READY);
        let proof = marker(&authority.staging_directory, PROOF);
        let release_runtime = marker(&authority.staging_directory, RELEASE_RUNTIME);
        let runtime_released = marker(&authority.staging_directory, RUNTIME_RELEASED);
        let spawn_probe = marker(&authority.staging_directory, SPAWN_PROBE);
        let probe_ready = marker(&authority.staging_directory, PROBE_READY);
        let probe_release = marker(&authority.staging_directory, PROBE_RELEASE);
        let failure = marker(&authority.staging_directory, FAILURE);

        wait_marker_while_parent_alive(&ready, &failure, &mut parent)?;

        let status = parent.wait().map_err(|e| format!("wait u2 parent: {e}"))?;
        if !status.success() {
            return Err(format!("u2 parent failed: {status}"));
        }

        wait_marker(&proof, &failure, "parent-exit proof")?;

        let install = mutex_name(parent_pid, "Install");
        let runtime = mutex_name(parent_pid, "Runtime");

        require_exists(&install, "install after parent exit")?;
        require_exists(&runtime, "runtime after parent exit")?;

        publish(&release_runtime)?;
        wait_marker(&runtime_released, &failure, "runtime release marker")?;

        wait_acquirable(&runtime, "runtime")?;
        require_exists(&install, "install while runtime released")?;

        publish(&spawn_probe)?;
        wait_marker(&probe_ready, &failure, "noninherit probe ready")?;

        // Child exits after launching the live probe. INSTALL must become
        // acquirable while that grandchild is still alive; otherwise the guard
        // HANDLE leaked into the new process.
        wait_acquirable(&install, "install")?;

        publish(&probe_release)?;

        println!("U2_GUARD_HANDOFF_CROSS_PROCESS_PROOF=PASS");
        println!("U2_GUARD_NONINHERIT_PROOF=PASS");

        let _ = fs::remove_dir_all(root);
        Ok(())
    }

    fn parent_mode(temp: &Path) -> Result<(), String> {
        let exe = env::current_exe().map_err(|e| format!("parent current_exe: {e}"))?;
        let pid = process::id();
        let install = acquire(&mutex_name(pid, "Install"))?;
        let runtime = acquire(&mutex_name(pid, "Runtime"))?;

        let authority =
            stage_cleanup_child(&exe, temp, pid, NONCE).map_err(|e| format!("stage child: {e}"))?;

        let child_pid = launch_update_child_with_guards(&authority, &install, &runtime)
            .map_err(|e| format!("launch update child: {e}"))?;
        if child_pid == 0 {
            return Err("update child returned zero pid".into());
        }

        let failure = marker(&authority.staging_directory, FAILURE);
        let ready = marker(&authority.staging_directory, READY);
        wait_marker(&ready, &failure, "child ready")?;

        Ok(())
    }

    fn child_mode(args: Vec<std::ffi::OsString>) -> Result<(), String> {
        let exe = env::current_exe().map_err(|e| format!("child current_exe: {e}"))?;
        let stage = exe
            .parent()
            .ok_or_else(|| "child has no stage directory".to_owned())?
            .to_path_buf();
        let failure = marker(&stage, FAILURE);

        match child_inner(args, &exe, &stage) {
            Ok(()) => Ok(()),
            Err(error) => {
                let _ = fs::write(&failure, format!("{error}\n"));
                Err(error)
            }
        }
    }

    fn child_inner(args: Vec<std::ffi::OsString>, exe: &Path, stage: &Path) -> Result<(), String> {
        let invocation = parse_invocation_mode(args.iter().map(|arg| arg.as_os_str()))
            .map_err(|_| "production parser rejected cleanup-update argv".to_owned())?;

        let (parent_pid, nonce, parent_handle, install_handle, runtime_handle) = match invocation {
            InvocationMode::CleanupUpdate {
                parent_process_id,
                nonce,
                parent_handle,
                install_handle,
                runtime_handle,
            } => (
                parent_process_id,
                nonce,
                parent_handle,
                install_handle,
                runtime_handle,
            ),
            _ => return Err("staged u2 child did not resolve CleanupUpdate".into()),
        };

        publish(&marker(stage, READY))?;

        let mut handoff = prove_update_parent_exit_handoff(
            exe,
            parent_pid,
            nonce,
            parent_handle,
            install_handle,
            runtime_handle,
        )
        .map_err(|e| format!("u2 parent-exit handoff: {e}"))?;

        publish(&marker(stage, PROOF))?;

        let failure = marker(stage, FAILURE);
        wait_marker(
            &marker(stage, RELEASE_RUNTIME),
            &failure,
            "release-runtime authority",
        )?;

        handoff.release_runtime();
        publish(&marker(stage, RUNTIME_RELEASED))?;

        wait_marker(
            &marker(stage, SPAWN_PROBE),
            &failure,
            "spawn-probe authority",
        )?;

        let grandchild = Command::new(exe)
            .arg("--u2-noninherit-probe")
            .arg(stage)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("spawn u2 noninherit probe: {e}"))?;

        if grandchild.id() == 0 {
            return Err("u2 noninherit probe returned zero pid".into());
        }

        Ok(())
    }

    fn probe_mode(stage: &Path) -> Result<(), String> {
        publish(&marker(stage, PROBE_READY))?;
        let failure = marker(stage, FAILURE);
        wait_marker(&marker(stage, PROBE_RELEASE), &failure, "probe release")
    }
}

#[cfg(windows)]
fn main() {
    if let Err(error) = windows_proof::run() {
        eprintln!("U2_GUARD_HANDOFF_CROSS_PROCESS_PROOF=FAIL {error}");
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {}
