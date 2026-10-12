#![allow(missing_docs)]

mod core;
mod disposal;
mod events;
mod mixer;
#[cfg(feature = "receive")]
mod udp_rx;
mod ws;

#[cfg(feature = "receive")]
pub use self::udp_rx::*;
pub use self::{core::*, disposal::*, events::*, mixer::*, ws::*};

use crate::driver::{
    LcrCausalDispatchOutcome, LcrCausalDispatcher, LcrCausalEvent,
    LcrImmediateDispatchOutcome, LcrImmediateDispatcher, LcrImmediateEvent,
};
use flume::Sender;
#[cfg(not(feature = "lcr-raw-events"))]
use tokio::spawn;
#[cfg(not(feature = "lcr-raw-events"))]
use tracing::trace;

#[derive(Clone, Debug)]
pub struct Interconnect {
    pub core: Sender<CoreMessage>,
    pub events: Sender<EventMessage>,
    pub mixer: Sender<MixerMessage>,
    pub(crate) lcr_causal: LcrCausalDispatcher,
    pub(crate) lcr_immediate: LcrImmediateDispatcher,
}

impl Interconnect {
    pub(crate) fn dispatch_lcr_causal(&self, event: LcrCausalEvent) -> LcrCausalDispatchOutcome {
        let outcome = self.lcr_causal.dispatch(event);
        if outcome == LcrCausalDispatchOutcome::Failed {
            drop(self.core.send(CoreMessage::Poison));
        }
        outcome
    }

    pub(crate) fn dispatch_lcr_immediate(
        &self,
        event: LcrImmediateEvent,
    ) -> LcrImmediateDispatchOutcome {
        let outcome = self.lcr_immediate.dispatch(event);
        if outcome == LcrImmediateDispatchOutcome::Failed {
            drop(self.core.send(CoreMessage::Poison));
        }
        outcome
    }

    pub fn poison(&self) {
        drop(self.events.send(EventMessage::Poison));
    }

    pub fn poison_all(&self) {
        self.lcr_causal.poison();
        drop(self.mixer.send(MixerMessage::Poison));
        self.poison();
    }

    pub fn restart_volatile_internals(
        &mut self,
        persistent_core_events: &[PersistentCoreEvent],
    ) {
        self.poison();

        #[cfg(not(feature = "lcr-raw-events"))]
        {
            let (evt_tx, evt_rx) = flume::unbounded();
            self.events = evt_tx;

            for event in persistent_core_events {
                drop(
                    self.events
                        .send(EventMessage::AddGlobalEvent(event.event_data())),
                );
            }

            spawn(async move {
                trace!("Event processor restarted.");
                super::events::runner(evt_rx).await;
                trace!("Event processor finished.");
            });
        }

        #[cfg(feature = "lcr-raw-events")]
        let _ = persistent_core_events;

        // Make mixer aware of new targets...
        drop(
            self.mixer
                .send(MixerMessage::ReplaceInterconnect(self.clone())),
        );
    }
}
