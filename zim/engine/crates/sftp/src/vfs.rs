//! The synthetic per-user root: one directory per visible instance.
//!
//! An SFTP session's `/` is not the host filesystem and not the
//! engine data dir — it is a directory this module assembles at
//! login, containing one entry per instance the user can see, named
//! by the instance's sanitized display name. Descending into an
//! entry chroots into that instance's data directory through
//! [`zim_engine_files::Filesystem`]; every §2.6 rule (traversal,
//! absolute paths, escaping symlinks) applies unchanged underneath.
//!
//! Visibility is the API's own rule (the `load_instance_for_caller`
//! contract): owner, admin, or permission row — anything else is
//! uniformly invisible. The grant set per entry is the API's own
//! resolution too: the owner's implicit set (everything except the
//! admin-scoped `AdminInstance`), the admin bypass, or the subuser
//! row verbatim.

use std::path::PathBuf;

use zim_engine_domain::{Instance, PermissionGrant, PermissionSet, User};
use zim_engine_files::Filesystem;

/// One visible instance in the synthetic root.
#[derive(Debug, Clone)]
pub(crate) struct VfsEntry {
    /// The sanitized directory name the client sees.
    pub name: String,
    /// The instance row.
    pub instance: Instance,
    /// The resolved effective grants (owner-implicit / admin / row).
    pub grants: PermissionSet,
    /// The chrooted filesystem rooted at the instance's data dir.
    pub fs: Filesystem,
}

impl VfsEntry {
    /// Whether the effective grants contain one grant.
    pub(crate) fn has(&self, grant: PermissionGrant) -> bool {
        self.grants.contains(grant)
    }
}

/// The assembled per-user root.
#[derive(Debug, Clone)]
pub(crate) struct Vfs {
    entries: Vec<VfsEntry>,
}

/// Where a client-supplied SFTP path lands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum VfsPath {
    /// The synthetic root itself (`/`).
    Root,
    /// Inside one instance: the entry's index plus the root-relative
    /// remainder (empty for the instance directory itself).
    Inside { entry: usize, relative: String },
}

impl Vfs {
    /// Assemble the root for one user (blocking-safe: persistence
    /// reads + directory stats; callers bridge with spawn_blocking).
    ///
    /// The visibility walk is the API's own `visible_instances`
    /// strategy: admins see every instance (per-node
    /// concatenation), members see owned ∪ permission rows — no row
    /// anywhere means uniformly invisible.
    pub fn build(
        data_dir: &std::path::Path,
        persistence: &dyn zim_engine_application::ports::Persistence,
        user: &User,
    ) -> Result<Self, String> {
        let instances: Vec<Instance> = if user.root_admin || user.is_admin {
            let nodes = persistence
                .list_nodes()
                .map_err(|e| format!("cannot list nodes: {e}"))?;
            let mut all = Vec::new();
            for node in nodes {
                all.extend(
                    persistence
                        .list_instances_by_node(&node.id)
                        .map_err(|e| format!("cannot list instances: {e}"))?,
                );
            }
            all
        } else {
            let mut seen = persistence
                .list_instances_by_owner(&user.id)
                .map_err(|e| format!("cannot list instances: {e}"))?;
            let rows = persistence
                .list_permissions_by_user(&user.id)
                .map_err(|e| format!("cannot list permissions: {e}"))?;
            for row in rows {
                if let Ok(Some(instance)) = persistence.get_instance(&row.instance_id) {
                    if !seen.iter().any(|i| i.id == instance.id) {
                        seen.push(instance);
                    }
                }
            }
            seen
        };

        let mut entries = Vec::new();
        for instance in instances {
            let grants = if user.root_admin || user.is_admin {
                // The admin bypass: the full vocabulary.
                all_grants()
            } else if instance.owner_id == user.id {
                // Owner-implicit: everything except the admin-scoped
                // grant — the API's `owner_implicit_grants`, parity
                // kept deliberately (a comment there points here).
                all_grants_except_admin_instance()
            } else {
                match persistence.get_permission(&user.id, &instance.id) {
                    Ok(Some(row)) => row.grants,
                    _ => PermissionSet::new(),
                }
            };
            if grants.is_empty() {
                // No row, no visibility (uniform invisibility — the
                // stranger cannot even learn the name exists).
                continue;
            }

            let fs = Filesystem::new(instance_root(data_dir, &instance.uuid_short));
            // The instance root appears on first touch (the HTTP
            // family's discipline): a never-started instance still
            // lists an empty directory, never an IO failure.
            let _ = std::fs::create_dir_all(fs.root());
            entries.push(VfsEntry {
                name: String::new(), // assigned after dedup below
                instance,
                grants,
                fs,
            });
        }

        // Sanitized, collision-free names: the display name with
        // path-hostile characters replaced, falling back to the
        // short uuid, deduplicated with a numeric suffix.
        let mut used: Vec<String> = Vec::new();
        for entry in &mut entries {
            let base = sanitize_name(&entry.instance.name);
            let base = if base.is_empty() {
                entry.instance.uuid_short.clone()
            } else {
                base
            };
            let mut candidate = base.clone();
            let mut suffix = 2;
            while used.contains(&candidate) {
                candidate = format!("{base}-{suffix}");
                suffix += 1;
            }
            used.push(candidate.clone());
            entry.name = candidate;
        }
        // Deterministic order for deterministic listings.
        entries.sort_by(|a, b| a.name.cmp(&b.name));

        Ok(Self { entries })
    }

