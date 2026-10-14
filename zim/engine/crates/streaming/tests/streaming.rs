//! Hub-level integration tests for the M7 streaming module — the
//! three roadmap acceptance criteria plus retention, gap, limit,
//! and filter behaviors.
//!
//! 1. *A subscriber receives `instance.state_change` events on
//!    lifecycle transitions* — `state_changes_reach_the_subscriber`.
//! 2. *A subscriber that disconnects and reconnects with
//!    `?since_generation={n}` receives the events it missed* —
//!    `replay_from_checkpoint_returns_the_missed_events`.
//! 3. *A slow subscriber's queue fills up; oldest events are
//!    dropped; `dropped_events` increments* —
//!    `slow_subscriber_drops_oldest_and_counts`.

use std::time::Duration;

use zim_engine_domain::{InstanceId, InstanceStatus};
use zim_engine_streaming::{
    EventData, EventKind, EventKindSet, StreamEvent, StreamFilter, StreamHandle, StreamHub,
    StreamingPolicy,
};

fn state_change(from: InstanceStatus, to: InstanceStatus) -> EventData {
    EventData::StateChange { from, to }
}

fn console(line: &str) -> EventData {
    EventData::ConsoleOutput {
        line: format!("{line}\n"),
    }
}

/// Receive events from a handle until `n` are collected or a
/// deadline passes (returns whatever arrived).
async fn collect(handle: &StreamHandle, n: usize) -> Vec<StreamEvent> {
    let mut out = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while out.len() < n {
        match tokio::time::timeout_at(deadline, handle.recv()).await {
            Ok(Some(event)) => out.push((*event).clone()),
            Ok(None) | Err(_) => break,
        }
    }
    out
}

#[tokio::test]
async fn state_changes_reach_the_subscriber() {
    let hub = StreamHub::new_default();
    let instance = InstanceId::new();
    let handle = hub
        .subscribe(&StreamFilter::instance(instance))
        .expect("first subscriber is accepted");

    // Lifecycle transitions: offline → starting → running.
    hub.publish(
        instance,
        state_change(InstanceStatus::Offline, InstanceStatus::Starting),
    );
    hub.publish(
        instance,
        state_change(InstanceStatus::Starting, InstanceStatus::Running),
    );

    let events = collect(&handle, 2).await;
    assert_eq!(events.len(), 2, "both transitions must arrive");
    assert_eq!(events[0].kind, EventKind::InstanceStateChange);
    assert_eq!(events[0].generation, 1);
    assert_eq!(events[1].generation, 2);
    assert_eq!(events[0].instance_id, Some(instance));
    // §11.2 ordering: same instance ⇒ strictly increasing generations.
    assert!(events[0].generation < events[1].generation);

    // The payload is the spec's from/to status vocabulary.
    let json = serde_json::to_value(&events[0]).unwrap();
    assert_eq!(json["data"]["from"], "offline");
    assert_eq!(json["data"]["to"], "starting");
}

#[tokio::test]
async fn replay_from_checkpoint_returns_the_missed_events() {
    let hub = StreamHub::new_default();
    let instance = InstanceId::new();

    // Events happen while the subscriber is disconnected.
    for (from, to) in [
        (InstanceStatus::Offline, InstanceStatus::Starting),
        (InstanceStatus::Starting, InstanceStatus::Running),
        (InstanceStatus::Running, InstanceStatus::Stopping),
        (InstanceStatus::Stopping, InstanceStatus::Offline),
    ] {
        hub.publish(instance, state_change(from, to));
    }
    assert_eq!(hub.last_generation(instance), 4);

    // Reconnect with the checkpoint from after event 2.
    let mut filter = StreamFilter::instance(instance);
    filter.since_generation = Some(2);
    let handle = hub.subscribe(&filter).expect("reconnect is accepted");

    let events = collect(&handle, 2).await;
    assert_eq!(events.len(), 2, "exactly the missed events replay");
    assert_eq!(events[0].generation, 3);
    assert_eq!(events[1].generation, 4);
    // No gap: everything after the checkpoint was retained.
    assert!(events.iter().all(|e| e.kind != EventKind::StreamGap));

    // Live events continue after the replay.
    hub.publish(
        instance,
        state_change(InstanceStatus::Offline, InstanceStatus::Starting),
    );
    let live = collect(&handle, 1).await;
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].generation, 5);
}

