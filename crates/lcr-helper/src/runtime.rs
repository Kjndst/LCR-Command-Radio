use crate::input::RadioKey;
#[cfg(any(windows, test))]
use crate::{config::Config, input::VK_ESCAPE};

#[cfg(windows)]
use crate::{
    config::{FileStore, Store},
    endpoint,
    protocol::PairReply,
    winhttp::WinHttp,
    worker::{ControlHandle, ControlRunner},
};
#[cfg(windows)]
use std::{
    path::{Path, PathBuf},
    sync::mpsc::{sync_channel, Receiver, TryRecvError},
    thread::{self, JoinHandle},
    time::Duration,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PairStartError {
    AlreadyPaired,
    Busy,
    EmptyCode,
    SpawnFailed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PairPoll {
    Idle,
    Pending,
    Paired,
    Invalidated,
    Denied,
    Transient,
    ActivationFailed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettingsError {
    Busy,
    ForbiddenKey,
    PersistFailed,
    ActivationFailed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnpairError {
    Busy,
    PersistFailed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SettingsView {
    pub radio_key: RadioKey,
    pub ptt_sound: bool,
}

#[cfg(any(windows, test))]
fn radio_key_from_config(config: &Config) -> RadioKey {
    if config.radio_key_kind.eq_ignore_ascii_case("keyboard") && config.radio_key_code != 0 {
        RadioKey::Keyboard(config.radio_key_code)
    } else if config.radio_key.eq_ignore_ascii_case("Mouse4") {
        RadioKey::Mouse4
    } else {
        RadioKey::Mouse5
    }
}

#[cfg(any(windows, test))]
fn apply_radio_key(config: &mut Config, key: &RadioKey) -> Result<(), SettingsError> {
    match key {
        RadioKey::Keyboard(code) if *code == VK_ESCAPE => {
            return Err(SettingsError::ForbiddenKey);
        }
        RadioKey::Keyboard(code) if *code == 0 => {
            return Err(SettingsError::ForbiddenKey);
        }
        RadioKey::Keyboard(code) => {
            config.radio_key = "Keyboard".into();
            config.radio_key_kind = "keyboard".into();
            config.radio_key_code = *code;
        }
        RadioKey::Mouse4 => {
            config.radio_key = "Mouse4".into();
            config.radio_key_kind = "mouse".into();
            config.radio_key_code = 0;
        }
        RadioKey::Mouse5 => {
            config.radio_key = "Mouse5".into();
            config.radio_key_kind = "mouse".into();
            config.radio_key_code = 0;
        }
    }

    Ok(())
}

#[cfg(any(windows, test))]
fn clear_local_pairing(config: &mut Config) {
    // This is deliberately local-only. Server revocation remains a separate
    // authority operation; unpairing must preserve all user-owned settings.
    config.device_token = None;
    config.pending_pair = false;
}

#[cfg(windows)]
enum PairTaskResult {
    Activated(ControlRunner),
    Invalidated,
    ActivationFailed,
    Denied,
    Transient,
}

#[cfg(windows)]
struct PairTask {
    receiver: Receiver<PairTaskResult>,
    thread: Option<JoinHandle<()>>,
}

#[cfg(windows)]
impl PairTask {
    fn join(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(windows)]
impl Drop for PairTask {
    fn drop(&mut self) {
        self.join();
    }
}

#[cfg(windows)]
pub struct HelperRuntime {
    config_path: PathBuf,
    endpoint_url: String,
    runner: Option<ControlRunner>,
    pair_task: Option<PairTask>,
    awaiting_pair_ack: bool,
}

#[cfg(windows)]
impl HelperRuntime {
    pub fn start_from_appdata(appdata: &str) -> Result<Self, ()> {
        let config_path = FileStore::appdata_path(appdata);
        let endpoint_url = endpoint::resolve_current_executable().map_err(|_| ())?.url;
        let runner = activate_from_store(&config_path, &endpoint_url, false)?;

        Ok(Self {
            config_path,
            endpoint_url,
            runner,
            pair_task: None,
            awaiting_pair_ack: false,
        })
    }

    pub fn control_handle(&self) -> Option<ControlHandle> {
        self.runner
            .as_ref()
            .filter(|runner| runner.is_authority_valid() && runner.startup_closed_acknowledged())
            .map(ControlRunner::handle)
    }

    /// Completes the runner's bounded CLOSED-before-stop lifecycle for the
    /// native tray Exit path. No UI code owns transport shutdown directly.
    pub fn shutdown(&mut self) {
        self.stop_control_runner();
    }

    pub fn is_paired(&self) -> bool {
        self.runner.as_ref().is_some_and(|runner| {
            runner.is_authority_valid() && runner.startup_closed_acknowledged()
        })
    }

    pub fn is_pairing(&self) -> bool {
        self.pair_task.is_some() || self.awaiting_pair_ack
    }

    pub fn settings(&self) -> SettingsView {
        let store = FileStore::new(self.config_path.clone());
        let config = store.load();

        SettingsView {
            radio_key: radio_key_from_config(&config),
            ptt_sound: config.ptt_sound,
        }
    }

    pub fn update_radio_key(&mut self, key: RadioKey) -> Result<(), SettingsError> {
        if self.is_pairing() {
            return Err(SettingsError::Busy);
        }

        if matches!(key, RadioKey::Keyboard(VK_ESCAPE) | RadioKey::Keyboard(0)) {
            return Err(SettingsError::ForbiddenKey);
        }

        self.stop_control_runner();

        let mut store = FileStore::new(self.config_path.clone());
        let mut config = store.load();

        apply_radio_key(&mut config, &key)?;

        if store.save(&config).is_err() {
            self.runner = activate_from_store(&self.config_path, &self.endpoint_url, false)
                .ok()
                .flatten();
            return Err(SettingsError::PersistFailed);
        }

        self.reactivate_after_settings()
    }

    pub fn update_ptt_sound(&mut self, enabled: bool) -> Result<(), SettingsError> {
        if self.is_pairing() {
            return Err(SettingsError::Busy);
        }

        // Config persistence is shared with the control worker. Reuse the
        // existing bounded CLOSED-before-stop lifecycle so this rare settings
        // mutation cannot race a generation write or later be overwritten by
        // stale in-memory worker config.
        let had_runner = self.runner.is_some();
        self.stop_control_runner();

        let mut store = FileStore::new(self.config_path.clone());
        let mut config = store.load();

        if config.ptt_sound != enabled {
            config.ptt_sound = enabled;

            if store.save(&config).is_err() {
                if had_runner {
                    self.runner = activate_from_store(&self.config_path, &self.endpoint_url, false)
                        .ok()
                        .flatten();
                }
                return Err(SettingsError::PersistFailed);
            }
        }

        if had_runner {
            self.reactivate_after_settings()
        } else {
            Ok(())
        }
    }

    /// Removes local pairing authority after the control runner has completed
    /// its bounded CLOSED-before-stop lifecycle. It never contacts a server.
    pub fn unpair_local(&mut self) -> Result<(), UnpairError> {
        if self.is_pairing() {
            return Err(UnpairError::Busy);
        }

        self.stop_control_runner();

        let mut store = FileStore::new(self.config_path.clone());
        let mut config = store.load();
        clear_local_pairing(&mut config);
        if store.save(&config).is_err() {
            // Persistence failed, so the on-disk credential remains authoritative.
            // Best-effort reactivate it rather than leaving this process in a
            // half-unpaired state until restart.
            self.runner = activate_from_store(&self.config_path, &self.endpoint_url, false)
                .ok()
                .flatten();
            return Err(UnpairError::PersistFailed);
        }

        Ok(())
    }

    fn reap_invalid_runner(&mut self) -> bool {
        let invalid = self
            .runner
            .as_ref()
            .is_some_and(|runner| !runner.is_authority_valid());

        if invalid {
            self.stop_control_runner();
            self.awaiting_pair_ack = false;
        }

        invalid
    }

    fn stop_control_runner(&mut self) {
        if let Some(runner) = self.runner.take() {
            runner.shutdown();
        }
    }

    fn reactivate_after_settings(&mut self) -> Result<(), SettingsError> {
        match activate_from_store(&self.config_path, &self.endpoint_url, false) {
            Ok(runner) => {
                self.runner = runner;
                Ok(())
            }
            Err(()) => {
                self.runner = None;
                Err(SettingsError::ActivationFailed)
            }
        }
    }

    pub fn begin_pair(&mut self, code: &str) -> Result<(), PairStartError> {
        if self.is_pairing() {
            return Err(PairStartError::Busy);
        }
        if self
            .runner
            .as_ref()
            .is_some_and(ControlRunner::is_authority_valid)
        {
            return Err(PairStartError::AlreadyPaired);
        }

        self.reap_invalid_runner();

        let code = code.trim();
        if code.is_empty() {
            return Err(PairStartError::EmptyCode);
        }

        let code = code.to_owned();
        let config_path = self.config_path.clone();
        let endpoint_url = self.endpoint_url.clone();
        let (sender, receiver) = sync_channel(1);

        let thread = thread::Builder::new()
            .name("lcr-pair".into())
            .spawn(move || {
                let result = run_pair_task(config_path, endpoint_url, code);
                let _ = sender.send(result);
            })
            .map_err(|_| PairStartError::SpawnFailed)?;

        self.pair_task = Some(PairTask {
            receiver,
            thread: Some(thread),
        });

        Ok(())
    }

    pub fn poll_pair(&mut self) -> PairPoll {
        if self.reap_invalid_runner() {
            return PairPoll::Invalidated;
        }
        if self.awaiting_pair_ack {
            if self
                .runner
                .as_ref()
                .is_some_and(ControlRunner::startup_closed_acknowledged)
            {
                self.awaiting_pair_ack = false;
                return PairPoll::Paired;
            }
            return PairPoll::Pending;
        }

        let result = match self.pair_task.as_ref() {
            None => return PairPoll::Idle,
            Some(task) => match task.receiver.try_recv() {
                Ok(result) => result,
                Err(TryRecvError::Empty) => return PairPoll::Pending,
                Err(TryRecvError::Disconnected) => PairTaskResult::Transient,
            },
        };

        if let Some(mut task) = self.pair_task.take() {
            task.join();
        }

        match result {
            PairTaskResult::Activated(runner) => {
                let ready = runner.startup_closed_acknowledged();
                self.runner = Some(runner);
                self.awaiting_pair_ack = !ready;
                if ready {
                    PairPoll::Paired
                } else {
                    PairPoll::Pending
                }
            }
            PairTaskResult::Invalidated => PairPoll::Invalidated,
            PairTaskResult::ActivationFailed => PairPoll::ActivationFailed,
            PairTaskResult::Denied => PairPoll::Denied,
            PairTaskResult::Transient => PairPoll::Transient,
        }
    }
}

#[cfg(windows)]
fn run_pair_task(config_path: PathBuf, endpoint_url: String, code: String) -> PairTaskResult {
    let mut store = FileStore::new(config_path.clone());
    let mut config = store.load();

    let (host, port) = match endpoint::https_authority(&endpoint_url) {
        Some(authority) => authority,
        None => return PairTaskResult::Transient,
    };
    let host = host.to_owned();

    let transport = match WinHttp::open(&host, port) {
        Ok(transport) => transport,
        Err(()) => return PairTaskResult::Transient,
    };

    match transport.pair(&code) {
        PairReply::Accepted {
            token,
            heartbeat_hint_ms,
        } => {
            // Pairing authority is persisted before ControlRunner can perform
            // any state action. The one-time code itself is never persisted.
            config.device_token = Some(token);
            config.pending_pair = true;
            config.heartbeat_hint_ms = heartbeat_hint_ms;

            if store.save(&config).is_err() {
                return PairTaskResult::Transient;
            }

            let mut runner = match activate_from_store(&config_path, &endpoint_url, true) {
                Ok(Some(runner)) => runner,
                Ok(None) | Err(()) => return PairTaskResult::ActivationFailed,
            };

            // SAFETY INVARIANT: Pair is not user-visible READY until the control
            // worker has completed its startup CLOSED attempt. Do not move this
            // readiness publication back ahead of worker startup: live 2026-10-13
            // reproduced a late-pair window where local PTT looked ready before
            // Unit -> Command relay could accept OPEN. This wait stays on the
            // existing lcr-pair background thread, never the Win32 UI thread.
            if !runner.wait_startup_attempt() {
                return PairTaskResult::ActivationFailed;
            }
            if !runner.is_authority_valid() {
                return PairTaskResult::Invalidated;
            }
            // Lost ACK is not a failed pair: transfer the worker to the
            // runtime while it remains Pending. The existing worker retries
            // unchanged CLOSED until its server authority is acknowledged.
            PairTaskResult::Activated(runner)
        }
        PairReply::Denied => PairTaskResult::Denied,
        PairReply::Transient => PairTaskResult::Transient,
    }
}

#[cfg(windows)]
fn activate_from_store(
    config_path: &Path,
    endpoint_url: &str,
    freshly_delivered: bool,
) -> Result<Option<ControlRunner>, ()> {
    let store = FileStore::new(config_path.to_path_buf());
    let config = store.load();

    if config.device_token.is_none() {
        return Ok(None);
    }

    let (host, port) = endpoint::https_authority(endpoint_url).ok_or(())?;
    // A freshly delivered staged credential may start activation. A token
    // recovered from disk after crash must first prove it was activated.
    // Run the probe on the existing control worker so transient network loss
    // cannot strand a saved credential until the next app restart.
    let probe_required = config.pending_pair && !freshly_delivered;
    let host = host.to_owned();
    let tick = Duration::from_millis(config.heartbeat_hint_ms);

    if tick.is_zero() {
        return Err(());
    }

    let runner = ControlRunner::spawn_with(config, store, tick, move || {
        WinHttp::open(&host, port).map(|transport| {
            if probe_required {
                transport.with_credential_probe()
            } else {
                transport
            }
        })
    })?;
    Ok(Some(runner))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_mutation_preserves_control_authority_fields() {
        let mut config = Config {
            device_token: Some("test-token-must-stay-redacted".into()),
            pending_pair: false,
            radio_key: "Mouse5".into(),
            radio_key_kind: "mouse".into(),
            radio_key_code: 0,
            ptt_sound: true,
            generation: 41,
            heartbeat_hint_ms: 275,
        };

        let before_token = config.device_token.clone();
        let before_generation = config.generation;
        let before_hint = config.heartbeat_hint_ms;
        let before_sound = config.ptt_sound;

        apply_radio_key(&mut config, &RadioKey::Keyboard(0x41)).unwrap();

        assert_eq!(config.device_token, before_token);
        assert_eq!(config.generation, before_generation);
        assert_eq!(config.heartbeat_hint_ms, before_hint);
        assert_eq!(config.ptt_sound, before_sound);
        assert_eq!(config.radio_key_kind, "keyboard");
        assert_eq!(config.radio_key_code, 0x41);
    }

    #[test]
    fn escape_is_forbidden_as_persisted_radio_key() {
        let mut config = Config::default();

        assert_eq!(
            apply_radio_key(&mut config, &RadioKey::Keyboard(VK_ESCAPE)),
            Err(SettingsError::ForbiddenKey)
        );

        assert_eq!(radio_key_from_config(&config), RadioKey::Mouse5);
    }

    #[test]
    fn local_unpair_clears_only_the_credential() {
        let mut config = Config {
            device_token: Some("redacted".into()),
            pending_pair: false,
            radio_key: "Mouse4".into(),
            radio_key_kind: "mouse".into(),
            radio_key_code: 0,
            ptt_sound: true,
            generation: 17,
            heartbeat_hint_ms: 275,
        };
        let preserved = (
            config.radio_key.clone(),
            config.radio_key_kind.clone(),
            config.radio_key_code,
            config.ptt_sound,
            config.generation,
            config.heartbeat_hint_ms,
        );

        clear_local_pairing(&mut config);

        assert!(config.device_token.is_none());
        assert_eq!(
            (
                config.radio_key,
                config.radio_key_kind,
                config.radio_key_code,
                config.ptt_sound,
                config.generation,
                config.heartbeat_hint_ms,
            ),
            preserved
        );
    }

    #[cfg(windows)]
    #[test]
    fn pair_poll_publishes_paired_only_after_background_activation_handoff() {
        let (sender, receiver) = sync_channel(1);
        sender
            .send(PairTaskResult::Activated(ControlRunner::new_for_test(true)))
            .expect("pair task hands off activated runner");
        let thread = thread::spawn(|| {});
        let mut runtime = HelperRuntime {
            config_path: PathBuf::from("unused"),
            endpoint_url: endpoint::DEFAULT_MANAGED_ENDPOINT.into(),
            runner: None,
            awaiting_pair_ack: false,
            pair_task: Some(PairTask {
                receiver,
                thread: Some(thread),
            }),
        };

        assert_eq!(runtime.poll_pair(), PairPoll::Paired);
        assert!(runtime.is_paired());
        assert!(runtime.runner.is_some());
        assert!(runtime.pair_task.is_none());
    }

    #[cfg(windows)]
    #[test]
    fn transient_pair_keeps_worker_pending_without_presenting_paired() {
        let (sender, receiver) = sync_channel(1);
        sender
            .send(PairTaskResult::Activated(
                ControlRunner::new_pending_for_test(),
            ))
            .unwrap();
        let thread = thread::spawn(|| {});
        let mut runtime = HelperRuntime {
            config_path: PathBuf::from("unused"),
            endpoint_url: endpoint::DEFAULT_MANAGED_ENDPOINT.into(),
            runner: None,
            awaiting_pair_ack: false,
            pair_task: Some(PairTask {
                receiver,
                thread: Some(thread),
            }),
        };
        assert_eq!(runtime.poll_pair(), PairPoll::Pending);
        assert!(runtime.is_pairing());
        assert!(!runtime.is_paired());
        assert!(runtime.control_handle().is_none());
        assert_eq!(runtime.poll_pair(), PairPoll::Pending);
        assert_eq!(
            runtime.begin_pair("another-code"),
            Err(PairStartError::Busy)
        );
        runtime.runner.as_ref().unwrap().acknowledge_for_test();
        assert_eq!(runtime.poll_pair(), PairPoll::Paired);
        assert!(runtime.is_paired());
        assert!(!runtime.is_pairing());
        assert!(runtime.control_handle().is_some());
    }

    #[cfg(windows)]
    #[test]
    fn invalid_runner_is_not_paired_and_poll_reaps_it() {
        let mut runtime = HelperRuntime {
            config_path: PathBuf::from("unused"),
            endpoint_url: endpoint::DEFAULT_MANAGED_ENDPOINT.into(),
            runner: Some(ControlRunner::new_for_test(false)),
            awaiting_pair_ack: false,
            pair_task: None,
        };

        assert!(!runtime.is_paired());
        assert!(runtime.control_handle().is_none());
        assert_eq!(runtime.poll_pair(), PairPoll::Invalidated);
        assert!(runtime.runner.is_none());
        assert_eq!(runtime.poll_pair(), PairPoll::Idle);
    }

    #[cfg(windows)]
    #[test]
    fn begin_pair_starts_new_task_after_invalid_runner_without_app_restart() {
        let path =
            std::env::temp_dir().join(format!("lcr-helper-h2-repair-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);

        std::fs::write(&path, r#"{}"#).unwrap();

        {
            let mut runtime = HelperRuntime {
                config_path: path.clone(),
                endpoint_url: "invalid".into(),
                runner: Some(ControlRunner::new_for_test(false)),
                awaiting_pair_ack: false,
                pair_task: None,
            };

            assert_eq!(runtime.begin_pair("test-pair-code"), Ok(()));
            assert!(runtime.runner.is_none());
            assert!(runtime.pair_task.is_some());
        }

        let _ = std::fs::remove_file(path);
    }
}
