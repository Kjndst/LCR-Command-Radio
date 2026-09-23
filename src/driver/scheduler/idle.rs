use std::{collections::HashMap, time::Duration};

use nohash_hasher::{BuildNoHashHasher, IntMap};
use tokio::time::{Instant as TokInstant, Interval};
use tracing::info;

use crate::constants::*;

use super::*;

const THREAD_CULL_TIMER: Duration = Duration::from_secs(60);

/// An async task responsible for maintaining UDP keepalives and event state for inactive
/// `Mixer` tasks.
pub(crate) struct Idle {
    config: Config,
    cull_timer: Duration,
    tasks: IntMap<TaskId, ParkedMixer>,
    // track taskids which are live to prevent their realloc? unlikely w u64 but still
    pub(crate) stats: Arc<StatBlock>,
    rx: Receiver<SchedulerMessage>,
    tx: Sender<SchedulerMessage>,
    next_id: TaskId,
    next_worker_id: WorkerId,
    workers: Vec<Worker>,
    to_cull: Vec<TaskId>,
}

impl Idle {
    pub fn new(config: Config) -> (Self, Sender<SchedulerMessage>) {
        let (tx, rx) = flume::unbounded();

        let stats = Arc::default();
        let tasks = HashMap::with_capacity_and_hasher(128, BuildNoHashHasher::default());

        // TODO: include heap of keepalive sending times?
        let out = Self {
            config,
            cull_timer: THREAD_CULL_TIMER,
            tasks,
            stats,
            rx,
            tx: tx.clone(),
            next_id: TaskId::new(),
            next_worker_id: WorkerId::new(),
            workers: Vec::with_capacity(16),
            to_cull: vec![],
        };

        (out, tx)
    }

    /// Run the inner task until all external `Scheduler` handles are dropped.
    async fn run(&mut self) {
        let mut interval = tokio::time::interval(TIMESTEP_LENGTH);
        while self.run_once(&mut interval).await {}
    }

    /// Run one 'tick' of idle thread maintenance.
    ///
    /// This is a priority system over 2 main tasks:
    ///  1) handle scheduling/upgrade/action requests for mixers
    ///  2) [every 20ms]tick the main timer for each task, send keepalive if
    ///     needed, reclaim & cull workers.
    ///
    /// Idle mixers spawn an async task each to forward their `MixerMessage`s
    /// on to this task to be handled by 1). These tasks self-terminate if a
    /// message would make a mixer `now_live`.
    async fn run_once(&mut self, interval: &mut Interval) -> bool {
        tokio::select! {
            biased;
            msg = self.rx.recv_async() => match msg {
                Ok(SchedulerMessage::NewMixer(rx, ic, cfg)) => {
                    let mut mixer = ParkedMixer::new(rx, ic, cfg);
                    let id = self.next_id.incr();

                    mixer.spawn_forwarder(self.tx.clone(), id);
                    self.tasks.insert(id, mixer);
                    self.stats.add_idle_mixer();
                },
                Ok(SchedulerMessage::Demote(id, mut task)) => {
                    task.send_gateway_not_speaking();

                    task.spawn_forwarder(self.tx.clone(), id);
                    self.tasks.insert(id, task);
                },
                Ok(SchedulerMessage::Do(id, mix_msg)) => {
                    let maybe_live = mix_msg.is_mixer_maybe_live();
                    if let Some(task) = self.tasks.get_mut(&id) {
                        match task.handle_message(mix_msg) {
                            Ok(false) if maybe_live => {
                                if task.mixer.has_live_media_source() {
                                    let task = self.tasks.remove(&id).unwrap();
                                    self.schedule_mixer(task, id, None);
                                } else {
                                    // No live media source, likely due to SetConn.
                                    // Recreate message forwarding task.
                                    task.spawn_forwarder(self.tx.clone(), id);
                                }
                            },
                            Ok(false) => {},
                            Ok(true) | Err(()) => self.to_cull.push(id),
                        }
                    } else {
                        info!("Received post-cull message for {id:?}, discarding.");
                    }
                },
                Ok(SchedulerMessage::Overspill(worker_id, id, task)) => {
                    self.schedule_mixer(task, id, Some(worker_id));
                },
                Ok(SchedulerMessage::GetStats(tx)) => {
                    _ = tx.send(self.workers.iter().map(Worker::stats).collect());
                },
                Ok(SchedulerMessage::Kill) | Err(_) => {
                    return false;
                },
            },
            _ = interval.tick() => {
                // TODO: store keepalive sends in another data structure so
                // we don't check every task every 20ms.
                //
                // if we can also make tick handling lazy(er), we can also optimise for that.
                let now = TokInstant::now();

                for (id, task) in &mut self.tasks {
                    // NOTE: this is a non-blocking send so safe from async context.
                    if task.tick_and_keepalive(now.into()).is_err() {
                        self.to_cull.push(*id);
                    }
                }

                let mut i = 0;
                while i < self.workers.len() {
                    if let Some(then) = self.workers[i].try_mark_empty(now) {
                        if now.duration_since(then) >= self.cull_timer {
                            self.workers.swap_remove(i);
                            continue;
                        }
                    }

                    i += 1;
                }
            },
        }

        for id in self.to_cull.drain(..) {
            if let Some(tx) = self.tasks.remove(&id).and_then(|t| t.cull_handle) {
                _ = tx.send_async(()).await;
            }
        }

        true
    }

