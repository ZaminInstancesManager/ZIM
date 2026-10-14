//! Subscriber handles and the bounded, drop-oldest delivery queue.
//!
//! One [`StreamHandle`] is one subscriber's view of the stream: an
//! ordered queue of [`StreamEvent`]s (the per-instance generation
//! order the hub serialized at publish time) with a bounded capacity
//! and explicit backpressure. The queue is deliberately *not* a
//! `tokio::sync::mpsc` channel: dropping the *oldest* queued event —
//! ZIM_API_SPEC.md §11.2's strategy, "older console output is less
//! valuable than newer" — requires deque semantics an mpsc receiver
//! cannot provide.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use tokio::sync::Notify;

use crate::event::{EventData, StreamEvent};
use crate::filter::StreamFilter;

/// A subscriber's registration id (diagnostics + test assertions).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubscriberId(pub u64);

/// The shared subscriber state (the handle is a cheap clone).
pub(crate) struct SubscriberShared {
    id: SubscriberId,
    filter: StreamFilter,
    queue: Mutex<VecDeque<Arc<StreamEvent>>>,
    notify: Notify,
    dropped: AtomicU64,
    drop_notice_pending: AtomicBool,
    closed: AtomicBool,
    capacity: usize,
}

impl SubscriberShared {
    pub(crate) fn new(id: SubscriberId, filter: StreamFilter, capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            id,
            filter,
            queue: Mutex::new(VecDeque::with_capacity(capacity)),
            notify: Notify::new(),
            dropped: AtomicU64::new(0),
            drop_notice_pending: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            capacity: capacity.max(2),
        })
    }

    /// The registration id.
    pub(crate) fn id(&self) -> SubscriberId {
        self.id
    }

    /// Whether the subscriber closed its handle.
    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// The cumulative dropped-event count (§11.2's
    /// `dropped_events`).
    pub(crate) fn dropped_events(&self) -> u64 {
        self.dropped.load(Ordering::Acquire)
    }

    /// Offer one event to the subscriber (the hub's fan-out call).
    ///
    /// Applies the filter, then the §11.2 backpressure invariant:
    /// the queue never exceeds `capacity`; when a push would
    /// overflow, the oldest queued event is dropped and counted, and
    /// a `stream.dropped` meta-event takes the freed slot (on "the
    /// next available slot", per the spec).
    pub(crate) fn offer(&self, event: Arc<StreamEvent>) {
        if !self.filter.matches(&event) {
            return;
        }
        let mut queue = self.lock_queue();
        if queue.len() >= self.capacity {
            // Drop the oldest event (never the newest) and remember
            // that a drop notice is owed.
            let _ = queue.pop_front();
            self.dropped.fetch_add(1, Ordering::AcqRel);
            self.drop_notice_pending.store(true, Ordering::Release);
        }
        if self.drop_notice_pending.load(Ordering::Acquire) && queue.len() < self.capacity {
            let notice = Arc::new(StreamEvent::meta_event(
                Utc::now(),
                event.instance_id,
                EventData::StreamDropped {
                    dropped_events: self.dropped.load(Ordering::Acquire),
                },
            ));
            queue.push_back(notice);
            self.drop_notice_pending.store(false, Ordering::Release);
            // The notice consumed the freed slot; make one more for
            // the event being delivered.
            if queue.len() >= self.capacity {
                let _ = queue.pop_front();
                self.dropped.fetch_add(1, Ordering::AcqRel);
                self.drop_notice_pending.store(true, Ordering::Release);
            }
        }
        queue.push_back(event);
        drop(queue);
        self.notify.notify_one();
    }

    /// Offer an engine-synthesized meta-event directly (bypasses the
    /// kind filter — the hub only synthesizes meta-events a
    /// reconnecting subscriber must see; it still respects capacity).
    pub(crate) fn offer_meta(&self, event: Arc<StreamEvent>) {
        let mut queue = self.lock_queue();
        if queue.len() >= self.capacity {
            let _ = queue.pop_front();
            self.dropped.fetch_add(1, Ordering::AcqRel);
            self.drop_notice_pending.store(true, Ordering::Release);
        }
        queue.push_back(event);
        drop(queue);
        self.notify.notify_one();
    }

    /// Pop the next event, or `None` when the queue is empty (the
    /// caller decides whether that is terminal).
    pub(crate) fn pop(&self) -> Option<Arc<StreamEvent>> {
        self.lock_queue().pop_front()
    }

    /// Whether the queue currently holds events.
    pub(crate) fn has_events(&self) -> bool {
        !self.lock_queue().is_empty()
    }

    /// Wake a blocked `recv` (used by close and by shutdown).
    pub(crate) fn wake(&self) {
        self.notify.notify_one();
    }

    fn lock_queue(&self) -> std::sync::MutexGuard<'_, VecDeque<Arc<StreamEvent>>> {
        self.queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// A subscriber's handle onto the stream (cheap to clone; the queue
/// lives in shared interior state and the subscription ends when the
/// last handle drops or calls [`StreamHandle::close`]).
///
/// Events arrive in per-instance generation order (§11.2). A closed
/// or dropped subscription drains its already-queued events first,
/// then yields `None`.
#[derive(Clone)]
pub struct StreamHandle {
    pub(crate) shared: Arc<SubscriberShared>,
}

