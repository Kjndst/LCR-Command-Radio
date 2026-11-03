//! Bounded worker seam: UI/input publishes only a wake, never network work.

use crate::{
    config::{Config, Store},
    protocol::ControlReply,
    reconciler::{Action, Desired, Reconciler},
    transport::ControlOutcome,
};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
        mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender},
        Arc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

#[cfg(not(windows))]
use std::{sync::OnceLock, time::Instant};

#[cfg(windows)]
fn monotonic_tick_ms() -> u64 {
    crate::input_broker::monotonic_tick_ms()
}

#[cfg(not(windows))]
fn monotonic_tick_ms() -> u64 {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();

    u64::try_from(ORIGIN.get_or_init(Instant::now).elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn live_lease_ms_for_tick(tick: Duration) -> Option<u64> {
    let tick_ms = u64::try_from(tick.as_millis()).ok()?;
    tick_ms.checked_mul(3).filter(|lease| *lease > 0)
}

pub trait ControlTransport {
    fn execute(&mut self, action: Action, device_token: Option<&str>) -> ControlOutcome;
}

pub struct ControlWorker<S, T> {
    pub reconciler: Reconciler,
    store: S,
    transport: T,
    authority_valid: Option<Arc<AtomicBool>>,
    live_deadline_ms: Option<Arc<AtomicU64>>,
    live_lease_ms: u64,
    control_shared: Option<Arc<ControlShared>>,
}

impl<S: Store, T: ControlTransport> ControlWorker<S, T> {
    pub fn new(config: Config, store: S, transport: T) -> Self {
        Self {
            reconciler: Reconciler::new(config),
            store,
            transport,
            authority_valid: None,
            live_deadline_ms: None,
            live_lease_ms: 0,
            control_shared: None,
        }
    }

    fn bind_authority_valid(&mut self, authority_valid: Arc<AtomicBool>) {
        authority_valid.store(
            self.reconciler.config.device_token.is_some(),
            Ordering::Release,
        );
        self.authority_valid = Some(authority_valid);
    }

    fn bind_control_shared(&mut self, control_shared: Arc<ControlShared>) {
        self.control_shared = Some(control_shared);
    }

    fn bind_live_deadline(&mut self, live_deadline_ms: Arc<AtomicU64>, live_lease_ms: u64) {
        live_deadline_ms.store(0, Ordering::Release);
        self.live_deadline_ms = Some(live_deadline_ms);
        self.live_lease_ms = live_lease_ms;
    }

    fn publication_is_current(&self) -> bool {
        let Some(shared) = self.control_shared.as_ref() else {
            return false;
        };

        shared.pending_desired.load(Ordering::SeqCst) == PENDING_NONE
            && !shared.shutdown.load(Ordering::SeqCst)
            && shared.authority_valid.load(Ordering::Acquire)
    }

    fn clear_live(&self) {
        if let Some(deadline) = self.live_deadline_ms.as_ref() {
            deadline.store(0, Ordering::SeqCst);
        }
    }

    fn publish_live_reply_at(&self, reply: &ControlReply, now_ms: u64) {
        let current_generation = self.reconciler.config.generation;

        if self.reconciler.desired_state() != Desired::Open
            || reply.generation != current_generation
            || self.live_lease_ms == 0
        {
            self.clear_live();
            return;
        }

        let Some(receipt) = reply.relay else {
            self.clear_live();
            return;
        };

        if receipt.cycle_generation != current_generation {
            self.clear_live();
            return;
        }

        let Some(deadline) = now_ms.checked_add(self.live_lease_ms) else {
            self.clear_live();
            return;
        };

        // No response may publish LIVE while a newer local desired edge or
        // shutdown request is pending.
        if !self.publication_is_current() {
            self.clear_live();
            return;
        }

        if let Some(live_deadline) = self.live_deadline_ms.as_ref() {
            live_deadline.store(deadline, Ordering::SeqCst);

            // Close the TOCTOU window between the first authority check and
            // the deadline publication. A newer local edge always wins.
            if !self.publication_is_current() {
                live_deadline.store(0, Ordering::SeqCst);
            }
        }
    }
    pub fn startup(&mut self) {
        match self.reconciler.startup(&mut self.store) {
            Ok(action) => {
                if !matches!(action, Action::None) {
                    self.clear_live();
                }
                self.apply(action);
            }
            Err(()) => {
                // No durable generation was admitted, so on_tick() cannot
                // retry startup. Fence this runner rather than publishing an
                // indefinitely pending but apparently valid pairing.
                self.clear_live();
                if let Some(authority_valid) = self.authority_valid.as_ref() {
                    authority_valid.store(false, Ordering::Release);
                }
            }
        }
    }

    pub fn desired(&mut self, desired: Desired) {
        let result = self.reconciler.set_desired(desired, &mut self.store);
        if result.is_err() && desired == Desired::Closed {
            // A failed durable release must never renew the old OPEN lease.
            // Reuse local authority invalidation; the server lease expires
            // without inventing an unpersisted generation or a retry owner.
            self.apply_outcome_at(ControlOutcome::Unauthorized, monotonic_tick_ms());
            return;
        }
        if let Ok(action) = result {
            if !matches!(action, Action::None) {
                self.clear_live();
            }

            self.apply(action);
        }
    }

    pub fn wake(&mut self) {
        let action = self.reconciler.on_tick();
        self.apply(action);
    }

    fn apply(&mut self, action: Action) {
        let device_token = self.reconciler.config.device_token.clone();

        let outcome = self.transport.execute(action, device_token.as_deref());

        self.apply_outcome_at(outcome, monotonic_tick_ms());
    }

    fn apply_outcome_at(&mut self, outcome: ControlOutcome, now_ms: u64) {
        match outcome {
            ControlOutcome::Idle => {}

            ControlOutcome::StateAck {
                desired,
                generation,
                reply,
            } => {
                if reply.generation != generation {
                    self.clear_live();
                    self.reconciler.transient_failure();
                    return;
                }

                self.reconciler.on_ack(desired, generation);
                if desired == Desired::Closed
                    && self.reconciler.config.generation == generation
                    && self.reconciler.desired_state() == Desired::Closed
                {
                    if self.reconciler.config.pending_pair {
                        // Only publish READY after durable promotion from candidate
                        // to active. If this save fails, retry the same CLOSED using
                        // the existing worker and unchanged generation.
                        let mut active = self.reconciler.config.clone();
                        active.pending_pair = false;
                        if self.store.save(&active).is_err() {
                            self.reconciler.transient_failure();
                            return;
                        }
                        self.reconciler.config = active;
                    }
                    if let Some(shared) = self.control_shared.as_ref() {
                        shared.startup_closed_acked.store(true, Ordering::Release);
                    }
                }
                self.publish_live_reply_at(&reply, now_ms);
            }

            ControlOutcome::Heartbeat(reply) => {
                self.publish_live_reply_at(&reply, now_ms);
            }

            ControlOutcome::Transient => {
                self.clear_live();
                self.reconciler.transient_failure();
            }

            ControlOutcome::Unauthorized => {
                self.clear_live();

                let _ = self.reconciler.unauthorized();

                if let Some(authority_valid) = self.authority_valid.as_ref() {
                    authority_valid.store(false, Ordering::Release);
                }

                let _ = self.store.save(&self.reconciler.config);
            }
        }
    }
}
const PENDING_NONE: u8 = 0;
const PENDING_CLOSED: u8 = 1;
const PENDING_OPEN: u8 = 2;

struct ControlShared {
    pending_desired: AtomicU8,
    shutdown: AtomicBool,
    authority_valid: Arc<AtomicBool>,
    live_deadline_ms: Arc<AtomicU64>,
    startup_closed_acked: AtomicBool,
}
/// UI/input publishes only latest desired state plus a coalesced wake.
/// This handle never owns transport, config, or credentials.
#[derive(Clone)]
pub struct ControlHandle {
    shared: Arc<ControlShared>,
    wake: SyncSender<()>,
}

impl ControlHandle {
    fn channel(authority_valid: bool) -> (Self, Receiver<()>) {
        let (wake, receiver) = sync_channel(1);

        (
            Self {
                shared: Arc::new(ControlShared {
                    pending_desired: AtomicU8::new(PENDING_NONE),
                    shutdown: AtomicBool::new(false),
                    authority_valid: Arc::new(AtomicBool::new(authority_valid)),
                    live_deadline_ms: Arc::new(AtomicU64::new(0)),
                    startup_closed_acked: AtomicBool::new(false),
                }),
                wake,
            },
            receiver,
        )
    }

    fn authority_valid_state(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.shared.authority_valid)
    }

    fn live_deadline_state(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.shared.live_deadline_ms)
    }

    fn clear_live(&self) {
        self.shared.live_deadline_ms.store(0, Ordering::SeqCst);
    }

    fn live_read_is_current(&self) -> bool {
        self.is_authority_valid()
            && self.shared.pending_desired.load(Ordering::SeqCst) == PENDING_NONE
            && !self.shared.shutdown.load(Ordering::SeqCst)
    }

    fn is_live_at(&self, now_ms: u64) -> bool {
        if !self.live_read_is_current() {
            return false;
        }

        let deadline = self.shared.live_deadline_ms.load(Ordering::SeqCst);

        let live = deadline != 0 && now_ms < deadline;

        live && self.live_read_is_current()
    }

    pub fn is_live(&self) -> bool {
        self.is_live_at(monotonic_tick_ms())
    }

    pub fn is_authority_valid(&self) -> bool {
        self.shared.authority_valid.load(Ordering::Acquire)
    }

    pub fn desired(&self, desired: Desired) {
        if !self.is_authority_valid() {
            self.clear_live();
            return;
        }

        let value = match desired {
            Desired::Open => PENDING_OPEN,
            Desired::Closed => PENDING_CLOSED,
        };

        // Publish newer intent before clearing LIVE. From this point every
        // reader and every in-flight response fails closed.
        self.shared.pending_desired.store(value, Ordering::SeqCst);

        self.clear_live();
        self.signal();
    }
    pub fn wake(&self) {
        self.signal();
    }

    pub fn request_shutdown(&self) {
        // Publish shutdown authority before clearing LIVE so an in-flight
        // response can never legitimately republish after this edge.
        self.shared.shutdown.store(true, Ordering::SeqCst);
        self.clear_live();
        self.signal();
    }

    fn signal(&self) {
        // Capacity one deliberately coalesces wakeups.
        // The newest desired state lives outside the channel.
        let _ = self.wake.try_send(());
    }

    fn take_desired(&self) -> Option<Desired> {
        match self
            .shared
            .pending_desired
            .swap(PENDING_NONE, Ordering::SeqCst)
        {
            PENDING_OPEN => Some(Desired::Open),
            PENDING_CLOSED => Some(Desired::Closed),
            _ => None,
        }
    }

    fn is_shutdown(&self) -> bool {
        self.shared.shutdown.load(Ordering::SeqCst)
    }
}
/// Owns exactly one ControlWorker execution thread.
pub struct ControlRunner {
    handle: ControlHandle,
    thread: Option<JoinHandle<()>>,
    startup_attempt: Option<Receiver<()>>,
}

