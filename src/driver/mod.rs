//! Runner for a voice connection.
//!
//! Songbird's driver is a mixed-sync system, using:
//!  * Asynchronous connection management, event-handling, and gateway integration.
//!  * Synchronous audio mixing, packet generation, and encoding.
//!
//! This splits up work according to its IO/compute bound nature, preventing packet
//! generation from being slowed down past its deadline, or from affecting other
//! asynchronous tasks your bot must handle.

#[cfg(feature = "internals")]
pub mod bench_internals;

pub(crate) mod connection;
mod crypto;
#[cfg(feature = "receive")]
mod decode_mode;
#[cfg(feature = "lcr-raw-opus-source")]
mod raw_opus;
mod mix_mode;
pub mod retry;
mod scheduler;
pub(crate) mod tasks;
#[cfg(test)]
pub(crate) mod test_config;
#[cfg(any(test, feature = "internals"))]
mod test_impls;

use connection::error::{Error, Result};
pub use crypto::CryptoMode;
pub(crate) use crypto::CryptoState;
#[cfg(feature = "receive")]
pub use decode_mode::*;
#[cfg(feature = "lcr-raw-opus-source")]
pub use raw_opus::{RawOpusHandle, RawOpusRead, RawOpusSource};
pub use mix_mode::MixMode;
pub use scheduler::{
    get_default_scheduler,
    Config as SchedulerConfig,
    Error as SchedulerError,
    LiveStatBlock,
    Mode as SchedulerMode,
    Scheduler,
};
#[cfg(test)]
pub use test_config::*;
#[cfg(any(test, feature = "internals"))]
pub use test_impls::*;

