use super::tasks::message::CoreMessage;
use crate::events::context_data::{DisconnectKind, DisconnectReason};
use flume::{Receiver, Sender, TrySendError};
use futures::FutureExt;
use std::{
    fmt,
    panic::AssertUnwindSafe,
    sync::{Arc, Mutex},
};

const LCR_CAUSAL_QUEUE_CAPACITY: usize = 8;

/// Low-rate ordered LCR lifecycle facts that require asynchronous handling.
///
/// Media never enters this lane. The bounded queue exists only to keep LCR's
/// asynchronous lifecycle observer independent from Songbird's generic Track/timed
/// event machinery and from the transport core task.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LcrCausalEvent {
    DriverDisconnect {
        kind: DisconnectKind,
        reason: Option<DisconnectReason>,
    },
    DriverReconnect,
}

#[async_trait::async_trait]
#[doc(hidden)]
pub trait LcrCausalEventHandler: Send + Sync {
    async fn handle(&self, event: LcrCausalEvent);
}

struct LcrCausalState {
    handler: Option<Arc<dyn LcrCausalEventHandler>>,
    generation: u64,
    failed: bool,
    closed: bool,
}

impl Default for LcrCausalState {
    fn default() -> Self {
        Self {
            handler: None,
            generation: 1,
            failed: false,
            closed: false,
        }
    }
}

enum LcrCausalMessage {
    Event {
        generation: u64,
        event: LcrCausalEvent,
    },
    Poison,
}

#[derive(Clone)]
pub(crate) struct LcrCausalDispatcher {
    state: Arc<Mutex<LcrCausalState>>,
    tx: Sender<LcrCausalMessage>,
}

impl fmt::Debug for LcrCausalDispatcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LcrCausalDispatcher(..)")
    }
}

pub(crate) struct LcrCausalReceiver {
    state: Arc<Mutex<LcrCausalState>>,
    rx: Receiver<LcrCausalMessage>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LcrCausalDispatchOutcome {
    NotInstalled,
    Queued,
    Failed,
}

impl LcrCausalDispatcher {
    pub(crate) fn new() -> (Self, LcrCausalReceiver) {
        let (tx, rx) = flume::bounded(LCR_CAUSAL_QUEUE_CAPACITY);
        let state = Arc::new(Mutex::new(LcrCausalState::default()));
        (
            Self {
                state: Arc::clone(&state),
                tx,
            },
            LcrCausalReceiver { state, rx },
        )
    }

    fn advance_generation(state: &mut LcrCausalState) -> bool {
        let Some(next) = state.generation.checked_add(1) else {
            state.failed = true;
            state.handler = None;
            return false;
        };
        state.generation = next;
        true
    }

    pub(crate) fn set_handler(&self, handler: Option<Arc<dyn LcrCausalEventHandler>>) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.failed || state.closed || !Self::advance_generation(&mut state) {
            return false;
        }
        state.handler = handler;
        true
    }

    pub(crate) fn dispatch(&self, event: LcrCausalEvent) -> LcrCausalDispatchOutcome {
        let generation = {
            let state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.failed || state.closed {
                return LcrCausalDispatchOutcome::Failed;
            }
            if state.handler.is_none() {
                return LcrCausalDispatchOutcome::NotInstalled;
            }
            state.generation
        };

        match self.tx.try_send(LcrCausalMessage::Event { generation, event }) {
            Ok(()) => LcrCausalDispatchOutcome::Queued,
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state.failed = true;
                state.handler = None;
                LcrCausalDispatchOutcome::Failed
            },
        }
    }

    pub(crate) fn poison(&self) {
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.closed = true;
            state.handler = None;
        }
        let _ = self.tx.try_send(LcrCausalMessage::Poison);
    }
}

