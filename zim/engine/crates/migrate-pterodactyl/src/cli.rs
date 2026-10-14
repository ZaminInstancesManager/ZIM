//! The `zim migrate pterodactyl` subcommand (M10's CLI surface per
//! the roadmap):
//!
//! ```text
//! zim_engine migrate pterodactyl \
//!     (--panel-mysql-dsn=DSN | --panel-fixture=PATH) \
//!     --zim-db=PATH [--dry-run] \
//!     [--allow-privileged-scripts] [--allow-root]
//! ```
//!
//! `--dry-run` rehearses the whole import against a private
//! in-memory store — the printed report is exactly what a real
//! import would produce (validation, counts, skips, warnings), and
//! the operator's database is never opened.
//!
//! The MySQL adapter requires a build with `--features mysql`; the
//! fixture source is always available.

use std::path::PathBuf;

use zim_engine_application::ports::Persistence;
use zim_engine_persistence_sqlite::SqlitePersistence;

use crate::report::ImportReport;
use crate::source::{JsonPanelSource, PanelSource, PanelSourceError};
use crate::{import_panel, ImportPolicy};

/// The subcommand's usage text.
const USAGE: &str = "zim_engine migrate pterodactyl — import a Pterodactyl Panel database

USAGE:
    zim_engine migrate pterodactyl \
(--panel-mysql-dsn=DSN | --panel-fixture=PATH) --zim-db=PATH [OPTIONS]

OPTIONS:
    --panel-mysql-dsn=DSN    Read the panel from a live MySQL database
                             (read-only connection). Requires a build
                             with --features mysql.
    --panel-fixture=PATH     Read the panel from a JSON fixture (the
                             shape of tests/fixtures/panel_fixture.json).
    --zim-db=PATH            The ZIM engine's SQLite database to write
                             (created and migrated when missing).
    --dry-run                Rehearse the import against a private
                             in-memory store; --zim-db is not touched.
    --allow-privileged-scripts
                             Opt in to eggs whose install scripts run
                             privileged (rejected by default).
    --allow-root             Opt in to eggs whose workloads run as root
                             (rejected by default).
    -h, --help               Print this help.";

/// Runs the `migrate` subcommand with the arguments *after* the
/// word `migrate`. Prints the import report to stdout and progress
/// notes to stderr; returns the process exit code.
pub fn run_cli(args: &[String]) -> i32 {
    match run(args) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("zim_engine migrate: {error}");
            1
        }
    }
}

fn run(args: &[String]) -> Result<i32, String> {
    let Some(subject) = args.first().map(String::as_str) else {
        eprintln!("{USAGE}");
        return Ok(2);
    };
    if subject != "pterodactyl" {
        eprintln!("unknown migrate subject {subject:?}; expected \"pterodactyl\"");
        eprintln!("{USAGE}");
        return Ok(2);
    }

    let mut fixture: Option<PathBuf> = None;
    let mut dsn: Option<String> = None;
    let mut zim_db: Option<PathBuf> = None;
    let mut dry_run = false;
    let mut policy = ImportPolicy::default();

    for arg in args.iter().skip(1) {
        let arg = arg.as_str();
        if let Some(value) = arg.strip_prefix("--panel-fixture=") {
            fixture = Some(PathBuf::from(value));
        } else if let Some(value) = arg.strip_prefix("--panel-mysql-dsn=") {
            dsn = Some(value.to_owned());
        } else if let Some(value) = arg.strip_prefix("--zim-db=") {
            zim_db = Some(PathBuf::from(value));
        } else if arg == "--dry-run" {
            dry_run = true;
        } else if arg == "--allow-privileged-scripts" {
            policy.allow_privileged_install_scripts = true;
        } else if arg == "--allow-root" {
            policy.allow_requires_root = true;
        } else if arg == "-h" || arg == "--help" {
            println!("{USAGE}");
            return Ok(0);
        } else {
            eprintln!("unknown argument {arg:?}");
            eprintln!("{USAGE}");
            return Ok(2);
        }
    }

    // Exactly one source; the target is required unless this is a
    // rehearsal (a dry run never opens it).
    let source: Box<dyn PanelSource> = match (fixture, dsn) {
        (Some(path), None) => Box::new(JsonPanelSource::from_path(path)),
        (None, Some(dsn)) => {
            #[cfg(feature = "mysql")]
            {
                Box::new(crate::mysql::MysqlPanelSource::from_dsn(&dsn).map_err(|e| e.to_string())?)
            }
            #[cfg(not(feature = "mysql"))]
            {
                let _ = dsn;
                eprintln!(
                    "this build has no MySQL support; rebuild with --features mysql, \
                     or export a JSON fixture and use --panel-fixture"
                );
                return Ok(2);
            }
        }
        (Some(_), Some(_)) => {
            eprintln!("--panel-fixture and --panel-mysql-dsn are mutually exclusive");
            return Ok(2);
        }
        (None, None) => {
            eprintln!("no panel source: pass --panel-fixture=PATH or --panel-mysql-dsn=DSN");
            eprintln!("{USAGE}");
            return Ok(2);
        }
    };

    if dry_run {
        let sink = SqlitePersistence::open_in_memory()
            .map_err(|e| format!("cannot open the rehearsal store: {e}"))?;
        sink.migrate()
            .map_err(|e| format!("rehearsal migrations failed: {e}"))?;
        let mut report = import(&*source, &sink, policy)?;
        report.rehearsed = true;
        print_report(&report);
        return Ok(0);
    }

    let Some(zim_db) = zim_db else {
        eprintln!("no target: pass --zim-db=PATH (or --dry-run to rehearse)");
        eprintln!("{USAGE}");
        return Ok(2);
    };
    let sink = SqlitePersistence::open(&zim_db)
        .map_err(|e| format!("cannot open {}: {e}", zim_db.display()))?;
    sink.migrate()
        .map_err(|e| format!("migrations failed on {}: {e}", zim_db.display()))?;
    let report = import(&*source, &sink, policy)?;
    print_report(&report);
    Ok(0)
}

/// One import over an already-migrated sink, with the source
/// failure surfaced distinctly.
fn import(
    source: &dyn PanelSource,
    sink: &dyn Persistence,
    policy: ImportPolicy,
) -> Result<ImportReport, String> {
    eprintln!("zim_engine migrate: reading the panel source...");
    let load = std::time::Instant::now();
    let result = import_panel(source, sink, &policy);
    match result {
        Ok(report) => {
            eprintln!(
                "zim_engine migrate: import finished in {:.1}s",
                load.elapsed().as_secs_f32()
            );
            Ok(report)
        }
        Err(crate::ImportError::Source(PanelSourceError::Read(reason))) => {
            Err(format!("cannot read the panel source: {reason}"))
        }
        Err(error) => Err(error.to_string()),
    }
}

fn print_report(report: &ImportReport) {
    println!("{report}");
}
