#![allow(missing_docs)]

pub(crate) mod disposal;
pub mod error;
mod events;
pub mod message;
pub mod mixer;
#[cfg(feature = "receive")]
pub(crate) mod udp_rx;
pub(crate) mod ws;

use super::connection::{error::Error as ConnectionError, Connection};
use crate::{
    events::{
        context_data::{DisconnectKind, DisconnectReason},
        internal_data::{InternalConnect, InternalDisconnect},
        CoreContext,
    },
    Config,
    ConnectionInfo,
    FloatDuration,
};
use flume::{Receiver, Sender};
use message::*;
use tokio::{spawn, time::sleep as tsleep};
use tracing::{debug, instrument, trace};

pub(crate) fn start(config: Config, rx: Receiver<CoreMessage>, tx: Sender<CoreMessage>) {
    spawn(async move {
        trace!("Driver started.");
        runner(config, rx, tx).await;
        trace!("Driver finished.");
    });
}

fn start_internals(core: Sender<CoreMessage>, config: &Config) -> Interconnect {
    let (evt_tx, evt_rx) = flume::unbounded();
    let (mix_tx, mix_rx) = flume::unbounded();

    spawn(async move {
        trace!("Event processor started.");
        events::runner(evt_rx).await;
        trace!("Event processor finished.");
    });

    let ic = Interconnect {
        core,
        events: evt_tx,
        mixer: mix_tx,
    };

    config.get_scheduler().new_mixer(config, ic.clone(), mix_rx);
    ic
}

