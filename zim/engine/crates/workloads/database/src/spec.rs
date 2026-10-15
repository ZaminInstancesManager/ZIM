//! The Database workload spec (M18).
//!
//! Per `docs/architecture/WORKLOAD_ABSTRACTION.md` §3, a `Workload`
//! row of kind `Database` carries this spec as its §10.15 `spec`
//! JSON. Unlike the GameServer spec (a snapshot of an imported
//! Pterodactyl egg), the Database spec is ZIM-native from the
//! start: the engine table ([`DatabaseEngine`]) contributes the
//! per-engine conventions, and the spec carries the user's
//! decisions on top — image override, credentials, endpoints,
//! and the engine's container posture.
//!
//! The spec's *resolved* form is the engine-facing half:
//! [`DatabaseSpec::resolved_env`] (the image's bootstrap env),
//! [`DatabaseSpec::resolved_command`] (the server argv), and the
//! argv builders the handle's capability implementations use.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::str::FromStr;

use serde::{Deserialize, Serialize};

use zim_engine_domain::DockerImage;
use zim_engine_workloads_sdk::errors::SpecError;

use crate::engine::DatabaseEngine;

/// The workload's credentials, spec-shaped.
///
/// `username`/`password` address the engine's bootstrap subject
/// (the Postgres superuser, the MySQL/MariaDB root, or Redis's
/// optional `requirepass`). A non-root `username` for the MySQL
/// family additionally creates an app user (the images'
/// `MYSQL_USER`/`MARIADB_USER` convention) whose password is
/// `app_password`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatabaseCredentials {
    /// The engine's bootstrap user (default: the engine's own —
    /// `postgres`, `root`, or none for Redis).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// The bootstrap subject's password (required for every
    /// engine except Redis, where it enables `requirepass`).
    #[serde(default)]
    pub password: String,
    /// The default database the image creates (`POSTGRES_DB`,
    /// `MYSQL_DATABASE`, `MARIADB_DATABASE`; Redis has none).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
    /// The MySQL-family app user's password (the
    /// `MYSQL_PASSWORD`/`MARIADB_PASSWORD` convention) when
    /// `username` is a non-root user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_password: Option<String>,
}

/// One logical endpoint the workload exposes (the
/// read/write/replication vocabulary; the same container port may
/// carry several logical roles — replication rides the primary
/// listener for every M18 engine).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecEndpoint {
    /// The endpoint's logical name (`"primary"`, `"replication"`,
    /// `"mysqlx"`, ...).
    pub name: String,
    /// The container-side port the engine listens on.
    pub container_port: u16,
}

impl SpecEndpoint {
    /// Construct one endpoint declaration.
    pub fn new(name: &str, container_port: u16) -> Self {
        Self {
            name: name.to_owned(),
            container_port,
        }
    }
}

/// The runtime-facing container posture one engine needs: the
/// entrypoint's user, the rootfs writability, and the capability
/// drop list (the per-instance overrides the M18 engine-side
/// carries).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerPosture {
    /// The container user (`uid:gid` or name) — the database
    /// images' root entrypoints, which `chown` the data directory
    /// before self-demoting to their service user.
    pub user: String,
    /// Whether the container keeps the engine's hardened
    /// read-only root filesystem.
    pub readonly_rootfs: bool,
    /// The capability-drop list (the reduced Wings posture: the
    /// ownership-management trio restored).
    pub cap_drop: Vec<String>,
}

/// The Database workload spec.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DatabaseSpec {
    /// The managed engine (drives every default below).
    pub engine: DatabaseEngine,
    /// The image override (default: the engine's
    /// [`table image`][DatabaseEngine::default_image]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// The engine version label (diagnostics only; the image
    /// reference is the source of truth).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The workload's credentials.
    #[serde(default)]
    pub credentials: DatabaseCredentials,
    /// The logical endpoints (default: the engine's table).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoints: Option<Vec<SpecEndpoint>>,
    /// Extra server args appended after the engine's default
    /// argv (e.g. Postgres `-c work_mem=64MB`, Redis
    /// `--maxmemory 512mb`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_args: Vec<String>,
}