#[cfg(feature = "builtin-queue")]
use crate::tracks::TrackQueue;
use crate::{
    events::EventData,
    input::Input,
    tracks::{Track, TrackHandle},
    Config,
    ConnectionInfo,
    CoreEvent,
    Event,
    EventHandler,
};
use core::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use flume::{r#async::RecvFut, SendError, Sender};
/// Opus encoder bitrate settings.
pub use opus2::{self as opus, Bitrate};

#[cfg(feature = "lcr-controlled-fault")]
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LcrControlledReconnectDiagnostic {
    pub sender_alive_before_send: bool,
    pub core_connection_present: Option<bool>,
}
#[cfg(feature = "builtin-queue")]
use std::time::Duration;
#[allow(unused_imports)]
pub use tasks::disposal::DisposalThread;
use tasks::message::{CoreMessage, PersistentCoreEvent};
use tokio::task::JoinHandle;
use tracing::instrument;

/// Opaque observation token for one Driver core task lifetime.
///
/// This token deliberately exposes completion waiting only. It does
/// not expose Tokio task abortion or core reconstruction authority.
#[doc(hidden)]
#[derive(Debug)]
pub struct LcrCoreTaskCompletion {
    task: JoinHandle<()>,
}

impl LcrCoreTaskCompletion {
    /// Waits for the owned Driver core task to finish.
    ///
    /// Dropping this token only detaches completion observation; it
    /// does not abort the Driver core task.
    #[doc(hidden)]
    pub async fn wait(
        self,
    ) -> std::result::Result<(), tokio::task::JoinError> {
        self.task.await
    }
}

/// The Driver core task can no longer accept commands.
///
/// This is a factual transport state only. It does not imply retry,
/// reconstruction, or any product recovery policy.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LcrCoreUnavailable;

/// The control object for a Discord voice connection, handling connection,
/// mixing, encoding, en/decryption, and event generation.
///
/// When compiled with the `"builtin-queue"` feature, each driver includes a track queue
/// as a convenience to prevent the additional overhead of per-guild state management.
#[derive(Debug)]
pub struct Driver {
    config: Config,
    self_mute: bool,
    sender: Sender<CoreMessage>,
    core_task: Option<LcrCoreTaskCompletion>,
    // Making this an Option is an abhorrent hack to coerce the borrow checker
    // into letting us have an &TrackQueue at the same time as an &mut Driver.
    // This is probably preferable to cloning the driver: Arc<...> should be nonzero
    // and if the compiler's smart we'll just codegen a pointer swap. It definitely makes
    // use of NonZero.
    #[cfg(feature = "builtin-queue")]
    queue: Option<TrackQueue>,
}

impl Driver {
    /// Creates a new voice driver.
    ///
    /// This will create the core voice tasks in the background.
    #[inline]
    #[must_use]
    pub fn new(config: Config) -> Self {
        let (sender, core_task) = Self::start_inner(config.clone());

        Driver {
            config,
            self_mute: false,
            sender,
            core_task: Some(core_task),
            #[cfg(feature = "builtin-queue")]
            queue: Some(TrackQueue::default()),
        }
    }

    fn start_inner(config: Config) -> (Sender<CoreMessage>, LcrCoreTaskCompletion) {
        let (tx, rx) = flume::unbounded();

        let core_task = LcrCoreTaskCompletion {
            task: tasks::start(config, rx, tx.clone()),
        };

        (tx, core_task)
    }

    /// Takes ownership of this Driver core task completion handle.
    ///
    /// This is a fork-specific lifecycle seam for LCR. The completion
    /// token is take-once: subsequent calls return `None`. The token
    /// grants observation only and cannot abort or rebuild the core.
    #[doc(hidden)]
    pub fn lcr_take_core_task_completion(
        &mut self,
    ) -> Option<LcrCoreTaskCompletion> {
        self.core_task.take()
    }

    /// Connects to a voice channel using the specified server.
    ///
    /// This method instantly contacts the driver tasks, and its
    /// does not need to be `await`ed to start the actual connection.
    #[instrument(skip(self))]
    pub fn connect(&mut self, info: ConnectionInfo) -> Connect {
        let (tx, rx) = flume::bounded(1);

        self.raw_connect(info, tx);

        Connect {
            inner: rx.into_recv_async(),
        }
    }

    /// Connects to a voice channel using the specified server.
    #[instrument(skip(self))]
    pub(crate) fn raw_connect(&mut self, info: ConnectionInfo, tx: Sender<Result<()>>) {
        if let Err(SendError(CoreMessage::ConnectWithResult(_, tx))) =
            self.sender.send(CoreMessage::ConnectWithResult(info, tx))
        {
            drop(tx.send(Err(Error::AttemptDiscarded)));
        }
    }

    /// Leaves the current voice channel, disconnecting from it.
    ///
    /// This does *not* forget settings, like whether to be self-deafened or
    /// self-muted.
    #[instrument(skip(self))]
    pub fn leave(&mut self) {
        self.send(CoreMessage::Disconnect);
    }

    /// Controlled fault-injection diagnostic for external transport recovery tests.
    ///
    /// Returns proof-only facts about the driver core immediately before the
    /// native reconnect algorithm runs.
    #[cfg(feature = "lcr-controlled-fault")]
    #[doc(hidden)]
    pub async fn lcr_controlled_reconnect(&mut self) -> LcrControlledReconnectDiagnostic {
        self.lcr_controlled_reconnect_inner(false).await
    }

    /// Controlled diagnostic that forces the first native reconnect attempt
    /// through the existing InterconnectFailure(AuxNetwork) rebuild branch.
    #[cfg(feature = "lcr-controlled-fault")]
    #[doc(hidden)]
    pub async fn lcr_controlled_reconnect_force_first_interconnect_failure(
        &mut self,
    ) -> LcrControlledReconnectDiagnostic {
        self.lcr_controlled_reconnect_inner(true).await
    }

    #[cfg(feature = "lcr-controlled-fault")]
    async fn lcr_controlled_reconnect_inner(
        &mut self,
        force_first_interconnect_failure: bool,
    ) -> LcrControlledReconnectDiagnostic {
        let (probe_tx, probe_rx) = flume::bounded(1);

        let sender_alive_before_send =
            match self.sender.send(CoreMessage::LcrControlledReconnect(
                probe_tx,
                force_first_interconnect_failure,
            )) {
                Ok(()) => true,
                Err(SendError(CoreMessage::LcrControlledReconnect(_, _))) => false,
                Err(_) => {
                    unreachable!("controlled reconnect send returns its original message")
                }
            };

        let core_connection_present =
            tokio::time::timeout(
                std::time::Duration::from_secs(2),
                probe_rx.recv_async(),
            )
            .await
            .ok()
            .and_then(std::result::Result::ok);

        LcrControlledReconnectDiagnostic {
            sender_alive_before_send,
            core_connection_present,
        }
    }

    /// Sets whether the current connection is to be muted.
    ///
    /// If there is no live voice connection, then this only acts as a settings
    /// update for future connections.
    #[instrument(skip(self))]
    pub fn mute(&mut self, mute: bool) {
        self.self_mute = mute;
        self.send(CoreMessage::Mute(mute));
    }

    /// Returns whether the driver is muted (i.e., processes audio internally
    /// but submits none).
    #[instrument(skip(self))]
    pub fn is_mute(&self) -> bool {
        self.self_mute
    }

    /// Plays audio from an input, returning a handle for further control.
    #[instrument(skip(self, input))]
    pub fn play_input(&mut self, input: Input) -> TrackHandle {
        self.play(input.into())
    }

    /// Plays audio from an input, returning a handle for further control.
    ///
    /// Unlike [`Self::play_input`], this stops all other inputs attached
    /// to the channel.
    #[instrument(skip(self, input))]
    pub fn play_only_input(&mut self, input: Input) -> TrackHandle {
        self.play_only(input.into())
    }

    /// Exclusively installs a synchronous encoded-Opus source.
    ///
    /// Songbird's existing mixer remains the sole pacing, RTP, DAVE, encryption,
    /// UDP, reconnect, and speaking-lifecycle owner. Installing this source
    /// removes any generic tracks already attached to the mixer.
    #[cfg(feature = "lcr-raw-opus-source")]
    #[instrument(skip(self, source))]
    pub fn play_only_raw_opus<S>(&mut self, source: S) -> RawOpusHandle
    where
        S: RawOpusSource,
    {
        let (handle, context) = raw_opus::RawOpusContext::new(source);
        self.send(CoreMessage::SetRawOpusSource(Some(Box::new(context))));
        handle
    }

    /// Fallible LCR seam for installing one synchronous encoded-Opus source.
    ///
    /// Returns [`LcrCoreUnavailable`] if this exact Driver core can no longer
    /// accept commands. No replacement core is created.
    #[cfg(feature = "lcr-raw-opus-source")]
    #[doc(hidden)]
    #[instrument(skip(self, source))]
    pub fn lcr_try_play_only_raw_opus<S>(
        &mut self,
        source: S,
    ) -> std::result::Result<RawOpusHandle, LcrCoreUnavailable>
    where
        S: RawOpusSource,
    {
        let (handle, context) = raw_opus::RawOpusContext::new(source);

        self.try_send(CoreMessage::SetRawOpusSource(Some(Box::new(context))))?;

        Ok(handle)
    }

    /// Plays audio from a [`Track`] object.
    ///
    /// The main difference between this function and [`Self::play_input`] is
    /// that this allows for direct manipulation of the [`Track`] object
    /// before it is passed over to the voice and mixing contexts.
    #[instrument(skip(self, track))]
    pub fn play(&mut self, track: Track) -> TrackHandle {
        let (handle, ctx) = track.into_context();
        self.send(CoreMessage::AddTrack(Box::new(ctx)));

        handle
    }

    /// Exclusively plays audio from a [`Track`] object.
    ///
    /// As in [`Self::play_only_input`], this stops all other sources attached to the
    /// channel. Like [`Self::play`], however, this allows for direct manipulation of the
    /// [`Track`] object before it is passed over to the voice and mixing contexts.
    #[instrument(skip(self, track))]
    pub fn play_only(&mut self, track: Track) -> TrackHandle {
        let (handle, ctx) = track.into_context();
        self.send(CoreMessage::SetTrack(Some(Box::new(ctx))));

        handle
    }

    /// Sets the bitrate for encoding Opus packets sent along
    /// the channel being managed.
    ///
    /// The default rate is 128 kbps.
    /// Sensible values range between `Bits(512)` and `Bits(512_000)`
    /// bits per second.
    /// Alternatively, `Auto` and `Max` remain available.
    #[instrument(skip(self))]
    pub fn set_bitrate(&mut self, bitrate: Bitrate) {
        self.send(CoreMessage::SetBitrate(bitrate));
    }

    /// Stops playing audio from all sources, if any are set.
    #[instrument(skip(self))]
    pub fn stop(&mut self) {
        self.send(CoreMessage::SetTrack(None));
    }

    /// Sets the configuration for this driver (and parent `Call`, if applicable).
    #[instrument(skip(self))]
    pub fn set_config(&mut self, config: Config) {
        self.config = config.clone();
        self.send(CoreMessage::SetConfig(config));
    }

    /// Returns a view of this driver's configuration.
    #[instrument(skip(self))]
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Attach a global event handler to an audio context. Global events may receive
    /// any [`EventContext`].
    ///
    /// Global timing events will tick regardless of whether audio is playing,
    /// so long as the bot is connected to a voice channel, and have no tracks.
    /// [`TrackEvent`]s will respond to all relevant tracks, giving some audio elements.
    ///
    /// Users **must** ensure that no costly work or blocking occurs
    /// within the supplied function or closure. *Taking excess time could prevent
    /// timely sending of packets, causing audio glitches and delays*.
    ///
    /// [`TrackEvent`]: crate::events::TrackEvent
    /// [`EventContext`]: crate::events::EventContext
    #[instrument(skip(self, action))]
    pub fn add_global_event<F: EventHandler + 'static>(&mut self, event: Event, action: F) {
        self.send(CoreMessage::AddEvent(EventData::new(event, action)));
    }

    /// Attach a persistent core event handler to this driver.
    ///
    /// Unlike [`Self::add_global_event`], this subscription is replayed when
    /// Songbird rebuilds only its volatile event processor. This does not
    /// preserve the subscription across a full Driver/core-task restart.
    ///
    /// This API is intentionally limited to [`CoreEvent`] listeners.
    #[instrument(skip(self, action))]
    pub fn add_persistent_core_event<F: EventHandler + 'static>(
        &mut self,
        event: CoreEvent,
        action: F,
    ) {
        self.send(CoreMessage::AddPersistentCoreEvent(
            PersistentCoreEvent::new(event, action),
        ));
    }

    /// Fallible LCR seam for installing a persistent core-event handler.
    ///
    /// Success means the command was accepted by this exact Driver core.
    /// It does not add retry or reconstruction authority.
    #[doc(hidden)]
    #[instrument(skip(self, action))]
    pub fn lcr_try_add_persistent_core_event<F: EventHandler + 'static>(
        &mut self,
        event: CoreEvent,
        action: F,
    ) -> std::result::Result<(), LcrCoreUnavailable> {
        self.try_send(CoreMessage::AddPersistentCoreEvent(
            PersistentCoreEvent::new(event, action),
        ))
    }

    /// Removes all global event handlers from an audio context.
    #[instrument(skip(self))]
    pub fn remove_all_global_events(&mut self) {
        self.send(CoreMessage::RemoveGlobalEvents);
    }

    /// Fallible LCR seam for clearing global and persistent core events.
    ///
    /// Success means the reset command was accepted by this exact Driver core.
    #[doc(hidden)]
    #[instrument(skip(self))]
    pub fn lcr_try_remove_all_global_events(
        &mut self,
    ) -> std::result::Result<(), LcrCoreUnavailable> {
        self.try_send(CoreMessage::RemoveGlobalEvents)
    }

    /// Sends a command only if this exact Driver core is still accepting work.
    fn try_send(
        &self,
        status: CoreMessage,
    ) -> std::result::Result<(), LcrCoreUnavailable> {
        self.sender.send(status).map_err(|_| LcrCoreUnavailable)
    }

    /// Sends a best-effort message to the inner tasks.
    ///
    /// A dead Driver core is terminal for this Driver lifetime. Generic
    /// void-style APIs fail closed and never reconstruct a replacement core.
    fn send(&mut self, status: CoreMessage) {
        let _ = self.try_send(status);
    }
}

