//! Readiness classification (M18): the §4 `ReadyProbeCapable`
//! semantics over the engines' probe output.
//!
//! Readiness is distinct from process presence (§4): a database
//! container can be `Running` while the server is still in
//! recovery ("the database system is starting up"), still
//! initializing (`initdb` / `mariadb-install-db` have not finished),
//! or refusing connections outright. The probe commands answer
//! with an exit code and a diagnostic line;
//! [`classify_probe`] turns one such answer into the SDK's
//! [`Readiness`] vocabulary.
//!
//! The classifiers are pure functions over
//! `(exit_code, stdout, stderr)` so the parity suite can exercise
//! every engine's vocabulary without a container, and the handle
//! simply bridges a real exec into them.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use zim_engine_workloads_sdk::traits::Readiness;

use crate::engine::DatabaseEngine;

/// Classify one probe execution's answer.
///
/// The rules:
///
/// - exit `0` → `Ready` (the engine's own client answered its
///   health command — `accepting connections`, `mysqld is alive`,
///   `PONG`);
/// - exit `1`/`2` with a diagnostic → `NotReady` carrying the
///   engine's own reason line (connection refused, "starting up",
///   "no response");
/// - a missing exit code (the exec could not observe one) →
///   `Unknown`.
///
/// An engine that is not running at all is classified by the
/// caller (the handle) as `NotReady` with a process-level reason —
/// a stopped database is certainly not ready — so the pure layer
/// never needs to guess.
pub fn classify_probe(
    engine: DatabaseEngine,
    exit_code: Option<i32>,
    stdout: &str,
    stderr: &str,
) -> Readiness {
    let Some(code) = exit_code else {
        return Readiness::Unknown;
    };
    if code == 0 {
        // The engine's client vouched for the service; carry its
        // own ready line in the classification's evidence when a
        // sanity check fails (e.g. an empty output for a zero exit
        // is suspicious but not disqualifying — the exit code is
        // the engine's contract).
        return Readiness::Ready;
    }
    let reason = probe_reason(engine, stdout, stderr);
    Readiness::NotReady { reason }
}

/// The reason line for a refused probe: the engine's own
/// diagnostic when it produced one (its vocabulary is the most
/// honest reason a caller can see), else a generic refusal.
fn probe_reason(engine: DatabaseEngine, stdout: &str, stderr: &str) -> String {
    let combined = format!("{}{}", stdout.trim(), stderr.trim());
    if combined.is_empty() {
        return format!("the {engine} probe refused the connection (no diagnostic)");
    }
    // The engines' reason lines are short; keep them that way (the
    // API surfaces them verbatim in state payloads).
    let mut line = combined
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_owned();
    if line.len() > 160 {
        line.truncate(160);
        line.push('…');
    }
    line
}

/// The canonical ready line each engine's probe prints (the
/// parity suite's fixture vocabulary and the classifier's
/// documentation, in one place).
pub fn ready_line(engine: DatabaseEngine) -> &'static str {
    match engine {
        DatabaseEngine::Postgres => "accepting connections",
        DatabaseEngine::MySql | DatabaseEngine::MariaDb => "mysqld is alive",
        DatabaseEngine::Redis => "PONG",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_exits_are_ready_regardless_of_engine() {
        for engine in DatabaseEngine::ALL {
            let line = ready_line(engine);
            assert_eq!(
                classify_probe(engine, Some(0), &format!("/tmp:5432 - {line}\n"), ""),
                Readiness::Ready,
                "{engine}"
            );
        }
    }

    #[test]
    fn refused_probes_carry_the_engines_reason_line() {
        // Postgres during recovery / startup.
        let readiness = classify_probe(
            DatabaseEngine::Postgres,
            Some(1),
            "",
            "pg_isready: /tmp:5432 - rejecting connections\n",
        );
        match readiness {
            Readiness::NotReady { reason } => {
                assert!(reason.contains("rejecting connections"), "{reason}")
            }
            other => panic!("expected NotReady, got {other:?}"),
        }

        // The recovery window's own message.
        let readiness = classify_probe(
            DatabaseEngine::Postgres,
            Some(1),
            "",
            "pg_isready: /tmp:5432 - the database system is starting up\n",
        );
        match readiness {
            Readiness::NotReady { reason } => {
                assert!(reason.contains("starting up"), "{reason}")
            }
            other => panic!("expected NotReady, got {other:?}"),
        }

        // The MySQL family during bootstrap.
        let readiness = classify_probe(
            DatabaseEngine::MariaDb,
            Some(1),
            "",
            "connect to server at '127.0.0.1' failed\n",
        );
        match readiness {
            Readiness::NotReady { reason } => {
                assert!(reason.contains("failed"), "{reason}")
            }
            other => panic!("expected NotReady, got {other:?}"),
        }

        // Redis before the listener exists.
        let readiness = classify_probe(
            DatabaseEngine::Redis,
            Some(1),
            "Could not connect to Redis at 127.0.0.1:6379: \
             Connection refused\n",
            "",
        );
        match readiness {
            Readiness::NotReady { reason } => {
                assert!(reason.contains("Connection refused"), "{reason}")
            }
            other => panic!("expected NotReady, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_exit_code_is_unknown() {
        for engine in DatabaseEngine::ALL {
            assert_eq!(
                classify_probe(engine, None, "anything", "else"),
                Readiness::Unknown,
                "{engine}"
            );
        }
    }

    #[test]
    fn an_empty_refusal_gets_a_generic_reason() {
        let readiness = classify_probe(DatabaseEngine::MySql, Some(1), "", "");
        match readiness {
            Readiness::NotReady { reason } => {
                assert!(reason.contains("mysql"), "{reason}");
                assert!(reason.contains("no diagnostic"), "{reason}");
            }
            other => panic!("expected NotReady, got {other:?}"),
        }
    }

    #[test]
    fn unwieldy_reason_lines_are_truncated() {
        let long = "x".repeat(400);
        let readiness = classify_probe(DatabaseEngine::Redis, Some(1), &long, "");
        match readiness {
            // 160 kept characters plus the ellipsis (which is three
            // bytes in UTF-8) — the byte length bound, not the char
            // count, is what the API payload cares about.
            Readiness::NotReady { reason } => {
                assert!(reason.len() <= 163, "{} ({} bytes)", reason, reason.len())
            }
            other => panic!("expected NotReady, got {other:?}"),
        }
    }

    #[test]
    fn the_ready_lines_match_the_engines_vocabulary() {
        assert_eq!(
            ready_line(DatabaseEngine::Postgres),
            "accepting connections"
        );
        assert_eq!(ready_line(DatabaseEngine::MySql), "mysqld is alive");
        assert_eq!(ready_line(DatabaseEngine::MariaDb), "mysqld is alive");
        assert_eq!(ready_line(DatabaseEngine::Redis), "PONG");
    }
}
