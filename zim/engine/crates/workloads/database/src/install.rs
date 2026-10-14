//! The Database install plan (M18): what a first run does before
//! the workload is "installed".
//!
//! Databases are their own installers — unlike the GameServer's
//! egg-script plan, there is nothing to download or patch: the
//! image's entrypoint *is* the installer (`initdb`,
//! `mariadb-install-db` / `mysqld --initialize`, an empty RDB),
//! driven by the environment the spec resolved. The plan documents
//! that sequence as the ordered steps the install job family
//! executes and the API surfaces:
//!
//! 1. place the environment (the credential-shaped env the
//!    image's bootstrap reads);
//! 2. initialize the data directory (the image's own first-boot
//!    step — the data directory bind mount is empty);
//! 3. wait for readiness (the probe's `NotReady` window is the
//!    initialization itself);
//! 4. verify with the engine's own client (the acceptance's
//!    "queried via a SQL client").

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use serde::{Deserialize, Serialize};

use crate::engine::DatabaseEngine;
use crate::spec::DatabaseSpec;

/// One step of the first-run install plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum InstallStep {
    /// The credential-shaped environment entries the image's
    /// bootstrap reads (`POSTGRES_PASSWORD` and kin).
    PlaceEnvironment {
        /// The entries, `KEY=value` shaped.
        env: Vec<String>,
    },
    /// The engine's own first-boot initialization (the image's
    /// entrypoint initializes the empty bind-mounted data
    /// directory; the engine column names the tool).
    InitializeDataDirectory {
        /// Which initializer the image runs.
        engine: String,
    },
    /// The readiness wait (the probe's `NotReady` window *is* the
    /// initialization; `Ready` ends the step).
    WaitForReady {
        /// The probe the step polls.
        probe: String,
    },
    /// The verification query through the engine's own client.
    VerifyQuery {
        /// The engine client the step runs.
        client: String,
        /// The statement the step evaluates.
        statement: String,
    },
}

/// The first-run install plan (§M18's "install" parity: the
/// sequence between "the workload row exists" and "the workload
/// is ready").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatabaseInstallPlan {
    /// The engine being installed (the plan's engine column).
    pub engine: String,
    /// The image the workload runs.
    pub image: String,
    /// The ordered steps.
    pub steps: Vec<InstallStep>,
}

/// Build the first-run install plan for one spec.
pub fn build_install_plan(spec: &DatabaseSpec) -> DatabaseInstallPlan {
    let creds = spec.resolved_credentials();
    let env: Vec<String> = spec
        .resolved_env()
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    let initializer = match spec.engine {
        DatabaseEngine::Postgres => "initdb (the postgres image's entrypoint)",
        DatabaseEngine::MySql => "mysqld --initialize (the mysql image's entrypoint)",
        DatabaseEngine::MariaDb => "mariadb-install-db (the mariadb image's entrypoint)",
        DatabaseEngine::Redis => "an empty dataset (redis starts with no RDB present)",
    };
    let probe = spec
        .engine
        .probe_argv(&creds, spec.primary_container_port())
        .join(" ");
    let (client, statement) = match spec.engine {
        DatabaseEngine::Redis => ("redis-cli", "PING"),
        _ => ("the engine's SQL client", "SELECT 1"),
    };
    DatabaseInstallPlan {
        engine: spec.engine.to_string(),
        image: spec.effective_image().to_owned(),
        steps: vec![
            InstallStep::PlaceEnvironment { env },
            InstallStep::InitializeDataDirectory {
                engine: initializer.to_owned(),
            },
            InstallStep::WaitForReady { probe },
            InstallStep::VerifyQuery {
                client: client.to_owned(),
                statement: statement.to_owned(),
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{default_spec, DatabaseCredentials};

    #[test]
    fn the_postgres_plan_places_the_env_then_waits_then_verifies() {
        let spec = default_spec();
        let plan = build_install_plan(&spec);
        assert_eq!(plan.engine, "postgres");
        assert_eq!(plan.image, "postgres:16-alpine");

        let InstallStep::PlaceEnvironment { env } = &plan.steps[0] else {
            panic!("the first step places the environment");
        };
        assert!(env
            .iter()
            .any(|entry| entry.starts_with("POSTGRES_USER=postgres")));
        assert!(env
            .iter()
            .any(|entry| entry.starts_with("POSTGRES_PASSWORD=zim-dev-change-me")));

        let InstallStep::InitializeDataDirectory { engine } = &plan.steps[1] else {
            panic!("the second step initializes");
        };
        assert!(engine.contains("initdb"));

        let InstallStep::WaitForReady { probe } = &plan.steps[2] else {
            panic!("the third step waits");
        };
        assert!(probe.starts_with("pg_isready"));
        assert!(probe.contains("5432"));

        let InstallStep::VerifyQuery { client, statement } = &plan.steps[3] else {
            panic!("the fourth step verifies");
        };
        assert_eq!(statement, "SELECT 1");
        assert_eq!(client, "the engine's SQL client");
    }

    #[test]
    fn the_redis_plan_verifies_with_a_ping() {
        let spec = DatabaseSpec {
            engine: DatabaseEngine::Redis,
            image: None,
            version: None,
            credentials: DatabaseCredentials::default(),
            endpoints: None,
            extra_args: Vec::new(),
        };
        let plan = build_install_plan(&spec);
        assert_eq!(plan.image, "redis:7-alpine");
        let InstallStep::InitializeDataDirectory { engine } = &plan.steps[1] else {
            panic!("the second step initializes");
        };
        assert!(engine.contains("empty dataset"));
        let InstallStep::VerifyQuery { client, statement } = &plan.steps[3] else {
            panic!("the fourth step verifies");
        };
        assert_eq!(client, "redis-cli");
        assert_eq!(statement, "PING");
        // No env entries to place for a passwordless Redis.
        let InstallStep::PlaceEnvironment { env } = &plan.steps[0] else {
            panic!("the first step places the environment");
        };
        assert!(env.is_empty());
    }

    #[test]
    fn the_mysql_family_plans_name_their_initializer() {
        let spec = DatabaseSpec {
            engine: DatabaseEngine::MariaDb,
            image: None,
            version: None,
            credentials: DatabaseCredentials {
                username: None,
                password: "pw".to_owned(),
                database: None,
                app_password: None,
            },
            endpoints: None,
            extra_args: Vec::new(),
        };
        let plan = build_install_plan(&spec);
        let InstallStep::InitializeDataDirectory { engine } = &plan.steps[1] else {
            panic!("the second step initializes");
        };
        assert!(engine.contains("mariadb-install-db"));
        let InstallStep::WaitForReady { probe } = &plan.steps[2] else {
            panic!("the third step waits");
        };
        assert!(probe.starts_with("mariadb-admin"));
    }

    #[test]
    fn the_plan_round_trips_through_serde() {
        let plan = build_install_plan(&default_spec());
        let json = serde_json::to_value(&plan).unwrap();
        let back: DatabaseInstallPlan = serde_json::from_value(json).unwrap();
        assert_eq!(back, plan);
    }
}