#[instrument(skip(rx, tx))]
async fn runner(mut config: Config, rx: Receiver<CoreMessage>, tx: Sender<CoreMessage>) {
    let mut next_config: Option<Config> = None;
    let mut connection: Option<Connection> = None;
    let mut interconnect = start_internals(tx, &config);
    let mut retrying = None;
    let mut attempt_idx = 0;
    let mut persistent_core_events = Vec::new();

    #[cfg(feature = "lcr-controlled-fault")]
    let mut lcr_reconnect_phase_seq = 0_u64;

    while let Ok(msg) = rx.recv_async().await {
        #[cfg(feature = "lcr-controlled-fault")]
        let (msg, lcr_force_first_interconnect_failure) = match msg {
            CoreMessage::LcrControlledReconnect(
                probe,
                force_first_interconnect_failure,
            ) => {
                let connection_present = connection.is_some();

                let _ = probe.send(connection_present);

                if !connection_present {
                    continue;
                }

                (
                    CoreMessage::Reconnect,
                    force_first_interconnect_failure,
                )
            },
            msg => (msg, false),
        };

        #[cfg(not(feature = "lcr-controlled-fault"))]
        let msg = msg;

        match msg {
            CoreMessage::ConnectWithResult(info, tx) => {
                config = if let Some(new_config) = next_config.take() {
                    drop(
                        interconnect
                            .mixer
                            .send(MixerMessage::SetConfig(new_config.clone())),
                    );
                    new_config
                } else {
                    config
                };

                if connection.as_ref().is_none_or(|conn| conn.info != info) {
                    // Only *actually* reconnect if the conn info changed, or we don't have an
                    // active connection.
                    // This allows the gateway component to keep sending join requests independent
                    // of driver failures.
                    connection = ConnectionRetryData::connect(tx, info, &mut attempt_idx)
                        .attempt(&mut retrying, &interconnect, &config)
                        .await;
                } else {
                    // No reconnection was attempted as there's a valid, identical connection;
                    // tell the outside listener that the operation was a success.
                    drop(tx.send(Ok(())));
                }
            },
            CoreMessage::RetryConnect(retry_idx) => {
                debug!("Retrying idx: {} (vs. {})", retry_idx, attempt_idx);
                if retry_idx == attempt_idx {
                    if let Some(progress) = retrying.take() {
                        connection = progress
                            .attempt(&mut retrying, &interconnect, &config)
                            .await;
                    }
                }
            },
            CoreMessage::Disconnect => {
                let last_conn = connection.take();
                drop(interconnect.mixer.send(MixerMessage::DropConn));
                drop(interconnect.mixer.send(MixerMessage::RebuildEncoder));

                if let Some(conn) = last_conn {
                    drop(interconnect.events.send(EventMessage::FireCoreEvent(
                        CoreContext::DriverDisconnect(InternalDisconnect {
                            kind: DisconnectKind::Runtime,
                            reason: Some(DisconnectReason::Requested),
                            info: conn.info.clone(),
                        }),
                    )));
                }
            },
            CoreMessage::SignalWsClosure(ws_idx, ws_info, mut reason) => {
                // if idx is not a match, quash reason
                // (i.e., prevent users from mistakenly trying to reconnect for an *old* dead conn).
                // if it *is* a match, the conn needs to die!
                // (as the WS channel has truly given up the ghost).
                let conn = if ws_idx == attempt_idx {
                    drop(interconnect.mixer.send(MixerMessage::DropConn));
                    drop(interconnect.mixer.send(MixerMessage::RebuildEncoder));
                    connection.take()
                } else {
                    reason = None;
                    None
                };

                // Conn may have been unset earlier (i.e., in a deliberate disconnect).
                // If so, do not repropagate/repeat the disconnect event.
                if conn.is_some() {
                    drop(interconnect.events.send(EventMessage::FireCoreEvent(
                        CoreContext::DriverDisconnect(InternalDisconnect {
                            kind: DisconnectKind::Runtime,
                            reason,
                            info: ws_info,
                        }),
                    )));
                }
            },
            CoreMessage::SetTrack(s) => {
                drop(interconnect.mixer.send(MixerMessage::SetTrack(s)));
            },
            CoreMessage::AddTrack(s) => {
                drop(interconnect.mixer.send(MixerMessage::AddTrack(s)));
            },
            #[cfg(feature = "lcr-raw-opus-source")]
            CoreMessage::SetRawOpusSource(s) => {
                drop(interconnect.mixer.send(MixerMessage::SetRawOpusSource(s)));
            },
            CoreMessage::SetBitrate(b) => {
                drop(interconnect.mixer.send(MixerMessage::SetBitrate(b)));
            },
            CoreMessage::SetConfig(mut new_config) => {
                next_config = Some(new_config.clone());

                new_config.make_safe(&config, connection.is_some());

                drop(interconnect.mixer.send(MixerMessage::SetConfig(new_config)));
            },
            CoreMessage::AddEvent(evt) => {
                drop(interconnect.events.send(EventMessage::AddGlobalEvent(evt)));
            },
            CoreMessage::AddPersistentCoreEvent(evt) => {
                let event_data = evt.event_data();
                persistent_core_events.push(evt);
                drop(
                    interconnect
                        .events
                        .send(EventMessage::AddGlobalEvent(event_data)),
                );
            },
            CoreMessage::RemoveGlobalEvents => {
                persistent_core_events.clear();
                drop(interconnect.events.send(EventMessage::RemoveGlobalEvents));
            },
            CoreMessage::Mute(m) => {
                drop(interconnect.mixer.send(MixerMessage::SetMute(m)));
            },
            CoreMessage::Reconnect => {
                #[cfg(feature = "lcr-controlled-fault")]
                {
                    lcr_reconnect_phase_seq = lcr_reconnect_phase_seq.wrapping_add(1);

                    eprintln!(
                        "LCR_G3_RECONNECT_PHASE seq={} phase=ARM_ENTER connection_present={}",
                        lcr_reconnect_phase_seq,
                        connection.is_some()
                    );
                }

                if let Some(mut conn) = connection.take() {
                    // try once: if interconnect, try again.
                    // if still issue, full connect.
                    let info = conn.info.clone();

                    #[cfg(feature = "lcr-controlled-fault")]
                    eprintln!(
                        "LCR_G3_RECONNECT_PHASE seq={} phase=FIRST_RECONNECT_BEGIN",
                        lcr_reconnect_phase_seq
                    );

                    #[cfg(feature = "lcr-controlled-fault")]
                    let first_reconnect = if lcr_force_first_interconnect_failure {
                        eprintln!("LCR_G3_RECONNECT_PHASE seq={lcr_reconnect_phase_seq} phase=FORCED_INTERCONNECT_FAILURE");
                        Err(ConnectionError::InterconnectFailure(
                            error::Recipient::AuxNetwork,
                        ))
                    } else {
                        conn.reconnect(&config).await
                    };

                    #[cfg(not(feature = "lcr-controlled-fault"))]
                    let first_reconnect = conn.reconnect(&config).await;

                    let full_connect = match first_reconnect {
                        Ok(()) => {
                            #[cfg(feature = "lcr-controlled-fault")]
                            eprintln!(
                                "LCR_G3_RECONNECT_PHASE seq={} phase=FIRST_RECONNECT_OK",
                                lcr_reconnect_phase_seq
                            );

                            connection = Some(conn);
                            false
                        },
                        Err(ConnectionError::InterconnectFailure(_)) => {
                            #[cfg(feature = "lcr-controlled-fault")]
                            eprintln!(
                                "LCR_G3_RECONNECT_PHASE seq={} phase=FIRST_RECONNECT_INTERCONNECT_FAILURE",
                                lcr_reconnect_phase_seq
                            );

                            interconnect.restart_volatile_internals(&persistent_core_events);

                            #[cfg(feature = "lcr-controlled-fault")]
                            eprintln!(
                                "LCR_G3_RECONNECT_PHASE seq={} phase=EVENT_PROCESSOR_RESTART",
                                lcr_reconnect_phase_seq
                            );

                            #[cfg(feature = "lcr-controlled-fault")]
                            eprintln!(
                                "LCR_G3_RECONNECT_PHASE seq={} phase=SECOND_RECONNECT_BEGIN",
                                lcr_reconnect_phase_seq
                            );

                            match conn.reconnect(&config).await {
                                Ok(()) => {
                                    #[cfg(feature = "lcr-controlled-fault")]
                                    eprintln!(
                                        "LCR_G3_RECONNECT_PHASE seq={} phase=SECOND_RECONNECT_OK",
                                        lcr_reconnect_phase_seq
                                    );

                                    connection = Some(conn);
                                    false
                                },
                                _ => {
                                    #[cfg(feature = "lcr-controlled-fault")]
                                    eprintln!(
                                        "LCR_G3_RECONNECT_PHASE seq={} phase=SECOND_RECONNECT_ERR",
                                        lcr_reconnect_phase_seq
                                    );

                                    true
                                },
                            }
                        },
                        _ => {
                            #[cfg(feature = "lcr-controlled-fault")]
                            eprintln!(
                                "LCR_G3_RECONNECT_PHASE seq={} phase=FIRST_RECONNECT_ERR",
                                lcr_reconnect_phase_seq
                            );

                            true
                        },
                    };

                    if full_connect {
                        #[cfg(feature = "lcr-controlled-fault")]
                        eprintln!(
                            "LCR_G3_RECONNECT_PHASE seq={} phase=FULL_RECONNECT_BEGIN",
                            lcr_reconnect_phase_seq
                        );

                        connection = ConnectionRetryData::reconnect(info, &mut attempt_idx)
                            .attempt(&mut retrying, &interconnect, &config)
                            .await;

                        #[cfg(feature = "lcr-controlled-fault")]
                        {
                            let phase = if connection.is_some() {
                                "FULL_RECONNECT_CONNECTED"
                            } else if retrying.is_some() {
                                "FULL_RECONNECT_RETRY_SCHEDULED"
                            } else {
                                "FULL_RECONNECT_TERMINAL"
                            };

                            eprintln!(
                                "LCR_G3_RECONNECT_PHASE seq={} phase={}",
                                lcr_reconnect_phase_seq,
                                phase
                            );
                        }
                    } else if let Some(ref connection) = &connection {
                        let event_send =
                            interconnect
                                .events
                                .send(EventMessage::FireCoreEvent(
                                    CoreContext::DriverReconnect(InternalConnect {
                                        info: connection.info.clone(),
                                        ssrc: connection.ssrc,
                                    }),
                                ));

                        #[cfg(feature = "lcr-controlled-fault")]
                        eprintln!(
                            "LCR_G3_RECONNECT_PHASE seq={} phase=DRIVER_RECONNECT_EVENT_SEND status={}",
                            lcr_reconnect_phase_seq,
                            if event_send.is_ok() { "OK" } else { "ERR" }
                        );

                        drop(event_send);
                    }
                } else {
                    #[cfg(feature = "lcr-controlled-fault")]
                    eprintln!(
                        "LCR_G3_RECONNECT_PHASE seq={} phase=ARM_NO_CONNECTION",
                        lcr_reconnect_phase_seq
                    );
                }
            },
            #[cfg(feature = "lcr-controlled-fault")]
            CoreMessage::LcrControlledReconnect(_, _) => {
                unreachable!("controlled reconnect is normalized before dispatch")
            },
            CoreMessage::FullReconnect =>
                if let Some(conn) = connection.take() {
                    let info = conn.info.clone();

                    connection = ConnectionRetryData::reconnect(info, &mut attempt_idx)
                        .attempt(&mut retrying, &interconnect, &config)
                        .await;
                },
            CoreMessage::RebuildInterconnect => {
                interconnect.restart_volatile_internals(&persistent_core_events);
            },
            #[cfg(test)]
            CoreMessage::TestFireCoreEvent(context) => {
                drop(
                    interconnect
                        .events
                        .send(EventMessage::FireCoreEvent(context)),
                );
            },
            CoreMessage::Poison => break,
        }
    }

    trace!("Main thread exited");
    interconnect.poison_all();
}

