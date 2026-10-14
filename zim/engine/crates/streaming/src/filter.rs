//! Subscription filters and the event-kind bit set.

use zim_engine_domain::InstanceId;

use crate::event::{EventKind, StreamEvent};

/// A compact set of [`EventKind`] values (one bit per kind) — the
/// M7 stand-in for the `EnumSet<StreamEventKind>` sketched in
/// `docs/architecture/RUST_ENGINE.md` §6, hand-rolled so the crate
/// keeps its minimal dependency surface.
///
/// An empty set matches nothing; [`EventKindSet::all`] matches
/// everything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EventKindSet(u16);

impl EventKindSet {
    /// The empty set (matches no event).
    pub const EMPTY: Self = Self(0);

    /// The full set (matches every event).
    pub fn all() -> Self {
        let mut set = Self::EMPTY;
        for kind in EventKind::ALL {
            set.insert(kind);
        }
        set
    }

    /// A set holding exactly `kinds`.
    pub fn of(kinds: &[EventKind]) -> Self {
        let mut set = Self::EMPTY;
        for kind in kinds {
            set.insert(*kind);
        }
        set
    }

    /// Insert one kind.
    pub fn insert(&mut self, kind: EventKind) {
        self.0 |= 1 << (kind.index());
    }

    /// Whether `kind` is in the set.
    pub fn contains(&self, kind: EventKind) -> bool {
        self.0 & (1 << kind.index()) != 0
    }

    /// Whether nothing is in the set.
    pub const fn is_empty(&self) -> bool {
        self.0 == 0
    }
}

impl EventKind {
    /// The bit index of this kind inside [`EventKindSet`] (the
    /// declaration order of [`EventKind::ALL`]).
    const fn index(self) -> u16 {
        match self {
            Self::InstanceStateChange => 0,
            Self::InstanceConsoleOutput => 1,
            Self::InstanceResourceUsage => 2,
            Self::InstanceInstallProgress => 3,
            Self::JobProgress => 4,
            Self::JobCompleted => 5,
            Self::NodeStateChange => 6,
            Self::AuditRecord => 7,
            Self::StreamDropped => 8,
            Self::StreamGap => 9,
        }
    }
}

/// What a subscriber wants from the stream
/// (RUST_ENGINE.md §6's `StreamFilter`).
#[derive(Debug, Clone)]
pub struct StreamFilter {
    /// Only events for this instance (`None` = engine-global events:
    /// node state changes and unscoped audit records).
    pub instance_id: Option<InstanceId>,
    /// Which event kinds to deliver.
    pub kinds: EventKindSet,
    /// Replay-from-checkpoint: replay events with
    /// `generation > since_generation` before live-streaming
    /// (ZIM_API_SPEC.md §11.3). `None` = live only.
    pub since_generation: Option<u64>,
}

impl StreamFilter {
    /// A live-only filter for one instance delivering every kind.
    pub fn instance(instance_id: InstanceId) -> Self {
        Self {
            instance_id: Some(instance_id),
            kinds: EventKindSet::all(),
            since_generation: None,
        }
    }

    /// A live-only engine-global filter delivering every kind.
    pub fn global() -> Self {
        Self {
            instance_id: None,
            kinds: EventKindSet::all(),
            since_generation: None,
        }
    }

    /// Whether an event passes the instance and kind constraints
    /// (the replay checkpoint is applied by the hub, not here).
    pub fn matches(&self, event: &StreamEvent) -> bool {
        let instance_ok = match (self.instance_id, event.instance_id) {
            (Some(wanted), Some(of)) => wanted == of,
            // A global subscriber sees engine-global events only.
            (None, None) => true,
            // An instance subscriber never sees global events, and a
            // global subscriber never sees another instance's events.
            _ => false,
        };
        instance_ok && self.kinds.contains(event.kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::EventData;
    use chrono::Utc;

    fn event(kind_data: EventData, instance: Option<InstanceId>) -> StreamEvent {
        StreamEvent::lane_event(1, Utc::now(), instance, kind_data)
    }

    #[test]
    fn kind_set_membership_round_trips() {
        let mut set = EventKindSet::EMPTY;
        assert!(set.is_empty());
        set.insert(EventKind::InstanceStateChange);
        set.insert(EventKind::StreamGap);
        assert!(set.contains(EventKind::InstanceStateChange));
        assert!(set.contains(EventKind::StreamGap));
        assert!(!set.contains(EventKind::InstanceConsoleOutput));
        assert!(!set.is_empty());
    }

    #[test]
    fn all_contains_every_kind() {
        let set = EventKindSet::all();
        for kind in EventKind::ALL {
            assert!(set.contains(kind), "{kind:?} missing from all()");
        }
    }

    #[test]
    fn instance_filter_scoping() {
        let a = InstanceId::new();
        let b = InstanceId::new();
        let state = EventData::StateChange {
            from: zim_engine_domain::InstanceStatus::Offline,
            to: zim_engine_domain::InstanceStatus::Running,
        };

        // Instance filter: own events yes, other instances no,
        // engine-global no.
        let filter = StreamFilter::instance(a);
        assert!(filter.matches(&event(state.clone(), Some(a))));
        assert!(!filter.matches(&event(state.clone(), Some(b))));
        assert!(!filter.matches(&event(state.clone(), None)));

        // Global filter: engine-global only.
        let filter = StreamFilter::global();
        assert!(filter.matches(&event(state.clone(), None)));
        assert!(!filter.matches(&event(state, Some(a))));
    }

    #[test]
    fn kind_filter_narrows() {
        let instance = InstanceId::new();
        let state = EventData::StateChange {
            from: zim_engine_domain::InstanceStatus::Offline,
            to: zim_engine_domain::InstanceStatus::Running,
        };
        let console = EventData::ConsoleOutput {
            line: "hello".to_owned(),
        };
        let mut filter = StreamFilter::instance(instance);
        filter.kinds = EventKindSet::of(&[EventKind::InstanceConsoleOutput]);
        assert!(!filter.matches(&event(state, Some(instance))));
        assert!(filter.matches(&event(console, Some(instance))));
    }
}