#[cfg(feature = "builtin-queue")]
impl Driver {
    /// Returns a reference to this driver's built-in queue.
    ///
    /// Requires the `"builtin-queue"` feature.
    /// Queue additions should be made via [`Driver::enqueue`] and
    /// [`Driver::enqueue_input`].
    #[must_use]
    pub fn queue(&self) -> &TrackQueue {
        self.queue
            .as_ref()
            .expect("Queue: The only case this can fail is if a previous queue operation panicked.")
    }

    /// Adds an audio [`Input`] to this driver's built-in queue.
    ///
    /// Requires the `"builtin-queue"` feature.
    pub async fn enqueue_input(&mut self, input: Input) -> TrackHandle {
        self.enqueue(input.into()).await
    }

    /// Adds an existing [`Track`] to this driver's built-in queue.
    ///
    /// Requires the `"builtin-queue"` feature.
    pub async fn enqueue(&mut self, mut track: Track) -> TrackHandle {
        let preload_time = TrackQueue::get_preload_time(&mut track).await;
        self.enqueue_with_preload(track, preload_time)
    }

    /// Add an existing [`Track`] to the queue, using a known time to preload the next track.
    ///
    /// See [`TrackQueue::add_with_preload`] for how `preload_time` is used.
    ///
    /// Requires the `"builtin-queue"` feature.
    pub fn enqueue_with_preload(
        &mut self,
        track: Track,
        preload_time: Option<Duration>,
    ) -> TrackHandle {
        let queue = self.queue.take().expect(
            "Enqueue: The only case this can fail is if a previous queue operation panicked.",
        );
        let handle = queue.add_with_preload(track, self, preload_time);
        self.queue = Some(queue);

        handle
    }
}

