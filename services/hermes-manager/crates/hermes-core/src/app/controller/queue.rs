//! The controller's work queue: each instance at most once in the queue and
//! at most once in flight. A request while it runs makes it run again after.
//! Time is passed in, so the queue is deterministic under test.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};

use uuid::Uuid;

#[derive(Debug, Default)]
pub struct WorkQueue {
    ready: VecDeque<Uuid>,
    queued: BTreeSet<Uuid>,
    running: BTreeSet<Uuid>,
    dirty: BTreeSet<Uuid>,
    delayed: BTreeMap<Uuid, Instant>,
    failures: BTreeMap<Uuid, u32>,
}

impl WorkQueue {
    /// Queues `instance` now.
    pub fn add(&mut self, instance: Uuid) {
        self.delayed.remove(&instance);
        if self.running.contains(&instance) {
            self.dirty.insert(instance);
        } else if self.queued.insert(instance) {
            self.ready.push_back(instance);
        }
    }

    /// Queues `instance` at `due`, unless it is already due earlier.
    pub fn add_at(&mut self, instance: Uuid, due: Instant) {
        if self.queued.contains(&instance) || self.dirty.contains(&instance) {
            return;
        }
        let entry = self.delayed.entry(instance).or_insert(due);
        if due < *entry {
            *entry = due;
        }
    }

    /// Moves every delayed item due at `now` into the ready queue.
    pub fn promote(&mut self, now: Instant) {
        let due: Vec<Uuid> = self
            .delayed
            .iter()
            .filter(|(_, at)| **at <= now)
            .map(|(id, _)| *id)
            .collect();
        for instance in due {
            self.add(instance);
        }
    }

    /// The next instance to reconcile; it stays in flight until [`Self::done`].
    pub fn take(&mut self) -> Option<Uuid> {
        let instance = self.ready.pop_front()?;
        self.queued.remove(&instance);
        self.running.insert(instance);
        Some(instance)
    }

    /// Finishes one pass. `requeue` schedules the next one at that time;
    /// a request that arrived meanwhile runs it again now.
    pub fn done(&mut self, instance: Uuid, outcome: Outcome, now: Instant) {
        self.running.remove(&instance);
        let retry = match outcome {
            Outcome::Requeue(after) => {
                self.failures.remove(&instance);
                after.map(|after| now + after)
            }
            Outcome::Failed => {
                let failures = self.failures.entry(instance).or_insert(0);
                *failures = failures.saturating_add(1);
                Some(now + backoff(*failures))
            }
        };
        if self.dirty.remove(&instance) {
            self.add(instance);
        } else if let Some(due) = retry {
            self.add_at(instance, due);
        }
    }

    /// When the earliest delayed item is due.
    pub fn next_due(&self) -> Option<Instant> {
        self.delayed.values().min().copied()
    }

    pub fn in_flight(&self) -> usize {
        self.running.len()
    }

    /// Forgets everything; used when leadership is lost.
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Succeeded; reconcile again after the delay, if any.
    Requeue(Option<Duration>),
    /// Failed; retry with capped exponential backoff.
    Failed,
}

/// 1 s, 2 s, 4 s … capped at 60 s.
pub fn backoff(failures: u32) -> Duration {
    Duration::from_secs(1u64 << failures.saturating_sub(1).min(6)).min(Duration::from_secs(60))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    #[test]
    fn an_instance_is_queued_once_and_runs_once_at_a_time() {
        let mut queue = WorkQueue::default();
        queue.add(id(1));
        queue.add(id(1));
        queue.add(id(2));
        assert_eq!(queue.take(), Some(id(1)));
        queue.add(id(1));
        assert_eq!(
            queue.take(),
            Some(id(2)),
            "a running instance is not handed out again"
        );
        assert_eq!(queue.take(), None);
        let now = Instant::now();
        queue.done(id(1), Outcome::Requeue(None), now);
        assert_eq!(
            queue.take(),
            Some(id(1)),
            "a request during the pass runs it again"
        );
        queue.done(id(1), Outcome::Requeue(None), now);
        queue.done(id(2), Outcome::Requeue(None), now);
        assert_eq!(queue.take(), None);
        assert_eq!(queue.in_flight(), 0);
    }

    #[test]
    fn requeues_wait_until_due_and_the_earliest_wins() {
        let mut queue = WorkQueue::default();
        let start = Instant::now();
        queue.add(id(1));
        assert_eq!(queue.take(), Some(id(1)));
        queue.done(
            id(1),
            Outcome::Requeue(Some(Duration::from_secs(30))),
            start,
        );
        queue.add_at(id(1), start + Duration::from_secs(5));
        assert_eq!(queue.next_due(), Some(start + Duration::from_secs(5)));
        queue.promote(start + Duration::from_secs(4));
        assert_eq!(queue.take(), None);
        queue.promote(start + Duration::from_secs(5));
        assert_eq!(queue.take(), Some(id(1)));
        assert_eq!(queue.next_due(), None);
    }

    #[test]
    fn a_notification_overrides_a_pending_delay() {
        let mut queue = WorkQueue::default();
        let start = Instant::now();
        queue.add_at(id(1), start + Duration::from_secs(60));
        queue.add(id(1));
        assert_eq!(queue.take(), Some(id(1)));
        assert_eq!(queue.next_due(), None);
    }

    #[test]
    fn failures_back_off_exponentially_and_reset_on_success() {
        assert_eq!(backoff(1), Duration::from_secs(1));
        assert_eq!(backoff(2), Duration::from_secs(2));
        assert_eq!(backoff(4), Duration::from_secs(8));
        assert_eq!(backoff(30), Duration::from_secs(60));
        let mut queue = WorkQueue::default();
        let start = Instant::now();
        for (round, expected) in [(0, 1), (1, 2), (2, 4)] {
            queue.add(id(1));
            assert_eq!(queue.take(), Some(id(1)), "round {round}");
            queue.done(id(1), Outcome::Failed, start);
            assert_eq!(
                queue.next_due(),
                Some(start + Duration::from_secs(expected))
            );
            queue.promote(start + Duration::from_secs(expected));
        }
        assert_eq!(queue.take(), Some(id(1)));
        queue.done(id(1), Outcome::Requeue(None), start);
        queue.add(id(1));
        queue.take();
        queue.done(id(1), Outcome::Failed, start);
        assert_eq!(
            queue.next_due(),
            Some(start + Duration::from_secs(1)),
            "success resets the backoff"
        );
    }

    #[test]
    fn clearing_forgets_everything() {
        let mut queue = WorkQueue::default();
        queue.add(id(1));
        queue.add_at(id(2), Instant::now());
        queue.take();
        queue.clear();
        assert_eq!(queue.take(), None);
        assert_eq!(queue.next_due(), None);
        assert_eq!(queue.in_flight(), 0);
    }
}
