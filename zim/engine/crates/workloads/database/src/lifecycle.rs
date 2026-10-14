//! Lifecycle and migration plans (M18): what each power action and
//! each migration phase *means* for a Database workload.
//!
//! The engine's supervision layer owns the generic lifecycle; the
//! integration contributes the kind-specific translation. The
//! database translation's headline difference from the
//! GameServer's: **there is no console**. A database's stop
//! convention is the signal path — SIGTERM, which every M18
//! engine's entrypoint translates into its own orderly shutdown
//! (Postgres's fast shutdown checkpoints and exits; the MySQL
//! family's entrypoint trap runs a clean shutdown; Redis saves and
//! exits). The runtime's graceful-then-forcible stop *is* the
//! integration's stop, with no console detour.
//!
//! The migration plan is the §M18 multi-step family — quiesce,
//! checkpoint WAL, snapshot, ship snapshot, restore, catch up —
//! documented per-engine by [`migration_steps`] so the API can
//! surface the steps it will run.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::time::Duration;

use serde::{Deserialize, Serialize};

use zim_engine_workloads_sdk::errors::IntegrationError;

use crate::engine::DatabaseEngine;
use crate::spec::DatabaseSpec;

/// The default graceful-stop deadline (the engine's 30-second
/// posture; a database's clean shutdown — checkpoint + flush —
/// fits comfortably, and the forcible backstop after it is the
/// crash-consistency path the engines' own recovery designs
/// tolerate).
pub const DEFAULT_STOP_DEADLINE: Duration = Duration::from_secs(30);

/// What one power action translates to for this workload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DatabaseLifecyclePlan {
    /// The action this plan answers.
    pub action: String,
    /// The graceful deadline before the forcible backstop.
    pub graceful_deadline_secs: u64,
    /// Whether the action requires the workload to be installed
    /// (the data directory initialized) first.
    pub requires_install: bool,
    /// The engine's quiesce behavior on stop, in words (the
    /// signal path plus what the entrypoint does with it).
    pub stop_semantics: String,
    /// Human-readable notes the engine may surface.
    pub notes: Vec<String>,
}

/// Build the lifecycle plan for one action against one spec.
///
/// Unknown actions answer
/// [`IntegrationError::Unsupported`][IntegrationError::Unsupported].
pub fn lifecycle_plan(
    spec: &DatabaseSpec,
    action: &str,
) -> Result<DatabaseLifecyclePlan, IntegrationError> {
    let stop_semantics = stop_semantics(spec.engine);
    match action {
        "start" => Ok(DatabaseLifecyclePlan {
            action: action.to_owned(),
            graceful_deadline_secs: DEFAULT_STOP_DEADLINE.as_secs(),
            requires_install: true,
            stop_semantics: stop_semantics.clone(),
            notes: vec!["the first boot initializes the data directory \
                 (initdb / mariadb-install-db / an empty RDB); the \
                 readiness probe is NotReady until initialization \
                 completes"
                .to_owned()],
        }),
        "stop" => Ok(DatabaseLifecyclePlan {
            action: action.to_owned(),
            graceful_deadline_secs: DEFAULT_STOP_DEADLINE.as_secs(),
            requires_install: false,
            stop_semantics,
            notes: Vec::new(),
        }),
        "restart" => Ok(DatabaseLifecyclePlan {
            action: action.to_owned(),
            graceful_deadline_secs: DEFAULT_STOP_DEADLINE.as_secs(),
            requires_install: false,
            stop_semantics: stop_semantics.clone(),
            notes: vec!["restart is stop-then-start: the engine's orderly \
                 shutdown (checkpoint + flush) completes before the \
                 relaunch, and WAL/redo replay finishes the catch-up \
                 on the way up"
                .to_owned()],
        }),
        "kill" => Ok(DatabaseLifecyclePlan {
            action: action.to_owned(),
            graceful_deadline_secs: 0,
            requires_install: false,
            stop_semantics: format!(
                "kill skips the orderly path: the next start replays \
                 the {} recovery log (committed transactions survive; \
                 in-flight ones roll back)",
                spec.engine
            ),
            notes: Vec::new(),
        }),
        _ => Err(IntegrationError::unsupported(
            "org.zaminmc.workload.database",
            "lifecycle_plan",
        )),
    }
}