    /// Promote a task to a live mixer thread.
    fn schedule_mixer(&mut self, mut task: ParkedMixer, id: TaskId, avoid: Option<WorkerId>) {
        if task.send_gateway_speaking().is_ok() {
            // If a worker ever completely fails, then we need to remove it here
            // `fetch_worker` will either find another, or generate us a new one if
            // none exist.

            // We need to track ownership of the task coming back via SendError using this
            // Option.
            let mut loop_task = Some(task);
            loop {
                let task = loop_task.take().unwrap();
                let (worker, idx) = self.fetch_worker(&task, avoid);
                match worker.schedule_mixer(id, task) {
                    Ok(()) => {
                        self.stats.move_mixer_to_live();
                        break;
                    },
                    Err(e) => {
                        loop_task = Some(e.0 .1);
                        let worker = self.workers.swap_remove(idx);

                        // NOTE: we have incremented worker's live counter for this mixer in
                        // `schedule_mixer`.
                        // The only time this branch is ever hit is if a worker crashed, so we
                        // need to replicate some of their cleanup.
                        self.stats
                            .remove_live_mixers(worker.stats().live_mixers().saturating_sub(1));
                        self.stats.remove_worker();
                    },
                }
            }
        }
    }

    /// Fetch the first `Worker` that has room for a new task, creating one if needed.
    ///
    /// If an inbound task has spilled from another thread, then do not reschedule it there.
    fn fetch_worker(
        &mut self,
        task: &ParkedMixer,
        avoid: Option<WorkerId>,
    ) -> (&mut Worker, usize) {
        let idx = self
            .workers
            .iter()
            .position(|w| w.can_schedule(task, avoid))
            .unwrap_or_else(|| {
                self.workers.push(Worker::new(
                    self.next_worker_id.incr(),
                    self.config.clone(),
                    self.tx.clone(),
                    self.stats.clone(),
                ));
                self.stats.add_worker();
                self.workers.len() - 1
            });

        (&mut self.workers[idx], idx)
    }

