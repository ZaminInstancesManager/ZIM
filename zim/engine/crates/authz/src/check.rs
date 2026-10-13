//! The capability check and the caller context it evaluates.

use thiserror::Error;
use zim_engine_domain::{Permission, PermissionGrant, PermissionSet, User, UserId};

/// The resolved caller at the API boundary.
///
/// Built by the M5 middleware from a validated credential (session
/// token or API key): the user it names, plus the *effective* grant
/// set for the request's target instance. Pass an empty set for
/// non-instance-scoped (global) operations — global operations are
/// gated on the admin flags instead of instance rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthzContext {
    /// The authenticated user.
    pub user_id: UserId,
    /// Root administrator: bypasses every check (cannot be deleted;
    /// exists to bootstrap installs).
    pub root_admin: bool,
    /// Instance administrator: the boundary granted them the full
    /// effective set for instances they do not own (Pterodactyl
    /// admin semantics).
    pub is_admin: bool,
    /// The effective grant set for the target instance (already
    /// resolved through [`resolve_instance_grants`] — root/admin
    /// resolution has already happened by the time the set lands
    /// here).
    pub grants: PermissionSet,
}

impl AuthzContext {
    /// Builds the context for `user` with `grants` as the effective
    /// set (root admins and admins get the full vocabulary; see
    /// [`resolve_instance_grants`]).
    pub fn for_user(user: &User, grants: PermissionSet, _instance: impl AnyInstanceId) -> Self {
        let (root_admin, is_admin) = (user.root_admin, user.is_admin);
        let grants = if root_admin || is_admin {
            PermissionSet::all()
        } else {
            grants
        };
        Self {
            user_id: user.id,
            root_admin,
            is_admin,
            grants,
        }
    }

    /// The effective grant set (root/admin contexts already hold the
    /// full vocabulary).
    pub fn effective_grants(&self) -> &PermissionSet {
        &self.grants
    }
}

/// Marker for the target-instance parameter of
/// [`AuthzContext::for_user`] — accepts any strongly-typed instance
/// id (kept as a trait so the context stays decoupled from the
/// domain id module's names).
pub trait AnyInstanceId {
    /// The instance's UUID form (unused for resolution; the trait
    /// exists to make the call site read `for_user(&u, set, id)`).
    fn instance_uuid(&self) -> uuid::Uuid;
}

impl AnyInstanceId for zim_engine_domain::InstanceId {
    fn instance_uuid(&self) -> uuid::Uuid {
        (*self).into()
    }
}

/// Resolves the effective grant set a `user` has on an instance.
///
/// - Root admins: the full vocabulary (they bypass rows).
/// - Instance admins (`is_admin`): the full vocabulary — Pterodactyl
///   semantics, admins administer instances they do not own.
/// - Everyone else: exactly the grants in their instance-scoped
///   `Permission` row (`None` → empty set).
///
/// Called by the boundary once per request, after loading the
/// user's row for the target instance.
pub fn resolve_instance_grants(user: &User, permission_row: Option<&Permission>) -> PermissionSet {
    if user.root_admin || user.is_admin {
        return PermissionSet::all();
    }
    match permission_row {
        Some(row) => row.grants.clone(),
        None => PermissionSet::new(),
    }
}

/// The capability check: every required grant must be present.
///
/// Root admins short-circuit to success regardless of the context's
/// set (belt and suspenders — [`AuthzContext::for_user`] already
/// materializes the full vocabulary for them).
///
/// On success returns `Ok(())`; on failure returns
/// [`AuthzError::PermissionDenied`] carrying the missing grants, the
/// canonical dotted names for the API's `missing_permissions` array
/// (`docs/api/ZIM_API_SPEC.md` §4), and the denied user (for the
/// audit log).
pub fn require(ctx: &AuthzContext, required: &[PermissionGrant]) -> Result<(), AuthzError> {
    if ctx.root_admin {
        return Ok(());
    }
    let held = ctx.effective_grants();
    let missing: Vec<PermissionGrant> = required
        .iter()
        .copied()
        .filter(|grant| !held.contains(*grant))
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(AuthzError::PermissionDenied {
            missing,
            user_id: ctx.user_id,
        })
    }
}

/// The authz layer's failure mode.
///
/// Maps to `403 Forbidden` with the structured `permission_denied`
/// body per `docs/api/ZIM_API_SPEC.md` §4 and §6.1. (The `401
/// unauthorized` path — missing/expired credential — is the auth
/// crate's business, not this one.)
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AuthzError {
    /// The credential is valid but lacks the required grants.
    #[error("permission denied: missing {} grant(s)", .missing.len())]
    PermissionDenied {
        /// The grants that were required but not held, in the order
        /// they were required.
        missing: Vec<PermissionGrant>,
        /// The denied user (for auditing).
        user_id: UserId,
    },
}