/// The engine's stop semantics, in words (the plan's
/// `stop_semantics` payload).
fn stop_semantics(engine: DatabaseEngine) -> String {
    match engine {
        DatabaseEngine::Postgres => "SIGTERM drives Postgres's fast shutdown: the \
             entrypoint stops accepting connections, checkpoints, and exits \
             cleanly within the deadline"
            .to_owned(),
        DatabaseEngine::MySql | DatabaseEngine::MariaDb => {
            "SIGTERM trips the entrypoint's shutdown trap: a clean \
             engine shutdown (buffers flushed, tables closed) within \
             the deadline"
                .to_owned()
        }
        DatabaseEngine::Redis => "SIGTERM is Redis's shutdown-save: the dataset is \
             persisted to the RDB before the exit"
            .to_owned(),
    }
}

/// One step of the multi-step migration plan (§M18).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationStep {
    /// The step's ordinal position.
    pub step: u8,
    /// The step's name.
    pub name: String,
    /// What happens (the engine-specific behavior).
    pub detail: String,
}

/// The §M18 multi-step migration plan, per engine: quiesce,
/// checkpoint WAL, snapshot, ship snapshot, restore, catch up.
///
/// This is the *documented* plan (the API's "what will happen"
/// payload); the executable form is the [`Migratable`]
/// implementation on [`DatabaseWorkload`][crate::DatabaseWorkload].
pub fn migration_steps(spec: &DatabaseSpec) -> Vec<MigrationStep> {
    let engine = spec.engine;
    let quiesce = match engine {
        DatabaseEngine::Postgres => {
            "CHECKPOINT flushes the buffers and advances the WAL \
             checkpoint record; the workload then takes the engine's \
             fast shutdown (no client may write past the checkpoint)"
        }
        DatabaseEngine::MySql | DatabaseEngine::MariaDb => {
            "the workload takes its clean shutdown (buffers flushed, \
             tables closed); the lock-held snapshot path is the \
             online alternative for live migrations"
        }
        DatabaseEngine::Redis => {
            "BGSAVE completes and the workload exits (Redis's \
             shutdown-save), leaving a persisted RDB"
        }
    };
    let checkpoint = match engine {
        DatabaseEngine::Postgres => {
            "the migration marker records pg_current_wal_lsn() — the \
             WAL position the destination replays up to"
        }
        DatabaseEngine::MySql | DatabaseEngine::MariaDb => {
            "the migration marker records the server's statement time \
             (the binlog position when binlogging is enabled)"
        }
        DatabaseEngine::Redis => {
            "the migration marker records LASTSAVE — the RDB epoch the \
             destination's restore materializes"
        }
    };
    let snapshot = match engine {
        DatabaseEngine::Postgres => {
            "the data directory is archived host-side after the \
             CHECKPOINT (a crash-consistent image; WAL replay \
             completes it on restore)"
        }
        DatabaseEngine::MySql | DatabaseEngine::MariaDb => {
            "the data directory is archived under FLUSH TABLES WITH \
             READ LOCK (container-side tar in the locked session)"
        }
        DatabaseEngine::Redis => "the RDB (post-BGSAVE) is archived host-side",
    };
    let catch_up = match engine {
        DatabaseEngine::Postgres => {
            "WAL replay brings the restored node to the checkpoint's \
             LSN (the migration marker's position)"
        }
        DatabaseEngine::MySql | DatabaseEngine::MariaDb => {
            "InnoDB redo replay completes the restored node's recovery; \
             replication from the source catches up past the marker \
             when a topology exists"
        }
        DatabaseEngine::Redis => {
            "the restored node serves the RDB's keyspace; a replica \
             resync catches up past the marker when a topology exists"
        }
    };
    vec![
        MigrationStep {
            step: 1,
            name: "quiesce".to_owned(),
            detail: quiesce.to_owned(),
        },
        MigrationStep {
            step: 2,
            name: "checkpoint".to_owned(),
            detail: checkpoint.to_owned(),
        },
        MigrationStep {
            step: 3,
            name: "snapshot".to_owned(),
            detail: snapshot.to_owned(),
        },
        MigrationStep {
            step: 4,
            name: "ship".to_owned(),
            detail: "the snapshot archive transfers to the destination \
                     node's data directory (the transfer engine's \
                     file-copy family)"
                .to_owned(),
        },
        MigrationStep {
            step: 5,
            name: "restore".to_owned(),
            detail: "the destination clears its data directory (keeping \
                     its snapshot store), extracts the archive, and \
                     starts the engine, which replays its recovery log"
                .to_owned(),
        },
        MigrationStep {
            step: 6,
            name: "catch-up".to_owned(),
            detail: catch_up.to_owned(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::default_spec;

    #[test]
    fn stop_is_the_signal_path_not_a_console_command() {
        let spec = default_spec();
        let plan = lifecycle_plan(&spec, "stop").unwrap();
        assert_eq!(plan.graceful_deadline_secs, 30);
        assert!(plan.stop_semantics.contains("SIGTERM"));
        assert!(plan.stop_semantics.contains("fast shutdown"));
        assert!(!plan.requires_install);
    }

    #[test]
    fn start_notes_the_first_boot_initialization() {
        let spec = default_spec();
        let plan = lifecycle_plan(&spec, "start").unwrap();
        assert!(plan.requires_install);
        assert!(plan.notes[0].contains("initializes the data directory"));
        assert!(plan.notes[0].contains("NotReady"));
    }

    #[test]
    fn restart_and_kill_carry_their_semantics() {
        let spec = default_spec();
        let plan = lifecycle_plan(&spec, "restart").unwrap();
        assert!(plan
            .notes
            .iter()
            .any(|note| note.contains("stop-then-start")));
        let plan = lifecycle_plan(&spec, "kill").unwrap();
        assert_eq!(plan.graceful_deadline_secs, 0);
        assert!(plan.stop_semantics.contains("recovery log"));
    }

    #[test]
    fn unknown_actions_are_unsupported() {
        let spec = default_spec();
        let err = lifecycle_plan(&spec, "hibernate").unwrap_err();
        assert!(matches!(err, IntegrationError::Unsupported { .. }));
    }

    #[test]
    fn the_mysql_family_stop_semantics_name_the_trap() {
        let spec = DatabaseSpec {
            engine: DatabaseEngine::MariaDb,
            image: None,
            version: None,
            credentials: crate::spec::DatabaseCredentials {
                username: None,
                password: "pw".to_owned(),
                database: None,
                app_password: None,
            },
            endpoints: None,
            extra_args: Vec::new(),
        };
        let plan = lifecycle_plan(&spec, "stop").unwrap();
        assert!(plan.stop_semantics.contains("shutdown trap"));
    }

    #[test]
    fn the_migration_steps_are_the_roadmaps_six() {
        let spec = default_spec();
        let steps = migration_steps(&spec);
        let names: Vec<&str> = steps.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "quiesce",
                "checkpoint",
                "snapshot",
                "ship",
                "restore",
                "catch-up"
            ]
        );
        assert_eq!(steps[0].step, 1);
        assert_eq!(steps[5].step, 6);
        // The Postgres plan names its engine specifics.
        assert!(steps[0].detail.contains("CHECKPOINT"));
        assert!(steps[1].detail.contains("pg_current_wal_lsn"));
        assert!(steps[5].detail.contains("WAL replay"));
    }

    #[test]
    fn every_engine_gets_six_steps_with_engine_specific_detail() {
        for engine in DatabaseEngine::ALL {
            let spec = DatabaseSpec {
                engine,
                image: None,
                version: None,
                credentials: crate::spec::DatabaseCredentials {
                    username: None,
                    password: "pw".to_owned(),
                    database: None,
                    app_password: None,
                },
                endpoints: None,
                extra_args: Vec::new(),
            };
            let steps = migration_steps(&spec);
            assert_eq!(steps.len(), 6, "{engine}");
            assert!(steps.iter().all(|step| !step.detail.is_empty()), "{engine}");
            // The snapshot step names the engine's own quiesce
            // vocabulary from the roadmap.
            let snapshot_detail = &steps[2].detail;
            match engine {
                DatabaseEngine::Postgres => {
                    assert!(snapshot_detail.contains("CHECKPOINT"))
                }
                DatabaseEngine::MySql | DatabaseEngine::MariaDb => {
                    assert!(snapshot_detail.contains("FLUSH TABLES WITH READ LOCK"))
                }
                DatabaseEngine::Redis => assert!(snapshot_detail.contains("RDB")),
            }
        }
    }

    #[test]
    fn the_plans_round_trip_through_serde() {
        let spec = default_spec();
        let plan = lifecycle_plan(&spec, "stop").unwrap();
        let json = serde_json::to_value(&plan).unwrap();
        let back: DatabaseLifecyclePlan = serde_json::from_value(json).unwrap();
        assert_eq!(back, plan);

        let steps = migration_steps(&spec);
        let json = serde_json::to_value(&steps).unwrap();
        let back: Vec<MigrationStep> = serde_json::from_value(json).unwrap();
        assert_eq!(back, steps);
    }
}