    pub fn spawn(mut self) {
        tokio::spawn(async move { self.run().await });
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{
        constants::test_data::FILE_WEBM_TARGET,
        driver::{tasks::mixer::Mixer, OutputMode},
        input::File,
        Driver,
    };
    use tokio::runtime::Handle;

    #[tokio::test]
    async fn inactive_mixers_dont_need_threads() {
        let sched = Scheduler::new(Config::default());
        let cfg = DriverConfig::default().scheduler(sched.clone());

        let _drivers: Vec<Driver> = (0..1024).map(|_| Driver::new(cfg.clone())).collect();
        tokio::time::sleep(Duration::from_secs(1)).await;

        assert_eq!(sched.total_tasks(), 1024);
        assert_eq!(sched.live_tasks(), 0);
        assert_eq!(sched.worker_threads(), 0);
    }

    #[tokio::test]
    async fn active_mixers_spawn_threads() {
        let config = Config {
            strategy: Mode::default(),
            move_expensive_tasks: false,
        };

        let sched = Scheduler::new(config);
        let (pkt_tx, _pkt_rx) = flume::unbounded();
        let cfg = DriverConfig::default()
            .scheduler(sched.clone())
            .override_connection(Some(OutputMode::Rtp(pkt_tx)));

        let n_tasks = 1024;

        let _drivers: Vec<Driver> = (0..n_tasks)
            .map(|_| {
                let mut driver = Driver::new(cfg.clone());
                let file = File::new(FILE_WEBM_TARGET);
                driver.play_input(file.into());
                driver
            })
            .collect();
        tokio::time::sleep(Duration::from_secs(10)).await;

        assert_eq!(sched.total_tasks(), n_tasks);
        assert_eq!(sched.live_tasks(), n_tasks);
        assert_eq!(
            sched.worker_threads(),
            n_tasks / (DEFAULT_MIXERS_PER_THREAD.get() as u64)
        );
    }

    #[tokio::test]
    async fn excess_threads_are_cleaned_up() {
        const TEST_TIMER: Duration = Duration::from_millis(500);

        let config = Config {
            strategy: Mode::MaxPerThread(1.try_into().unwrap()),
            move_expensive_tasks: true,
        };

        let (mut core, tx) = Idle::new(config.clone());
        core.cull_timer = TEST_TIMER;

        let mut next_id = TaskId::new();
        let mut thread_id = WorkerId::new();
        let mut handles = vec![];
        for i in 0..2 {
            let mut worker = Worker::new(
                thread_id.incr(),
                config.clone(),
                tx.clone(),
                core.stats.clone(),
            );
            let ((mixer, listeners), track_handle) =
                Mixer::test_with_float_unending(Handle::current(), false);

            let send_mixer = ParkedMixer {
                mixer: Box::new(mixer),
                ssrc: i,
                rtp_sequence: i as u16,
                rtp_timestamp: i,
                park_time: TokInstant::now().into(),
                last_cost: None,
                cull_handle: None,
            };
            core.stats.add_idle_mixer();
            core.stats.move_mixer_to_live();
            worker.schedule_mixer(next_id.incr(), send_mixer).unwrap();
            handles.push((track_handle, listeners));
            core.workers.push(worker);
        }

        let mut timer = tokio::time::interval(TIMESTEP_LENGTH);
        assert!(core.run_once(&mut timer).await);

        // Stop one of the handles, allow it to exit, and then run core again.
        handles[1].0.stop().unwrap();
        while core.workers[1].is_busy() {
            assert!(core.run_once(&mut timer).await);
        }

        tokio::time::sleep(TEST_TIMER + Duration::from_secs(1)).await;
        while core.workers.len() != 1 {
            assert!(core.run_once(&mut timer).await);
        }

        assert_eq!(core.stats.worker_threads(), 0);
    }
}

#[cfg(all(test, feature = "lcr-raw-opus-source"))]
mod lcr_raw_opus_scheduler_tests {
    use super::*;
    use crate::driver::{
        raw_opus::RawOpusContext,
        tasks::{
            message::{MixerMessage, WsMessage},
            mixer::Mixer,
        },
        test_config::{OutputMessage, OutputMode, TickMessage, TickStyle},
        RawOpusRead,
        RawOpusSource,
    };
    use std::{
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc,
        },
        time::Instant,
    };
    use tokio::runtime::Handle;

    const REAL_FRAME: &[u8] = &[0x11, 0x22, 0x33, 0x44, 0x55];

    struct SpeakingAwareRawSource {
        ws_rx: flume::Receiver<WsMessage>,
        pulls: Arc<AtomicUsize>,
        speaking_true_seen_before_first_pull: Arc<AtomicBool>,
    }

    impl RawOpusSource for SpeakingAwareRawSource {
        fn pull_opus(&mut self, dst: &mut [u8]) -> RawOpusRead {
            let pull_index = self.pulls.fetch_add(1, Ordering::SeqCst);

            if pull_index == 0 {
                let speaking_true_seen =
                    matches!(self.ws_rx.try_recv(), Ok(WsMessage::Speaking(true)));

                self.speaking_true_seen_before_first_pull
                    .store(speaking_true_seen, Ordering::SeqCst);

                assert!(
                    speaking_true_seen,
                    "scheduler must enqueue Speaking(true) before the first raw source pull"
                );

                dst[..REAL_FRAME.len()].copy_from_slice(REAL_FRAME);
                RawOpusRead::Frame(REAL_FRAME.len())
            } else {
                RawOpusRead::End
            }
        }
    }

    fn expect_passthrough(
        rx: &flume::Receiver<TickMessage<OutputMessage>>,
    ) {
        let output = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("timed out waiting for raw passthrough output");

        match output {
            TickMessage::El(OutputMessage::Passthrough(frame)) => {
                assert_eq!(frame.as_slice(), REAL_FRAME);
            },
            _ => panic!("expected one raw passthrough frame"),
        }
    }