impl AuthzError {
    /// The stable machine-readable error code for the API body.
    pub fn code(&self) -> &'static str {
        match self {
            Self::PermissionDenied { .. } => "permission_denied",
        }
    }

    /// The HTTP status this error maps to.
    pub fn http_status(&self) -> u16 {
        match self {
            Self::PermissionDenied { .. } => 403,
        }
    }

    /// The missing grants in their canonical dotted form
    /// (`"control.start"`, ...) — the API body's
    /// `missing_permissions` array.
    pub fn missing_as_str(&self) -> Vec<&'static str> {
        match self {
            Self::PermissionDenied { missing, .. } => missing.iter().map(|g| g.as_str()).collect(),
        }
    }

    /// The denied user (for the audit log).
    pub fn user_id(&self) -> &UserId {
        match self {
            Self::PermissionDenied { user_id, .. } => user_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zim_engine_domain::InstanceId;

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

    fn member_ctx(grants: &[PermissionGrant]) -> AuthzContext {
        let user = user_with(false, false);
        let mut set = PermissionSet::new();
        for grant in grants {
            set.grant(*grant);
        }
        AuthzContext::for_user(&user, set, InstanceId::new())
    }

    #[test]
    fn member_passes_when_all_required_grants_held() {
        let ctx = member_ctx(&[
            PermissionGrant::ControlStart,
            PermissionGrant::ControlStop,
            PermissionGrant::FileRead,
        ]);
        assert!(require(&ctx, &[PermissionGrant::ControlStart]).is_ok());
        assert!(require(
            &ctx,
            &[
                PermissionGrant::ControlStart,
                PermissionGrant::ControlStop,
                PermissionGrant::FileRead
            ]
        )
        .is_ok());
        assert!(require(&ctx, &[]).is_ok(), "no requirements = allowed");
    }

    #[test]
    fn member_fails_when_a_grant_is_missing() {
        let ctx = member_ctx(&[PermissionGrant::ControlStart]);
        let err = require(
            &ctx,
            &[
                PermissionGrant::ControlStart,
                PermissionGrant::FileRead,
                PermissionGrant::FileWrite,
            ],
        )
        .unwrap_err();
        assert_eq!(
            err.missing_as_str(),
            vec!["file.read", "file.write"],
            "missing grants listed in requirement order"
        );
        assert_eq!(err.code(), "permission_denied");
        assert_eq!(err.http_status(), 403);
    }

    #[test]
    fn empty_grant_set_denies_everything() {
        let ctx = member_ctx(&[]);
        let err = require(&ctx, &[PermissionGrant::SettingsRead]).unwrap_err();
        assert_eq!(err.missing_as_str(), vec!["settings.read"]);
    }

    #[test]
    fn root_admin_bypasses_everything() {
        let user = user_with(false, true);
        let ctx = AuthzContext::for_user(&user, PermissionSet::new(), InstanceId::new());
        assert!(ctx.effective_grants().len() == PermissionGrant::ALL.len());
        for grant in PermissionGrant::ALL {
            assert!(require(&ctx, &[*grant]).is_ok(), "{grant:?} should pass");
        }
    }

    #[test]
    fn instance_admin_gets_full_vocabulary() {
        let user = user_with(true, false);
        let ctx = AuthzContext::for_user(&user, PermissionSet::new(), InstanceId::new());
        assert!(require(&ctx, &[PermissionGrant::AdminInstance]).is_ok());
        assert!(require(&ctx, &[PermissionGrant::UserDelete]).is_ok());
    }

    #[test]
    fn resolve_instance_grants_reflects_admin_flags() {
        let row = Permission {
            user_id: UserId::new(),
            instance_id: InstanceId::new(),
            grants: {
                let mut set = PermissionSet::new();
                set.grant(PermissionGrant::FileRead);
                set
            },
        };

        // Member: exactly the row.
        let member = user_with(false, false);
        assert_eq!(resolve_instance_grants(&member, Some(&row)).len(), 1);

        // Admin: full vocabulary, row ignored.
        let admin = user_with(true, false);
        assert_eq!(
            resolve_instance_grants(&admin, Some(&row)).len(),
            PermissionGrant::ALL.len()
        );

        // Root: full vocabulary even with no row.
        let root = user_with(false, true);
        assert_eq!(
            resolve_instance_grants(&root, None).len(),
            PermissionGrant::ALL.len()
        );
    }

    #[test]
    fn error_display_is_stable() {
        let ctx = member_ctx(&[]);
        let err = require(&ctx, &[PermissionGrant::ControlStart]).unwrap_err();
        assert_eq!(err.to_string(), "permission denied: missing 1 grant(s)");
    }
}
