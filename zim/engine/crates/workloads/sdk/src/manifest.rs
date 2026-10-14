//! The workload-integration manifest (`zim.toml`, §5).
//!
//! Every integration — first-party or a third-party package —
//! declares itself with this manifest: identity, the
//! [`WorkloadKind`] it implements, its capability requirements, the
//! runtime classes it can be placed on, and the spec schema that
//! validates workload rows of its kind. The engine's placement logic
//! consults exactly these fields.
//!
//! ```
//! use zim_engine_workloads_sdk::IntegrationManifest;
//!
//! let toml = r#"
//!     manifest_version = 1
//!     id = "org.zaminmc.workload.minecraft"
//!     version = "0.1.0"
//!     api_version = "v1"
//!     kind = "GameServer"
//!     display_name = "Minecraft Server"
//!     description = "Minecraft and Minecraft-compatible game servers."
//!     required_capabilities = ["ConsoleCapable", "FilesystemCapable"]
//!     optional_capabilities = ["Migratable", "SnapshotCapable"]
//!     runtime_kinds = ["Container", "Remote"]
//! "#;
//!
//! let manifest = IntegrationManifest::from_toml(toml).unwrap();
//! assert_eq!(manifest.kind, zim_engine_domain::WorkloadKind::GameServer);
//! assert_eq!(manifest.id, "org.zaminmc.workload.minecraft");
//! assert!(manifest.required_capabilities.contains(
//!     zim_engine_workloads_sdk::Capability::ConsoleCapable,
//! ));
//! assert!(manifest.accepts_runtime(zim_engine_workloads_sdk::RuntimeKindRef::Remote));
//! ```

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::capabilities::CapabilitySet;

/// The manifest schema major this engine speaks.
pub const MANIFEST_VERSION: u32 = 1;

/// The runtime classes an integration can be placed on.
///
/// The manifest form (`"Container"`, `"Remote"`, `"Host"`) names the
/// engine's runtime families: the local container runtime
/// (`zim_engine_runtime_docker`), the remote-node contract
/// (`zim_engine_node_runtime`, the future Wings-parity node), and
/// the opt-in host process runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
#[non_exhaustive]
pub enum RuntimeKindRef {
    /// An isolated container runtime on the engine's own node.
    Container,
    /// A runtime managed by a remote node (the node-runtime
    /// contract).
    Remote,
    /// A native process on the host, without isolation (opt-in,
    /// admin-consent-gated).
    Host,
}

impl RuntimeKindRef {
    /// The manifest's canonical string.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Container => "Container",
            Self::Remote => "Remote",
            Self::Host => "Host",
        }
    }

    /// Parse the manifest's string form (case-sensitive).
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "Container" => Some(Self::Container),
            "Remote" => Some(Self::Remote),
            "Host" => Some(Self::Host),
            _ => None,
        }
    }
}

impl std::fmt::Display for RuntimeKindRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A validated workload-integration manifest (§5).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrationManifest {
    /// Schema major (must be [`MANIFEST_VERSION`]).
    pub manifest_version: u32,
    /// The integration's reverse-DNS identifier.
    pub id: String,
    /// The integration's own semver version.
    pub version: String,
    /// The engine API generation this integration speaks.
    pub api_version: String,
    /// The workload kind this integration implements (the manifest's
    /// PascalCase form maps onto the domain enum's snake_case one).
    #[serde(with = "kind_pascal")]
    pub kind: zim_engine_domain::WorkloadKind,
    /// The human name surfaced in the API and UI.
    pub display_name: String,
    /// The human description.
    #[serde(default)]
    pub description: String,
    /// Capabilities a placement must provide (§4 vocabulary).
    pub required_capabilities: CapabilitySet,
    /// Capabilities the integration uses when present.
    #[serde(default)]
    pub optional_capabilities: CapabilitySet,
    /// Permissions the integration itself needs (admin-only
    /// typically; the engine grants these at load).
    #[serde(default)]
    pub permissions: Vec<String>,
    /// Package-relative path of the kind's spec JSON Schema.
    #[serde(default)]
    pub config_schema: Option<String>,
    /// The runtime classes this integration can be placed on.
    pub runtime_kinds: Vec<RuntimeKindRef>,
}

/// The §5 manifest's PascalCase `kind` strings ↔ the domain enum.
mod kind_pascal {
    use serde::{Deserialize, Deserializer, Serializer};
    use zim_engine_domain::WorkloadKind;

