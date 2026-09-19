#![allow(missing_docs)]

use crate::{
    driver::{connection::error::Error, Bitrate, Config},
    events::{
        context_data::DisconnectReason,
        CoreEvent,
        Event,
        EventContext,
        EventData,
        EventHandler,
    },
    tracks::{Track, TrackCommand, TrackHandle},
    ConnectionInfo,
};
#[cfg(test)]
use crate::events::CoreContext;
use flume::{Receiver, Sender};
use std::sync::Arc;

#[derive(Clone)]
pub struct PersistentCoreEvent {
    event: CoreEvent,
    action: Arc<dyn EventHandler>,
}

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

#[derive(Clone)]
struct SharedPersistentCoreEventHandler(Arc<dyn EventHandler>);

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
    SetTrack(Option<Box<TrackContext>>),
    AddTrack(Box<TrackContext>),
    SetBitrate(Bitrate),
    AddEvent(EventData),
    AddPersistentCoreEvent(PersistentCoreEvent),
    RemoveGlobalEvents,
    SetConfig(Config),
    Mute(bool),
    Reconnect,
    #[cfg(feature = "lcr-controlled-fault")]
    LcrControlledReconnect(Sender<bool>, bool),
    FullReconnect,
    RebuildInterconnect,
    #[cfg(test)]
    TestFireCoreEvent(CoreContext),
    Poison,
}

pub struct TrackContext {
    pub track: Track,
    pub handle: TrackHandle,
    pub receiver: Receiver<TrackCommand>,
}
