// Helper-internal fail-closed seams intentionally collapse transport/storage
// failures to unit errors; callers expose typed user-facing state where needed.
#![allow(clippy::result_unit_err)]

//! Pure Rust Helper V2 control core. Windows UI/input wiring is deliberately P4.

// P2B4A3A1 intentionally stages the private signed-artifact verifier before
// A3A2/A3B wire its production caller. Remove this allowance when that wiring lands.
#[cfg_attr(not(test), allow(dead_code))]
mod broker_artifact;
#[cfg(windows)]
pub mod broker_install;
#[cfg(windows)]
pub mod broker_security;
pub mod config;
pub mod distribution;
pub mod endpoint;
pub mod input;
#[cfg(windows)]
pub mod input_broker;
#[cfg(windows)]
pub mod instance;
pub mod localization;
pub mod protocol;
pub mod reconciler;
#[cfg(windows)]
pub mod registration;
pub mod runtime;
#[cfg(windows)]
pub mod shortcut;
#[cfg(windows)]
pub mod startup;
#[cfg(windows)]
pub mod task_authority;
pub mod transport;
#[cfg(windows)]
pub mod uninstall;
pub mod updater;
#[cfg(windows)]
pub mod winhttp;
pub mod worker;

#[cfg(windows)]
pub mod windows;

pub use config::Config;
pub use reconciler::{Action, Desired, Reconciler};
