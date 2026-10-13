//! Permission vocabulary + capability enforcement.
//!
//! The `PermissionGrant` enum preserves the Pterodactyl `Permission`
//! constants verbatim (per AD-12) plus ZIM additions (Install,
//! Transfer, AdminInstance, workload.* from
//! `docs/architecture/WORKLOAD_ABSTRACTION.md` §9). Capability
//! checks are at the API boundary per invariant 15 in
//! `docs/architecture/ZIM_ARCHITECTURE.md` §4: domain services do
//! NOT re-check permissions — this crate is the single enforcement
//! point's logic.
//!
//! # Milestone scope (M4)
//!
//! This crate ships the enforcement core of milestone M4
//! (`docs/architecture/IMPLEMENTATION_ROADMAP.md` §M4):
//!
//! - `AuthzContext`: the resolved caller (user, admin flags, and
//!   *effective* grant set) that the API middleware builds from a
//!   validated credential (session or API key).
//! - `require()`: the capability check — every required grant must
//!   be present or the call fails with a structured
//!   `permission_denied` error carrying the missing grants in their
//!   canonical dotted form (ready for the API spec's §4 error body).
//! - `resolve_instance_grants()`: how the boundary turns a `User`
//!   plus the user's instance-scoped `Permission` row into the
//!   effective set (root admins and instance admins bypass rows).
//!
//! The HTTP middleware itself (credential extraction, `401`/`403`
//! mapping, audit emission) lands with the API surface in M5.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod check;

pub use check::{require, resolve_instance_grants, AuthzContext, AuthzError};

/// The canonical crate name, useful for runtime identification in
/// logs and crash reports.
pub const CRATE_NAME: &str = "zim_engine_authz";

#[cfg(test)]
mod tests {
    //! Cross-module integration-ish tests; per-function behavior is
    //! covered in `check.rs`.

    use super::*;
    use zim_engine_domain::{InstanceId, Permission, PermissionGrant, PermissionSet, User, UserId};

    fn user_with(is_admin: bool, root_admin: bool) -> User {
        User {
            id: UserId::new(),
            email: "ops@example.org".to_owned(),
            username: "ops".to_owned(),
            external_id: None,
            password_hash: Some("$argon2id$v=19$m=65536,t=3,p=1$x".to_owned()),
            oauth_subject: None,
            is_admin,
            root_admin,
        }
    }

    #[test]
    fn crate_name_is_set() {
        assert_eq!(CRATE_NAME, "zim_engine_authz");
    }

    #[test]
    fn full_boundary_flow_member_vs_admin() {
        // The M4 acceptance scenario, minus HTTP: a member with
        // control.start can call it, a member without cannot, and
        // the error lists exactly what is missing.
        let instance_id = InstanceId::new();

        let member = user_with(false, false);
        let mut member_grants = PermissionSet::new();
        member_grants
            .grant(PermissionGrant::ControlStart)
            .grant(PermissionGrant::FileRead);

        let ctx = AuthzContext::for_user(&member, member_grants.clone(), instance_id);
        assert!(require(&ctx, &[PermissionGrant::ControlStart]).is_ok());
        assert!(require(&ctx, &[PermissionGrant::FileRead]).is_ok());

        let err = require(&ctx, &[PermissionGrant::ControlStop]).unwrap_err();
        assert_eq!(err.code(), "permission_denied");
        assert_eq!(err.missing_as_str(), vec!["control.stop"]);
        assert_eq!(err.user_id(), &member.id);

        // An admin sails through the same check.
        let admin = user_with(true, false);
        let admin_ctx = AuthzContext::for_user(&admin, PermissionSet::new(), instance_id);
        assert!(require(&admin_ctx, &[PermissionGrant::ControlStop]).is_ok());
    }

    #[test]
    fn resolve_uses_the_permission_row_for_plain_members() {
        let user = user_with(false, false);
        let mut grants = PermissionSet::new();
        grants.grant(PermissionGrant::BackupRead);
        let resolved = resolve_instance_grants(
            &user,
            Some(&Permission {
                user_id: user.id,
                instance_id: InstanceId::new(),
                grants,
            }),
        );
        assert!(resolved.contains(PermissionGrant::BackupRead));
        assert!(!resolved.contains(PermissionGrant::BackupCreate));

        // No row at all: no grants.
        let none = resolve_instance_grants(&user, None);
        assert!(none.is_empty());
    }
}
