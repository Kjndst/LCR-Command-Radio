#[cfg(feature = "receive")]
use crate::events::context_data::RtpData;
use crate::events::context_data::{DisconnectKind, DisconnectReason};
use std::{
    fmt,
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{Arc, Mutex},
};

/// Synchronous LCR transport facts delivered outside Songbird's generic event queue.
///
/// This seam carries only facts that can change immediate relay safety. It owns no
/// buffering, retry, recovery, or asynchronous work.
#[doc(hidden)]
#[derive(Clone, Debug)]
pub enum LcrImmediateEvent {
    Speaking {
        ssrc: u32,
        user_id: Option<u64>,
        microphone: bool,
    },
    #[cfg(feature = "receive")]
    Rtp(RtpData),
    ClientDisconnect {
        user_id: u64,
    },
    DriverDisconnect {
        kind: DisconnectKind,
        reason: Option<DisconnectReason>,
    },
    DriverReconnect,
}

/// Packet-safe synchronous handler for [`LcrImmediateEvent`].
///
/// Implementations must not block on network or disk I/O, await asynchronous work,
/// or call back into handler installation/removal. Songbird serializes calls for one
/// Driver lifetime and terminalizes that exact Driver core if the handler panics.
#[doc(hidden)]
pub trait LcrImmediateEventHandler: Send + Sync {
    fn handle(&self, event: LcrImmediateEvent);
}

#[derive(Default)]
struct LcrImmediateState {
    handler: Option<Arc<dyn LcrImmediateEventHandler>>,
    failed: bool,
}

#[derive(Clone, Default)]
pub(crate) struct LcrImmediateDispatcher {
    state: Arc<Mutex<LcrImmediateState>>,
}

impl fmt::Debug for LcrImmediateDispatcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LcrImmediateDispatcher(..)")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LcrImmediateDispatchOutcome {
    NotInstalled,
    Delivered,
    Failed,
}

impl LcrImmediateDispatcher {
    pub(crate) fn set_handler(&self, handler: Option<Arc<dyn LcrImmediateEventHandler>>) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.failed {
            return;
        }
        state.handler = handler;
    }

    pub(crate) fn dispatch(&self, event: LcrImmediateEvent) -> LcrImmediateDispatchOutcome {
        let result = catch_unwind(AssertUnwindSafe(|| {
            let state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.failed {
                return LcrImmediateDispatchOutcome::Failed;
            }
            let Some(handler) = state.handler.as_ref() else {
                return LcrImmediateDispatchOutcome::NotInstalled;
            };
            handler.handle(event);
            LcrImmediateDispatchOutcome::Delivered
        }));

        if let Ok(outcome) = result {
            outcome
        } else {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.failed = true;
            state.handler = None;
            LcrImmediateDispatchOutcome::Failed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Barrier,
        },
        thread,
        time::Duration,
    };

    fn disconnect_event() -> LcrImmediateEvent {
        LcrImmediateEvent::DriverDisconnect {
            kind: DisconnectKind::Runtime,
            reason: None,
        }
    }

    struct CountingHandler(Arc<AtomicUsize>);

    impl LcrImmediateEventHandler for CountingHandler {
        fn handle(&self, _event: LcrImmediateEvent) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn no_handler_is_explicitly_not_installed() {
        let dispatcher = LcrImmediateDispatcher::default();
        assert_eq!(
            dispatcher.dispatch(disconnect_event()),
            LcrImmediateDispatchOutcome::NotInstalled
        );
    }

    #[test]
    fn installed_handler_receives_immediate_fact() {
        let dispatcher = LcrImmediateDispatcher::default();
        let calls = Arc::new(AtomicUsize::new(0));
        dispatcher.set_handler(Some(Arc::new(CountingHandler(Arc::clone(&calls)))));

        assert_eq!(
            dispatcher.dispatch(LcrImmediateEvent::DriverReconnect),
            LcrImmediateDispatchOutcome::Delivered
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn cloned_dispatcher_retains_handler_and_clear_is_shared() {
        let dispatcher = LcrImmediateDispatcher::default();
        let clone = dispatcher.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        dispatcher.set_handler(Some(Arc::new(CountingHandler(Arc::clone(&calls)))));

        assert_eq!(
            clone.dispatch(LcrImmediateEvent::DriverReconnect),
            LcrImmediateDispatchOutcome::Delivered
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        clone.set_handler(None);
        assert_eq!(
            dispatcher.dispatch(LcrImmediateEvent::DriverReconnect),
            LcrImmediateDispatchOutcome::NotInstalled
        );
    }

    struct SerialHandler {
        active: AtomicBool,
        overlap: Arc<AtomicBool>,
    }

    impl LcrImmediateEventHandler for SerialHandler {
        fn handle(&self, _event: LcrImmediateEvent) {
            if self.active.swap(true, Ordering::SeqCst) {
                self.overlap.store(true, Ordering::SeqCst);
            }
            thread::sleep(Duration::from_millis(20));
            self.active.store(false, Ordering::SeqCst);
        }
    }

    #[test]
    fn concurrent_producers_are_serialized() {
        let dispatcher = LcrImmediateDispatcher::default();
        let overlap = Arc::new(AtomicBool::new(false));
        dispatcher.set_handler(Some(Arc::new(SerialHandler {
            active: AtomicBool::new(false),
            overlap: Arc::clone(&overlap),
        })));
        let start = Arc::new(Barrier::new(3));

        let first_dispatcher = dispatcher.clone();
        let first_start = Arc::clone(&start);
        let first = thread::spawn(move || {
            first_start.wait();
            first_dispatcher.dispatch(disconnect_event())
        });

        let second_dispatcher = dispatcher.clone();
        let second_start = Arc::clone(&start);
        let second = thread::spawn(move || {
            second_start.wait();
            second_dispatcher.dispatch(LcrImmediateEvent::DriverReconnect)
        });

        start.wait();
        assert_eq!(
            first.join().expect("first immediate thread"),
            LcrImmediateDispatchOutcome::Delivered
        );
        assert_eq!(
            second.join().expect("second immediate thread"),
            LcrImmediateDispatchOutcome::Delivered
        );
        assert!(!overlap.load(Ordering::SeqCst));
    }

    struct PanicHandler;

    impl LcrImmediateEventHandler for PanicHandler {
        fn handle(&self, _event: LcrImmediateEvent) {
            panic!("deterministic immediate-handler panic");
        }
    }

    #[test]
    fn panic_fences_dispatcher_for_driver_lifetime() {
        let dispatcher = LcrImmediateDispatcher::default();
        dispatcher.set_handler(Some(Arc::new(PanicHandler)));

        assert_eq!(
            dispatcher.dispatch(disconnect_event()),
            LcrImmediateDispatchOutcome::Failed
        );
        assert_eq!(
            dispatcher.dispatch(LcrImmediateEvent::DriverReconnect),
            LcrImmediateDispatchOutcome::Failed
        );
    }
}