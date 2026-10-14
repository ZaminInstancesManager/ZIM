//! The capability vocabulary and its set type (§4).
//!
//! A runtime adapter implements some subset of the §4 capability
//! traits; an integration *requires* a subset and may *optionally
//! use* another. Placement matches the two: a required capability
//! the runtime lacks fails with `runtime_incompatible`
//! (`docs/api/ZIM_API_SPEC.md` §6.1), an optional one is simply
//! unavailable to that workload.
//!
//! [`CapabilitySet`] is the compact, comparable form both sides
//! report — the manifest parser produces it from the
//! `required_capabilities` / `optional_capabilities` lists, and the
//! acceptance surface for M16 exercises it directly.

use serde::{Deserialize, Serialize};

/// One §4 capability trait an integration can require or use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Capability {
    /// `ConsoleCapable` — text console attach + input
    /// (`attach_console` / `send_input`).
    ConsoleCapable,
    /// `ReadyProbeCapable` — readiness distinct from process
    /// presence (`readiness` / `wait_until_ready`).
    ReadyProbeCapable,
    /// `HealthCheckCapable` — a health endpoint reporting liveness
    /// (`health`).
    HealthCheckCapable,
    /// `FilesystemCapable` — the workload's filesystem
    /// (list/read/write/delete/archive/extract).
    FilesystemCapable,
    /// `NetworkCapable` — multiple network endpoints
    /// (`endpoints` / `expose_port` / `unexpose_port`).
    NetworkCapable,
    /// `SnapshotCapable` — snapshot-style backups
    /// (`snapshot` / `restore_snapshot`).
    SnapshotCapable,
    /// `Migratable` — migration with quiesce semantics
    /// (`migration_plan` / `prepare_for_migration` / …).
    Migratable,
    /// `ExecCapable` — one-shot command execution in the workload's
    /// context (`exec`).
    ExecCapable,
    /// `MetricsCapable` — resource-usage metrics, point-in-time and
    /// streamed (`metrics` / `metrics_stream`).
    MetricsCapable,
}

impl Capability {
    /// Every capability in the vocabulary, in declaration order.
    pub const ALL: [Capability; 9] = [
        Capability::ConsoleCapable,
        Capability::ReadyProbeCapable,
        Capability::HealthCheckCapable,
        Capability::FilesystemCapable,
        Capability::NetworkCapable,
        Capability::SnapshotCapable,
        Capability::Migratable,
        Capability::ExecCapable,
        Capability::MetricsCapable,
    ];

    /// The manifest's canonical string (`"ConsoleCapable"`, …).
    pub const fn as_str(self) -> &'static str {
        match self {
            Capability::ConsoleCapable => "ConsoleCapable",
            Capability::ReadyProbeCapable => "ReadyProbeCapable",
            Capability::HealthCheckCapable => "HealthCheckCapable",
            Capability::FilesystemCapable => "FilesystemCapable",
            Capability::NetworkCapable => "NetworkCapable",
            Capability::SnapshotCapable => "SnapshotCapable",
            Capability::Migratable => "Migratable",
            Capability::ExecCapable => "ExecCapable",
            Capability::MetricsCapable => "MetricsCapable",
        }
    }

    /// Parse the manifest's string form (case-sensitive; the §5
    /// manifest names traits verbatim).
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == value)
    }
}

impl std::fmt::Display for Capability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A compact set of [`Capability`] values (one bit each).
///
/// Cheap to copy, compare, and intersect — the placement-time
/// question "does this runtime satisfy this workload's
/// requirements?" is a subset test. On the wire (the §5 manifest)
/// a set is the sorted list of capability names.
///
/// ```
/// use zim_engine_workloads_sdk::{Capability, CapabilitySet};
///
/// let required: CapabilitySet = [
///     Capability::ConsoleCapable,
///     Capability::FilesystemCapable,
/// ].into_iter().collect();
/// let runtime: CapabilitySet = [
///     Capability::ConsoleCapable,
///     Capability::FilesystemCapable,
///     Capability::MetricsCapable,
/// ].into_iter().collect();
/// assert!(runtime.contains_all(&required));
/// assert!(required.contains(Capability::ConsoleCapable));
/// assert!(!required.contains(Capability::MetricsCapable));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct CapabilitySet(u16);

impl CapabilitySet {
    /// The empty set.
    pub const EMPTY: Self = Self(0);

    /// Insert one capability (idempotent).
    pub fn insert(&mut self, capability: Capability) {
        self.0 |= bit(capability);
    }

    /// Remove one capability (idempotent).
    pub fn remove(&mut self, capability: Capability) {
        self.0 &= !bit(capability);
    }

    /// Whether the set contains one capability.
    pub fn contains(&self, capability: Capability) -> bool {
        self.0 & bit(capability) != 0
    }

    /// Whether every capability of `other` is present (the
    /// placement-time requirement check).
    pub fn contains_all(&self, other: &Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// The intersection of two sets.
    pub fn intersection(&self, other: &Self) -> Self {
        Self(self.0 & other.0)
    }

    /// Whether the set is empty.
    pub fn is_empty(&self) -> bool {
        self.0 == 0
    }

    /// How many capabilities the set holds.
    pub fn len(&self) -> usize {
        self.0.count_ones() as usize
    }

    /// Iterate the contained capabilities in vocabulary order.
    pub fn iter(&self) -> impl Iterator<Item = Capability> + '_ {
        Capability::ALL
            .into_iter()
            .filter(move |c| self.contains(*c))
    }