pub(crate) async fn runner(receiver: LcrCausalReceiver, core: Sender<CoreMessage>) {
    while let Ok(message) = receiver.rx.recv_async().await {
        let (generation, event) = match message {
            LcrCausalMessage::Poison => break,
            LcrCausalMessage::Event { generation, event } => (generation, event),
        };

        let handler = {
            let state = receiver
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.closed {
                break;
            }
            if state.failed || state.generation != generation {
                continue;
            }
            state.handler.clone()
        };
        let Some(handler) = handler else {
            continue;
        };

        if AssertUnwindSafe(handler.handle(event))
            .catch_unwind()
            .await
            .is_err()
        {
            let mut state = receiver
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.failed = true;
            state.handler = None;
            drop(state);
            drop(core.send(CoreMessage::Poison));
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Clone)]
    struct CountingHandler(Arc<AtomicUsize>);

    #[async_trait::async_trait]
    impl LcrCausalEventHandler for CountingHandler {
        async fn handle(&self, _event: LcrCausalEvent) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn poison_fences_pending_work() {
        let (dispatcher, receiver) = LcrCausalDispatcher::new();
        let (core_tx, _core_rx) = flume::unbounded();
        let hits = Arc::new(AtomicUsize::new(0));
        assert!(dispatcher.set_handler(Some(Arc::new(CountingHandler(Arc::clone(&hits))))));

        assert_eq!(
            dispatcher.dispatch(LcrCausalEvent::DriverDisconnect {
                kind: DisconnectKind::Runtime,
                reason: None,
            }),
            LcrCausalDispatchOutcome::Queued
        );
        assert_eq!(
            dispatcher.dispatch(LcrCausalEvent::DriverReconnect),
            LcrCausalDispatchOutcome::Queued
        );
        dispatcher.poison();
        runner(receiver, core_tx).await;
        assert_eq!(hits.load(Ordering::SeqCst), 0, "poison fences pending work");
    }

    struct RecordingHandler(Sender<&'static str>);

    #[async_trait::async_trait]
    impl LcrCausalEventHandler for RecordingHandler {
        async fn handle(&self, event: LcrCausalEvent) {
            let label = match event {
                LcrCausalEvent::DriverDisconnect { .. } => "disconnect",
                LcrCausalEvent::DriverReconnect => "reconnect",
            };
            self.0.send(label).expect("record causal event");
        }
    }

    #[tokio::test]
    async fn current_generation_preserves_fifo_order() {
        let (dispatcher, receiver) = LcrCausalDispatcher::new();
        let (core_tx, _core_rx) = flume::unbounded();
        let (record_tx, record_rx) = flume::bounded(2);
        assert!(dispatcher.set_handler(Some(Arc::new(RecordingHandler(record_tx)))));
        let task = tokio::spawn(runner(receiver, core_tx));

        assert_eq!(
            dispatcher.dispatch(LcrCausalEvent::DriverDisconnect {
                kind: DisconnectKind::Runtime,
                reason: None,
            }),
            LcrCausalDispatchOutcome::Queued
        );
        assert_eq!(
            dispatcher.dispatch(LcrCausalEvent::DriverReconnect),
            LcrCausalDispatchOutcome::Queued
        );

        assert_eq!(record_rx.recv_async().await.unwrap(), "disconnect");
        assert_eq!(record_rx.recv_async().await.unwrap(), "reconnect");
        dispatcher.poison();
        task.await.expect("causal runner exits");
    }

    #[tokio::test]
    async fn clear_generation_drops_already_queued_event() {
        let (dispatcher, receiver) = LcrCausalDispatcher::new();
        let (core_tx, _core_rx) = flume::unbounded();
        let hits = Arc::new(AtomicUsize::new(0));
        assert!(dispatcher.set_handler(Some(Arc::new(CountingHandler(Arc::clone(&hits))))));
        assert_eq!(
            dispatcher.dispatch(LcrCausalEvent::DriverReconnect),
            LcrCausalDispatchOutcome::Queued
        );
        assert!(dispatcher.set_handler(None));
        let task = tokio::spawn(runner(receiver, core_tx));
        tokio::task::yield_now().await;
        dispatcher.poison();
        task.await.expect("causal runner exits");
        assert_eq!(hits.load(Ordering::SeqCst), 0);
    }

    struct PanicHandler;

    #[async_trait::async_trait]
    impl LcrCausalEventHandler for PanicHandler {
        async fn handle(&self, _event: LcrCausalEvent) {
            panic!("deterministic causal handler panic");
        }
    }

    #[tokio::test]
    async fn handler_panic_poison_exact_core() {
        let (dispatcher, receiver) = LcrCausalDispatcher::new();
        let (core_tx, core_rx) = flume::unbounded();
        assert!(dispatcher.set_handler(Some(Arc::new(PanicHandler))));
        assert_eq!(
            dispatcher.dispatch(LcrCausalEvent::DriverReconnect),
            LcrCausalDispatchOutcome::Queued
        );

        runner(receiver, core_tx).await;
        assert!(matches!(core_rx.try_recv(), Ok(CoreMessage::Poison)));
        assert_eq!(
            dispatcher.dispatch(LcrCausalEvent::DriverReconnect),
            LcrCausalDispatchOutcome::Failed
        );
    }

    #[test]
    fn queue_is_bounded_and_overflow_fails_closed() {
        let (dispatcher, _receiver) = LcrCausalDispatcher::new();
        let hits = Arc::new(AtomicUsize::new(0));
        assert!(dispatcher.set_handler(Some(Arc::new(CountingHandler(hits)))));
        for _ in 0..LCR_CAUSAL_QUEUE_CAPACITY {
            assert_eq!(
                dispatcher.dispatch(LcrCausalEvent::DriverReconnect),
                LcrCausalDispatchOutcome::Queued
            );
        }
        assert_eq!(
            dispatcher.dispatch(LcrCausalEvent::DriverReconnect),
            LcrCausalDispatchOutcome::Failed
        );
    }
}