#[tokio::test]
async fn slow_subscriber_drops_oldest_and_counts() {
    // A tiny queue and a subscriber that never reads.
    let policy = StreamingPolicy {
        queue_capacity: 4,
        ..StreamingPolicy::default()
    };
    let hub = StreamHub::new(policy);
    let instance = InstanceId::new();
    let handle = hub
        .subscribe(&StreamFilter::instance(instance))
        .expect("subscriber is accepted");

    // Publish well past the queue capacity without draining.
    for generation in 1..=20u64 {
        hub.publish(
            instance,
            EventData::ConsoleOutput {
                line: format!("line {generation}\n"),
            },
        );
    }

    assert!(
        handle.dropped_events() > 0,
        "the dropped_events counter must increment"
    );
    assert!(
        handle.dropped_events() >= 20 - 4,
        "at least (published - capacity) events were dropped, got {}",
        handle.dropped_events()
    );

    // The retained tail is delivered newest-last, and a
    // stream.dropped meta-event is present (§11.2).
    let events = collect(&handle, 4).await;
    assert!(!events.is_empty());
    assert!(
        events.iter().any(|e| e.kind == EventKind::StreamDropped),
        "a stream.dropped meta-event must follow backpressure"
    );
    let generations: Vec<u64> = events.iter().map(|e| e.generation).collect();
    assert_eq!(
        generations.last().copied(),
        Some(20),
        "the newest event survives"
    );
}

#[tokio::test]
async fn console_retention_eviction_produces_a_gap_on_replay() {
    // Tiny console retention: 8 events, 4 KiB.
    let policy = StreamingPolicy {
        console_retention_events: 8,
        console_retention_bytes: 4096,
        ..StreamingPolicy::default()
    };
    let hub = StreamHub::new(policy);
    let instance = InstanceId::new();

    // 40 console events: generations 1..=40, retention holds the
    // last 8 (33..=40).
    for generation in 1..=40u64 {
        hub.publish(
            instance,
            EventData::ConsoleOutput {
                line: format!("line {generation}\n"),
            },
        );
    }
    // One durable event: generation 41, retained forever.
    hub.publish(
        instance,
        state_change(InstanceStatus::Offline, InstanceStatus::Running),
    );

    // Reconnect from zero: generations 1..=32 are missing (evicted),
    // 33..=41 are retained.
    let mut filter = StreamFilter::instance(instance);
    filter.since_generation = Some(0);
    let handle = hub.subscribe(&filter).expect("reconnect is accepted");
    let events = collect(&handle, 12).await;

    let gap = events
        .iter()
        .find(|e| e.kind == EventKind::StreamGap)
        .expect("a stream.gap meta-event must cover the eviction");
    let json = serde_json::to_value(gap).unwrap();
    assert_eq!(json["data"]["from_generation"], 1);
    assert_eq!(json["data"]["to_generation"], 32);

    // Everything retained past the checkpoint is replayed, in
    // generation order, ending at the newest event.
    let replayed: Vec<&StreamEvent> = events
        .iter()
        .filter(|e| e.kind != EventKind::StreamGap)
        .collect();
    let generations: Vec<u64> = replayed.iter().map(|e| e.generation).collect();
    assert_eq!(generations.first().copied(), Some(33));
    assert_eq!(generations.last().copied(), Some(41));
    assert_eq!(generations, {
        let mut sorted = generations.clone();
        sorted.sort_unstable();
        sorted
    });
}

#[tokio::test]
async fn durable_events_are_never_evicted() {
    let policy = StreamingPolicy {
        console_retention_events: 2,
        console_retention_bytes: 128,
        ..StreamingPolicy::default()
    };
    let hub = StreamHub::new(policy);
    let instance = InstanceId::new();

    // State changes interleave with heavy console traffic.
    for round in 0..10 {
        hub.publish(
            instance,
            state_change(InstanceStatus::Offline, InstanceStatus::Running),
        );
        for line in 0..5 {
            hub.publish(instance, console(&format!("r{round}l{line}")));
        }
    }

    // Replaying from zero must still see every state change
    // ("all since instance creation", §11.3).
    let mut filter = StreamFilter::instance(instance);
    filter.since_generation = Some(0);
    let handle = hub.subscribe(&filter).expect("reconnect is accepted");
    let events = collect(&handle, 64).await;
    let state_changes: Vec<&StreamEvent> = events
        .iter()
        .filter(|e| e.kind == EventKind::InstanceStateChange)
        .collect();
    assert_eq!(
        state_changes.len(),
        10,
        "state changes are retained forever"
    );

    // …while the console lane only holds its bounded tail.
    let console_events: Vec<&StreamEvent> = events
        .iter()
        .filter(|e| e.kind == EventKind::InstanceConsoleOutput)
        .collect();
    assert!(console_events.len() <= 2, "console retention is bounded");

    // A gap notice covered the evicted console history.
    assert!(events.iter().any(|e| e.kind == EventKind::StreamGap));
}

