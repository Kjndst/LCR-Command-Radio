#![allow(missing_docs)]

#[cfg(feature = "lcr-raw-opus-source")]
use crate::driver::raw_opus::RawOpusContext;

use crate::{
    driver::{connection::error::Error, Bitrate, Config, LcrCausalEventHandler, LcrImmediateEventHandler},
    events::context_data::DisconnectReason,
    ConnectionInfo,
};
#[cfg(feature = "driver-media")]
use crate::{
    events::{CoreEvent, Event, EventContext, EventData, EventHandler},
    tracks::{Track, TrackCommand, TrackHandle},
};
#[cfg(all(test, feature = "driver-media"))]
use crate::events::CoreContext;
use flume::Sender;
#[cfg(feature = "driver-media")]
use flume::Receiver;
use std::sync::Arc;

#[cfg(not(feature = "driver-media"))]
pub struct PersistentCoreEvent;
#[cfg(feature = "driver-media")]
#[derive(Clone)]
pub struct PersistentCoreEvent {
    event: CoreEvent,
    action: Arc<dyn EventHandler>,
}

#[cfg(feature = "driver-media")]
impl PersistentCoreEvent {
    pub(crate) fn new<F: EventHandler + 'static>(event: CoreEvent, action: F) -> Self {
        Self {
            event,
            action: Arc::new(action),
        }
    }

    pub(crate) fn event_data(&self) -> EventData {
        EventData::new(
            Event::Core(self.event),
            SharedPersistentCoreEventHandler(Arc::clone(&self.action)),
        )
    }
}

#[cfg(feature = "driver-media")]
#[derive(Clone)]
struct SharedPersistentCoreEventHandler(Arc<dyn EventHandler>);

#[cfg(feature = "driver-media")]
#[async_trait::async_trait]
impl EventHandler for SharedPersistentCoreEventHandler {
    async fn act(&self, context: &EventContext<'_>) -> Option<Event> {
        self.0.act(context).await
    }
}

pub enum CoreMessage {
    ConnectWithResult(ConnectionInfo, Sender<Result<(), Error>>),
    RetryConnect(usize),
    SignalWsClosure(usize, ConnectionInfo, Option<DisconnectReason>),
    Disconnect,
    #[cfg(feature = "driver-media")]
    SetTrack(Option<Box<TrackContext>>),
    #[cfg(feature = "driver-media")]
    AddTrack(Box<TrackContext>),
    #[cfg(feature = "lcr-raw-opus-source")]
    SetRawOpusSource(Option<Box<RawOpusContext>>),
    SetBitrate(Bitrate),
    #[cfg(feature = "driver-media")]
    AddEvent(EventData),
    SetLcrCausalEventHandler(Option<Arc<dyn LcrCausalEventHandler>>),
    SetLcrImmediateEventHandler(Option<Arc<dyn LcrImmediateEventHandler>>),
    #[cfg(feature = "driver-media")]
    AddPersistentCoreEvent(PersistentCoreEvent),
    #[cfg(feature = "driver-media")]
    RemoveGlobalEvents,
    SetConfig(Config),
    Mute(bool),
    Reconnect,
    #[cfg(feature = "lcr-controlled-fault")]
    LcrControlledReconnect(Sender<bool>, bool),
    FullReconnect,
    RebuildInterconnect,
    #[cfg(all(test, feature = "driver-media"))]
    TestFireCoreEvent(CoreContext),
    Poison,
}

#[cfg(feature = "driver-media")]
pub struct TrackContext {
    pub track: Track,
    pub handle: TrackHandle,
    pub receiver: Receiver<TrackCommand>,
}
