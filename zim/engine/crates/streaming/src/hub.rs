//! The streaming hub: per-instance event lanes, retention, and
//! subscriber fan-out.
//!
//! [`StreamHub`] is the concrete [`StreamSource`] the engine wires
//! into the supervisor (producer side) and the `/api/v1/stream`
//! WebSocket endpoint (consumer side). It is a cheap handle over
//! shared interior state; clone it freely.
//!
//! # Lanes and ordering
//!
//! Each instance owns one lane. All publishes to a lane — and the
//! fan-out to that lane's subscribers — happen under the lane's
//! lock, which establishes the §11.2 ordering guarantee (events for
//! one `instance_id` are delivered in generation order) and makes
//! replay + live handoff race-free: a subscribe either sees an
//! event in the retention log (and replays it) or is registered
//! before the event's fan-out — never both, never neither.
//!
//! Engine-global events (node state changes, audit records) live on
//! a single global lane with its own generation counter.
//!
//! # Retention (ZIM_API_SPEC.md §11.3)
//!
//! - Console output: bounded by events *and* bytes (defaults 10k
//!   events / 1 MB per instance). Eviction is from the front (the
//!   oldest lines go first).
//! - Everything else (state changes, install/job progress): kept
//!   for the process lifetime — "all since instance creation".
//! - The global lane: bounded by event count.
//!
//! Retention is in-memory by design: the engine's restart clears
//! the buffers, and a reconnecting client that asks for generations
//! the fresh engine never produced receives a `stream.gap` (it then
//! re-fetches current state over HTTP, per the spec).
//!
//! # Replay gaps
//!
//! On a checkpointed subscribe (`since_generation = Some(n)`), the
//! hub replays retained events with generation greater than `n`. If
//! any generation in `(n, last_stamped]` is no longer retained
//! (console or global-lane eviction), the subscriber first receives
//! one `stream.gap` meta-event covering the affected range — the
//! union `[min(missing), max(missing)]` — followed by every retained
//! event. The range is a summary: it may contain a few generations
//! that are also replayed after it. The spec's client contract is
//! "gap ⇒ re-fetch state via HTTP", so a precise per-hole listing
//! would add noise without changing what a correct client does.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use chrono::Utc;
use zim_engine_domain::InstanceId;

use crate::event::{EventData, StreamEvent};
use crate::filter::StreamFilter;
use crate::handle::{StreamHandle, SubscriberId, SubscriberShared};
use crate::policy::StreamingPolicy;

/// The concrete fan-out hub (cheap to clone; state is shared).
#[derive(Clone)]
pub struct StreamHub {
    inner: Arc<HubInner>,
}

struct HubInner {
    policy: StreamingPolicy,
    lanes: RwLock<HashMap<InstanceId, Arc<Mutex<Lane>>>>,
    global: Mutex<Lane>,
    next_subscriber_id: AtomicU64,
}

/// One event lane: the retained log plus its live subscribers.
struct Lane {
    /// The next generation to stamp (starts at 1; 0 is reserved for
    /// meta-events).
    next_generation: u64,
    /// Durable events, kept since lane creation (state changes,
    /// install/job progress — everything not subject to retention
    /// bounds).
    durable: VecDeque<Arc<StreamEvent>>,
    /// Console events, bounded by count and bytes.
    console: VecDeque<Arc<StreamEvent>>,
    /// The console lane's retained byte total (line payload sizes).
    console_bytes: usize,
    /// Live subscribers (weak: a dropped handle unsubscribes).
    subscribers: Vec<std::sync::Weak<SubscriberShared>>,
}

impl Lane {
    fn new() -> Self {
        Self {
            next_generation: 1,
            durable: VecDeque::new(),
            console: VecDeque::new(),
            console_bytes: 0,
            subscribers: Vec::new(),
        }
    }

    /// Stamp the next generation.
    fn stamp(&mut self) -> u64 {
        let generation = self.next_generation;
        self.next_generation += 1;
        generation
    }

    /// Append an event under its retention class.
    fn retain(&mut self, event: Arc<StreamEvent>, console_policy: (usize, usize)) {
        if event.kind.is_console() {
            self.console.push_back(Arc::clone(&event));
            self.console_bytes += event.console_bytes();
            let (max_events, max_bytes) = console_policy;
            while self.console.len() > max_events || self.console_bytes > max_bytes {
                match self.console.pop_front() {
                    Some(evicted) => {
                        self.console_bytes =
                            self.console_bytes.saturating_sub(evicted.console_bytes());
                    }
                    None => break,
                }
            }
        } else {
            self.durable.push_back(event);
        }
    }

    /// All retained events (both classes), sorted by generation.
    fn retained(&self) -> Vec<Arc<StreamEvent>> {
        let mut events: Vec<Arc<StreamEvent>> = self
            .durable
            .iter()
            .chain(self.console.iter())
            .cloned()
            .collect();
        events.sort_by_key(|e| e.generation);
        events
    }

    /// Prune dead subscribers; return the live count.
    fn prune_subscribers(&mut self) -> usize {
        self.subscribers
            .retain(|weak| weak.upgrade().is_some_and(|s| !s.is_closed()));
        self.subscribers.len()
    }