    /// The PascalCase form of a kind.
    pub fn as_pascal(kind: WorkloadKind) -> &'static str {
        match kind {
            WorkloadKind::GameServer => "GameServer",
            WorkloadKind::Database => "Database",
            WorkloadKind::Website => "Website",
            WorkloadKind::DevProcess => "DevProcess",
            WorkloadKind::GenericContainer => "GenericContainer",
            WorkloadKind::Custom => "Custom",
        }
    }

    /// Parse the manifest's PascalCase form.
    pub fn parse_pascal(value: &str) -> Option<WorkloadKind> {
        match value {
            "GameServer" => Some(WorkloadKind::GameServer),
            "Database" => Some(WorkloadKind::Database),
            "Website" => Some(WorkloadKind::Website),
            "DevProcess" => Some(WorkloadKind::DevProcess),
            "GenericContainer" => Some(WorkloadKind::GenericContainer),
            "Custom" => Some(WorkloadKind::Custom),
            _ => None,
        }
    }

    pub fn serialize<S: Serializer>(kind: &WorkloadKind, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(as_pascal(*kind))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<WorkloadKind, D::Error> {
        let value = String::deserialize(deserializer)?;
        parse_pascal(&value).ok_or_else(|| {
            <D::Error as serde::de::Error>::custom(format!(
                "unknown workload kind {value:?} (GameServer, Database, Website, DevProcess, \
                 GenericContainer, Custom)"
            ))
        })
    }
}

/// Every way a manifest can fail validation.
#[derive(Debug, thiserror::Error)]
#[allow(missing_docs)]
pub enum ManifestError {
    /// The manifest was not valid TOML.
    #[error("the manifest is not valid TOML: {0}")]
    NotToml(String),
    /// A field failed validation.
    #[error("field {field:?}: {message}")]
    Invalid {
        /// The offending field.
        field: &'static str,
        /// What is wrong with it.
        message: String,
    },
}

