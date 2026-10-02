//! The whole-connection idle timeout: the lock-free clock both relay directions
//! bump on activity, and the watchdog that resolves once both have been silent for
//! `--timeout`.
//!
//! Only the mechanism lives here. The clock is created, handed to both
//! [`relay`](super::relay) calls and raced against them in
//! [`incoming_connection_handle`](super::incoming_connection_handle), which is also
//! where the idle-close line is logged — see the comments there.

use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::time::Instant;
use tokio::time::sleep_until;

/// Shared "last activity" clock for a connection's idle timeout. It records the
/// most recent moment either direction relayed data, as milliseconds since the
/// connection started; interior mutability lets both relay directions update it
/// through a shared reference.
pub(super) struct ActivityClock {
    started: Instant,
    // `Relaxed` is deliberate. The relays and the watchdog that touch this are
    // cooperatively-scheduled sub-futures of a *single* task: `super::incoming_connection_handle`
    // composes them with `join!`/`select!` rather than spawning them, so they never
    // access this from two threads at once. That is enforced, not just intended:
    // `record` takes `&self` and `wait_until_idle` takes `&ActivityClock`, so a
    // future holding either is not `'static` and cannot be handed to `tokio::spawn`.
    // It is also a self-contained timestamp that guards no other memory, so there is
    // nothing for Acquire/Release to publish; single-location coherence is the whole
    // requirement, and the watchdog re-reads after sleeping whole seconds, far longer
    // than any store can take to become visible.
    last_active_millis: AtomicU64,
}

impl ActivityClock {
    pub(super) fn new() -> Self {
        Self {
            started: Instant::now(),
            last_active_millis: AtomicU64::new(0),
        }
    }

    /// Record that data just moved in some direction (resets the idle timer).
    ///
    /// `pub(super)` only because the caller, [`relay`](super::relay), lives one
    /// module up beside the composition it is part of.
    pub(super) fn record(&self) {
        self.last_active_millis
            .store(self.started.elapsed().as_millis() as u64, Ordering::Relaxed);
    }

    /// The instant at which the connection is considered idle for `idle`.
    fn idle_deadline(&self, idle: Duration) -> Instant {
        let last_active = Duration::from_millis(self.last_active_millis.load(Ordering::Relaxed));
        // `idle` is the `--timeout` value, which `args::Arguments` range-validates to
        // at most ~100 years, so this `Instant + Duration` can never overflow the
        // monotonic clock (which would otherwise panic).
        self.started + last_active + idle
    }
}

/// Resolve once the connection has seen no activity in either direction for
/// `idle`, re-arming whenever fresh activity pushes the deadline out.
pub(super) async fn wait_until_idle(clock: &ActivityClock, idle: Duration) {
    loop {
        sleep_until(clock.idle_deadline(idle)).await;
        if Instant::now() >= clock.idle_deadline(idle) {
            return;
        }
    }
}