#[cfg(all(test, feature = "lcr-controlled-fault"))]
#[tokio::test]
async fn lcr_controlled_reconnect_sends_native_reconnect_message() {
    let (sender, receiver) = flume::unbounded();

    let mut driver = Driver {
        config: Config::default(),
        self_mute: false,
        sender,
        core_task: None,
        #[cfg(feature = "builtin-queue")]
        queue: Some(TrackQueue::default()),
    };

    let responder = async {
        match receiver
            .recv_async()
            .await
            .expect("controlled reconnect message")
        {
            CoreMessage::LcrControlledReconnect(probe, force) => {
                assert!(!force);
                probe.send(true).expect("controlled reconnect probe response");
            }
            _ => panic!("unexpected controlled reconnect message"),
        }
    };

    let (diagnostic, ()) =
        tokio::join!(driver.lcr_controlled_reconnect(), responder);

    assert!(diagnostic.sender_alive_before_send);
    assert_eq!(diagnostic.core_connection_present, Some(true));
}

#[cfg(all(test, feature = "lcr-controlled-fault"))]
#[tokio::test]
async fn lcr_controlled_reconnect_can_force_first_interconnect_failure() {
    let (sender, receiver) = flume::unbounded();

    let mut driver = Driver {
        config: Config::default(),
        self_mute: false,
        sender,
        core_task: None,
        #[cfg(feature = "builtin-queue")]
        queue: Some(TrackQueue::default()),
    };

    let responder = async {
        match receiver
            .recv_async()
            .await
            .expect("controlled reconnect message")
        {
            CoreMessage::LcrControlledReconnect(probe, force) => {
                assert!(force);
                probe.send(true).expect("controlled reconnect probe response");
            }
            _ => panic!("unexpected controlled reconnect message"),
        }
    };

    let (diagnostic, ()) = tokio::join!(
        driver.lcr_controlled_reconnect_force_first_interconnect_failure(),
        responder
    );

    assert!(diagnostic.sender_alive_before_send);
    assert_eq!(diagnostic.core_connection_present, Some(true));
}

