#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
fn main() {
    use std::ffi::OsString;

    // Reuse the already-reviewed broker argument parser without entering
    // Helper startup/updater/runtime. R1C2P1 establishes only the dedicated
    // artifact; R1C2P2 will own its protected persistent launch authority.
    let args =
        std::iter::once(OsString::from("--lcr-input-broker")).chain(std::env::args_os().skip(1));

    let exit_code = lcr_helper::input_broker::maybe_run_from_args(args).unwrap_or(2);

    if exit_code != 0 {
        std::process::exit(exit_code);
    }
}

#[cfg(not(windows))]
fn main() {}
