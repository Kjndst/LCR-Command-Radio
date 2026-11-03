use crate::config::{Config, Store};
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Desired {
    Open,
    Closed,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    SendState { desired: Desired, generation: u64 },
    Heartbeat { generation: u64 },
    InvalidatePairing,
    None,
}
pub struct Reconciler {
    pub config: Config,
    desired: Desired,
    acknowledged: Option<(Desired, u64)>,
    started: bool,
}
impl Reconciler {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            desired: Desired::Closed,
            acknowledged: None,
            started: false,
        }
    }
    fn advance<S: Store>(&mut self, store: &mut S) -> Result<u64, ()> {
        let generation = self
            .config
            .generation
            .checked_add(1)
            .filter(|generation| *generation != u64::MAX)
            .ok_or(())?;

        let mut next = self.config.clone();
        next.generation = generation;

        // Persistence is authority. Never expose a new generation in memory
        // or on the network until the durable store accepts it.
        store.save(&next)?;

        self.config = next;
        Ok(generation)
    }
    pub fn startup<S: Store>(&mut self, store: &mut S) -> Result<Action, ()> {
        let n = self.advance(store)?;
        self.started = true;
        Ok(Action::SendState {
            desired: Desired::Closed,
            generation: n,
        })
    }
    pub fn set_desired<S: Store>(&mut self, desired: Desired, store: &mut S) -> Result<Action, ()> {
        if desired == self.desired {
            return Ok(Action::None);
        }

        // Persist the next generation before changing in-memory desired state.
        // A failed disk write therefore cannot create an unpersisted OPEN.
        let generation = self.advance(store)?;

        self.desired = desired;
        self.acknowledged = None;

        Ok(Action::SendState {
            desired,
            generation,
        })
    }
    pub fn on_ack(&mut self, desired: Desired, generation: u64) {
        if self.started && desired == self.desired && generation == self.config.generation {
            self.acknowledged = Some((desired, generation));
        }
    }
    pub fn desired_state(&self) -> Desired {
        self.desired
    }

    pub fn on_tick(&self) -> Action {
        if self.config.device_token.is_none() || !self.started {
            return Action::None;
        }

        match (self.desired, self.acknowledged) {
            (Desired::Closed, Some((Desired::Closed, generation)))
                if generation == self.config.generation =>
            {
                Action::None
            }

            (Desired::Open, Some((Desired::Open, generation)))
                if generation == self.config.generation =>
            {
                Action::Heartbeat {
                    generation: self.config.generation,
                }
            }

            _ => Action::SendState {
                desired: self.desired,
                generation: self.config.generation,
            },
        }
    }
    pub fn transient_failure(&mut self) {
        self.acknowledged = None;
    }
    pub fn unauthorized(&mut self) -> Action {
        self.desired = Desired::Closed;
        self.acknowledged = None;
        self.config.device_token = None;
        self.config.pending_pair = false;
        Action::InvalidatePairing
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct MemoryStore {
        saved: Vec<Config>,
        fail: bool,
    }
    impl Store for MemoryStore {
        fn save(&mut self, config: &Config) -> Result<(), ()> {
            if self.fail {
                Err(())
            } else {
                self.saved.push(config.clone());
                Ok(())
            }
        }
    }
    fn paired(generation: u64) -> Config {
        Config {
            device_token: Some("secret".into()),
            generation,
            ..Config::default()
        }
    }

    #[test]
    fn tick_is_quiet_when_closed_and_reasserts_open_same_generation() {
        let mut store = MemoryStore {
            saved: vec![],
            fail: false,
        };
        let mut reconciler = Reconciler::new(paired(0));

        assert_eq!(reconciler.on_tick(), Action::None);

        assert_eq!(
            reconciler.startup(&mut store),
            Ok(Action::SendState {
                desired: Desired::Closed,
                generation: 1,
            })
        );
        assert_eq!(
            reconciler.on_tick(),
            Action::SendState {
                desired: Desired::Closed,
                generation: 1,
            }
        );

        reconciler.on_ack(Desired::Closed, 1);
        assert_eq!(reconciler.on_tick(), Action::None);

        assert_eq!(
            reconciler.set_desired(Desired::Open, &mut store),
            Ok(Action::SendState {
                desired: Desired::Open,
                generation: 2,
            })
        );
        assert_eq!(
            reconciler.on_tick(),
            Action::SendState {
                desired: Desired::Open,
                generation: 2,
            }
        );

        reconciler.on_ack(Desired::Open, 2);
        assert_eq!(reconciler.on_tick(), Action::Heartbeat { generation: 2 });

        reconciler.transient_failure();
        assert_eq!(
            reconciler.on_tick(),
            Action::SendState {
                desired: Desired::Open,
                generation: 2,
            }
        );
    }

    #[test]
    fn startup_edges_retries_and_stale_acks_are_monotonic() {
        let mut store = MemoryStore {
            saved: vec![],
            fail: false,
        };
        let mut r = Reconciler::new(paired(4));
        assert_eq!(
            r.startup(&mut store),
            Ok(Action::SendState {
                desired: Desired::Closed,
                generation: 5
            })
        );
        assert_eq!(
            store
                .saved
                .iter()
                .map(|config| config.generation)
                .collect::<Vec<_>>(),
            vec![5]
        );
        assert_eq!(
            r.set_desired(Desired::Open, &mut store),
            Ok(Action::SendState {
                desired: Desired::Open,
                generation: 6
            })
        );
        assert_eq!(r.set_desired(Desired::Open, &mut store), Ok(Action::None));
        assert_eq!(
            r.on_tick(),
            Action::SendState {
                desired: Desired::Open,
                generation: 6
            }
        );
        r.on_ack(Desired::Open, 5);
        assert_eq!(
            r.on_tick(),
            Action::SendState {
                desired: Desired::Open,
                generation: 6
            }
        );
        r.on_ack(Desired::Open, 6);
        assert_eq!(r.on_tick(), Action::Heartbeat { generation: 6 });
        r.transient_failure();
        assert_eq!(
            r.on_tick(),
            Action::SendState {
                desired: Desired::Open,
                generation: 6
            }
        );
        assert_eq!(
            r.set_desired(Desired::Closed, &mut store),
            Ok(Action::SendState {
                desired: Desired::Closed,
                generation: 7
            })
        );
        assert_eq!(
            r.set_desired(Desired::Open, &mut store),
            Ok(Action::SendState {
                desired: Desired::Open,
                generation: 8
            })
        );
        r.on_ack(Desired::Closed, 7);
        assert_eq!(
            r.on_tick(),
            Action::SendState {
                desired: Desired::Open,
                generation: 8
            }
        );
    }
    #[test]
    fn failure_unauthorized_exhaustion_and_redaction_fail_closed() {
        let mut store = MemoryStore {
            saved: vec![],
            fail: true,
        };
        let mut r = Reconciler::new(paired(0));
        assert_eq!(r.startup(&mut store), Err(()));
        assert_eq!(r.config.generation, 0);
        assert!(!format!("{:?}", r.config).contains("secret"));
        assert_eq!(r.unauthorized(), Action::InvalidatePairing);
        assert!(r.config.device_token.is_none());
        let mut max = Reconciler::new(paired(u64::MAX));
        let mut ok = MemoryStore {
            saved: vec![],
            fail: false,
        };
        assert_eq!(max.startup(&mut ok), Err(()));
    }
    #[test]
    fn failed_desired_persistence_does_not_latch_open_or_advance_generation() {
        let mut store = MemoryStore {
            saved: vec![],
            fail: false,
        };
        let mut r = Reconciler::new(paired(0));

        assert_eq!(
            r.startup(&mut store),
            Ok(Action::SendState {
                desired: Desired::Closed,
                generation: 1
            })
        );

        r.on_ack(Desired::Closed, 1);

        store.fail = true;

        assert_eq!(r.set_desired(Desired::Open, &mut store), Err(()));

        assert_eq!(r.config.generation, 1);

        // Desired must still be CLOSED and acknowledged.
        assert_eq!(r.on_tick(), Action::None);

        store.fail = false;

        assert_eq!(
            r.set_desired(Desired::Open, &mut store),
            Ok(Action::SendState {
                desired: Desired::Open,
                generation: 2
            })
        );
    }

    #[test]
    fn terminal_generation_is_rejected_before_persistence() {
        let mut store = MemoryStore {
            saved: vec![],
            fail: false,
        };

        let mut r = Reconciler::new(paired(u64::MAX - 1));

        assert_eq!(r.startup(&mut store), Err(()));
        assert_eq!(r.config.generation, u64::MAX - 1);
        assert!(store.saved.is_empty());
    }
    #[test]
    fn persisted_restart_allocates_strictly_newer_closed_generations() {
        let mut store = MemoryStore {
            saved: vec![],
            fail: false,
        };
        let mut first = Reconciler::new(paired(40));
        let first_action = first.startup(&mut store).unwrap();
        assert_eq!(
            first_action,
            Action::SendState {
                desired: Desired::Closed,
                generation: 41
            }
        );
        assert_eq!(
            first.on_tick(),
            Action::SendState {
                desired: Desired::Closed,
                generation: 41
            }
        );
        let persisted = store.saved.last().unwrap().clone();
        let mut second = Reconciler::new(persisted);
        let second_action = second.startup(&mut store).unwrap();
        assert_eq!(
            second_action,
            Action::SendState {
                desired: Desired::Closed,
                generation: 42
            }
        );
        assert_eq!(
            second.on_tick(),
            Action::SendState {
                desired: Desired::Closed,
                generation: 42
            }
        );
        assert!(second.config.generation > 0);
    }
}