struct ConnectionRetryData {
    flavour: ConnectionFlavour,
    attempts: u8,
    last_wait: Option<FloatDuration>,
    info: ConnectionInfo,
    idx: usize,
}

impl ConnectionRetryData {
    fn connect(
        tx: Sender<Result<(), ConnectionError>>,
        info: ConnectionInfo,
        idx_src: &mut usize,
    ) -> Self {
        Self::base(ConnectionFlavour::Connect(tx), info, idx_src)
    }

    fn reconnect(info: ConnectionInfo, idx_src: &mut usize) -> Self {
        Self::base(ConnectionFlavour::Reconnect, info, idx_src)
    }

    fn base(flavour: ConnectionFlavour, info: ConnectionInfo, idx_src: &mut usize) -> Self {
        *idx_src = idx_src.wrapping_add(1);

        Self {
            flavour,
            attempts: 0,
            last_wait: None,
            info,
            idx: *idx_src,
        }
    }

    async fn attempt(
        mut self,
        attempt_slot: &mut Option<Self>,
        interconnect: &Interconnect,
        config: &Config,
    ) -> Option<Connection> {
        match Connection::new(self.info.clone(), interconnect, config, self.idx).await {
            Ok(connection) => {
                match self.flavour {
                    ConnectionFlavour::Connect(tx) => {
                        // Other side may not be listening: this is fine.
                        drop(tx.send(Ok(())));

                        drop(interconnect.events.send(EventMessage::FireCoreEvent(
                            CoreContext::DriverConnect(InternalConnect {
                                info: connection.info.clone(),
                                ssrc: connection.ssrc,
                            }),
                        )));
                    },
                    ConnectionFlavour::Reconnect => {
                        drop(interconnect.events.send(EventMessage::FireCoreEvent(
                            CoreContext::DriverReconnect(InternalConnect {
                                info: connection.info.clone(),
                                ssrc: connection.ssrc,
                            }),
                        )));
                    },
                }

                Some(connection)
            },
            Err(why) => {
                debug!("Failed to connect for {:?}: {}", self.info.guild_id, why);
                if let Some(t) = config.driver_retry.retry_in(self.last_wait, self.attempts) {
                    let remote_ic = interconnect.clone();
                    let idx = self.idx;

                    spawn(async move {
                        tsleep(t.into()).await;
                        drop(remote_ic.core.send(CoreMessage::RetryConnect(idx)));
                    });

                    self.attempts += 1;
                    self.last_wait = Some(t);

                    debug!(
                        "Retrying connection for {:?} in {}s ({}/{:?})",
                        self.info.guild_id,
                        t.as_secs_f32(),
                        self.attempts,
                        config.driver_retry.retry_limit
                    );

                    *attempt_slot = Some(self);
                } else {
                    let reason = Some(DisconnectReason::from(&why));

                    match self.flavour {
                        ConnectionFlavour::Connect(tx) => {
                            // See above.
                            drop(tx.send(Err(why)));

                            drop(interconnect.events.send(EventMessage::FireCoreEvent(
                                CoreContext::DriverDisconnect(InternalDisconnect {
                                    kind: DisconnectKind::Connect,
                                    reason,
                                    info: self.info,
                                }),
                            )));
                        },
                        ConnectionFlavour::Reconnect => {
                            drop(interconnect.events.send(EventMessage::FireCoreEvent(
                                CoreContext::DriverDisconnect(InternalDisconnect {
                                    kind: DisconnectKind::Reconnect,
                                    reason,
                                    info: self.info,
                                }),
                            )));
                        },
                    }
                }

                None
            },
        }
    }
}