#[cfg(all(test, feature = "lcr-controlled-fault"))]
#[tokio::test]
async fn lcr_controlled_reconnect_force_is_request_local() {
    let (sender, receiver) = flume::unbounded();

    let mut driver = Driver {
        config: Config::default(),
        self_mute: false,
        sender,
        core_task: None,
        #[cfg(feature = "builtin-queue")]
        queue: Some(TrackQueue::default()),
    };

    let responder = async {
        match receiver
            .recv_async()
            .await
            .expect("forced controlled reconnect message")
        {
            CoreMessage::LcrControlledReconnect(probe, force) => {
                assert!(force);
                probe
                    .send(true)
                    .expect("forced controlled reconnect probe response");
            }
            _ => panic!("unexpected forced controlled reconnect message"),
        }

        match receiver
            .recv_async()
            .await
            .expect("normal controlled reconnect message")
        {
            CoreMessage::LcrControlledReconnect(probe, force) => {
                assert!(!force);
                probe
                    .send(true)
                    .expect("normal controlled reconnect probe response");
            }
            _ => panic!("unexpected normal controlled reconnect message"),
        }
    };

    let requests = async {
        let forced = driver
            .lcr_controlled_reconnect_force_first_interconnect_failure()
            .await;

        assert!(forced.sender_alive_before_send);
        assert_eq!(forced.core_connection_present, Some(true));

        let normal = driver.lcr_controlled_reconnect().await;

        assert!(normal.sender_alive_before_send);
        assert_eq!(normal.core_connection_present, Some(true));
    };

    let ((), ()) = tokio::join!(requests, responder);
}