    fn expect_silence(
        rx: &flume::Receiver<TickMessage<OutputMessage>>,
    ) {
        let output = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("timed out waiting for existing Songbird silence tail");

        assert!(
            matches!(output, TickMessage::El(OutputMessage::Silent)),
            "expected existing Songbird explicit silence frame"
        );
    }

    #[tokio::test]
    async fn raw_opus_scheduler_speaking_wraps_existing_five_frame_tail() {
        let (mut mixer, _listeners) = Mixer::mock(Handle::current(), false);

        let (tick_tx, tick_rx) = flume::unbounded();
        let (output_tx, output_rx) = flume::unbounded();
        let (ws_tx, ws_rx) = flume::unbounded();

        mixer.config = Arc::new(
            (*mixer.config)
                .clone()
                .tick_style(TickStyle::UntimedWithExecLimit(tick_rx))
                .override_connection(Some(OutputMode::Raw(output_tx))),
        );

        mixer.ws = Some(ws_tx);

        let pulls = Arc::new(AtomicUsize::new(0));
        let speaking_true_seen_before_first_pull = Arc::new(AtomicBool::new(false));

        let source = SpeakingAwareRawSource {
            ws_rx: ws_rx.clone(),
            pulls: Arc::clone(&pulls),
            speaking_true_seen_before_first_pull: Arc::clone(
                &speaking_true_seen_before_first_pull,
            ),
        };

        let (raw_handle, raw_context) = RawOpusContext::new(source);

        let mut setup_packet = [0u8; VOICE_PACKET_MAX];

        assert_eq!(
            mixer.handle_message(
                MixerMessage::SetRawOpusSource(Some(Box::new(raw_context))),
                &mut setup_packet,
            ),
            (false, false, false)
        );

        assert!(mixer.has_live_media_source());

        let parked = ParkedMixer {
            mixer: Box::new(mixer),
            ssrc: 42,
            rtp_sequence: 7,
            rtp_timestamp: 11,
            park_time: Instant::now(),
            last_cost: None,
            cull_handle: None,
        };

        let scheduler_config = Config {
            strategy: Mode::MaxPerThread(1.try_into().unwrap()),
            move_expensive_tasks: false,
        };

        let (mut idle, _scheduler_tx) = Idle::new(scheduler_config);
        let id = TaskId::new();

        //
        // schedule_mixer() synchronously enqueues Speaking(true) before handing
        // the ParkedMixer to the live worker. The source itself verifies this
        // on its first pull, avoiding a test-side scheduling race.
        //
        idle.schedule_mixer(parked, id, None);

        tick_tx
            .send(1)
            .expect("live worker tick channel must remain open");

        expect_passthrough(&output_rx);

        assert!(
            speaking_true_seen_before_first_pull.load(Ordering::SeqCst),
            "Speaking(true) must precede first media pull"
        );
        assert_eq!(pulls.load(Ordering::SeqCst), 1);
        assert!(!raw_handle.is_ended());

        //
        // Second pull returns End. Existing Songbird lifecycle must emit
        // exactly five silence frames before scheduler demotion.
        //
        for tail_index in 0..5 {
            tick_tx
                .send(1)
                .expect("live worker tick channel must remain open");

            expect_silence(&output_rx);

            if tail_index == 0 {
                assert!(raw_handle.is_ended());
                assert_eq!(pulls.load(Ordering::SeqCst), 2);
            }

            assert!(
                matches!(
                    ws_rx.try_recv(),
                    Err(flume::TryRecvError::Empty)
                ),
                "Speaking(false) must not be emitted before scheduler demotion"
            );
        }

        assert_eq!(pulls.load(Ordering::SeqCst), 2);

        //
        // After the fifth silence frame, the live worker's next loop sees:
        // no raw source + silence_frames == 0, then sends Demote to Idle.
        // Consume Idle's immediate timer tick first so the bounded wait below
        // waits specifically for a scheduler message rather than a clock guess.
        //
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        interval.tick().await;

        let run_result = tokio::time::timeout(
            Duration::from_secs(2),
            idle.run_once(&mut interval),
        )
        .await
        .expect("timed out waiting for live mixer demotion");

        assert!(run_result);

        let speaking_false = ws_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("timed out waiting for Speaking(false) after demotion");

        assert!(
            matches!(speaking_false, WsMessage::Speaking(false)),
            "scheduler demotion must emit Speaking(false)"
        );

        assert_eq!(idle.stats.live_mixers(), 0);
        assert!(
            output_rx.try_recv().is_err(),
            "no media packet may follow the completed five-frame tail"
        );
    }
}