impl DatabaseSpec {
    /// The spec's effective image (the override or the engine's
    /// table default).
    pub fn effective_image(&self) -> &str {
        self.image.as_deref().unwrap_or(self.engine.default_image())
    }

    /// The spec's effective endpoints (the override or the
    /// engine's table defaults).
    pub fn effective_endpoints(&self) -> Vec<SpecEndpoint> {
        self.endpoints
            .clone()
            .unwrap_or_else(|| self.engine.default_endpoints())
    }

    /// The primary container port (the first endpoint's — the
    /// probe and the eval client target it).
    pub fn primary_container_port(&self) -> u16 {
        self.effective_endpoints()
            .first()
            .map(|endpoint| endpoint.container_port)
            .unwrap_or_else(|| match self.engine {
                DatabaseEngine::Postgres => 5432,
                DatabaseEngine::MySql | DatabaseEngine::MariaDb => 3306,
                DatabaseEngine::Redis => 6379,
            })
    }

    /// The resolved credentials: the spec's own values with the
    /// engine's defaults filled in.
    pub fn resolved_credentials(&self) -> DatabaseCredentials {
        DatabaseCredentials {
            username: self
                .credentials
                .username
                .clone()
                .or_else(|| self.engine.default_username().map(str::to_owned)),
            password: self.credentials.password.clone(),
            database: self
                .credentials
                .database
                .clone()
                .or_else(|| self.engine.default_database().map(str::to_owned)),
            app_password: self.credentials.app_password.clone(),
        }
    }

    /// The image's bootstrap environment (the engine table's
    /// env over the resolved credentials), plus the Redis
    /// password's argv form when one is set.
    pub fn resolved_env(&self) -> Vec<(String, String)> {
        self.engine.bootstrap_env(&self.resolved_credentials())
    }

    /// The resolved server argv: the engine's default command
    /// plus the spec's extra args, plus Redis's `--requirepass`
    /// when a password is set.
    pub fn resolved_command(&self) -> Vec<String> {
        let mut argv = self.engine.default_command();
        if matches!(self.engine, DatabaseEngine::Redis) && !self.credentials.password.is_empty() {
            argv.push("--requirepass".to_owned());
            argv.push(self.credentials.password.clone());
        }
        argv.extend(self.extra_args.iter().cloned());
        argv
    }

    /// Validate the spec's structural invariants.
    ///
    /// Strict where the engine must not launch garbage (a
    /// missing required password, a malformed image reference,
    /// empty or duplicate endpoint names); permissive where the
    /// engine's own runtime will surface the failure (extra args
    /// pass through verbatim — a bad flag is the server's error,
    /// not a schema violation).
    pub fn validate(&self) -> Result<(), SpecError> {
        if self.engine.requires_password() && self.credentials.password.trim().is_empty() {
            return Err(SpecError::Invalid {
                field: "credentials.password".to_owned(),
                message: format!(
                    "the {} engine requires a bootstrap password \
                     (the image refuses to start without one)",
                    self.engine
                ),
            });
        }
        let image = self.effective_image();
        if image.trim().is_empty() {
            return Err(SpecError::Invalid {
                field: "image".to_owned(),
                message: "the workload image must not be empty".to_owned(),
            });
        }
        if DockerImage::from_str(image).is_err() {
            return Err(SpecError::Invalid {
                field: "image".to_owned(),
                message: format!("{image:?} is not a valid image reference"),
            });
        }
        let endpoints = self.effective_endpoints();
        if endpoints.is_empty() {
            return Err(SpecError::Invalid {
                field: "endpoints".to_owned(),
                message: "the workload must expose at least one endpoint".to_owned(),
            });
        }
        let mut names = std::collections::BTreeSet::new();
        for endpoint in &endpoints {
            if endpoint.name.trim().is_empty() {
                return Err(SpecError::Invalid {
                    field: "endpoints".to_owned(),
                    message: "an endpoint has an empty name".to_owned(),
                });
            }
            if !names.insert(endpoint.name.clone()) {
                return Err(SpecError::Invalid {
                    field: "endpoints".to_owned(),
                    message: format!("the endpoint name {:?} is declared twice", endpoint.name),
                });
            }
            if endpoint.container_port == 0 {
                return Err(SpecError::Invalid {
                    field: "endpoints".to_owned(),
                    message: format!(
                        "the endpoint {:?} declares port 0 \
                         (the container port must be the engine's own)",
                        endpoint.name
                    ),
                });
            }
        }
        Ok(())
    }

