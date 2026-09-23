use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

/// Result of one synchronous raw-Opus source pull.
///
/// The source is called at Songbird's existing mixer cadence. It must not
/// block, perform network I/O, create another pacing clock, or write beyond
/// the supplied destination buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RawOpusRead {
    /// One encoded Opus frame was written into the destination buffer.
    Frame(usize),
    /// The source has ended normally.
    End,
    /// The source encountered a terminal media error.
    Error,
}

/// A synchronous encoded-Opus source owned and paced by Songbird's mixer.
///
/// Implementations supply already validated encoded Opus. Songbird remains
/// responsible for packet cadence, RTP state, DAVE, Discord encryption, UDP,
/// reconnect, and speaking lifecycle.
pub trait RawOpusSource: Send + 'static {
    /// Writes at most one encoded Opus frame into `dst`.
    fn pull_opus(&mut self, dst: &mut [u8]) -> RawOpusRead;
}

#[derive(Debug, Default)]
struct RawOpusControl {
    stop_requested: AtomicBool,
    ended: AtomicBool,
}

/// Lightweight control handle for an installed raw-Opus source.
///
/// Each handle owns only the control state of the exact source instance that
/// created it. A stale handle therefore cannot stop a later replacement source.
#[derive(Clone, Debug)]
pub struct RawOpusHandle {
    control: Arc<RawOpusControl>,
}

impl RawOpusHandle {
    /// Requests that this exact source stop on the next mixer pull.
    pub fn stop(&self) {
        self.control.stop_requested.store(true, Ordering::Release);
    }

    /// Returns whether this exact source instance has left the mixer.
    #[must_use]
    pub fn is_ended(&self) -> bool {
        self.control.ended.load(Ordering::Acquire)
    }
}

/// Internal transport-message payload for one raw-Opus source instance.
#[doc(hidden)]
pub struct RawOpusContext {
    pub(crate) source: Box<dyn RawOpusSource>,
    control: Arc<RawOpusControl>,
}

impl RawOpusContext {
    pub(crate) fn new<S: RawOpusSource>(source: S) -> (RawOpusHandle, Self) {
        let control = Arc::new(RawOpusControl::default());
        let handle = RawOpusHandle {
            control: Arc::clone(&control),
        };

        (
            handle,
            Self {
                source: Box::new(source),
                control,
            },
        )
    }

    pub(crate) fn stop_requested(&self) -> bool {
        self.control.stop_requested.load(Ordering::Acquire)
    }

    pub(crate) fn mark_ended(&self) {
        self.control.ended.store(true, Ordering::Release);
    }
}

impl Drop for RawOpusContext {
    fn drop(&mut self) {
        self.mark_ended();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EmptySource;

    impl RawOpusSource for EmptySource {
        fn pull_opus(&mut self, _dst: &mut [u8]) -> RawOpusRead {
            RawOpusRead::End
        }
    }

    #[test]
    fn handle_controls_only_its_source_context() {
        let (handle, context) = RawOpusContext::new(EmptySource);

        assert!(!handle.is_ended());
        assert!(!context.stop_requested());

        handle.stop();

        assert!(context.stop_requested());
        assert!(!handle.is_ended());

        drop(context);

        assert!(handle.is_ended());
    }
}