#[tokio::test]
async fn kind_filter_applies_to_live_and_replay() {
    let hub = StreamHub::new_default();
    let instance = InstanceId::new();
    for generation in 1..=4 {
        if generation % 2 == 0 {
            hub.publish(
                instance,
                state_change(InstanceStatus::Offline, InstanceStatus::Running),
            );
        } else {
            hub.publish(instance, console("noise"));
        }
    }

    let mut filter = StreamFilter::instance(instance);
    filter.kinds = EventKindSet::of(&[EventKind::InstanceStateChange]);
    filter.since_generation = Some(0);
    let handle = hub.subscribe(&filter).expect("subscription is accepted");

    let events = collect(&handle, 4).await;
    assert!(
        events
            .iter()
            .all(|e| e.kind == EventKind::InstanceStateChange),
        "only the requested kinds arrive"
    );
    assert_eq!(events.len(), 2);
}

#[tokio::test]
async fn subscriber_limit_is_enforced() {
    let policy = StreamingPolicy {
        max_subscribers_per_instance: 2,
        ..StreamingPolicy::default()
    };
    let hub = StreamHub::new(policy);
    let instance = InstanceId::new();

    let first = hub.subscribe(&StreamFilter::instance(instance)).unwrap();
    let second = hub.subscribe(&StreamFilter::instance(instance)).unwrap();
    let third = hub.subscribe(&StreamFilter::instance(instance));

    assert!(third.is_err(), "the third subscriber must be rejected");
    assert_eq!(hub.subscriber_count(instance), 2);

    // Dropping a handle frees its slot.
    drop(second);
    // The hub prunes dead subscribers on the next count/subscribe.
    assert_eq!(hub.subscriber_count(instance), 1);
    let replacement = hub.subscribe(&StreamFilter::instance(instance));
    assert!(replacement.is_ok());
    let _ = first;
}

#[tokio::test]
async fn global_lane_streams_and_replays() {
    let hub = StreamHub::new_default();
    let node = zim_engine_domain::NodeId::new();

    let handle = hub
        .subscribe(&StreamFilter::global())
        .expect("global subscriber is accepted");
    hub.publish_global(EventData::NodeStateChange {
        node_id: node,
        state: "online".to_owned(),
    });

    let events = collect(&handle, 1).await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, EventKind::NodeStateChange);
    assert_eq!(events[0].instance_id, None);

    // Replay from zero after a "reconnect".
    drop(handle);
    let mut filter = StreamFilter::global();
    filter.since_generation = Some(0);
    let handle = hub.subscribe(&filter).expect("reconnect is accepted");
    let replayed = collect(&handle, 1).await;
    assert_eq!(
        replayed.len(),
        1,
        "the global lane replays its retained log"
    );
}

#[tokio::test]
async fn events_serialize_as_spec_envelopes() {
    let hub = StreamHub::new_default();
    let instance = InstanceId::new();
    let handle = hub
        .subscribe(&StreamFilter::instance(instance))
        .expect("subscriber is accepted");

    hub.publish(
        instance,
        EventData::ResourceUsage(zim_engine_streaming::ResourceUsageData {
            memory_usage_bytes: Some(512 * 1024 * 1024),
            memory_limit_bytes: Some(1024 * 1024 * 1024),
            pids_current: Some(42),
            ..Default::default()
        }),
    );

    let events = collect(&handle, 1).await;
    let json = serde_json::to_value(&events[0]).unwrap();
    assert_eq!(json["type"], "instance.resource_usage");
    assert_eq!(json["generation"], 1);
    assert!(json["ts"].as_str().is_some_and(|ts| ts.ends_with('Z')));
    assert_eq!(
        json["instance_id"].as_str(),
        Some(instance.to_string().as_str())
    );
    assert_eq!(json["data"]["memory_usage_bytes"], 536_870_912);
    assert_eq!(json["data"]["pids_current"], 42);
    assert!(json.get("data").is_some());
}