enum ConnectionFlavour {
    Connect(Sender<Result<(), ConnectionError>>),
    Reconnect,
}

#[cfg(test)]
mod persistent_core_event_tests {
    use super::*;
    use crate::{
        events::{Event, EventContext, EventData, EventHandler},
        id::{ChannelId, GuildId, UserId},
        CoreEvent,
    };
    use std::{
        num::NonZeroU64,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
        time::Duration,
    };

    #[derive(Clone)]
    struct CountHandler {
        hits: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl EventHandler for CountHandler {
        async fn act(&self, _context: &EventContext<'_>) -> Option<Event> {
            self.hits.fetch_add(1, Ordering::SeqCst);
            None
        }
    }

    struct BarrierHandler {
        sender: Sender<()>,
    }

    #[async_trait::async_trait]
    impl EventHandler for BarrierHandler {
        async fn act(&self, _context: &EventContext<'_>) -> Option<Event> {
            let _ = self.sender.send(());
            None
        }
    }

    fn test_connection_info() -> ConnectionInfo {
        ConnectionInfo {
            channel_id: ChannelId(NonZeroU64::new(1).unwrap()),
            endpoint: "voice.example.invalid".to_owned(),
            guild_id: GuildId(NonZeroU64::new(2).unwrap()),
            session_id: "session".to_owned(),
            token: "token".to_owned(),
            user_id: UserId(NonZeroU64::new(3).unwrap()),
        }
    }