    /// The manifest's string list form (sorted for stability).
    pub fn to_strings(&self) -> Vec<String> {
        let mut names: Vec<String> = self.iter().map(|c| c.as_str().to_owned()).collect();
        names.sort();
        names
    }
}

impl std::iter::FromIterator<Capability> for CapabilitySet {
    fn from_iter<T: IntoIterator<Item = Capability>>(iter: T) -> Self {
        let mut set = Self::EMPTY;
        for capability in iter {
            set.insert(capability);
        }
        set
    }
}

impl Extend<Capability> for CapabilitySet {
    fn extend<T: IntoIterator<Item = Capability>>(&mut self, iter: T) {
        for capability in iter {
            self.insert(capability);
        }
    }
}

impl IntoIterator for CapabilitySet {
    type Item = Capability;
    type IntoIter = std::vec::IntoIter<Capability>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter().collect::<Vec<_>>().into_iter()
    }
}

impl std::fmt::Display for CapabilitySet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names = self.to_strings();
        write!(f, "[{}]", names.join(", "))
    }
}

/// The bit position of a capability (index in [`Capability::ALL`]).
const fn bit(capability: Capability) -> u16 {
    match capability {
        Capability::ConsoleCapable => 1 << 0,
        Capability::ReadyProbeCapable => 1 << 1,
        Capability::HealthCheckCapable => 1 << 2,
        Capability::FilesystemCapable => 1 << 3,
        Capability::NetworkCapable => 1 << 4,
        Capability::SnapshotCapable => 1 << 5,
        Capability::Migratable => 1 << 6,
        Capability::ExecCapable => 1 << 7,
        Capability::MetricsCapable => 1 << 8,
    }
}

// ── wire form: the sorted capability-name list (§5 manifest) ─────

impl Serialize for CapabilitySet {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.to_strings())
    }
}

impl<'de> Deserialize<'de> for CapabilitySet {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let names = Vec::<String>::deserialize(deserializer)?;
        let mut set = Self::EMPTY;
        for name in names {
            let capability = Capability::parse(&name).ok_or_else(|| {
                <D::Error as serde::de::Error>::custom(format!(
                    "unknown capability {name:?} (not in the §4 vocabulary)"
                ))
            })?;
            set.insert(capability);
        }
        Ok(set)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_vocabulary_round_trips() {
        for capability in Capability::ALL {
            assert_eq!(Capability::parse(capability.as_str()), Some(capability));
            assert_eq!(capability.to_string(), capability.as_str());
        }
        assert_eq!(Capability::parse("consolecapable"), None);
        assert_eq!(Capability::parse(""), None);
        assert_eq!(Capability::parse("Bogus"), None);
    }

    #[test]
    fn sets_behave_like_sets() {
        let mut set = CapabilitySet::EMPTY;
        assert!(set.is_empty());
        assert_eq!(set.len(), 0);

        set.insert(Capability::ConsoleCapable);
        set.insert(Capability::ConsoleCapable); // idempotent
        set.insert(Capability::MetricsCapable);
        assert_eq!(set.len(), 2);
        assert!(set.contains(Capability::ConsoleCapable));
        assert!(set.contains(Capability::MetricsCapable));
        assert!(!set.contains(Capability::FilesystemCapable));

        set.remove(Capability::ConsoleCapable);
        set.remove(Capability::ConsoleCapable); // idempotent
        assert!(!set.contains(Capability::ConsoleCapable));
        assert_eq!(set.len(), 1);

        // Copy + equality semantics.
        let clone = set;
        assert_eq!(clone, set);
        assert_ne!(set, CapabilitySet::EMPTY);
    }

    #[test]
    fn subset_and_intersection_answer_placement_questions() {
        let required: CapabilitySet = [Capability::ConsoleCapable, Capability::FilesystemCapable]
            .into_iter()
            .collect();
        let docker: CapabilitySet = [
            Capability::ConsoleCapable,
            Capability::FilesystemCapable,
            Capability::MetricsCapable,
            Capability::ExecCapable,
        ]
        .into_iter()
        .collect();
        let null: CapabilitySet = CapabilitySet::EMPTY;

        assert!(docker.contains_all(&required));
        assert!(!null.contains_all(&required));
        assert!(null.contains_all(&CapabilitySet::EMPTY));

        let shared = docker.intersection(&required);
        assert_eq!(shared, required);
        assert_eq!(shared.len(), 2);
    }

    #[test]
    fn iteration_is_vocabulary_ordered_and_strings_sorted() {
        let set: CapabilitySet = [
            Capability::MetricsCapable,
            Capability::ConsoleCapable,
            Capability::ExecCapable,
        ]
        .into_iter()
        .collect();
        let order: Vec<&str> = set.iter().map(|c| c.as_str()).collect();
        assert_eq!(
            order,
            vec!["ConsoleCapable", "ExecCapable", "MetricsCapable"]
        );
        // The manifest list form is sorted alphabetically.
        assert_eq!(
            set.to_strings(),
            vec![
                "ConsoleCapable".to_owned(),
                "ExecCapable".to_owned(),
                "MetricsCapable".to_owned()
            ]
        );
        assert_eq!(
            set.to_string(),
            "[ConsoleCapable, ExecCapable, MetricsCapable]"
        );
        // IntoIterator parity.
        assert_eq!(set.into_iter().count(), 3);
    }
}
