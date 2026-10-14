//! Error types for the integration surface.

/// A workload spec failed the integration's validation.
///
/// Specs arrive as JSON (the §10.15 `spec` field of a workload
/// row); the integration knows its own schema and reports the
/// violated constraint.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[allow(missing_docs)]
pub enum SpecError {
    /// The spec is not the shape this kind speaks (wrong type for
    /// a field, missing a required field, unknown fields).
    #[error("field {field:?}: {message}")]
    Invalid {
        /// The offending field.
        field: String,
        /// What is wrong with it.
        message: String,
    },
    /// The spec could not be parsed at all.
    #[error("the spec is not valid for this workload kind: {0}")]
    Unparsable(String),
}

/// A workload-integration operation failed.
///
/// The M16 skeletons produced only the [`Unsupported`][Self::Unsupported]
/// form; the M17 integrations add the file-surface, runtime-bridge,
/// limit, and state-gated forms the capability implementations
/// report. The vocabulary stays stable for the engine-side call
/// sites either way.
#[derive(Debug, thiserror::Error)]
#[allow(missing_docs)]
pub enum IntegrationError {
    /// The operation is not implemented by this integration yet.
    #[error("the {integration} integration does not implement {operation} yet")]
    Unsupported {
        /// The integration's id.
        integration: String,
        /// The operation's name.
        operation: &'static str,
    },
    /// The runtime the workload is placed on lacks a capability the
    /// operation needs (§4's placement failure).
    #[error("the workload's runtime does not provide {capability}")]
    CapabilityMissing {
        /// The capability the operation requires.
        capability: crate::capabilities::Capability,
    },
    /// A spec failed validation.
    #[error("{0}")]
    Spec(#[from] SpecError),
    /// A file-surface operation failed (path validation rejected the
    /// request, or the underlying IO errored). Arrives with the M17
    /// integrations' [`FilesystemCapable`][crate::traits::FilesystemCapable]
    /// implementations.
    #[error("file operation failed: {0}")]
    File(String),
    /// The runtime bridging this call failed (the §4 runtime error
    /// vocabulary stringified — the integration surface flattens
    /// the runtime's typed errors).
    #[error("runtime error: {0}")]
    Runtime(String),
    /// The operation exceeded a configured limit (the §2.7 archive
    /// caps, console retention bounds, ...).
    #[error("limit exceeded: {0}")]
    Limit(String),
    /// The workload is not in a state the operation requires (e.g.
    /// console input to a stopped workload).
    #[error("workload state does not allow {operation}: {reason}")]
    NotRunning {
        /// The operation's name.
        operation: &'static str,
        /// Why the current state blocks it.
        reason: String,
    },
}

impl IntegrationError {
    /// The [`Unsupported`][Self::Unsupported] constructor — the
    /// skeleton hooks' standard answer.
    pub fn unsupported(integration: &str, operation: &'static str) -> Self {
        Self::Unsupported {
            integration: integration.to_owned(),
            operation,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_error_vocabulary_displays_usefully() {
        let error =
            IntegrationError::unsupported("org.zaminmc.workload.minecraft", "validate_spec");
        assert_eq!(
            error.to_string(),
            "the org.zaminmc.workload.minecraft integration does not implement validate_spec yet"
        );
        let spec_error = SpecError::Invalid {
            field: "eula".to_owned(),
            message: "must be accepted before first start".to_owned(),
        };
        let error = IntegrationError::Spec(spec_error);
        assert!(error.to_string().contains("eula"));
    }
}