#[cfg(all(test, feature = "lcr-controlled-fault"))]
#[tokio::test]
async fn lcr_controlled_reconnect_reports_empty_live_core() {
    let mut driver = Driver::new(Config::default());

    let diagnostic = driver.lcr_controlled_reconnect().await;

    assert!(diagnostic.sender_alive_before_send);
    assert_eq!(diagnostic.core_connection_present, Some(false));
}

#[cfg(all(test, feature = "lcr-controlled-fault"))]
#[tokio::test]
async fn lcr_controlled_reconnect_reports_dead_sender_without_restart() {
    let (sender, receiver) = flume::unbounded();

    drop(receiver);

    let mut driver = Driver {
        config: Config::default(),
        self_mute: false,
        sender,
        core_task: None,
        #[cfg(feature = "builtin-queue")]
        queue: Some(TrackQueue::default()),
    };

    let diagnostic = driver.lcr_controlled_reconnect().await;

    assert!(!diagnostic.sender_alive_before_send);
    assert_eq!(diagnostic.core_connection_present, None);
}
#[cfg(test)]
#[tokio::test]
async fn lcr_core_task_completion_is_take_once_and_observes_return() {
    let mut driver = Driver::new(Config::default());

    let completion = driver
        .lcr_take_core_task_completion()
        .expect("Driver owns one core completion token");

    assert!(driver.lcr_take_core_task_completion().is_none());

    drop(driver);

    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        completion.wait(),
    )
    .await
    .expect("Driver core terminates after Driver drop")
    .expect("Driver core returns without panic");
}

#[cfg(test)]
struct LcrCriticalNoopEventHandler;

#[cfg(test)]
#[async_trait::async_trait]
impl EventHandler for LcrCriticalNoopEventHandler {
    async fn act(
        &self,
        _ctx: &crate::events::EventContext<'_>,
    ) -> Option<Event> {
        None
    }
}

#[cfg(all(test, feature = "lcr-raw-opus-source"))]
struct LcrCriticalEmptyRawSource;

#[cfg(all(test, feature = "lcr-raw-opus-source"))]
impl RawOpusSource for LcrCriticalEmptyRawSource {
    fn pull_opus(&mut self, _dst: &mut [u8]) -> RawOpusRead {
        RawOpusRead::End
    }
}

#[cfg(all(test, feature = "lcr-raw-opus-source"))]
struct LcrCriticalDropObservedRawSource {
    dropped: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(all(test, feature = "lcr-raw-opus-source"))]
impl RawOpusSource for LcrCriticalDropObservedRawSource {
    fn pull_opus(&mut self, _dst: &mut [u8]) -> RawOpusRead {
        RawOpusRead::End
    }
}

#[cfg(all(test, feature = "lcr-raw-opus-source"))]
impl Drop for LcrCriticalDropObservedRawSource {
    fn drop(&mut self) {
        self.dropped
            .store(true, std::sync::atomic::Ordering::Release);
    }
}

#[cfg(test)]
fn lcr_critical_dead_core_driver() -> Driver {
    let (sender, receiver) = flume::unbounded();
    drop(receiver);

    Driver {
        config: Config::default(),
        self_mute: false,
        sender,
        core_task: None,
        #[cfg(feature = "builtin-queue")]
        queue: Some(TrackQueue::default()),
    }
}

#[cfg(all(test, feature = "lcr-raw-opus-source"))]
#[test]
fn lcr_critical_raw_install_rejects_dead_core() {
    let mut driver = lcr_critical_dead_core_driver();
    let dropped = std::sync::Arc::new(
        std::sync::atomic::AtomicBool::new(false),
    );

    let result = driver.lcr_try_play_only_raw_opus(
        LcrCriticalDropObservedRawSource {
            dropped: std::sync::Arc::clone(&dropped),
        },
    );

    assert!(matches!(result, Err(LcrCoreUnavailable)));
    assert!(
        dropped.load(std::sync::atomic::Ordering::Acquire),
        "failed raw install must deterministically drop its source context"
    );
    assert!(driver.core_task.is_none());
}

