#![cfg_attr(windows, windows_subsystem = "windows")]
#![cfg_attr(not(windows), allow(dead_code))]

#[cfg(windows)]
fn main() {
    if let Some(exit_code) =
        lcr_helper::startup::maybe_run_broker_first_install_from_args(std::env::args_os().skip(1))
    {
        if exit_code != 0 {
            std::process::exit(exit_code);
        }

        return;
    }
    if let Some(exit_code) =
        lcr_helper::input_broker::maybe_run_from_args(std::env::args_os().skip(1))
    {
        if exit_code != 0 {
            std::process::exit(exit_code);
        }
        return;
    }

    use lcr_helper::{
        startup::{prepare_startup, StartupDisposition},
        uninstall::{
            parse_invocation_mode, run_clean_uninstall_parent, run_cleanup_uninstall_child,
            CleanupChildOutcome, InvocationMode, UninstallParentOutcome,
        },
        updater::{run_update_apply_child, UpdateApplyChildOutcome},
    };

    let invocation = match parse_invocation_mode(std::env::args_os().skip(1)) {
        Ok(invocation) => invocation,
        Err(_) => std::process::exit(2),
    };

    let exit_code = match invocation {
        InvocationMode::Normal => {
            match prepare_startup() {
                StartupDisposition::Exit => {}
                StartupDisposition::ActivateExisting => {
                    if lcr_helper::windows::activate_existing_runtime().is_err() {
                        std::process::exit(1);
                    }
                }
                StartupDisposition::Run(_runtime_guard) => {
                    lcr_helper::updater::spawn_startup_update_check();
                    lcr_helper::windows::run();
                }
            }
            0
        }
        InvocationMode::Uninstall => match run_clean_uninstall_parent() {
            Ok(UninstallParentOutcome::Cancelled | UninstallParentOutcome::CleanupLaunched) => 0,
            Err(_) => 1,
        },
        InvocationMode::CleanupUninstall {
            parent_process_id,
            nonce,
            parent_handle,
        } => match run_cleanup_uninstall_child(parent_process_id, nonce, parent_handle) {
            Ok(CleanupChildOutcome::Complete) => 0,
            Ok(CleanupChildOutcome::Incomplete) | Err(_) => 1,
        },
        InvocationMode::CleanupUpdate {
            parent_process_id,
            nonce,
            parent_handle,
            install_handle,
            runtime_handle,
        } => match run_update_apply_child(
            parent_process_id,
            nonce,
            parent_handle,
            install_handle,
            runtime_handle,
        ) {
            Ok(
                UpdateApplyChildOutcome::Committed | UpdateApplyChildOutcome::RecoveredOldRuntime,
            ) => 0,
            Ok(UpdateApplyChildOutcome::Incomplete) | Err(_) => 1,
        },
    };

    if exit_code != 0 {
        std::process::exit(exit_code);
    }
}

#[cfg(not(windows))]
fn main() {}