impl ControlRunner {
    pub fn spawn<S, T>(mut worker: ControlWorker<S, T>, tick: Duration) -> Result<Self, ()>
    where
        S: Store + Send + 'static,
        T: ControlTransport + Send + 'static,
    {
        let live_lease_ms = live_lease_ms_for_tick(tick).ok_or(())?;

        let initial_authority_valid = worker.reconciler.config.device_token.is_some();

        let (handle, receiver) = ControlHandle::channel(initial_authority_valid);

        worker.bind_authority_valid(handle.authority_valid_state());
        worker.bind_control_shared(Arc::clone(&handle.shared));

        worker.bind_live_deadline(handle.live_deadline_state(), live_lease_ms);

        let worker_handle = handle.clone();
        let (startup_attempt_tx, startup_attempt_rx) = sync_channel(1);

        let thread = thread::Builder::new()
            .name("lcr-control".into())
            .spawn(move || {
                run_worker(
                    worker,
                    worker_handle,
                    receiver,
                    tick,
                    Some(startup_attempt_tx),
                )
            })
            .map_err(|_| ())?;

        Ok(Self {
            handle,
            thread: Some(thread),
            startup_attempt: Some(startup_attempt_rx),
        })
    }

    /// Creates the transport inside the control thread, allowing native
    /// connection handles to remain thread-confined rather than requiring Send.
    pub fn spawn_with<S, T, F>(
        config: Config,
        store: S,
        tick: Duration,
        make_transport: F,
    ) -> Result<Self, ()>
    where
        S: Store + Send + 'static,
        T: ControlTransport + 'static,
        F: FnOnce() -> Result<T, ()> + Send + 'static,
    {
        let live_lease_ms = live_lease_ms_for_tick(tick).ok_or(())?;

        let initial_authority_valid = config.device_token.is_some();

        let (handle, receiver) = ControlHandle::channel(initial_authority_valid);

        let authority_valid = handle.authority_valid_state();
        let live_deadline_ms = handle.live_deadline_state();
        let control_shared = Arc::clone(&handle.shared);
        let worker_handle = handle.clone();
        let (startup_tx, startup_rx) = sync_channel(1);
        let (startup_attempt_tx, startup_attempt_rx) = sync_channel(1);

        let thread = thread::Builder::new()
            .name("lcr-control".into())
            .spawn(move || {
                let transport = match make_transport() {
                    Ok(transport) => transport,
                    Err(()) => {
                        let _ = startup_tx.send(Err(()));
                        return;
                    }
                };

                let mut worker = ControlWorker::new(config, store, transport);

                worker.bind_authority_valid(authority_valid);
                worker.bind_control_shared(control_shared);

                worker.bind_live_deadline(live_deadline_ms, live_lease_ms);

                if startup_tx.send(Ok(())).is_err() {
                    return;
                }

                run_worker(
                    worker,
                    worker_handle,
                    receiver,
                    tick,
                    Some(startup_attempt_tx),
                );
            })
            .map_err(|_| ())?;

        match startup_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                handle,
                thread: Some(thread),
                startup_attempt: Some(startup_attempt_rx),
            }),
            _ => {
                let _ = thread.join();
                Err(())
            }
        }
    }

    pub fn handle(&self) -> ControlHandle {
        self.handle.clone()
    }

    pub fn is_authority_valid(&self) -> bool {
        self.handle.is_authority_valid()
    }

    /// Waits for the one-shot startup CLOSED attempt to finish. Pairing uses
    /// this only on its existing background task so the UI thread never waits
    /// on control-plane I/O.
    pub fn wait_startup_attempt(&mut self) -> bool {
        self.startup_attempt
            .take()
            .is_none_or(|startup_attempt| startup_attempt.recv().is_ok())
    }

    // A successful TCP write is not successful staged activation. Only the
    // accepted first CLOSED is proof that Server committed this credential.
    pub fn startup_closed_acknowledged(&self) -> bool {
        self.handle
            .shared
            .startup_closed_acked
            .load(Ordering::Acquire)
    }

    #[cfg(all(test, windows))]
    pub(crate) fn new_for_test(authority_valid: bool) -> Self {
        let (handle, _receiver) = ControlHandle::channel(authority_valid);
        handle
            .shared
            .startup_closed_acked
            .store(authority_valid, Ordering::Release);

        Self {
            handle,
            thread: None,
            startup_attempt: None,
        }
    }

    #[cfg(all(test, windows))]
    pub(crate) fn new_pending_for_test() -> Self {
        let (handle, _receiver) = ControlHandle::channel(true);
        Self {
            handle,
            thread: None,
            startup_attempt: None,
        }
    }

    #[cfg(all(test, windows))]
    pub(crate) fn acknowledge_for_test(&self) {
        self.handle
            .shared
            .startup_closed_acked
            .store(true, Ordering::Release);
    }

    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        self.handle.request_shutdown();

        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