    /// The engine's container posture for the runtime-facing
    /// instance configuration.
    ///
    /// M18's engine-side extensions (the per-instance user,
    /// rootfs, and capability-drop overrides) exist precisely to
    /// carry this posture: the database images' root entrypoints,
    /// their writable-surface needs, and their
    /// ownership-management capabilities.
    pub fn container_posture(&self) -> ContainerPosture {
        ContainerPosture {
            user: self.engine.default_container_user().to_owned(),
            readonly_rootfs: self.engine.default_readonly_rootfs(),
            cap_drop: DatabaseEngine::default_cap_drop()
                .iter()
                .map(|cap| (*cap).to_owned())
                .collect(),
        }
    }
}

/// The default spec: a Postgres workload with a clearly-marked
/// development password (the API's creation form binds real
/// credentials at instantiation; a default that fails its own
/// validation would be useless as a form template).
pub fn default_spec() -> DatabaseSpec {
    DatabaseSpec {
        engine: DatabaseEngine::Postgres,
        image: None,
        version: Some("16".to_owned()),
        credentials: DatabaseCredentials {
            username: None,
            password: "zim-dev-change-me".to_owned(),
            database: None,
            app_password: None,
        },
        endpoints: None,
        extra_args: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_spec_validates_and_resolves() {
        let spec = default_spec();
        spec.validate().unwrap();
        assert_eq!(spec.effective_image(), "postgres:16-alpine");
        assert_eq!(spec.primary_container_port(), 5432);
        let env = spec.resolved_env();
        assert!(env.contains(&("POSTGRES_USER".to_owned(), "postgres".to_owned())));
        assert!(env.contains(&(
            "POSTGRES_PASSWORD".to_owned(),
            "zim-dev-change-me".to_owned()
        )));
        // POSTGRES_DB defaults to the engine's own.
        assert!(env.contains(&("POSTGRES_DB".to_owned(), "postgres".to_owned())));
        let command = spec.resolved_command();
        assert_eq!(command, vec!["postgres".to_owned()]);
        let posture = spec.container_posture();
        assert_eq!(posture.user, "0:0");
        // The SQL engines take the writable-rootfs posture.
        assert!(!posture.readonly_rootfs);
        // The reduced drop list restores the ownership trio.
        assert!(!posture.cap_drop.contains(&"dac_override".to_owned()));
        assert!(!posture.cap_drop.contains(&"fowner".to_owned()));
        assert!(!posture.cap_drop.contains(&"fsetid".to_owned()));
        // And keeps the dangerous ones dropped.
        assert!(posture.cap_drop.contains(&"mknod".to_owned()));
        assert!(posture.cap_drop.contains(&"net_raw".to_owned()));
        assert!(posture.cap_drop.contains(&"sys_chroot".to_owned()));
        // No PGHOST (the entrypoint blanks it for its own client;
        // the socket stays at the compiled-in default).
        assert!(spec.resolved_env().iter().all(|(key, _)| key != "PGHOST"));
    }

    #[test]
    fn a_passwordless_postgres_is_refused() {
        let mut spec = default_spec();
        spec.credentials.password = String::new();
        let err = spec.validate().unwrap_err();
        assert!(err.to_string().contains("postgres"), "{err}");

        // Redis is the exception: no password required.
        spec.engine = DatabaseEngine::Redis;
        spec.credentials.password = String::new();
        spec.validate().unwrap();
    }

    #[test]
    fn a_malformed_image_is_refused() {
        let mut spec = default_spec();
        spec.image = Some("ghcr.io/ parkour/db:1".to_owned());
        assert!(spec.validate().is_err());
        spec.image = Some(String::new());
        assert!(spec.validate().is_err());
        spec.image = None;
        spec.validate().unwrap();
    }

    #[test]
    fn endpoint_shapes_are_validated() {
        let mut spec = default_spec();
        spec.endpoints = Some(Vec::new());
        assert!(spec.validate().is_err());

        spec.endpoints = Some(vec![
            SpecEndpoint::new("primary", 5432),
            SpecEndpoint::new("primary", 5433),
        ]);
        let err = spec.validate().unwrap_err();
        assert!(err.to_string().contains("twice"), "{err}");

        spec.endpoints = Some(vec![SpecEndpoint::new("", 5432)]);
        assert!(spec.validate().is_err());

        spec.endpoints = Some(vec![SpecEndpoint::new("primary", 0)]);
        assert!(spec.validate().is_err());

        // The same port carrying two logical roles is fine (the
        // read/write/replication vocabulary rides one listener).
        spec.endpoints = Some(vec![
            SpecEndpoint::new("primary", 5432),
            SpecEndpoint::new("replication", 5432),
        ]);
        spec.validate().unwrap();
    }

    #[test]
    fn the_mysql_family_resolves_its_shapes() {
        let spec = DatabaseSpec {
            engine: DatabaseEngine::MySql,
            image: None,
            version: None,
            credentials: DatabaseCredentials {
                username: None,
                password: "rootpw".to_owned(),
                database: None,
                app_password: None,
            },
            endpoints: None,
            extra_args: Vec::new(),
        };
        spec.validate().unwrap();
        assert_eq!(spec.effective_image(), "mysql:8.4");
        let endpoints = spec.effective_endpoints();
        assert_eq!(endpoints.len(), 2);
        assert_eq!(endpoints[1].name, "mysqlx");
        assert_eq!(endpoints[1].container_port, 33060);
        let env = spec.resolved_env();
        assert!(env.contains(&("MYSQL_ROOT_PASSWORD".to_owned(), "rootpw".to_owned())));
        let command = spec.resolved_command();
        assert_eq!(command[0], "mysqld");
        let posture = spec.container_posture();
        assert_eq!(posture.user, "0:0");
        assert!(!posture.readonly_rootfs);
        assert!(!posture.cap_drop.contains(&"dac_override".to_owned()));
    }

    #[test]
    fn extra_args_and_the_redis_password_ride_the_argv() {
        let mut spec = DatabaseSpec {
            engine: DatabaseEngine::Redis,
            image: None,
            version: None,
            credentials: DatabaseCredentials {
                username: None,
                password: "hunter2".to_owned(),
                database: None,
                app_password: None,
            },
            endpoints: None,
            extra_args: vec!["--maxmemory".to_owned(), "256mb".to_owned()],
        };
        let command = spec.resolved_command();
        assert_eq!(command[0], "redis-server");
        assert!(command.contains(&"--requirepass".to_owned()));
        assert!(command.contains(&"hunter2".to_owned()));
        assert!(command.contains(&"--maxmemory".to_owned()));
        assert!(command.contains(&"256mb".to_owned()));

        // Without a password, no requirepass.
        spec.credentials.password = String::new();
        let command = spec.resolved_command();
        assert!(!command.contains(&"--requirepass".to_owned()));
    }

    #[test]
    fn the_spec_round_trips_through_serde() {
        let spec = default_spec();
        let json = serde_json::to_value(&spec).unwrap();
        let back: DatabaseSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, spec);
    }

    #[test]
    fn a_minimal_json_spec_parses_with_engine_defaults() {
        let spec: DatabaseSpec = serde_json::from_value(serde_json::json!({
            "engine": "mariadb",
            "credentials": { "password": "pw" }
        }))
        .unwrap();
        spec.validate().unwrap();
        assert_eq!(spec.effective_image(), "mariadb:11");
        assert_eq!(
            spec.effective_endpoints()[0],
            SpecEndpoint::new("primary", 3306)
        );
        assert_eq!(
            spec.resolved_credentials().username.as_deref(),
            Some("root")
        );
    }

    #[test]
    fn an_unknown_engine_is_unparsable() {
        let err = serde_json::from_value::<DatabaseSpec>(serde_json::json!({
            "engine": "mongodb"
        }))
        .unwrap_err();
        assert!(err.to_string().contains("mongodb"), "{err}");
    }
}