    /// Fan an event out to the lane's live subscribers.
    fn fan_out(&mut self, event: &Arc<StreamEvent>) {
        self.subscribers.retain(|weak| {
            if let Some(subscriber) = weak.upgrade() {
                if subscriber.is_closed() {
                    return false;
                }
                subscriber.offer(Arc::clone(event));
                true
            } else {
                false
            }
        });
    }
}

impl StreamHub {
    /// Build a hub with the default policy.
    pub fn new_default() -> Self {
        Self::new(StreamingPolicy::default())
    }

    /// Build a hub with an explicit policy (sanitized — see
    /// [`StreamingPolicy::sanitized`]).
    pub fn new(policy: StreamingPolicy) -> Self {
        Self {
            inner: Arc::new(HubInner {
                policy: policy.sanitized(),
                lanes: RwLock::new(HashMap::new()),
                global: Mutex::new(Lane::new()),
                next_subscriber_id: AtomicU64::new(1),
            }),
        }
    }

    /// The effective policy.
    pub fn policy(&self) -> &StreamingPolicy {
        &self.inner.policy
    }

    /// Publish an instance-scoped event: stamps the lane's next
    /// generation and the current timestamp, applies retention, and
    /// fans out to the lane's subscribers. Returns the published
    /// event.
    ///
    /// Synchronous by design: supervision's state transitions happen
    /// on the power worker under a plain lock scope, and a publish
    /// must never await (the runtime must never be slowed by a
    /// subscriber).
    pub fn publish(&self, instance_id: InstanceId, data: EventData) -> Arc<StreamEvent> {
        let lane = self.lane_for(instance_id);
        let mut lane = lock_lane(&lane);
        let event = Arc::new(StreamEvent::lane_event(
            lane.stamp(),
            Utc::now(),
            Some(instance_id),
            data,
        ));
        lane.retain(
            Arc::clone(&event),
            (
                self.inner.policy.console_retention_events,
                self.inner.policy.console_retention_bytes,
            ),
        );
        lane.fan_out(&event);
        event
    }

    /// Publish an engine-global event (node state changes, audit
    /// records) on the global lane.
    pub fn publish_global(&self, data: EventData) -> Arc<StreamEvent> {
        let mut lane = self.inner.global.lock().unwrap_or_else(|p| p.into_inner());
        let event = Arc::new(StreamEvent::lane_event(
            lane.stamp(),
            Utc::now(),
            None,
            data,
        ));
        // The global lane keeps everything durable-bounded by count.
        lane.durable.push_back(Arc::clone(&event));
        while lane.durable.len() > self.inner.policy.global_retention_events {
            if lane.durable.pop_front().is_none() {
                break;
            }
        }
        lane.fan_out(&event);
        event
    }

    /// The retained console buffer for an instance (oldest first) —
    /// the `GET /instances/{id}/console` source.
    pub fn console_buffer(&self, instance_id: InstanceId) -> Vec<Arc<StreamEvent>> {
        let lane = self.lane_for(instance_id);
        let lane = lock_lane(&lane);
        lane.console.iter().cloned().collect()
    }

    /// The most recent retained resource-usage samples for an
    /// instance (oldest first, at most `limit`) — the
    /// `GET /instances/{id}/metrics` source.
    pub fn recent_metrics(&self, instance_id: InstanceId, limit: usize) -> Vec<Arc<StreamEvent>> {
        let lane = self.lane_for(instance_id);
        let lane = lock_lane(&lane);
        let mut samples: Vec<Arc<StreamEvent>> = lane
            .durable
            .iter()
            .filter(|event| event.kind == crate::event::EventKind::InstanceResourceUsage)
            .cloned()
            .collect();
        if samples.len() > limit {
            let excess = samples.len() - limit;
            samples.drain(..excess);
        }
        samples
    }

    /// The highest generation stamped on an instance's lane (0 for
    /// an unknown instance — the replay cursor a fully caught-up
    /// client would hold).
    pub fn last_generation(&self, instance_id: InstanceId) -> u64 {
        let lane = self.lane_for(instance_id);
        let lane = lock_lane(&lane);
        lane.next_generation.saturating_sub(1)
    }

    /// How many live subscribers an instance's lane currently has
    /// (diagnostics and tests).
    pub fn subscriber_count(&self, instance_id: InstanceId) -> usize {
        let lane = self.lane_for(instance_id);
        let mut lane = lock_lane(&lane);
        lane.prune_subscribers()
    }