    fn driver_reconnect_context() -> CoreContext {
        CoreContext::DriverReconnect(InternalConnect {
            info: test_connection_info(),
            ssrc: 7,
        })
    }

    fn driver_connect_context() -> CoreContext {
        CoreContext::DriverConnect(InternalConnect {
            info: test_connection_info(),
            ssrc: 7,
        })
    }

    fn isolated_test_config() -> Config {
        Config::default().scheduler(crate::driver::Scheduler::new(
            crate::driver::SchedulerConfig::default(),
        ))
    }
    async fn fire_reconnect_then_barrier(tx: &Sender<CoreMessage>) {
        let (barrier_tx, barrier_rx) = flume::bounded(1);

        tx.send(CoreMessage::AddEvent(EventData::new(
            Event::Core(CoreEvent::DriverConnect),
            BarrierHandler { sender: barrier_tx },
        )))
        .unwrap();

        tx.send(CoreMessage::TestFireCoreEvent(
            driver_reconnect_context(),
        ))
        .unwrap();

        tx.send(CoreMessage::TestFireCoreEvent(
            driver_connect_context(),
        ))
        .unwrap();

        tokio::time::timeout(
            Duration::from_secs(2),
            barrier_rx.recv_async(),
        )
        .await
        .expect("event-processor barrier timed out")
        .expect("event-processor barrier channel closed");
    }

    async fn stop_runner(tx: &Sender<CoreMessage>, task: tokio::task::JoinHandle<()>) {
        tx.send(CoreMessage::Poison).unwrap();

        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("core runner shutdown timed out")
            .expect("core runner task failed");
    }

    #[tokio::test]
    async fn persistent_core_event_survives_rebuild_without_duplication() {
        let (tx, rx) = flume::unbounded();
        let task = tokio::spawn(runner(
            isolated_test_config(),
            rx,
            tx.clone(),
        ));

        let hits = Arc::new(AtomicUsize::new(0));

        tx.send(CoreMessage::AddPersistentCoreEvent(
            PersistentCoreEvent::new(
                CoreEvent::DriverReconnect,
                CountHandler {
                    hits: Arc::clone(&hits),
                },
            ),
        ))
        .unwrap();

        fire_reconnect_then_barrier(&tx).await;
        assert_eq!(hits.load(Ordering::SeqCst), 1);

        tx.send(CoreMessage::RebuildInterconnect).unwrap();
        fire_reconnect_then_barrier(&tx).await;
        assert_eq!(
            hits.load(Ordering::SeqCst),
            2,
            "first rebuild must replay exactly one persistent handler",
        );

        tx.send(CoreMessage::RebuildInterconnect).unwrap();
        fire_reconnect_then_barrier(&tx).await;
        assert_eq!(
            hits.load(Ordering::SeqCst),
            3,
            "second rebuild must not duplicate persistent handlers",
        );

        tx.send(CoreMessage::RemoveGlobalEvents).unwrap();
        tx.send(CoreMessage::RebuildInterconnect).unwrap();

        fire_reconnect_then_barrier(&tx).await;
        assert_eq!(
            hits.load(Ordering::SeqCst),
            3,
            "RemoveGlobalEvents must prevent persistent resurrection",
        );

        stop_runner(&tx, task).await;
    }

    #[tokio::test]
    async fn ordinary_global_event_remains_volatile_across_rebuild() {
        let (tx, rx) = flume::unbounded();
        let task = tokio::spawn(runner(
            isolated_test_config(),
            rx,
            tx.clone(),
        ));

        let hits = Arc::new(AtomicUsize::new(0));

        tx.send(CoreMessage::AddEvent(EventData::new(
            Event::Core(CoreEvent::DriverReconnect),
            CountHandler {
                hits: Arc::clone(&hits),
            },
        )))
        .unwrap();

        fire_reconnect_then_barrier(&tx).await;
        assert_eq!(hits.load(Ordering::SeqCst), 1);

        tx.send(CoreMessage::RebuildInterconnect).unwrap();

        fire_reconnect_then_barrier(&tx).await;
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "ordinary add_global_event must remain volatile",
        );

        stop_runner(&tx, task).await;
    }
}