    /// The entries, in listing order.
    pub fn entries(&self) -> &[VfsEntry] {
        &self.entries
    }

    /// One entry by index.
    pub fn entry(&self, index: usize) -> Option<&VfsEntry> {
        self.entries.get(index)
    }

    /// Resolve one client-supplied SFTP path (leading `/` optional;
    /// `.`/`..` normalize chroot-style — `..` at the root IS the
    /// root; deeper `..` components are left to the files crate's
    /// own §2.6 refusal via the relative remainder). `None` means
    /// the first component names no visible instance (the session
    /// layer maps that onto the protocol's no-such-file answer).
    pub(crate) fn resolve(&self, path: &str) -> Option<VfsPath> {
        let trimmed = path.trim_start_matches('/');
        if trimmed.is_empty() || trimmed == "." {
            return Some(VfsPath::Root);
        }
        let mut components = trimmed.split('/').filter(|c| !c.is_empty() && *c != ".");
        let Some(first) = components.next() else {
            return Some(VfsPath::Root);
        };
        let index = self
            .entries
            .iter()
            .position(|e| e.name.eq_ignore_ascii_case(first))?;
        // ".." inside the remainder is §2.6's problem once it lands
        // in the files crate; strip leading ".." components that
        // would climb above the instance root back to the instance
        // root (chroot convention).
        let mut relative: Vec<&str> = components.collect();
        while relative.first() == Some(&"..") {
            relative.remove(0);
        }
        Some(VfsPath::Inside {
            entry: index,
            relative: relative.join("/"),
        })
    }
}

/// The instance data root (`{data_dir}/instances/{uuid_short}`) —
/// the same resolution `build_instance_config` performs for the
/// runtime and the backup pipeline.
pub(crate) fn instance_root(data_dir: &std::path::Path, uuid_short: &str) -> PathBuf {
    data_dir.join("instances").join(uuid_short)
}

/// The full grant vocabulary (the admin bypass).
fn all_grants() -> PermissionSet {
    let mut set = PermissionSet::new();
    for grant in PermissionGrant::ALL {
        set.grant(*grant);
    }
    set
}

/// Everything except the admin-scoped `AdminInstance` grant (the
/// owner-implicit set; parity with the API's private helper).
fn all_grants_except_admin_instance() -> PermissionSet {
    let mut set = PermissionSet::new();
    for grant in PermissionGrant::ALL {
        if *grant != PermissionGrant::AdminInstance {
            set.grant(*grant);
        }
    }
    set
}

/// Path-hostile characters out, something listable in: `/`,
/// backslashes, control bytes, and leading dots (hidden files would
/// hide the instance from its own listing UIs) all become `_`.
fn sanitize_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_control() || c == '/' || c == '\\' {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = cleaned.trim().to_owned();
    let bare = trimmed.trim_start_matches('.').trim();
    if bare.is_empty() {
        String::new()
    } else {
        bare.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_replaces_hostile_characters() {
        assert_eq!(sanitize_name("My Server"), "My Server");
        assert_eq!(sanitize_name("a/b\\c"), "a_b_c");
        assert_eq!(sanitize_name(".hidden"), "hidden");
        assert_eq!(sanitize_name("   "), "");
        assert_eq!(sanitize_name("..weird.."), "weird..");
    }
}