    /// Look up (or lazily create) an instance's lane.
    fn lane_for(&self, instance_id: InstanceId) -> Arc<Mutex<Lane>> {
        if let Some(lane) = self
            .inner
            .lanes
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&instance_id)
        {
            return Arc::clone(lane);
        }
        let mut map = self
            .inner
            .lanes
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Arc::clone(
            map.entry(instance_id)
                .or_insert_with(|| Arc::new(Mutex::new(Lane::new()))),
        )
    }

    /// Register a new subscriber id.
    fn next_subscriber_id(&self) -> SubscriberId {
        SubscriberId(self.inner.next_subscriber_id.fetch_add(1, Ordering::AcqRel))
    }
    /// Subscribe with a filter: live events (and, when the filter
    /// carries a checkpoint, the retained replay) are delivered on
    /// the returned handle.
    ///
    /// Inherent (not only the [`crate::StreamSource`] trait method)
    /// so callers that hold a concrete hub — the supervisor and the
    /// API's stream endpoint — need no trait import; the trait
    /// remains the documented seam for substitute implementations.
    pub fn subscribe(
        &self,
        filter: &StreamFilter,
    ) -> Result<StreamHandle, crate::source::SubscribeError> {
        use crate::source::SubscribeError;

        let subscriber_id = self.next_subscriber_id();
        let shared = SubscriberShared::new(
            subscriber_id,
            filter.clone(),
            self.inner.policy.queue_capacity,
        );

        match filter.instance_id {
            Some(instance_id) => {
                let lane_arc = self.lane_for(instance_id);
                let mut lane = lock_lane(&lane_arc);
                if lane.prune_subscribers() >= self.inner.policy.max_subscribers_per_instance {
                    return Err(SubscribeError::TooManySubscribers {
                        instance_id: Some(instance_id),
                        limit: self.inner.policy.max_subscribers_per_instance,
                    });
                }
                replay_into(&mut lane, &shared, filter);
                lane.subscribers.push(Arc::downgrade(&shared));
            }
            None => {
                let mut lane = self.inner.global.lock().unwrap_or_else(|p| p.into_inner());
                if lane.prune_subscribers() >= self.inner.policy.max_subscribers_per_instance {
                    return Err(SubscribeError::TooManySubscribers {
                        instance_id: None,
                        limit: self.inner.policy.max_subscribers_per_instance,
                    });
                }
                replay_into(&mut lane, &shared, filter);
                lane.subscribers.push(Arc::downgrade(&shared));
            }
        }

        Ok(StreamHandle { shared })
    }
}

/// Lock a lane, tolerating a poisoned lock (a panicking producer
/// must not wedge the whole stream; the lane state is still
/// internally consistent for reads).
fn lock_lane(lane: &Arc<Mutex<Lane>>) -> std::sync::MutexGuard<'_, Lane> {
    lane.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl crate::source::StreamSource for StreamHub {
    /// Subscribe with a filter (see the trait docs for semantics).
    fn subscribe(
        &self,
        filter: &StreamFilter,
    ) -> Result<StreamHandle, crate::source::SubscribeError> {
        StreamHub::subscribe(self, filter)
    }
}

/// Replay-from-checkpoint (ZIM_API_SPEC.md §11.3) into a fresh
/// subscriber, under the lane lock (so the replay→live handoff is
/// race-free).
///
/// Gap semantics are documented on the module: one summary
/// `stream.gap` covering the union of missing generations, then the
/// retained events in generation order.
fn replay_into(lane: &mut Lane, subscriber: &Arc<SubscriberShared>, filter: &StreamFilter) {
    let Some(since) = filter.since_generation else {
        return;
    };
    let retained: Vec<Arc<StreamEvent>> = lane.retained();
    let last_stamped = lane.next_generation.saturating_sub(1);

    // The generations a fully-retained lane could still deliver.
    let expected: Vec<u64> = (since + 1..=last_stamped).collect();
    if expected.is_empty() {
        return; // Nothing newer than the checkpoint: live only.
    }

    // Which of the expected generations survived retention?
    let present: std::collections::HashSet<u64> =
        retained.iter().map(|event| event.generation).collect();
    let missing: Vec<u64> = expected
        .iter()
        .copied()
        .filter(|g| !present.contains(g))
        .collect();

    if let (Some(&first), Some(&last)) = (missing.first(), missing.last()) {
        subscriber.offer_meta(Arc::new(StreamEvent::meta_event(
            Utc::now(),
            filter.instance_id,
            EventData::StreamGap {
                from_generation: first,
                to_generation: last,
            },
        )));
    }

    // Replay the retained events past the checkpoint that pass the
    // filter, in generation order, suppressing duplicates (§11.2's
    // same-generation suppression belt-and-braces: generations are
    // unique per lane by construction).
    let mut last_sent: Option<u64> = None;
    for event in retained {
        if event.generation <= since {
            continue;
        }
        if last_sent.is_some_and(|g| g >= event.generation) {
            continue; // Duplicate suppression.
        }
        if filter.matches(&event) {
            subscriber.offer(Arc::clone(&event));
            last_sent = Some(event.generation);
        } else {
            // Kind-filtered events still advance the cursor: the
            // subscriber simply does not receive them.
            last_sent = Some(event.generation);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_clone_shares_state() {
        let hub = StreamHub::new_default();
        let clone = hub.clone();
        let instance = InstanceId::new();
        let event = hub.publish(
            instance,
            EventData::StateChange {
                from: zim_engine_domain::InstanceStatus::Offline,
                to: zim_engine_domain::InstanceStatus::Starting,
            },
        );
        assert_eq!(event.generation, 1);
        assert_eq!(clone.last_generation(instance), 1);
    }
}
