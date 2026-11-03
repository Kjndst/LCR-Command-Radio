//! Platform-neutral Raw Input interpretation; Win32 only supplies physical events.

use crate::reconciler::Desired;

pub const VK_ESCAPE: u32 = 0x1B;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RadioKey {
    Keyboard(u32),
    Mouse4,
    Mouse5,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Edge {
    Down,
    Up,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputProvider {
    None,
    Raw,
    Broker,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BrokerObservation {
    pub epoch: u64,
    pub key_revision: u64,
    pub sequence: u64,
    pub observed_tick: u64,
    pub level: Edge,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InputOutcome {
    Desired(Desired),
    Captured(RadioKey),
    Cancelled,
    Ignored,
}

pub struct InputAuthority {
    selected: RadioKey,
    down: bool,
    capture: bool,
    provider: InputProvider,
    epoch: u64,
    key_revision: u64,
    broker_last_sequence: Option<u64>,
    broker_down_refresh_tick: Option<u64>,
    poisoned_until_up: bool,
}

impl InputAuthority {
    pub fn new(selected: RadioKey) -> Self {
        Self {
            selected,
            down: false,
            capture: false,
            provider: InputProvider::Raw,
            epoch: 0,
            key_revision: 1,
            broker_last_sequence: None,
            broker_down_refresh_tick: None,
            poisoned_until_up: false,
        }
    }

    pub fn begin_capture(&mut self) {
        self.capture = true;
    }

    pub fn set_selected(&mut self, selected: RadioKey) -> InputOutcome {
        let outcome = if self.down {
            self.down = false;
            InputOutcome::Desired(Desired::Closed)
        } else {
            InputOutcome::Ignored
        };

        self.selected = selected;
        self.capture = false;
        self.broker_last_sequence = None;
        self.broker_down_refresh_tick = None;

        // A key change is an authority-session boundary even when the physical
        // provider itself does not change. Old observations must lose authority,
        // and the newly selected key must observe UP before DOWN may OPEN.
        self.poisoned_until_up = true;

        let next_epoch = self.epoch.checked_add(1).filter(|next| *next != u64::MAX);

        let next_key_revision = self
            .key_revision
            .checked_add(1)
            .filter(|next| *next != u64::MAX);

        match (next_epoch, next_key_revision) {
            (Some(epoch), Some(key_revision)) => {
                self.epoch = epoch;
                self.key_revision = key_revision;
            }
            _ => {
                // Counter exhaustion is terminal for this process. Never alias
                // a previously authorized epoch or key revision.
                self.epoch = u64::MAX;
                self.key_revision = u64::MAX;
                self.provider = InputProvider::None;
            }
        }

        outcome
    }

    pub fn selected(&self) -> RadioKey {
        self.selected.clone()
    }

    pub fn provider(&self) -> InputProvider {
        self.provider
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn key_revision(&self) -> u64 {
        self.key_revision
    }

    pub fn poisoned_until_up(&self) -> bool {
        self.poisoned_until_up
    }

    /// Revokes the current PTT provider before an authority handoff.
    ///
    /// The old provider becomes unable to mutate PTT immediately. Any open
    /// state is synchronously closed before a new provider may activate.
    pub fn begin_handoff(&mut self) -> (u64, InputOutcome) {
        let outcome = if self.down {
            self.down = false;
            InputOutcome::Desired(Desired::Closed)
        } else {
            InputOutcome::Ignored
        };

        self.provider = InputProvider::None;
        self.broker_last_sequence = None;
        self.broker_down_refresh_tick = None;

        // A clean authority handoff does not imply uncertain physical state
        // once the new provider supplies a fresh observation. Preserve any
        // poison already established by failure/lease expiry/force_release,
        // but do not create poison merely because the provider changes.
        self.epoch = match self.epoch.checked_add(1) {
            Some(next) if next != u64::MAX => next,
            _ => u64::MAX,
        };

        (self.epoch, outcome)
    }

    /// Activates exactly one provider for the current handoff epoch.
    pub fn activate_provider(&mut self, provider: InputProvider, epoch: u64) -> bool {
        if provider == InputProvider::None
            || self.provider != InputProvider::None
            || epoch != self.epoch
            || epoch == u64::MAX
            || self.key_revision == u64::MAX
        {
            return false;
        }

        self.provider = provider;
        self.down = false;
        self.broker_last_sequence = None;
        self.broker_down_refresh_tick = None;

        // Do not clear poisoned_until_up here. Provider activation is not a
        // physical release edge.
        true
    }

    /// Applies one authenticated broker observation.
    ///
    /// Provider/session/key identity is checked before sequence consumption.
    /// Once a sequence is consumed, a stale or future DOWN cannot later be
    /// replayed and reinterpreted as fresh.
    pub fn broker_observation(
        &mut self,
        observation: BrokerObservation,
        now_tick: u64,
        max_down_age: u64,
    ) -> InputOutcome {
        if self.provider != InputProvider::Broker
            || self.epoch == u64::MAX
            || self.key_revision == u64::MAX
            || observation.epoch != self.epoch
            || observation.key_revision != self.key_revision
        {
            return InputOutcome::Ignored;
        }

        if self
            .broker_last_sequence
            .is_some_and(|last| observation.sequence <= last)
        {
            return InputOutcome::Ignored;
        }

        self.broker_last_sequence = Some(observation.sequence);

        // All observations share the same monotonic clock domain. A timestamp
        // from the future is protocol-invalid. Consume its sequence so the same
        // packet cannot be replayed later after wall time catches up.
        if observation.observed_tick > now_tick {
            return InputOutcome::Ignored;
        }

        match observation.level {
            Edge::Up => {
                self.broker_down_refresh_tick = None;
                self.poisoned_until_up = false;

                if self.down {
                    self.down = false;
                    InputOutcome::Desired(Desired::Closed)
                } else {
                    InputOutcome::Ignored
                }
            }
            Edge::Down => {
                let Some(age) = now_tick.checked_sub(observation.observed_tick) else {
                    return InputOutcome::Ignored;
                };

                if age > max_down_age || self.poisoned_until_up {
                    return InputOutcome::Ignored;
                }

                // Lease evidence is the broker's physical observation time,
                // never the parent's message-dequeue/receive time.
                self.broker_down_refresh_tick = Some(observation.observed_tick);

                if self.down {
                    InputOutcome::Ignored
                } else {
                    self.down = true;
                    InputOutcome::Desired(Desired::Open)
                }
            }
        }
    }

    /// Expires a positive broker DOWN lease.
    ///
    /// Expiry fails CLOSED and quarantines the same continuous hold until an
    /// authenticated current-session UP is observed.
    pub fn expire_broker_down_lease(&mut self, now_tick: u64, lease_duration: u64) -> InputOutcome {
        if self.provider != InputProvider::Broker || !self.down {
            return InputOutcome::Ignored;
        }

        let expired = self
            .broker_down_refresh_tick
            .and_then(|observed| now_tick.checked_sub(observed))
            .is_none_or(|age| age >= lease_duration);

        if !expired {
            return InputOutcome::Ignored;
        }

        self.down = false;
        self.broker_down_refresh_tick = None;
        self.poisoned_until_up = true;
        InputOutcome::Desired(Desired::Closed)
    }

    /// Reconciles an authoritative physical level without treating periodic
    /// level refreshes as capture candidates.
    pub fn reconcile_selected_level(&mut self, down: bool) -> InputOutcome {
        if self.capture {
            return InputOutcome::Ignored;
        }

        let selected = self.selected.clone();
        self.event(selected, if down { Edge::Down } else { Edge::Up })
    }

    /// Fail-closed escape hatch for a lost release edge or dead input provider.
    pub fn force_release(&mut self) -> InputOutcome {
        self.broker_down_refresh_tick = None;

        if self.down {
            self.down = false;
            self.poisoned_until_up = true;
            InputOutcome::Desired(Desired::Closed)
        } else {
            InputOutcome::Ignored
        }
    }

    pub fn event(&mut self, key: RadioKey, edge: Edge) -> InputOutcome {
        if self.capture && edge == Edge::Down {
            self.capture = false;

            if key == RadioKey::Keyboard(VK_ESCAPE) {
                return InputOutcome::Cancelled;
            }

            // Candidate only. The caller persists it first and calls
            // set_selected() only after persistence succeeds.
            return InputOutcome::Captured(key);
        }

        if self.provider != InputProvider::Raw {
            return InputOutcome::Ignored;
        }

        if self.key_revision == u64::MAX || key != self.selected {
            return InputOutcome::Ignored;
        }

        if self.poisoned_until_up {
            return match edge {
                Edge::Down => InputOutcome::Ignored,
                Edge::Up => {
                    self.poisoned_until_up = false;

                    if self.down {
                        self.down = false;
                        InputOutcome::Desired(Desired::Closed)
                    } else {
                        InputOutcome::Ignored
                    }
                }
            };
        }

        match edge {
            Edge::Down if !self.down => {
                self.down = true;
                InputOutcome::Desired(Desired::Open)
            }
            Edge::Up if self.down => {
                self.down = false;
                InputOutcome::Desired(Desired::Closed)
            }
            _ => InputOutcome::Ignored,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_and_mouse_edges_are_deduplicated() {
        let mut input = InputAuthority::new(RadioKey::Keyboard(0x41));

        assert_eq!(
            input.event(RadioKey::Keyboard(0x41), Edge::Down),
            InputOutcome::Desired(Desired::Open)
        );
        assert_eq!(
            input.event(RadioKey::Keyboard(0x41), Edge::Down),
            InputOutcome::Ignored
        );
        assert_eq!(
            input.event(RadioKey::Keyboard(0x41), Edge::Up),
            InputOutcome::Desired(Desired::Closed)
        );
        assert_eq!(
            input.event(RadioKey::Mouse5, Edge::Down),
            InputOutcome::Ignored
        );
    }

    #[test]
    fn capture_is_candidate_until_explicit_commit() {
        let mut input = InputAuthority::new(RadioKey::Mouse4);

        input.begin_capture();
        assert_eq!(
            input.event(RadioKey::Mouse5, Edge::Down),
            InputOutcome::Captured(RadioKey::Mouse5)
        );

        // Capture alone must not change the active key.
        assert_eq!(
            input.event(RadioKey::Mouse5, Edge::Down),
            InputOutcome::Ignored
        );

        assert_eq!(input.set_selected(RadioKey::Mouse5), InputOutcome::Ignored);

        // The capture DOWN cannot leak through the key-authority boundary.
        assert_eq!(
            input.event(RadioKey::Mouse5, Edge::Down),
            InputOutcome::Ignored
        );
        assert_eq!(
            input.event(RadioKey::Mouse5, Edge::Up),
            InputOutcome::Ignored
        );

        assert_eq!(
            input.event(RadioKey::Mouse5, Edge::Down),
            InputOutcome::Desired(Desired::Open)
        );
        assert_eq!(
            input.event(RadioKey::Mouse5, Edge::Up),
            InputOutcome::Desired(Desired::Closed)
        );
    }

    #[test]
    fn physical_level_reconciliation_closes_a_lost_release() {
        let mut input = InputAuthority::new(RadioKey::Keyboard(0x54));

        assert_eq!(
            input.reconcile_selected_level(true),
            InputOutcome::Desired(Desired::Open)
        );
        assert_eq!(input.reconcile_selected_level(true), InputOutcome::Ignored);
        assert_eq!(
            input.force_release(),
            InputOutcome::Desired(Desired::Closed)
        );
        assert_eq!(input.force_release(), InputOutcome::Ignored);
    }

    #[test]
    fn broker_level_refresh_never_becomes_a_capture_candidate() {
        let mut input = InputAuthority::new(RadioKey::Mouse4);
        input.begin_capture();

        assert_eq!(input.reconcile_selected_level(true), InputOutcome::Ignored);
        assert_eq!(
            input.event(RadioKey::Mouse5, Edge::Down),
            InputOutcome::Captured(RadioKey::Mouse5)
        );
    }

    fn broker(
        epoch: u64,
        key_revision: u64,
        sequence: u64,
        observed_tick: u64,
        level: Edge,
    ) -> BrokerObservation {
        BrokerObservation {
            epoch,
            key_revision,
            sequence,
            observed_tick,
            level,
        }
    }

    fn activate_broker(input: &mut InputAuthority) -> (u64, u64) {
        let (epoch, outcome) = input.begin_handoff();
        assert_eq!(outcome, InputOutcome::Ignored);
        assert!(input.activate_provider(InputProvider::Broker, epoch));

        // This fixture models a clean provider handoff. Failure-induced poison
        // is tested separately and must survive provider changes.
        assert!(!input.poisoned_until_up());

        (epoch, input.key_revision())
    }

    #[test]
    fn provider_handoff_closes_and_old_raw_authority_cannot_reopen() {
        let mut input = InputAuthority::new(RadioKey::Keyboard(0x54));

        assert_eq!(
            input.event(RadioKey::Keyboard(0x54), Edge::Down),
            InputOutcome::Desired(Desired::Open)
        );

        let (epoch, outcome) = input.begin_handoff();

        assert_eq!(outcome, InputOutcome::Desired(Desired::Closed));
        assert_eq!(input.provider(), InputProvider::None);

        assert_eq!(
            input.event(RadioKey::Keyboard(0x54), Edge::Down),
            InputOutcome::Ignored
        );

        assert!(input.activate_provider(InputProvider::Broker, epoch));

        assert_eq!(
            input.event(RadioKey::Keyboard(0x54), Edge::Down),
            InputOutcome::Ignored
        );

        assert_eq!(
            input.broker_observation(
                broker(
                    epoch.saturating_sub(1),
                    input.key_revision(),
                    1,
                    100,
                    Edge::Down,
                ),
                100,
                10,
            ),
            InputOutcome::Ignored
        );
    }

    #[test]
    fn broker_capture_remains_available_without_raw_ptt_authority() {
        let mut input = InputAuthority::new(RadioKey::Mouse4);
        let _ = activate_broker(&mut input);

        input.begin_capture();

        assert_eq!(
            input.event(RadioKey::Mouse5, Edge::Down),
            InputOutcome::Captured(RadioKey::Mouse5)
        );
    }

    #[test]
    fn broker_rejects_old_epoch_old_key_revision_and_non_monotonic_sequence() {
        let mut input = InputAuthority::new(RadioKey::Mouse4);
        let (epoch, key_revision) = activate_broker(&mut input);

        assert_eq!(
            input.broker_observation(broker(epoch, key_revision, 9, 99, Edge::Up), 100, 10,),
            InputOutcome::Ignored
        );

        assert_eq!(
            input.broker_observation(broker(epoch, key_revision, 10, 100, Edge::Down), 100, 10,),
            InputOutcome::Desired(Desired::Open)
        );

        assert_eq!(
            input.broker_observation(broker(epoch, key_revision, 10, 101, Edge::Up), 101, 10,),
            InputOutcome::Ignored
        );

        assert_eq!(
            input.set_selected(RadioKey::Mouse5),
            InputOutcome::Desired(Desired::Closed)
        );

        let new_epoch = input.epoch();
        let new_revision = input.key_revision();

        assert!(new_epoch > epoch);
        assert!(new_revision > key_revision);
        assert!(input.poisoned_until_up());

        // Old key revision and old epoch have both lost authority.
        assert_eq!(
            input.broker_observation(broker(epoch, key_revision, 11, 102, Edge::Down), 102, 10,),
            InputOutcome::Ignored
        );

        assert_eq!(
            input.broker_observation(broker(epoch, new_revision, 11, 102, Edge::Down), 102, 10,),
            InputOutcome::Ignored
        );

        // Sequence belongs to the new epoch and may restart there.
        assert_eq!(
            input.broker_observation(broker(new_epoch, new_revision, 1, 102, Edge::Down), 102, 10,),
            InputOutcome::Ignored
        );

        assert_eq!(
            input.broker_observation(broker(new_epoch, new_revision, 2, 103, Edge::Up), 103, 10,),
            InputOutcome::Ignored
        );

        assert!(!input.poisoned_until_up());

        assert_eq!(
            input.broker_observation(broker(new_epoch, new_revision, 3, 104, Edge::Down), 104, 10,),
            InputOutcome::Desired(Desired::Open)
        );

        assert_eq!(
            input.broker_observation(broker(new_epoch, new_revision, 3, 105, Edge::Up), 105, 10,),
            InputOutcome::Ignored
        );
    }
    #[test]
    fn stale_or_future_observation_never_becomes_fresh_at_receive_time() {
        let mut input = InputAuthority::new(RadioKey::Mouse4);
        let (epoch, key_revision) = activate_broker(&mut input);

        // Clean provider activation is not poisoned.
        assert!(!input.poisoned_until_up());

        // Establish a real DOWN, then lose its positive lease. This is the
        // failure path that must create quarantine.
        assert_eq!(
            input.broker_observation(broker(epoch, key_revision, 1, 190, Edge::Down), 190, 99,),
            InputOutcome::Desired(Desired::Open)
        );

        assert_eq!(
            input.expire_broker_down_lease(200, 10),
            InputOutcome::Desired(Desired::Closed)
        );
        assert!(input.poisoned_until_up());

        // Future UP is protocol-invalid. Its sequence is consumed, but it must
        // not clear quarantine.
        assert_eq!(
            input.broker_observation(broker(epoch, key_revision, 2, 201, Edge::Up), 200, 99,),
            InputOutcome::Ignored
        );
        assert!(input.poisoned_until_up());

        // The same packet cannot become valid later when time catches up.
        assert_eq!(
            input.broker_observation(broker(epoch, key_revision, 2, 201, Edge::Up), 201, 99,),
            InputOutcome::Ignored
        );
        assert!(input.poisoned_until_up());

        // A valid authenticated UP is fail-closed and may clear quarantine
        // even when its observation itself is old.
        assert_eq!(
            input.broker_observation(broker(epoch, key_revision, 3, 0, Edge::Up), 200, 99,),
            InputOutcome::Ignored
        );
        assert!(!input.poisoned_until_up());

        // Stale DOWN is consumed and cannot later be rejuvenated.
        assert_eq!(
            input.broker_observation(broker(epoch, key_revision, 4, 100, Edge::Down), 200, 99,),
            InputOutcome::Ignored
        );

        assert_eq!(
            input.broker_observation(broker(epoch, key_revision, 4, 100, Edge::Down), 100, 99,),
            InputOutcome::Ignored
        );

        // Future DOWN is likewise invalid and consumed.
        assert_eq!(
            input.broker_observation(broker(epoch, key_revision, 5, 201, Edge::Down), 200, 99,),
            InputOutcome::Ignored
        );

        // A genuinely fresh, newer DOWN remains usable.
        assert_eq!(
            input.broker_observation(broker(epoch, key_revision, 6, 200, Edge::Down), 200, 99,),
            InputOutcome::Desired(Desired::Open)
        );
    }
    #[test]
    fn fresh_down_refreshes_lease_without_repeating_open() {
        let mut input = InputAuthority::new(RadioKey::Mouse4);
        let (epoch, key_revision) = activate_broker(&mut input);

        assert_eq!(
            input.broker_observation(broker(epoch, key_revision, 1, 99, Edge::Up), 100, 10,),
            InputOutcome::Ignored
        );

        assert_eq!(
            input.broker_observation(broker(epoch, key_revision, 2, 100, Edge::Down), 100, 10,),
            InputOutcome::Desired(Desired::Open)
        );

        assert_eq!(
            input.broker_observation(broker(epoch, key_revision, 3, 105, Edge::Down), 105, 10,),
            InputOutcome::Ignored
        );

        assert_eq!(
            input.expire_broker_down_lease(114, 10),
            InputOutcome::Ignored
        );
    }

    #[test]
    fn lease_expiry_closes_and_requires_up_before_reopen() {
        let mut input = InputAuthority::new(RadioKey::Mouse4);
        let (epoch, key_revision) = activate_broker(&mut input);

        assert_eq!(
            input.broker_observation(broker(epoch, key_revision, 1, 99, Edge::Up), 100, 10,),
            InputOutcome::Ignored
        );

        assert_eq!(
            input.broker_observation(broker(epoch, key_revision, 2, 100, Edge::Down), 100, 10,),
            InputOutcome::Desired(Desired::Open)
        );

        assert_eq!(
            input.expire_broker_down_lease(110, 10),
            InputOutcome::Desired(Desired::Closed)
        );
        assert!(input.poisoned_until_up());

        assert_eq!(
            input.broker_observation(broker(epoch, key_revision, 3, 110, Edge::Down), 110, 10,),
            InputOutcome::Ignored
        );
        assert!(input.poisoned_until_up());

        // UP is fail-closed and clears poison without needing DOWN freshness.
        assert_eq!(
            input.broker_observation(broker(epoch, key_revision, 4, 0, Edge::Up), 110, 10,),
            InputOutcome::Ignored
        );
        assert!(!input.poisoned_until_up());

        assert_eq!(
            input.broker_observation(broker(epoch, key_revision, 5, 111, Edge::Down), 111, 10,),
            InputOutcome::Desired(Desired::Open)
        );
    }

    #[test]
    fn clean_handoff_allows_fresh_broker_snapshot_to_reestablish_down() {
        let mut input = InputAuthority::new(RadioKey::Mouse4);

        assert_eq!(
            input.event(RadioKey::Mouse4, Edge::Down),
            InputOutcome::Desired(Desired::Open)
        );

        let (epoch, closed) = input.begin_handoff();
        let key_revision = input.key_revision();

        assert_eq!(closed, InputOutcome::Desired(Desired::Closed));
        assert!(!input.poisoned_until_up());
        assert!(input.activate_provider(InputProvider::Broker, epoch));

        // This models the authenticated initial physical snapshot from the
        // newly authoritative Broker, not a stale event from the old provider.
        assert_eq!(
            input.broker_observation(broker(epoch, key_revision, 1, 100, Edge::Down), 100, 10,),
            InputOutcome::Desired(Desired::Open)
        );
    }
    #[test]
    fn clean_handoff_back_to_raw_accepts_fresh_level_reconciliation() {
        let mut input = InputAuthority::new(RadioKey::Keyboard(0x54));
        let (broker_epoch, key_revision) = activate_broker(&mut input);

        assert_eq!(
            input.broker_observation(
                broker(broker_epoch, key_revision, 1, 100, Edge::Down,),
                100,
                10,
            ),
            InputOutcome::Desired(Desired::Open)
        );

        let (raw_epoch, closed) = input.begin_handoff();

        assert_eq!(closed, InputOutcome::Desired(Desired::Closed));
        assert!(!input.poisoned_until_up());
        assert!(input.activate_provider(InputProvider::Raw, raw_epoch));

        // R1C is responsible for ensuring this reconciliation comes from a
        // post-handoff physical-level observation rather than a queued old
        // Raw Input message.
        assert_eq!(
            input.reconcile_selected_level(true),
            InputOutcome::Desired(Desired::Open)
        );
    }

    #[test]
    fn failure_poison_survives_provider_handoff_until_real_up() {
        let mut input = InputAuthority::new(RadioKey::Mouse4);

        assert_eq!(
            input.event(RadioKey::Mouse4, Edge::Down),
            InputOutcome::Desired(Desired::Open)
        );

        assert_eq!(
            input.force_release(),
            InputOutcome::Desired(Desired::Closed)
        );
        assert!(input.poisoned_until_up());

        let (epoch, outcome) = input.begin_handoff();

        assert_eq!(outcome, InputOutcome::Ignored);
        assert!(input.poisoned_until_up());
        assert!(input.activate_provider(InputProvider::Broker, epoch));
        assert!(input.poisoned_until_up());

        let key_revision = input.key_revision();

        assert_eq!(
            input.broker_observation(broker(epoch, key_revision, 1, 100, Edge::Down), 100, 10,),
            InputOutcome::Ignored
        );

        assert_eq!(
            input.broker_observation(broker(epoch, key_revision, 2, 101, Edge::Up), 101, 10,),
            InputOutcome::Ignored
        );

        assert!(!input.poisoned_until_up());

        assert_eq!(
            input.broker_observation(broker(epoch, key_revision, 3, 102, Edge::Down), 102, 10,),
            InputOutcome::Desired(Desired::Open)
        );
    }
    #[test]
    fn force_release_quarantines_continuous_raw_hold() {
        let mut input = InputAuthority::new(RadioKey::Mouse4);

        assert_eq!(
            input.event(RadioKey::Mouse4, Edge::Down),
            InputOutcome::Desired(Desired::Open)
        );

        assert_eq!(
            input.force_release(),
            InputOutcome::Desired(Desired::Closed)
        );

        assert_eq!(
            input.event(RadioKey::Mouse4, Edge::Down),
            InputOutcome::Ignored
        );

        assert_eq!(
            input.event(RadioKey::Mouse4, Edge::Up),
            InputOutcome::Ignored
        );

        assert_eq!(
            input.event(RadioKey::Mouse4, Edge::Down),
            InputOutcome::Desired(Desired::Open)
        );
    }

    #[test]
    fn terminal_epoch_and_key_revision_fail_closed() {
        let mut epoch_exhausted = InputAuthority::new(RadioKey::Mouse4);
        epoch_exhausted.epoch = u64::MAX - 1;

        let (terminal_epoch, _) = epoch_exhausted.begin_handoff();

        assert_eq!(terminal_epoch, u64::MAX);
        assert_eq!(epoch_exhausted.provider(), InputProvider::None);
        assert!(!epoch_exhausted.activate_provider(InputProvider::Broker, terminal_epoch));

        let mut revision_exhausted = InputAuthority::new(RadioKey::Mouse4);
        revision_exhausted.key_revision = u64::MAX - 1;

        assert_eq!(
            revision_exhausted.set_selected(RadioKey::Mouse5),
            InputOutcome::Ignored
        );

        assert_eq!(revision_exhausted.key_revision(), u64::MAX);
        assert_eq!(revision_exhausted.provider(), InputProvider::None);

        assert_eq!(
            revision_exhausted.event(RadioKey::Mouse5, Edge::Down),
            InputOutcome::Ignored
        );
    }

    #[test]
    fn key_change_closes_advances_authority_and_requires_current_up() {
        let mut input = InputAuthority::new(RadioKey::Mouse4);
        let (old_epoch, old_revision) = activate_broker(&mut input);

        assert_eq!(
            input.broker_observation(broker(old_epoch, old_revision, 10, 100, Edge::Up), 100, 10,),
            InputOutcome::Ignored
        );

        assert_eq!(
            input.broker_observation(
                broker(old_epoch, old_revision, 11, 101, Edge::Down),
                101,
                10,
            ),
            InputOutcome::Desired(Desired::Open)
        );

        assert_eq!(
            input.set_selected(RadioKey::Mouse5),
            InputOutcome::Desired(Desired::Closed)
        );

        let new_epoch = input.epoch();
        let new_revision = input.key_revision();

        assert!(new_epoch > old_epoch);
        assert!(new_revision > old_revision);
        assert!(input.poisoned_until_up());

        assert_eq!(
            input.broker_observation(
                broker(old_epoch, old_revision, 12, 102, Edge::Down),
                102,
                10,
            ),
            InputOutcome::Ignored
        );

        // A DOWN for the new authority cannot cross the key boundary before UP.
        assert_eq!(
            input.broker_observation(broker(new_epoch, new_revision, 1, 102, Edge::Down), 102, 10,),
            InputOutcome::Ignored
        );

        assert_eq!(
            input.broker_observation(broker(new_epoch, new_revision, 2, 103, Edge::Up), 103, 10,),
            InputOutcome::Ignored
        );

        assert_eq!(
            input.broker_observation(broker(new_epoch, new_revision, 3, 104, Edge::Down), 104, 10,),
            InputOutcome::Desired(Desired::Open)
        );
    }
    #[test]
    fn broker_observation_is_rejected_while_raw_is_authority() {
        let mut input = InputAuthority::new(RadioKey::Mouse4);

        assert_eq!(
            input.broker_observation(
                broker(input.epoch(), input.key_revision(), 1, 100, Edge::Down,),
                100,
                10,
            ),
            InputOutcome::Ignored
        );
    }

    #[test]
    fn escape_cancels_capture_and_preserves_existing_key() {
        let mut input = InputAuthority::new(RadioKey::Mouse4);

        input.begin_capture();

        assert_eq!(
            input.event(RadioKey::Keyboard(VK_ESCAPE), Edge::Down),
            InputOutcome::Cancelled
        );

        assert_eq!(
            input.event(RadioKey::Mouse4, Edge::Down),
            InputOutcome::Desired(Desired::Open)
        );
    }
}