impl StreamHandle {
    /// Await the next event.
    ///
    /// Returns `None` once the subscription is closed *and* the
    /// queued events are drained. Waking is notification-based: an
    /// idle subscriber parks on a `Notify` and consumes no CPU.
    pub async fn recv(&self) -> Option<Arc<StreamEvent>> {
        loop {
            if let Some(event) = self.shared.pop() {
                return Some(event);
            }
            if self.shared.is_closed() {
                return None;
            }
            // Register interest *before* re-checking so a publish
            // racing between the pop attempt and the park cannot
            // lose its wakeup (Notify stores a permit).
            let notified = self.shared.notify.notified();
            if self.shared.has_events() || self.shared.is_closed() {
                continue;
            }
            notified.await;
        }
    }

    /// The cumulative count of events dropped from this subscriber's
    /// queue by backpressure (§11.2's `dropped_events`).
    pub fn dropped_events(&self) -> u64 {
        self.shared.dropped_events()
    }

    /// End the subscription (queued events may still be drained via
    /// [`Self::recv`]; the hub stops delivering to this subscriber at
    /// the next publish).
    pub fn close(&self) {
        self.shared.closed.store(true, Ordering::Release);
        self.shared.wake();
    }

    /// The registration id.
    pub fn id(&self) -> SubscriberId {
        self.shared.id()
    }
}

impl std::fmt::Debug for StreamHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamHandle")
            .field("id", &self.shared.id())
            .field("dropped_events", &self.shared.dropped_events())
            .field("closed", &self.shared.is_closed())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::EventData;
    use crate::filter::{EventKindSet, StreamFilter};
    use zim_engine_domain::{InstanceId, InstanceStatus};

    fn state_event(generation: u64, instance: InstanceId) -> Arc<StreamEvent> {
        Arc::new(StreamEvent::lane_event(
            generation,
            Utc::now(),
            Some(instance),
            EventData::StateChange {
                from: InstanceStatus::Offline,
                to: InstanceStatus::Running,
            },
        ))
    }

    fn shared_with_capacity(capacity: usize, instance: InstanceId) -> Arc<SubscriberShared> {
        SubscriberShared::new(SubscriberId(1), StreamFilter::instance(instance), capacity)
    }

    #[test]
    fn queue_never_exceeds_capacity_and_counts_drops() {
        let instance = InstanceId::new();
        let shared = shared_with_capacity(4, instance);
        for generation in 1..=10u64 {
            shared.offer(state_event(generation, instance));
        }
        let queue = shared.lock_queue();
        assert!(queue.len() <= 4, "capacity invariant violated");
        // 10 pushes into a 4-slot queue with the notice occupying a
        // slot: at least 10 - 4 events were dropped.
        assert!(shared.dropped_events() >= 6);
        assert!(shared.dropped_events() >= 1);
        // The newest event survived (drop-oldest, never newest).
        let last = queue.back().expect("queue is not empty");
        assert_eq!(last.generation, 10);
        // A drop notice is in the queue (the "next available slot"
        // meta-event).
        assert!(queue
            .iter()
            .any(|e| e.kind == crate::event::EventKind::StreamDropped));
    }

    #[tokio::test]
    async fn recv_drains_then_parks_then_wakes() {
        let instance = InstanceId::new();
        let shared = shared_with_capacity(8, instance);
        shared.offer(state_event(1, instance));
        shared.offer(state_event(2, instance));

        let handle = StreamHandle {
            shared: Arc::clone(&shared),
        };
        assert_eq!(handle.recv().await.unwrap().generation, 1);
        assert_eq!(handle.recv().await.unwrap().generation, 2);

        // Park a receiver, then wake it with a publish from outside
        // the task.
        let waiter = tokio::spawn({
            let handle = handle.clone();
            async move { handle.recv().await.map(|e| e.generation) }
        });
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(!waiter.is_finished());
        shared.offer(state_event(3, instance));
        assert_eq!(waiter.await.unwrap(), Some(3));
    }

    #[tokio::test]
    async fn closed_handle_drains_then_returns_none() {
        let instance = InstanceId::new();
        let shared = shared_with_capacity(8, instance);
        shared.offer(state_event(1, instance));
        let handle = StreamHandle {
            shared: Arc::clone(&shared),
        };
        handle.close();
        // Queued event still drains…
        assert_eq!(handle.recv().await.unwrap().generation, 1);
        // …then the closed subscription yields None.
        assert!(handle.recv().await.is_none());
    }

    #[test]
    fn filter_mismatch_is_ignored() {
        let filter = StreamFilter {
            instance_id: Some(InstanceId::new()),
            kinds: EventKindSet::of(&[crate::event::EventKind::InstanceConsoleOutput]),
            since_generation: None,
        };
        let shared = SubscriberShared::new(SubscriberId(9), filter, 8);
        let other = InstanceId::new();
        shared.offer(state_event(1, other)); // wrong kind AND wrong instance
        assert!(!shared.has_events());
        assert_eq!(shared.dropped_events(), 0);
    }
}