#[cfg(test)]
#[test]
fn lcr_critical_persistent_event_rejects_dead_core() {
    let mut driver = lcr_critical_dead_core_driver();

    let result = driver.lcr_try_add_persistent_core_event(
        CoreEvent::DriverDisconnect,
        LcrCriticalNoopEventHandler,
    );

    assert_eq!(result, Err(LcrCoreUnavailable));
    assert!(driver.core_task.is_none());
}

#[cfg(test)]
#[test]
fn lcr_critical_event_reset_rejects_dead_core() {
    let mut driver = lcr_critical_dead_core_driver();

    let result = driver.lcr_try_remove_all_global_events();

    assert_eq!(result, Err(LcrCoreUnavailable));
    assert!(driver.core_task.is_none());
}

#[cfg(all(test, feature = "lcr-raw-opus-source"))]
#[tokio::test]
async fn lcr_critical_raw_install_accepts_live_core() {
    let mut driver = Driver::new(Config::default());

    let result = driver.lcr_try_play_only_raw_opus(
        LcrCriticalEmptyRawSource,
    );

    assert!(result.is_ok());
}

#[cfg(test)]
#[tokio::test]
async fn lcr_critical_event_seams_accept_live_core() {
    let mut driver = Driver::new(Config::default());

    assert_eq!(
        driver.lcr_try_add_persistent_core_event(
            CoreEvent::DriverDisconnect,
            LcrCriticalNoopEventHandler,
        ),
        Ok(())
    );

    assert_eq!(
        driver.lcr_try_remove_all_global_events(),
        Ok(())
    );
}

#[cfg(test)]
fn lcr_test_connection_info() -> ConnectionInfo {
    let one = std::num::NonZeroU64::new(1)
        .expect("test Discord ID is non-zero");

    ConnectionInfo {
        channel_id: crate::id::ChannelId(one),
        endpoint: "127.0.0.1".to_owned(),
        guild_id: crate::id::GuildId(one),
        session_id: "lcr-r0b-test-session".to_owned(),
        token: "lcr-r0b-test-token".to_owned(),
        user_id: crate::id::UserId(one),
    }
}

#[cfg(test)]
#[test]
fn lcr_generic_command_does_not_resurrect_dead_core() {
    let mut driver = lcr_critical_dead_core_driver();

    driver.leave();

    assert!(driver.core_task.is_none());
    assert!(
        driver.sender.send(CoreMessage::Mute(false)).is_err(),
        "generic command must not replace the dead core sender"
    );
}

#[cfg(test)]
#[tokio::test]
async fn lcr_connect_resolves_attempt_discarded_on_dead_core() {
    let mut driver = lcr_critical_dead_core_driver();

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        driver.connect(lcr_test_connection_info()),
    )
    .await
    .expect("dead-core connect must resolve without hanging");

    assert!(
        matches!(result, Err(Error::AttemptDiscarded)),
        "dead-core connect must surface AttemptDiscarded"
    );
    assert!(driver.core_task.is_none());
}

#[cfg(test)]
#[tokio::test]
async fn lcr_core_task_completion_observes_test_panic() {
    let completion = LcrCoreTaskCompletion {
        task: tokio::spawn(async {
            panic!("deterministic LCR core-completion panic proof");
        }),
    };

    let error = completion
        .wait()
        .await
        .expect_err("test task must complete by panic");

    assert!(
        error.is_panic(),
        "completion observer must surface panic rather than detach silently"
    );
}

impl Default for Driver {
    fn default() -> Self {
        Self::new(Config::default())
    }
}

impl Drop for Driver {
    /// Leaves the current connected voice channel, if connected to one, and
    /// forgets all configurations relevant to this Handler.
    fn drop(&mut self) {
        drop(self.sender.send(CoreMessage::Poison));
    }
}

/// Future for a call to [`Driver::connect`].
///
/// This future awaits the *result* of a connection; the driver
/// is messaged at the time of the call.
pub struct Connect {
    inner: RecvFut<'static, Result<()>>,
}

impl Future for Connect {
    type Output = Result<()>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.inner).poll(cx) {
            Poll::Ready(r) => Poll::Ready(r.map_err(|_| Error::AttemptDiscarded).and_then(|x| x)),
            Poll::Pending => Poll::Pending,
        }
    }
}
