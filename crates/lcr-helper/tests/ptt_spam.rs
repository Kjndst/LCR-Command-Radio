use lcr_helper::{
    config::{Config, Store},
    input::{Edge, InputAuthority, InputOutcome, RadioKey},
    reconciler::{Action, Desired},
    transport::{self, ControlOutcome},
    worker::{ControlRunner, ControlTransport, ControlWorker},
};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Condvar, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Clone)]
struct CountingStore {
    saves: Arc<AtomicUsize>,
}

impl Store for CountingStore {
    fn save(&mut self, _: &Config) -> Result<(), ()> {
        self.saves.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[derive(Default)]
struct BlockState {
    entered: bool,
    released: bool,
}

struct TransportMetrics {
    send_state_calls: AtomicUsize,
    heartbeat_calls: AtomicUsize,
    noop_calls: AtomicUsize,
    send_states: Mutex<Vec<Desired>>,
    block_on_send_state_call: Option<usize>,
    block_state: Mutex<BlockState>,
    block_cv: Condvar,
}

impl TransportMetrics {
    fn new(block_on_send_state_call: Option<usize>) -> Self {
        Self {
            send_state_calls: AtomicUsize::new(0),
            heartbeat_calls: AtomicUsize::new(0),
            noop_calls: AtomicUsize::new(0),
            send_states: Mutex::new(Vec::new()),
            block_on_send_state_call,
            block_state: Mutex::new(BlockState::default()),
            block_cv: Condvar::new(),
        }
    }

    fn send_state_calls(&self) -> usize {
        self.send_state_calls.load(Ordering::SeqCst)
    }

    fn wait_for_send_state_calls(&self, expected: usize) {
        let deadline = Instant::now() + Duration::from_secs(3);

        while self.send_state_calls() < expected {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {expected} SendState calls; observed {}",
                self.send_state_calls()
            );
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn wait_until_blocked(&self) {
        assert!(
            self.block_on_send_state_call.is_some(),
            "blocking wait requires a configured blocked SendState call"
        );

        let deadline = Instant::now() + Duration::from_secs(3);
        let mut state = self.block_state.lock().expect("block state");

        while !state.entered {
            let now = Instant::now();

            if now >= deadline {
                state.released = true;
                self.block_cv.notify_all();
                panic!("timed out waiting for the transport to block");
            }

            let remaining = deadline.saturating_duration_since(now);
            let (next, timeout) = self
                .block_cv
                .wait_timeout(state, remaining)
                .expect("block wait");

            state = next;

            if timeout.timed_out() && !state.entered {
                state.released = true;
                self.block_cv.notify_all();
                panic!("timed out waiting for the transport to block");
            }
        }
    }

    fn release_block(&self) {
        let mut state = self.block_state.lock().expect("block state");
        state.released = true;
        self.block_cv.notify_all();
    }

    fn states(&self) -> Vec<Desired> {
        self.send_states.lock().expect("send states").clone()
    }
}

struct MetricsTransport {
    metrics: Arc<TransportMetrics>,
}

impl ControlTransport for MetricsTransport {
    fn execute(&mut self, action: Action, _device_token: Option<&str>) -> ControlOutcome {
        match action {
            Action::SendState {
                desired,
                generation,
            } => {
                let call = self.metrics.send_state_calls.fetch_add(1, Ordering::SeqCst) + 1;

                self.metrics
                    .send_states
                    .lock()
                    .expect("send states")
                    .push(desired);

                if self.metrics.block_on_send_state_call == Some(call) {
                    let mut state = self.metrics.block_state.lock().expect("block state");

                    state.entered = true;
                    self.metrics.block_cv.notify_all();

                    while !state.released {
                        state = self
                            .metrics
                            .block_cv
                            .wait(state)
                            .expect("blocked transport wait");
                    }
                }

                let body = format!(
                    r#"{{"desiredTransmit":{},"generation":{},"relay":null}}"#,
                    matches!(desired, Desired::Open),
                    generation
                );

                transport::classify(202, body.as_bytes(), desired, generation)
            }

            Action::Heartbeat { generation } => {
                self.metrics.heartbeat_calls.fetch_add(1, Ordering::SeqCst);

                let body = format!(r#"{{"generation":{},"relay":null}}"#, generation);

                transport::classify_heartbeat(200, body.as_bytes(), generation)
            }

            Action::InvalidatePairing | Action::None => {
                self.metrics.noop_calls.fetch_add(1, Ordering::SeqCst);

                ControlOutcome::Idle
            }
        }
    }
}
fn paired_config() -> Config {
    Config {
        device_token: Some("ptt-spam-test-token".into()),
        ..Config::default()
    }
}

#[test]
fn repeated_same_level_input_is_bounded_before_control_work() {
    const REPEATS: usize = 100_000;

    let saves = Arc::new(AtomicUsize::new(0));
    let metrics = Arc::new(TransportMetrics::new(None));

    let mut worker = ControlWorker::new(
        paired_config(),
        CountingStore {
            saves: Arc::clone(&saves),
        },
        MetricsTransport {
            metrics: Arc::clone(&metrics),
        },
    );

    worker.startup();

    let mut input = InputAuthority::new(RadioKey::Mouse5);
    let mut desired_edges = 0usize;

    for _ in 0..REPEATS {
        if let InputOutcome::Desired(desired) = input.event(RadioKey::Mouse5, Edge::Down) {
            desired_edges += 1;
            worker.desired(desired);
        }
    }

    for _ in 0..REPEATS {
        if let InputOutcome::Desired(desired) = input.event(RadioKey::Mouse5, Edge::Up) {
            desired_edges += 1;
            worker.desired(desired);
        }
    }

    assert_eq!(desired_edges, 2);
    assert_eq!(saves.load(Ordering::SeqCst), 3);
    assert_eq!(metrics.send_state_calls(), 3);
    assert_eq!(
        metrics.states(),
        vec![Desired::Closed, Desired::Open, Desired::Closed]
    );

    println!(
        "PTT_SPAM_SAME_LEVEL RAW_EDGES={} DESIRED_EDGES={} STORE_SAVES={} SEND_STATE={} RESULT=PASS",
        REPEATS * 2,
        desired_edges,
        saves.load(Ordering::SeqCst),
        metrics.send_state_calls()
    );
}

#[test]
fn sequential_alternating_edges_are_the_worst_case_one_to_one_amplification() {
    const EDGES: usize = 100_000;

    let saves = Arc::new(AtomicUsize::new(0));
    let metrics = Arc::new(TransportMetrics::new(None));

    let mut worker = ControlWorker::new(
        paired_config(),
        CountingStore {
            saves: Arc::clone(&saves),
        },
        MetricsTransport {
            metrics: Arc::clone(&metrics),
        },
    );

    worker.startup();

    let mut input = InputAuthority::new(RadioKey::Mouse5);
    let mut desired_edges = 0usize;

    for index in 0..EDGES {
        let edge = if index % 2 == 0 { Edge::Down } else { Edge::Up };

        match input.event(RadioKey::Mouse5, edge) {
            InputOutcome::Desired(desired) => {
                desired_edges += 1;
                worker.desired(desired);
            }
            other => panic!("alternating physical edge was unexpectedly suppressed: {other:?}"),
        }
    }

    assert_eq!(desired_edges, EDGES);
    assert_eq!(saves.load(Ordering::SeqCst), EDGES + 1);
    assert_eq!(metrics.send_state_calls(), EDGES + 1);
    assert_eq!(
        worker.reconciler.config.generation,
        u64::try_from(EDGES + 1).unwrap()
    );

    println!(
        "PTT_SPAM_SEQUENTIAL RAW_EDGES={} DESIRED_EDGES={} STORE_SAVES={} SEND_STATE={} RESULT=PASS",
        EDGES,
        desired_edges,
        saves.load(Ordering::SeqCst),
        metrics.send_state_calls()
    );
}

#[test]
fn burst_while_transport_is_busy_coalesces_to_latest_closed_without_stale_open() {
    const BURST_EDGES: usize = 100_001;

    let saves = Arc::new(AtomicUsize::new(0));
    let metrics = Arc::new(TransportMetrics::new(Some(2)));

    let runner = ControlRunner::spawn_with(
        paired_config(),
        CountingStore {
            saves: Arc::clone(&saves),
        },
        Duration::from_secs(60),
        {
            let metrics = Arc::clone(&metrics);
            move || {
                Ok(MetricsTransport {
                    metrics: Arc::clone(&metrics),
                })
            }
        },
    )
    .expect("runner must start");

    metrics.wait_for_send_state_calls(1);

    let handle = runner.handle();
    let mut input = InputAuthority::new(RadioKey::Mouse5);

    let first = input.event(RadioKey::Mouse5, Edge::Down);
    assert_eq!(first, InputOutcome::Desired(Desired::Open));
    handle.desired(Desired::Open);

    metrics.wait_until_blocked();

    let mut burst_desired_edges = 0usize;

    for index in 0..BURST_EDGES {
        let edge = if index % 2 == 0 { Edge::Up } else { Edge::Down };

        match input.event(RadioKey::Mouse5, edge) {
            InputOutcome::Desired(desired) => {
                burst_desired_edges += 1;
                handle.desired(desired);
            }
            other => panic!("alternating burst edge was unexpectedly suppressed: {other:?}"),
        }
    }

    assert_eq!(burst_desired_edges, BURST_EDGES);

    metrics.release_block();
    metrics.wait_for_send_state_calls(3);

    runner.shutdown();

    assert_eq!(saves.load(Ordering::SeqCst), 3);
    assert_eq!(metrics.send_state_calls(), 3);
    assert_eq!(metrics.heartbeat_calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        metrics.states(),
        vec![Desired::Closed, Desired::Open, Desired::Closed]
    );

    println!(
        "PTT_SPAM_BURST BURST_EDGES={} STORE_SAVES={} SEND_STATE={} HEARTBEAT={} NO_STALE_OPEN_AFTER_FINAL_CLOSED=PASS RESULT=PASS",
        BURST_EDGES,
        saves.load(Ordering::SeqCst),
        metrics.send_state_calls(),
        metrics.heartbeat_calls.load(Ordering::SeqCst)
    );
}