impl IntegrationManifest {
    /// Parse and validate manifest TOML (the `zim.toml` form).
    pub fn from_toml(text: &str) -> Result<Self, ManifestError> {
        let manifest: IntegrationManifest =
            toml::from_str(text).map_err(|e| ManifestError::NotToml(e.to_string()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Structural validation (the §5 schema, as the engine enforces
    /// it).
    pub fn validate(&self) -> Result<(), ManifestError> {
        let invalid = |field: &'static str, message: &str| ManifestError::Invalid {
            field,
            message: message.to_owned(),
        };

        if self.manifest_version != MANIFEST_VERSION {
            return Err(invalid(
                "manifest_version",
                &format!("this engine speaks manifest version {MANIFEST_VERSION}"),
            ));
        }
        if self.id.trim().is_empty()
            || !self
                .id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
            || !self.id.contains('.')
        {
            return Err(invalid(
                "id",
                "must be a non-empty reverse-DNS identifier (letters, digits, dots, hyphens, \
                 underscores)",
            ));
        }
        if semver::Version::parse(&self.version).is_err() {
            return Err(invalid("version", "must be a valid semver version"));
        }
        let major = self
            .api_version
            .trim()
            .trim_start_matches('v')
            .split('.')
            .next()
            .unwrap_or("");
        if major.is_empty() || major == "0" {
            return Err(invalid(
                "api_version",
                "must name the API generation (e.g. \"v1\")",
            ));
        }
        if matches!(self.kind, zim_engine_domain::WorkloadKind::Custom)
            && !self.id.starts_with("org.zaminmc.")
        {
            // Custom is the third-party extension kind; first-party
            // manifests declare one of the five named kinds. The
            // engine-side check keeps the manifest honest about what
            // it is.
            return Err(invalid(
                "kind",
                "the Custom kind is reserved for third-party extension packages",
            ));
        }
        if self.display_name.trim().is_empty() {
            return Err(invalid("display_name", "must be non-empty"));
        }
        if self.runtime_kinds.is_empty() {
            return Err(invalid(
                "runtime_kinds",
                "at least one runtime class is required",
            ));
        }
        if let Some(schema) = &self.config_schema {
            if schema.is_empty()
                || schema.starts_with('/')
                || schema.contains("..")
                || schema.contains('\\')
            {
                return Err(invalid("config_schema", "must be a package-relative path"));
            }
        }
        // A capability cannot be both required and optional.
        let overlap = self
            .required_capabilities
            .intersection(&self.optional_capabilities);
        if !overlap.is_empty() {
            return Err(invalid(
                "optional_capabilities",
                &format!(
                    "also listed as required: {}",
                    overlap.to_strings().join(", ")
                ),
            ));
        }
        if self.required_capabilities.is_empty()
            && self.kind != zim_engine_domain::WorkloadKind::GenericContainer
        {
            return Err(invalid(
                "required_capabilities",
                "an integration must require at least one capability (GenericContainer alone \
                 negotiates its requirements at placement time, per §5)",
            ));
        }
        Ok(())
    }

    /// Whether a runtime class is acceptable for placement.
    pub fn accepts_runtime(&self, kind: RuntimeKindRef) -> bool {
        self.runtime_kinds.contains(&kind)
    }

    /// The permissions the integration declares, deduplicated.
    pub fn permission_set(&self) -> BTreeSet<&str> {
        self.permissions.iter().map(String::as_str).collect()
    }

    /// Construct a manifest from validated parts — the path the
    /// `#[workload_integration]` macro and the first-party crates
    /// take. The values come from compile-time literals that the
    /// macro itself validated (unknown capability/kind names are
    /// compile errors), so this constructor only asserts the
    /// invariants the macro cannot check (the id/version formats).
    ///
    /// (Eight parts is the honest declaration surface — a builder
    /// would hide which halves are optional; nothing here is.)
    #[allow(clippy::too_many_arguments)]
    pub fn builtin(
        kind: zim_engine_domain::WorkloadKind,
        id: &str,
        version: &str,
        display_name: &str,
        description: &str,
        required: CapabilitySet,
        optional: CapabilitySet,
        runtime_kinds: &[RuntimeKindRef],
    ) -> Self {
        let manifest = Self {
            manifest_version: MANIFEST_VERSION,
            id: id.to_owned(),
            version: version.to_owned(),
            api_version: "v1".to_owned(),
            kind,
            display_name: display_name.to_owned(),
            description: description.to_owned(),
            required_capabilities: required,
            optional_capabilities: optional,
            permissions: Vec::new(),
            config_schema: None,
            runtime_kinds: runtime_kinds.to_vec(),
        };
        // First-party declarations are authored next to this code;
        // a malformed one is a bug to fix, not a runtime condition.
        if let Err(error) = manifest.validate() {
            panic!("invalid builtin workload manifest for {id}: {error}");
        }
        manifest
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::Capability;

    /// The §5 example manifest, complete.
    fn example_toml() -> String {
        r#"
            manifest_version = 1
            id = "org.zaminmc.workload.minecraft"
            version = "0.1.0"
            api_version = "v1"
            kind = "GameServer"
            display_name = "Minecraft Server"
            description = "Minecraft and Minecraft-compatible game servers."
            required_capabilities = ["ConsoleCapable", "FilesystemCapable", "MetricsCapable", "ExecCapable"]
            optional_capabilities = ["Migratable", "SnapshotCapable"]
            permissions = []
            config_schema = "schemas/game-server-spec.v1.json"
            runtime_kinds = ["Container", "Remote"]
        "#
        .to_owned()
    }

    #[test]
    fn parses_the_canonical_example() {
        let manifest = IntegrationManifest::from_toml(&example_toml()).unwrap();
        assert_eq!(manifest.id, "org.zaminmc.workload.minecraft");
        assert_eq!(manifest.kind, zim_engine_domain::WorkloadKind::GameServer);
        assert_eq!(manifest.display_name, "Minecraft Server");
        assert_eq!(
            manifest.config_schema.as_deref(),
            Some("schemas/game-server-spec.v1.json")
        );
        assert_eq!(manifest.runtime_kinds.len(), 2);
        assert!(manifest.accepts_runtime(RuntimeKindRef::Container));
        assert!(manifest.accepts_runtime(RuntimeKindRef::Remote));
        assert!(!manifest.accepts_runtime(RuntimeKindRef::Host));
        assert_eq!(manifest.required_capabilities.len(), 4);
        assert!(manifest
            .required_capabilities
            .contains(Capability::ConsoleCapable));
        assert!(manifest
            .required_capabilities
            .contains(Capability::ExecCapable));
        assert_eq!(manifest.optional_capabilities.len(), 2);
        assert!(manifest
            .optional_capabilities
            .contains(Capability::Migratable));
        assert!(manifest.permission_set().is_empty());
    }

    #[test]
    fn round_trips_through_serialization() {
        let manifest = IntegrationManifest::from_toml(&example_toml()).unwrap();
        let serialized = toml::to_string(&manifest).unwrap();
        let reparsed = IntegrationManifest::from_toml(&serialized).unwrap();
        assert_eq!(reparsed.id, manifest.id);
        assert_eq!(reparsed.kind, manifest.kind);
        assert_eq!(
            reparsed.required_capabilities,
            manifest.required_capabilities
        );
        assert_eq!(
            reparsed.optional_capabilities,
            manifest.optional_capabilities
        );
    }

    #[test]
    fn rejects_broken_manifests() {
        let base = example_toml();

        let mut broken = base.replace("manifest_version = 1", "manifest_version = 2");
        assert!(matches!(
            IntegrationManifest::from_toml(&broken),
            Err(ManifestError::Invalid {
                field: "manifest_version",
                ..
            })
        ));

        broken = base.replace(
            "id = \"org.zaminmc.workload.minecraft\"",
            "id = \"minecraft\"",
        );
        assert!(matches!(
            IntegrationManifest::from_toml(&broken),
            Err(ManifestError::Invalid { field: "id", .. })
        ));

        broken = base.replace("version = \"0.1.0\"", "version = \"not-semver\"");
        assert!(matches!(
            IntegrationManifest::from_toml(&broken),
            Err(ManifestError::Invalid {
                field: "version",
                ..
            })
        ));

        broken = base.replace("api_version = \"v1\"", "api_version = \"\"");
        assert!(matches!(
            IntegrationManifest::from_toml(&broken),
            Err(ManifestError::Invalid {
                field: "api_version",
                ..
            })
        ));

        // An unknown capability string.
        broken = base.replace("\"ExecCapable\"", "\"TeleportCapable\"");
        assert!(matches!(
            IntegrationManifest::from_toml(&broken),
            Err(ManifestError::NotToml(_))
        ));

        // Empty runtime list.
        broken = base.replace(
            "runtime_kinds = [\"Container\", \"Remote\"]",
            "runtime_kinds = []",
        );
        assert!(matches!(
            IntegrationManifest::from_toml(&broken),
            Err(ManifestError::Invalid {
                field: "runtime_kinds",
                ..
            })
        ));

        // A capability both required and optional.
        broken = base.replace(
            "optional_capabilities = [\"Migratable\", \"SnapshotCapable\"]",
            "optional_capabilities = [\"Migratable\", \"ConsoleCapable\"]",
        );
        assert!(matches!(
            IntegrationManifest::from_toml(&broken),
            Err(ManifestError::Invalid {
                field: "optional_capabilities",
                ..
            })
        ));

        // No requirements at all.
        broken = base.replace(
            "required_capabilities = [\"ConsoleCapable\", \"FilesystemCapable\", \"MetricsCapable\", \"ExecCapable\"]",
            "required_capabilities = []",
        );
        assert!(matches!(
            IntegrationManifest::from_toml(&broken),
            Err(ManifestError::Invalid {
                field: "required_capabilities",
                ..
            })
        ));

        // Escaping config schema path.
        broken = base.replace(
            "config_schema = \"schemas/game-server-spec.v1.json\"",
            "config_schema = \"../etc/passwd\"",
        );
        assert!(matches!(
            IntegrationManifest::from_toml(&broken),
            Err(ManifestError::Invalid {
                field: "config_schema",
                ..
            })
        ));

        // Unknown runtime class.
        broken = base.replace("\"Remote\"", "\"Quantum\"");
        assert!(matches!(
            IntegrationManifest::from_toml(&broken),
            Err(ManifestError::NotToml(_))
        ));
    }

    #[test]
    fn builtin_constructor_validates() {
        let manifest = IntegrationManifest::builtin(
            zim_engine_domain::WorkloadKind::Database,
            "org.zaminmc.workload.database",
            "0.1.0",
            "Database",
            "Managed databases.",
            [Capability::ReadyProbeCapable, Capability::SnapshotCapable]
                .into_iter()
                .collect(),
            CapabilitySet::EMPTY,
            &[RuntimeKindRef::Container],
        );
        assert!(manifest.validate().is_ok());
        // An invalid builtin panics — first-party declarations are
        // compile-time-authored.
        let result = std::panic::catch_unwind(|| {
            IntegrationManifest::builtin(
                zim_engine_domain::WorkloadKind::Database,
                "not-reverse-dns",
                "0.1.0",
                "x",
                "",
                [Capability::ReadyProbeCapable].into_iter().collect(),
                CapabilitySet::EMPTY,
                &[RuntimeKindRef::Container],
            );
        });
        assert!(result.is_err());
    }

    #[test]
    fn runtime_kind_refs_round_trip() {
        for kind in [
            RuntimeKindRef::Container,
            RuntimeKindRef::Remote,
            RuntimeKindRef::Host,
        ] {
            assert_eq!(RuntimeKindRef::parse(kind.as_str()), Some(kind));
            assert_eq!(kind.to_string(), kind.as_str());
        }
        assert_eq!(RuntimeKindRef::parse("container"), None);
    }
}