impl Drop for ControlRunner {
    fn drop(&mut self) {
        self.stop();
    }
}

fn close_before_stop<S, T>(worker: &mut ControlWorker<S, T>)
where
    S: Store,
    T: ControlTransport,
{
    // CLOSED always supersedes any pending/open desired state.
    // desired() performs the persisted generation edge and first send.
    worker.desired(Desired::Closed);

    // If the first CLOSED was acknowledged, on_tick() is quiet.
    // If it failed transiently, this is exactly one bounded retry
    // using the same persisted CLOSED generation.
    worker.wake();
}

fn run_worker<S, T>(
    mut worker: ControlWorker<S, T>,
    handle: ControlHandle,
    receiver: Receiver<()>,
    tick: Duration,
    startup_attempt: Option<SyncSender<()>>,
) where
    S: Store,
    T: ControlTransport,
{
    worker.startup();
    if let Some(startup_attempt) = startup_attempt {
        let _ = startup_attempt.send(());
    }

    loop {
        if handle.is_shutdown() {
            // Unacknowledged startup CLOSED may still be a staged credential.
            // Do not activate a replacement as an EXIT side effect.
            if handle.shared.startup_closed_acked.load(Ordering::Acquire) {
                close_before_stop(&mut worker);
            }
            break;
        }

        match receiver.recv_timeout(tick) {
            Ok(()) => {
                if handle.is_shutdown() {
                    // No new activation request on exit for an unACKed stage.
                    if handle.shared.startup_closed_acked.load(Ordering::Acquire) {
                        close_before_stop(&mut worker);
                    }
                    break;
                }

                if let Some(desired) = handle.take_desired() {
                    if handle.is_authority_valid() {
                        worker.desired(desired);
                    }
                } else {
                    worker.wake();
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                worker.wake();
            }
            Err(RecvTimeoutError::Disconnected) => {
                // Unacknowledged startup CLOSED may still be a staged credential.
                // Do not activate a replacement as an EXIT side effect.
                if handle.shared.startup_closed_acked.load(Ordering::Acquire) {
                    close_before_stop(&mut worker);
                }
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Memory;

    impl Store for Memory {
        fn save(&mut self, _: &Config) -> Result<(), ()> {
            Ok(())
        }
    }

    struct Fake {
        result: ControlOutcome,
        seen: Vec<Action>,
    }

    impl ControlTransport for Fake {
        fn execute(&mut self, action: Action, _device_token: Option<&str>) -> ControlOutcome {
            self.seen.push(action);
            self.result.clone()
        }
    }

    struct BlockingStartup {
        entered: SyncSender<()>,
        release: Option<Receiver<()>>,
    }

    impl ControlTransport for BlockingStartup {
        fn execute(&mut self, _action: Action, _device_token: Option<&str>) -> ControlOutcome {
            if let Some(release) = self.release.take() {
                self.entered.send(()).expect("startup attempt entered");
                release.recv().expect("startup attempt released");
            }
            ControlOutcome::Transient
        }
    }

    fn relay_receipt(cycle_generation: u64) -> crate::protocol::RelayReceipt {
        crate::protocol::RelayReceipt {
            cycle_generation,
            command: false,
            units_mask: 1,
        }
    }

    fn reply(generation: u64, relay_cycle: Option<u64>) -> ControlReply {
        ControlReply {
            generation,
            relay: relay_cycle.map(relay_receipt),
        }
    }

    fn state_ack(desired: Desired, generation: u64, relay_cycle: Option<u64>) -> ControlOutcome {
        ControlOutcome::StateAck {
            desired,
            generation,
            reply: reply(generation, relay_cycle),
        }
    }

    #[test]
    fn failed_startup_persistence_invalidates_runner_instead_of_stalling_pair() {
        struct FailingStore;
        impl Store for FailingStore {
            fn save(&mut self, _: &Config) -> Result<(), ()> {
                Err(())
            }
        }
        let worker = ControlWorker::new(
            Config {
                device_token: Some("candidate".into()),
                pending_pair: true,
                ..Config::default()
            },
            FailingStore,
            Fake {
                result: ControlOutcome::Transient,
                seen: Vec::new(),
            },
        );
        let mut runner = ControlRunner::spawn(worker, Duration::from_secs(60)).unwrap();
        assert!(runner.wait_startup_attempt());
        assert!(!runner.is_authority_valid());
        assert!(!runner.startup_closed_acknowledged());
        runner.shutdown();
    }

    #[test]
    fn unacked_pending_pair_drop_cannot_trigger_second_closed() {
        use std::sync::Mutex;
        struct CountTransport(Arc<Mutex<Vec<Action>>>);
        impl ControlTransport for CountTransport {
            fn execute(&mut self, action: Action, _: Option<&str>) -> ControlOutcome {
                self.0.lock().unwrap().push(action);
                ControlOutcome::Transient
            }
        }
        let seen = Arc::new(Mutex::new(Vec::new()));
        let worker = ControlWorker::new(
            Config {
                device_token: Some("candidate".into()),
                pending_pair: true,
                ..Config::default()
            },
            Memory,
            CountTransport(Arc::clone(&seen)),
        );
        let mut runner = ControlRunner::spawn(worker, Duration::from_secs(60)).unwrap();
        assert!(runner.wait_startup_attempt());
        assert!(!runner.startup_closed_acknowledged());
        assert_eq!(seen.lock().unwrap().len(), 1);
        drop(runner);
        assert_eq!(
            seen.lock().unwrap().len(),
            1,
            "shutdown must not activate a staged token"
        );
    }

    #[test]
    fn pending_pair_uses_existing_worker_retry_and_durably_promotes_on_ack() {
        use std::sync::Mutex;
        struct CapturingStore(Arc<Mutex<Vec<Config>>>);
        impl Store for CapturingStore {
            fn save(&mut self, config: &Config) -> Result<(), ()> {
                self.0.lock().unwrap().push(config.clone());
                Ok(())
            }
        }
        struct RetryTransport(usize);
        impl ControlTransport for RetryTransport {
            fn execute(&mut self, action: Action, _: Option<&str>) -> ControlOutcome {
                if let Action::SendState {
                    desired: Desired::Closed,
                    generation,
                } = action
                {
                    self.0 += 1;
                    if self.0 == 1 {
                        ControlOutcome::Transient
                    } else {
                        state_ack(Desired::Closed, generation, None)
                    }
                } else {
                    ControlOutcome::Idle
                }
            }
        }
        let saves = Arc::new(Mutex::new(Vec::new()));
        let worker = ControlWorker::new(
            Config {
                device_token: Some("candidate".into()),
                pending_pair: true,
                ..Config::default()
            },
            CapturingStore(Arc::clone(&saves)),
            RetryTransport(0),
        );
        let mut runner = ControlRunner::spawn(worker, Duration::from_secs(60)).unwrap();
        assert!(runner.wait_startup_attempt());
        assert!(!runner.startup_closed_acknowledged());
        runner.handle().wake();
        let started = std::time::Instant::now();
        while !runner.startup_closed_acknowledged() && started.elapsed() < Duration::from_secs(2) {
            std::thread::yield_now();
        }
        assert!(runner.startup_closed_acknowledged());
        let saved = saves.lock().unwrap();
        assert!(saved.iter().any(|config| config.pending_pair));
        assert!(
            !saved.last().unwrap().pending_pair,
            "ACK is visible only after durable promotion"
        );
        drop(saved);
        runner.shutdown();
    }

    #[test]
    fn failed_pending_promotion_write_never_publishes_ready() {
        struct FailOnce(bool);
        impl Store for FailOnce {
            fn save(&mut self, config: &Config) -> Result<(), ()> {
                if !config.pending_pair && self.0 {
                    self.0 = false;
                    return Err(());
                }
                Ok(())
            }
        }
        let worker = ControlWorker::new(
            Config {
                device_token: Some("candidate".into()),
                pending_pair: true,
                ..Config::default()
            },
            FailOnce(true),
            Fake {
                result: state_ack(Desired::Closed, 1, None),
                seen: Vec::new(),
            },
        );
        let mut worker = worker;
        let (handle, _) = ControlHandle::channel(true);
        worker.bind_control_shared(Arc::clone(&handle.shared));
        worker.startup();
        assert!(!handle.shared.startup_closed_acked.load(Ordering::Acquire));
        assert!(worker.reconciler.config.pending_pair);
        assert_eq!(
            worker.reconciler.on_tick(),
            Action::SendState {
                desired: Desired::Closed,
                generation: 1
            }
        );
        worker.wake();
        assert!(handle.shared.startup_closed_acked.load(Ordering::Acquire));
        assert!(!worker.reconciler.config.pending_pair);
    }

    #[test]
    fn failed_release_persistence_stops_open_renewal_after_storage_recovers() {
        struct FailingStore(bool);
        impl Store for FailingStore {
            fn save(&mut self, _: &Config) -> Result<(), ()> {
                if self.0 {
                    Err(())
                } else {
                    Ok(())
                }
            }
        }
        for acknowledged in [true, false] {
            let config = Config {
                device_token: Some("synthetic".into()),
                ..Config::default()
            };
            let mut worker = ControlWorker::new(
                config,
                FailingStore(false),
                Fake {
                    result: ControlOutcome::Idle,
                    seen: vec![],
                },
            );
            let (handle, _) = ControlHandle::channel(true);
            worker.bind_authority_valid(handle.authority_valid_state());
            worker.bind_live_deadline(handle.live_deadline_state(), 750);
            worker.startup();
            worker.desired(Desired::Open);
            if acknowledged {
                worker.reconciler.on_ack(Desired::Open, 2);
            }
            handle
                .shared
                .live_deadline_ms
                .store(10_000, Ordering::SeqCst);
            worker.transport.seen.clear();
            worker.store.0 = true;
            worker.desired(Desired::Closed);
            assert!(!handle.is_authority_valid());
            assert!(!handle.is_live_at(1));
            assert_eq!(worker.reconciler.desired_state(), Desired::Closed);
            assert_eq!(worker.reconciler.config.generation, 2);
            assert!(worker.reconciler.config.device_token.is_none());
            worker.wake();
            worker.store.0 = false;
            worker.wake();
            assert!(worker
                .transport
                .seen
                .iter()
                .all(|action| *action == Action::None));
        }
    }

    #[test]
    fn control_handle_coalesces_wakes_and_latest_desired_wins() {
        let (handle, receiver) = ControlHandle::channel(true);

        handle
            .shared
            .live_deadline_ms
            .store(10_000, Ordering::Release);

        handle.desired(Desired::Open);
        handle.desired(Desired::Closed);
        handle.desired(Desired::Open);

        assert_eq!(handle.shared.live_deadline_ms.load(Ordering::Acquire), 0);

        assert!(receiver.try_recv().is_ok());
        assert!(receiver.try_recv().is_err());

        assert_eq!(handle.take_desired(), Some(Desired::Open));

        assert_eq!(handle.take_desired(), None);

        handle
            .shared
            .live_deadline_ms
            .store(10_000, Ordering::Release);

        handle.request_shutdown();

        assert_eq!(handle.shared.live_deadline_ms.load(Ordering::Acquire), 0);

        assert!(handle.is_shutdown());
        assert!(receiver.try_recv().is_ok());
    }

    #[test]
    fn staged_pair_ui_readiness_requires_acked_startup_closed() {
        let config = Config {
            device_token: Some("test-only-placeholder".into()),
            ..Config::default()
        };
        let acked = ControlWorker::new(
            config.clone(),
            Memory,
            Fake {
                result: state_ack(Desired::Closed, 1, None),
                seen: Vec::new(),
            },
        );
        let mut good = ControlRunner::spawn(acked, Duration::from_millis(250)).unwrap();
        assert!(good.wait_startup_attempt());
        assert!(good.startup_closed_acknowledged());
        good.shutdown();

        let not_acked = ControlWorker::new(
            config,
            Memory,
            Fake {
                result: ControlOutcome::Transient,
                seen: Vec::new(),
            },
        );
        let mut pending = ControlRunner::spawn(not_acked, Duration::from_millis(250)).unwrap();
        assert!(pending.wait_startup_attempt());
        assert!(!pending.startup_closed_acknowledged());
        pending.shutdown();
    }

    #[test]
    fn startup_attempt_barrier_waits_for_initial_state_attempt() {
        let (entered_tx, entered_rx) = sync_channel(1);
        let (release_tx, release_rx) = sync_channel(1);
        let config = Config {
            device_token: Some("paired".into()),
            ..Config::default()
        };

        let mut runner = ControlRunner::spawn_with::<Memory, BlockingStartup, _>(
            config,
            Memory,
            Duration::from_millis(250),
            move || {
                Ok(BlockingStartup {
                    entered: entered_tx,
                    release: Some(release_rx),
                })
            },
        )
        .expect("runner starts after transport construction");

        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("startup CLOSED attempt entered");

        let (waiter_started_tx, waiter_started_rx) = sync_channel(1);
        let (done_tx, done_rx) = sync_channel(1);
        let waiter = thread::spawn(move || {
            waiter_started_tx.send(()).expect("waiter started");
            let ready = runner.wait_startup_attempt();
            done_tx.send((ready, runner)).expect("waiter result");
        });

        waiter_started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("waiter is blocking on startup barrier");
        assert!(matches!(
            done_rx.recv_timeout(Duration::from_millis(50)),
            Err(RecvTimeoutError::Timeout)
        ));

        release_tx.send(()).expect("release startup attempt");
        let (ready, runner) = done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("startup barrier releases after attempt");
        assert!(ready);
        runner.shutdown();
        waiter.join().expect("waiter joins");
    }

    #[test]
    fn live_lease_is_three_heartbeats() {
        assert_eq!(
            live_lease_ms_for_tick(Duration::from_millis(250)),
            Some(750)
        );

        assert_eq!(
            live_lease_ms_for_tick(Duration::from_millis(275)),
            Some(825)
        );

        assert_eq!(live_lease_ms_for_tick(Duration::ZERO), None);
    }

    #[test]
    fn live_requires_current_open_fresh_receipt_and_expires_at_deadline() {
        let config = Config {
            device_token: Some("t".into()),
            ..Config::default()
        };

        let mut worker = ControlWorker::new(
            config,
            Memory,
            Fake {
                result: ControlOutcome::Idle,
                seen: vec![],
            },
        );

        let (handle, _receiver) = ControlHandle::channel(true);

        worker.bind_authority_valid(handle.authority_valid_state());

        worker.bind_control_shared(Arc::clone(&handle.shared));

        worker.bind_live_deadline(handle.live_deadline_state(), 750);

        worker.startup();
        worker.desired(Desired::Open);

        assert_eq!(worker.reconciler.config.generation, 2);

        worker.apply_outcome_at(state_ack(Desired::Open, 2, Some(2)), 1_000);

        assert!(handle.is_live_at(1_749));
        assert!(!handle.is_live_at(1_750));

        worker.apply_outcome_at(ControlOutcome::Heartbeat(reply(2, Some(2))), 1_300);

        assert!(handle.is_live_at(2_049));
        assert!(!handle.is_live_at(2_050));

        worker.apply_outcome_at(ControlOutcome::Heartbeat(reply(2, None)), 1_400);

        assert!(!handle.is_live_at(1_400));

        worker.apply_outcome_at(ControlOutcome::Heartbeat(reply(2, Some(2))), 1_500);

        assert!(handle.is_live_at(2_000));

        worker.apply_outcome_at(ControlOutcome::Heartbeat(reply(3, Some(3))), 1_600);

        assert!(!handle.is_live_at(1_600));
    }

    #[test]
    fn transient_close_shutdown_and_unauthorized_clear_live() {
        let config = Config {
            device_token: Some("t".into()),
            ..Config::default()
        };

        let mut worker = ControlWorker::new(
            config,
            Memory,
            Fake {
                result: ControlOutcome::Idle,
                seen: vec![],
            },
        );

        let (handle, _receiver) = ControlHandle::channel(true);

        worker.bind_authority_valid(handle.authority_valid_state());

        worker.bind_control_shared(Arc::clone(&handle.shared));

        worker.bind_live_deadline(handle.live_deadline_state(), 750);

        worker.startup();
        worker.desired(Desired::Open);

        worker.apply_outcome_at(state_ack(Desired::Open, 2, Some(2)), 100);

        assert!(handle.is_live_at(200));

        // Transient transport failure immediately clears LIVE.
        worker.apply_outcome_at(ControlOutcome::Transient, 200);

        assert!(!handle.is_live_at(200));

        // Re-establish valid LIVE proof for the CLOSE-edge check.
        worker.apply_outcome_at(state_ack(Desired::Open, 2, Some(2)), 300);

        assert!(handle.is_live_at(301));

        // Local generation edge must clear old LIVE proof immediately,
        // before the control worker processes the queued CLOSED intent.
        handle.desired(Desired::Closed);

        assert_eq!(handle.shared.live_deadline_ms.load(Ordering::Acquire), 0);

        assert_eq!(handle.take_desired(), Some(Desired::Closed));

        // Shutdown independently clears any currently published LIVE lease.
        handle
            .shared
            .live_deadline_ms
            .store(1_000, Ordering::Release);

        handle.request_shutdown();

        assert_eq!(handle.shared.live_deadline_ms.load(Ordering::Acquire), 0);

        // Unauthorized is an independent failure domain. Use fresh shared
        // state so previous pending desired/shutdown state cannot contaminate
        // this authority-invalidation proof.
        let mut unauthorized_worker = ControlWorker::new(
            Config {
                device_token: Some("u".into()),
                ..Config::default()
            },
            Memory,
            Fake {
                result: ControlOutcome::Unauthorized,
                seen: vec![],
            },
        );

        let (unauthorized_handle, unauthorized_receiver) = ControlHandle::channel(true);

        unauthorized_worker.bind_authority_valid(unauthorized_handle.authority_valid_state());

        unauthorized_worker.bind_control_shared(Arc::clone(&unauthorized_handle.shared));

        unauthorized_worker.bind_live_deadline(unauthorized_handle.live_deadline_state(), 750);

        unauthorized_handle
            .shared
            .live_deadline_ms
            .store(1_000, Ordering::Release);

        unauthorized_worker.startup();

        assert!(unauthorized_worker.reconciler.config.device_token.is_none());

        assert!(!unauthorized_handle.is_authority_valid());

        assert_eq!(
            unauthorized_handle
                .shared
                .live_deadline_ms
                .load(Ordering::Acquire),
            0
        );

        unauthorized_handle.desired(Desired::Open);

        assert!(unauthorized_receiver.try_recv().is_err());
        assert_eq!(unauthorized_handle.take_desired(), None);
    }

    #[test]
    fn stale_inflight_open_reply_cannot_resurrect_live_after_new_intent_or_shutdown() {
        let mut worker = ControlWorker::new(
            Config {
                device_token: Some("t".into()),
                ..Config::default()
            },
            Memory,
            Fake {
                result: ControlOutcome::Idle,
                seen: vec![],
            },
        );

        let (handle, _receiver) = ControlHandle::channel(true);

        worker.bind_authority_valid(handle.authority_valid_state());

        worker.bind_control_shared(Arc::clone(&handle.shared));

        worker.bind_live_deadline(handle.live_deadline_state(), 750);

        worker.startup();
        worker.desired(Desired::Open);

        assert_eq!(worker.reconciler.config.generation, 2);

        worker.apply_outcome_at(state_ack(Desired::Open, 2, Some(2)), 100);

        assert!(handle.is_live_at(200));

        // CLOSED arrives while the otherwise valid generation-2 OPEN
        // response is still in flight.
        handle.desired(Desired::Closed);

        assert!(!handle.is_live_at(200));

        // The stale in-flight OPEN response must not resurrect LIVE.
        worker.apply_outcome_at(state_ack(Desired::Open, 2, Some(2)), 250);

        assert_eq!(handle.shared.live_deadline_ms.load(Ordering::SeqCst), 0);

        assert!(!handle.is_live_at(250));

        assert_eq!(handle.take_desired(), Some(Desired::Closed));

        worker.desired(Desired::Closed);

        assert_eq!(worker.reconciler.config.generation, 3);

        worker.desired(Desired::Open);

        assert_eq!(worker.reconciler.config.generation, 4);

        worker.apply_outcome_at(state_ack(Desired::Open, 4, Some(4)), 300);

        assert!(handle.is_live_at(301));

        // Shutdown is independent authority. A valid heartbeat from the
        // previous OPEN cycle is equally forbidden to republish LIVE.
        handle.request_shutdown();

        assert!(!handle.is_live_at(301));

        worker.apply_outcome_at(ControlOutcome::Heartbeat(reply(4, Some(4))), 350);

        assert_eq!(handle.shared.live_deadline_ms.load(Ordering::SeqCst), 0);

        assert!(!handle.is_live_at(350));
    }
    #[test]
    fn graceful_stop_forces_closed_and_retries_same_generation_once() {
        let config = Config {
            device_token: Some("t".into()),
            ..Config::default()
        };

        let mut worker = ControlWorker::new(
            config,
            Memory,
            Fake {
                result: state_ack(Desired::Closed, 1, None),
                seen: vec![],
            },
        );

        worker.startup();

        worker.transport.result = state_ack(Desired::Open, 2, None);

        worker.desired(Desired::Open);

        worker.transport.result = ControlOutcome::Transient;

        close_before_stop(&mut worker);

        let closed: Vec<_> = worker
            .transport
            .seen
            .iter()
            .filter_map(|action| match action {
                Action::SendState {
                    desired: Desired::Closed,
                    generation,
                } if *generation == 3 => Some(*generation),
                _ => None,
            })
            .collect();

        assert_eq!(closed, vec![3, 3]);

        assert_eq!(worker.reconciler.config.generation, 3);
    }

    #[test]
    fn worker_keeps_reconciler_as_authority_after_errors() {
        let config = Config {
            device_token: Some("t".into()),
            ..Config::default()
        };

        let mut worker = ControlWorker::new(
            config,
            Memory,
            Fake {
                result: state_ack(Desired::Closed, 1, None),
                seen: vec![],
            },
        );

        worker.startup();

        assert_eq!(worker.reconciler.config.generation, 1);

        worker.transport.result = ControlOutcome::Transient;

        worker.desired(Desired::Open);
        worker.wake();

        assert!(worker.transport.seen.iter().any(|action| matches!(
            action,
            Action::SendState {
                desired: Desired::Open,
                generation: 2
            }
        )));

        worker.transport.result = ControlOutcome::Unauthorized;

        worker.wake();

        assert!(worker.reconciler.config.device_token.is_none());
    }
}
