//! Pull-based paste pacing without sleeping, threads, or an async runtime.

use std::collections::VecDeque;
use std::time::Duration;

use crate::paste::{PastePlan, Stroke};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SchedulePoll {
    Ready(Stroke),
    Wait(Duration),
    Complete,
    Cancelled,
}

/// Emits at most one complete stroke per poll. Pass monotonic elapsed time, for
/// example `Instant::elapsed()`. Late polls do not create catch-up bursts.
///
/// A stroke represents both press and release; the caller must finish that pair
/// before polling again. Cancelling discards queued strokes but cannot release
/// keys already handed to the caller; use `InputState::release_all` for cleanup.
#[derive(Debug)]
pub struct PasteScheduler {
    pending: VecDeque<Stroke>,
    interval: Duration,
    last_emitted_at: Option<Duration>,
    cancelled: bool,
}

impl PasteScheduler {
    pub fn new(plan: PastePlan, interval: Duration) -> Self {
        Self {
            pending: plan.into_strokes().into(),
            interval,
            last_emitted_at: None,
            cancelled: false,
        }
    }

    pub fn remaining(&self) -> usize {
        self.pending.len()
    }

    pub fn cancel(&mut self) {
        self.pending.clear();
        self.cancelled = true;
    }

    pub fn poll(&mut self, now: Duration) -> SchedulePoll {
        if self.cancelled {
            return SchedulePoll::Cancelled;
        }
        if self.pending.is_empty() {
            return SchedulePoll::Complete;
        }
        if let Some(last_emitted_at) = self.last_emitted_at {
            // Subtraction also avoids overflowing a deadline near Duration::MAX.
            // A backwards clock cannot make a stroke due before its interval.
            let elapsed = now.saturating_sub(last_emitted_at);
            if elapsed < self.interval {
                return SchedulePoll::Wait(self.interval - elapsed);
            }
        }
        let stroke = self.pending.pop_front().expect("pending queue was checked");
        self.last_emitted_at = Some(now);
        SchedulePoll::Ready(stroke)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FakeClock(Duration);

    impl FakeClock {
        fn now(&self) -> Duration {
            self.0
        }

        fn advance(&mut self, millis: u64) {
            self.0 += Duration::from_millis(millis);
        }
    }

    fn scheduler(text: &str, interval_ms: u64) -> PasteScheduler {
        PasteScheduler::new(
            PastePlan::new(text, false).unwrap(),
            Duration::from_millis(interval_ms),
        )
    }

    #[test]
    fn fake_clock_controls_exact_pacing_and_completion() {
        let mut clock = FakeClock::default();
        let mut scheduler = scheduler("ab", 10);
        assert_eq!(scheduler.remaining(), 2);
        assert_eq!(
            scheduler.poll(clock.now()),
            SchedulePoll::Ready(Stroke {
                usb_key: 4,
                modifiers: 0
            })
        );
        assert_eq!(
            scheduler.poll(clock.now()),
            SchedulePoll::Wait(Duration::from_millis(10))
        );
        clock.advance(9);
        assert_eq!(
            scheduler.poll(clock.now()),
            SchedulePoll::Wait(Duration::from_millis(1))
        );
        clock.advance(1);
        assert_eq!(
            scheduler.poll(clock.now()),
            SchedulePoll::Ready(Stroke {
                usb_key: 5,
                modifiers: 0
            })
        );
        assert_eq!(scheduler.remaining(), 0);
        assert_eq!(scheduler.poll(clock.now()), SchedulePoll::Complete);
    }

    #[test]
    fn a_late_poll_does_not_emit_a_catch_up_burst() {
        let mut clock = FakeClock::default();
        let mut scheduler = scheduler("abc", 10);
        assert!(matches!(
            scheduler.poll(clock.now()),
            SchedulePoll::Ready(_)
        ));
        clock.advance(1000);
        assert!(matches!(
            scheduler.poll(clock.now()),
            SchedulePoll::Ready(_)
        ));
        assert_eq!(
            scheduler.poll(clock.now()),
            SchedulePoll::Wait(Duration::from_millis(10))
        );
        assert_eq!(scheduler.remaining(), 1);
    }

    #[test]
    fn cancellation_before_start_and_mid_paste_is_final() {
        for emitted in [0, 1] {
            let mut clock = FakeClock::default();
            let mut scheduler = scheduler("ab", 10);
            if emitted == 1 {
                assert!(matches!(
                    scheduler.poll(clock.now()),
                    SchedulePoll::Ready(_)
                ));
            }
            scheduler.cancel();
            assert_eq!(scheduler.remaining(), 0);
            assert_eq!(scheduler.poll(clock.now()), SchedulePoll::Cancelled);
            clock.advance(1000);
            scheduler.cancel();
            assert_eq!(scheduler.poll(clock.now()), SchedulePoll::Cancelled);
        }
    }

    #[test]
    fn empty_plans_complete_immediately() {
        let mut scheduler = scheduler("", 10);
        assert_eq!(scheduler.poll(Duration::ZERO), SchedulePoll::Complete);
        assert_eq!(scheduler.poll(Duration::MAX), SchedulePoll::Complete);
    }

    #[test]
    fn zero_interval_still_emits_one_stroke_per_poll() {
        let mut scheduler = scheduler("ab", 0);
        assert!(matches!(
            scheduler.poll(Duration::ZERO),
            SchedulePoll::Ready(_)
        ));
        assert_eq!(scheduler.remaining(), 1);
        assert!(matches!(
            scheduler.poll(Duration::ZERO),
            SchedulePoll::Ready(_)
        ));
        assert_eq!(scheduler.poll(Duration::ZERO), SchedulePoll::Complete);
    }

    #[test]
    fn backwards_time_does_not_make_a_stroke_due() {
        let mut scheduler = scheduler("ab", 10);
        assert!(matches!(
            scheduler.poll(Duration::from_millis(20)),
            SchedulePoll::Ready(_)
        ));
        assert_eq!(
            scheduler.poll(Duration::from_millis(10)),
            SchedulePoll::Wait(Duration::from_millis(10))
        );
        assert_eq!(
            scheduler.poll(Duration::from_millis(29)),
            SchedulePoll::Wait(Duration::from_millis(1))
        );
        assert!(matches!(
            scheduler.poll(Duration::from_millis(30)),
            SchedulePoll::Ready(_)
        ));
    }

    #[test]
    fn time_near_duration_max_does_not_overflow() {
        let mut scheduler = scheduler("ab", 10);
        let start = Duration::MAX - Duration::from_millis(5);
        assert!(matches!(scheduler.poll(start), SchedulePoll::Ready(_)));
        assert_eq!(
            scheduler.poll(Duration::MAX),
            SchedulePoll::Wait(Duration::from_millis(5))
        );
    }
}
