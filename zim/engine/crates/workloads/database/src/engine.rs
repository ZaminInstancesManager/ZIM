//! The managed-database engine table (M18).
//!
//! The four engines M18 ships — Postgres, MySQL, MariaDB, Redis —
//! differ in exactly the places a workload integration cares
//! about: which image, where the data directory lives inside the
//! container, which env entries seed the credentials, which
//! argv probes readiness, which command quiesces for a snapshot,
//! and which container posture (user, rootfs) the image's
//! entrypoint needs. [`DatabaseEngine`] is that table: one enum
//! whose variants each carry the engine's conventions as data.
//!
//! The per-engine argv builders take the *resolved*
//! [`EngineCredentials`] (spec defaults already applied), so the
//! same builders serve the spec validation, the parity tests, and
//! the per-workload handle.
//!
//! # The readiness probes (§M18: `pg_isready`, `mysqladmin ping`,
//! `redis-cli ping`)
//!
//! Every probe runs *inside* the workload's container (the
//! `ExecCapable` bridge), so the probe's `127.0.0.1` is the
//! container's own loopback — no cross-container networking, no
//! dependency on the published host port. A probe that cannot
//! connect is the engine's own "not ready yet" answer, which is
//! precisely what [`crate::probe`] classifies.
//!
//! # The quiesce commands (§M18: `CHECKPOINT`,
//! `FLUSH TABLES WITH READ LOCK`, `BGSAVE`)
//!
//! The snapshot path's consistency step — what each engine does
//! to make its on-disk state a restorable point-in-time — is
//! per-engine and lives here as data:
//!
//! - **Postgres** — `CHECKPOINT`: flushes the buffer cache to the
//!   data files and advances the checkpoint record in the WAL. The
//!   archive that follows is a crash-consistent image whose WAL
//!   replay completes the recovery on restore (Postgres's own
//!   durability design, exercised by the parity suite).
//! - **MySQL / MariaDB** — `FLUSH TABLES WITH READ LOCK` held
//!   across the archive *in the same client session* (the classic
//!   `SYSTEM <command>` snapshot trick: the mysql-family client
//!   runs a shell command while the session's global read lock
//!   blocks all writes). The archive is container-side tar.
//! - **Redis** — `BGSAVE` + a poll of `INFO persistence` until
//!   `rdb_bgsave_in_progress:0`: the `dump.rdb` the archive then
//!   captures is a complete, atomically-renamed point-in-time.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::spec::{DatabaseCredentials, SpecEndpoint};

/// The managed database engines (M18's three families; MySQL and
/// MariaDB share a protocol but stay separate rows — their images,
/// env vocabularies, and client binary names differ).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DatabaseEngine {
    /// PostgreSQL (`postgres:16-alpine`; `pg_isready`, `CHECKPOINT`).
    Postgres,
    /// MySQL 8.x (`mysql:8.4`; `mysqladmin ping`,
    /// `FLUSH TABLES WITH READ LOCK`).
    MySql,
    /// MariaDB (`mariadb:11`; `mariadb-admin ping`,
    /// `FLUSH TABLES WITH READ LOCK`).
    MariaDb,
    /// Redis (`redis:7-alpine`; `redis-cli ping`, `BGSAVE`).
    Redis,
}

impl DatabaseEngine {
    /// Every engine, in declaration order (the spec's
    /// documentation order and the parity suite's coverage list).
    pub const ALL: [DatabaseEngine; 4] = [
        DatabaseEngine::Postgres,
        DatabaseEngine::MySql,
        DatabaseEngine::MariaDb,
        DatabaseEngine::Redis,
    ];

    /// The engine's family label (MySQL and MariaDB share one —
    /// the roadmap's "MySQL/MariaDB" row).
    pub fn family(&self) -> &'static str {
        match self {
            DatabaseEngine::Postgres => "postgres",
            DatabaseEngine::MySql | DatabaseEngine::MariaDb => "mysql",
            DatabaseEngine::Redis => "redis",
        }
    }

    /// The engine's default image (the spec's `image` default).
    pub fn default_image(&self) -> &'static str {
        match self {
            DatabaseEngine::Postgres => "postgres:16-alpine",
            DatabaseEngine::MySql => "mysql:8.4",
            DatabaseEngine::MariaDb => "mariadb:11",
            DatabaseEngine::Redis => "redis:7-alpine",
        }
    }

    /// The engine's default logical endpoints (the
    /// read/write/replication vocabulary; `mysqlx` is MySQL's X
    /// protocol listener, the one engine with a real second live
    /// port).
    pub fn default_endpoints(&self) -> Vec<SpecEndpoint> {
        match self {
            DatabaseEngine::Postgres => vec![
                SpecEndpoint::new("primary", 5432),
                SpecEndpoint::new("replication", 5432),
            ],
            DatabaseEngine::MySql => vec![
                SpecEndpoint::new("primary", 3306),
                SpecEndpoint::new("mysqlx", 33060),
            ],
            DatabaseEngine::MariaDb => vec![
                SpecEndpoint::new("primary", 3306),
                SpecEndpoint::new("replication", 3306),
            ],
            DatabaseEngine::Redis => vec![
                SpecEndpoint::new("primary", 6379),
                SpecEndpoint::new("replication", 6379),
            ],
        }
    }

    /// The default credential subject (the superuser the engine's
    /// image bootstraps; Redis has none).
    pub fn default_username(&self) -> Option<&'static str> {
        match self {
            DatabaseEngine::Postgres => Some("postgres"),
            DatabaseEngine::MySql | DatabaseEngine::MariaDb => Some("root"),
            DatabaseEngine::Redis => None,
        }
    }

    /// Whether the engine requires a password (Redis's is optional).
    pub fn requires_password(&self) -> bool {
        !matches!(self, DatabaseEngine::Redis)
    }

    /// The default database name the engine's image creates (the
    /// `POSTGRES_DB` / `MYSQL_DATABASE` convention; Redis's is a
    /// logical number, not a create target).
    pub fn default_database(&self) -> Option<&'static str> {
        match self {
            DatabaseEngine::Postgres => Some("postgres"),
            DatabaseEngine::MySql | DatabaseEngine::MariaDb => Some("mysql"),
            DatabaseEngine::Redis => None,
        }
    }

    /// The container-side path the engine's data directory lives
    /// at — the path the runtime bind-mounts the workload's data
    /// directory onto (the per-engine equivalent of the
    /// game-server `/home/container` convention: the images'
    /// entrypoints hardcode these).
    pub fn default_container_data_dir(&self) -> &'static str {
        match self {
            DatabaseEngine::Postgres => "/var/lib/postgresql/data",
            DatabaseEngine::MySql | DatabaseEngine::MariaDb => "/var/lib/mysql",
            DatabaseEngine::Redis => "/data",
        }
    }

    /// The default server argv (the image's `CMD` equivalent —
    /// first token must be the binary the entrypoint gates on).
    ///
    /// MySQL/MariaDB relocate socket and pid file into the `/tmp`
    /// tmpfs (belt and braces for the writable-rootfs posture —
    /// see [`Self::default_readonly_rootfs`]). Postgres runs its
    /// stock argv: the entrypoint's own initialization client
    /// hard-codes the compiled-in socket directory
    /// (`docker_process_sql` explicitly blanks `PGHOST`/`PGHOSTADDR`),
    /// so the socket must stay where libpq's default says it is.
    pub fn default_command(&self) -> Vec<String> {
        match self {
            DatabaseEngine::Postgres => vec!["postgres".to_owned()],
            DatabaseEngine::MySql => vec![
                "mysqld".to_owned(),
                "--socket=/tmp/mysqld.sock".to_owned(),
                "--pid-file=/tmp/mysqld.pid".to_owned(),
            ],
            DatabaseEngine::MariaDb => vec![
                "mariadbd".to_owned(),
                "--socket=/tmp/mysqld.sock".to_owned(),
                "--pid-file=/tmp/mysqld.pid".to_owned(),
            ],
            DatabaseEngine::Redis => vec!["redis-server".to_owned()],
        }
    }

    /// The default container user: the database images' root
    /// entrypoints `chown` the bind-mounted data directory and
    /// then self-demote to the engine's own unprivileged user — a
    /// `988:988` start dies inside `initdb` before any service
    /// exists. The runtime's per-instance override carries this.
    pub fn default_container_user(&self) -> &'static str {
        "0:0"
    }

    /// The database engines' capability-drop list (the runtime's
    /// per-instance override carries it).
    ///
    /// The Wings posture drops `dac_override`/`fowner`/`fsetid`,
    /// which makes even root unable to manage these images'
    /// ownership-taken data directories: the entrypoints `chown`
    /// the bind-mounted data directory to their own service user
    /// (postgres/mysql/redis) at first boot, and from then on a
    /// `dac_override`-less root can neither read MySQL's
    /// `secure-file-priv` directory (its first-boot `find` dies, and
    /// `set -e` takes the container with it) nor write the
    /// snapshot store into the service-user-owned directory. The
    /// reduced list keeps the genuinely dangerous capabilities
    /// dropped — `mknod`, `net_raw`, `sys_chroot`, `setfcap`,
    /// `setpcap`, `audit_write`, `net_bind_service` — and restores
    /// the ownership-management trio the engines' entrypoints and
    /// the snapshot/restore paths need.
    pub fn default_cap_drop() -> &'static [&'static str] {
        &[
            "setpcap",
            "mknod",
            "audit_write",
            "net_raw",
            "net_bind_service",
            "sys_chroot",
            "setfcap",
        ]
    }

    /// The default rootfs posture.
    ///
    /// Redis keeps the engine's hardened read-only root filesystem
    /// (its only writable surface is the bind-mounted data
    /// directory). The three SQL engines need the writable
    /// posture: the MySQL family writes its runtime socket
    /// directory (`/run/mysqld`) during first-boot initialization,
    /// and Postgres's entrypoint parks its unix socket — and its
    /// own initialization client's connection — in
    /// `/var/run/postgresql`, which the entrypoint hard-codes
    /// (`docker_process_sql` blanks `PGHOST`/`PGHOSTADDR`, so no
    /// relocation can reach it).
    pub fn default_readonly_rootfs(&self) -> bool {
        matches!(self, DatabaseEngine::Redis)
    }

    /// The environment entries the image needs to bootstrap
    /// (credential-shaped; the spec's resolved credentials in).
    ///
    /// Redis carries no env entry — an optional password rides
    /// the server argv (`--requirepass`) instead, which the
    /// [`DatabaseSpec`] applies in its own env/argv resolution.
    pub fn bootstrap_env(&self, creds: &DatabaseCredentials) -> Vec<(String, String)> {
        match self {
            DatabaseEngine::Postgres => {
                let mut env = vec![
                    (
                        "POSTGRES_USER".to_owned(),
                        creds
                            .username
                            .clone()
                            .unwrap_or_else(|| "postgres".to_owned()),
                    ),
                    ("POSTGRES_PASSWORD".to_owned(), creds.password.clone()),
                ];
                if let Some(database) = &creds.database {
                    env.push(("POSTGRES_DB".to_owned(), database.clone()));
                }
                env
            }
            DatabaseEngine::MySql => {
                let mut env = vec![("MYSQL_ROOT_PASSWORD".to_owned(), creds.password.clone())];
                if let Some(database) = &creds.database {
                    env.push(("MYSQL_DATABASE".to_owned(), database.clone()));
                }
                if let (Some(user), Some(password)) = (&creds.username, &creds.app_password) {
                    if user != "root" {
                        env.push(("MYSQL_USER".to_owned(), user.clone()));
                        env.push(("MYSQL_PASSWORD".to_owned(), password.clone()));
                    }
                }
                env
            }
            DatabaseEngine::MariaDb => {
                let mut env = vec![("MARIADB_ROOT_PASSWORD".to_owned(), creds.password.clone())];
                if let Some(database) = &creds.database {
                    env.push(("MARIADB_DATABASE".to_owned(), database.clone()));
                }
                if let (Some(user), Some(password)) = (&creds.username, &creds.app_password) {
                    if user != "root" {
                        env.push(("MARIADB_USER".to_owned(), user.clone()));
                        env.push(("MARIADB_PASSWORD".to_owned(), password.clone()));
                    }
                }
                env
            }
            DatabaseEngine::Redis => Vec::new(),
        }
    }

    /// The readiness-probe argv (§M18's per-engine probe), for the
    /// spec's resolved credentials and the engine's primary
    /// container port.
    pub fn probe_argv(&self, creds: &DatabaseCredentials, primary_port: u16) -> Vec<String> {
        match self {
            DatabaseEngine::Postgres => vec![
                "pg_isready".to_owned(),
                "-h".to_owned(),
                "/var/run/postgresql".to_owned(),
                "-p".to_owned(),
                primary_port.to_string(),
                "-U".to_owned(),
                creds
                    .username
                    .clone()
                    .unwrap_or_else(|| "postgres".to_owned()),
            ],
            DatabaseEngine::MySql => {
                let mut argv = vec![
                    "mysqladmin".to_owned(),
                    "--protocol=tcp".to_owned(),
                    "-h".to_owned(),
                    "127.0.0.1".to_owned(),
                    "-P".to_owned(),
                    primary_port.to_string(),
                    "-u".to_owned(),
                    creds.username.clone().unwrap_or_else(|| "root".to_owned()),
                ];
                if !creds.password.is_empty() {
                    argv.push(format!("-p{}", creds.password));
                }
                argv.push("ping".to_owned());
                argv
            }
            DatabaseEngine::MariaDb => {
                let mut argv = vec![
                    "mariadb-admin".to_owned(),
                    "--protocol=tcp".to_owned(),
                    "-h".to_owned(),
                    "127.0.0.1".to_owned(),
                    "-P".to_owned(),
                    primary_port.to_string(),
                    "-u".to_owned(),
                    creds.username.clone().unwrap_or_else(|| "root".to_owned()),
                ];
                if !creds.password.is_empty() {
                    argv.push(format!("-p{}", creds.password));
                }
                argv.push("ping".to_owned());
                argv
            }
            DatabaseEngine::Redis => {
                let mut argv = vec![
                    "redis-cli".to_owned(),
                    "-h".to_owned(),
                    "127.0.0.1".to_owned(),
                    "-p".to_owned(),
                    primary_port.to_string(),
                ];
                if !creds.password.is_empty() {
                    argv.push("-a".to_owned());
                    argv.push(creds.password.clone());
                }
                argv.push("ping".to_owned());
                argv
            }
        }
    }

    /// A one-shot SQL/eval argv for the engine (the parity
    /// suite's query client and the integration's marker queries).
    ///
    /// The SQL string is passed as the last argument — the caller
    /// composes the statement (see [`DatabaseWorkload`][crate::DatabaseWorkload]
    /// for the async twin).
    pub fn eval_argv(&self, creds: &DatabaseCredentials, statement: &str) -> Vec<String> {
        match self {
            DatabaseEngine::Postgres => {
                let mut argv = vec![
                    "psql".to_owned(),
                    "-h".to_owned(),
                    "/var/run/postgresql".to_owned(),
                    "-U".to_owned(),
                    creds
                        .username
                        .clone()
                        .unwrap_or_else(|| "postgres".to_owned()),
                    "-X".to_owned(),
                    "-v".to_owned(),
                    "ON_ERROR_STOP=1".to_owned(),
                ];
                if let Some(database) = &creds.database {
                    argv.push("-d".to_owned());
                    argv.push(database.clone());
                }
                argv.push("-c".to_owned());
                argv.push(statement.to_owned());
                argv
            }
            DatabaseEngine::MySql | DatabaseEngine::MariaDb => {
                let binary = if matches!(self, DatabaseEngine::MySql) {
                    "mysql"
                } else {
                    "mariadb"
                };
                let mut argv = vec![
                    binary.to_owned(),
                    "--protocol=tcp".to_owned(),
                    "-h".to_owned(),
                    "127.0.0.1".to_owned(),
                    "-u".to_owned(),
                    creds.username.clone().unwrap_or_else(|| "root".to_owned()),
                ];
                if !creds.password.is_empty() {
                    argv.push(format!("-p{}", creds.password));
                }
                argv.push("-e".to_owned());
                argv.push(statement.to_owned());
                argv
            }
            DatabaseEngine::Redis => {
                let mut argv = vec![
                    "redis-cli".to_owned(),
                    "-h".to_owned(),
                    "127.0.0.1".to_owned(),
                ];
                if !creds.password.is_empty() {
                    argv.push("-a".to_owned());
                    argv.push(creds.password.clone());
                }
                // redis-cli's native argv shape: the command and
                // its arguments are separate words (the SQL
                // engines take one statement string; redis takes
                // words).
                argv.extend(statement.split_whitespace().map(str::to_owned));
                argv
            }
        }
    }

    /// The quiesce argv (§M18's per-engine snapshot consistency
    /// step) for the host-archive engines — Postgres's `CHECKPOINT`
    /// and Redis's `BGSAVE`. The MySQL/MariaDB family quiesces
    /// with the lock-held archive command instead (see
    /// [`Self::quiesce_archive_argv`]).
    pub fn quiesce_argv(&self, creds: &DatabaseCredentials) -> Option<Vec<String>> {
        match self {
            DatabaseEngine::Postgres => Some(self.eval_argv(creds, "CHECKPOINT")),
            DatabaseEngine::Redis => {
                let mut argv = vec![
                    "redis-cli".to_owned(),
                    "-h".to_owned(),
                    "127.0.0.1".to_owned(),
                ];
                if !creds.password.is_empty() {
                    argv.push("-a".to_owned());
                    argv.push(creds.password.clone());
                }
                argv.push("BGSAVE".to_owned());
                Some(argv)
            }
            DatabaseEngine::MySql | DatabaseEngine::MariaDb => None,
        }
    }

    /// The MySQL-family's lock-held snapshot command: the client
    /// session takes the global read lock, runs the archive
    /// command as its own child (`SYSTEM`), then releases —
    /// one exec round-trip, writes blocked for the archive's
    /// whole duration.
    ///
    /// The shell command deliberately avoids `;` (the client's
    /// `-e` parser splits on it) and excludes the snapshot
    /// directory from the archive so the archive never contains
    /// itself.
    pub fn quiesce_archive_argv(
        &self,
        creds: &DatabaseCredentials,
        data_dir: &str,
        archive_path: &str,
    ) -> Option<Vec<String>> {
        if !matches!(self, DatabaseEngine::MySql | DatabaseEngine::MariaDb) {
            return None;
        }
        let binary = if matches!(self, DatabaseEngine::MySql) {
            "mysql"
        } else {
            "mariadb"
        };
        let shell = format!(
            "mkdir -p {data_dir}/snapshots && tar -czf {data_dir}/{archive_path} \
             --exclude=snapshots -C {data_dir} ."
        );
        let statement = format!("FLUSH TABLES WITH READ LOCK; SYSTEM {shell}; UNLOCK TABLES;");
        let mut argv = vec![
            binary.to_owned(),
            "--protocol=tcp".to_owned(),
            "-h".to_owned(),
            "127.0.0.1".to_owned(),
            "-u".to_owned(),
            creds.username.clone().unwrap_or_else(|| "root".to_owned()),
        ];
        if !creds.password.is_empty() {
            argv.push(format!("-p{}", creds.password));
        }
        argv.push("-e".to_owned());
        argv.push(statement);
        Some(argv)
    }

    /// The point-in-time marker statement (the migration
    /// checkpoint's payload): Postgres records the current WAL
    /// LSN; Redis its `LASTSAVE` epoch; the MySQL family its
    /// server boot time (`NOW()`-shaped, a session-stable marker
    /// without requiring the binlog).
    pub fn marker_statement(&self) -> &'static str {
        match self {
            DatabaseEngine::Postgres => "SELECT pg_current_wal_lsn()",
            DatabaseEngine::MySql | DatabaseEngine::MariaDb => {
                "SELECT CONCAT('server-time=', NOW(6))"
            }
            DatabaseEngine::Redis => "LASTSAVE",
        }
    }

    /// The snapshot store's name inside the engine's data
    /// directory (the container-side archive target; every archive
    /// excludes it so an archive never contains itself).
    pub const SNAPSHOT_DIR: &str = "snapshots";

    /// Whether the engine's running-state snapshot holds
    /// `FLUSH TABLES WITH READ LOCK` across the archive (the MySQL
    /// family's online form; the other engines quiesce with their
    /// own command first).
    pub fn uses_lock_held_snapshot(&self) -> bool {
        matches!(self, DatabaseEngine::MySql | DatabaseEngine::MariaDb)
    }

    /// The container-side archive shell line (the `sh -c` body that
    /// produces one snapshot archive inside the engine's data
    /// directory).
    ///
    /// The line is `&&`-chained with no `;` (the MySQL family embeds
    /// it inside a client `SYSTEM` command, and the client's `-e`
    /// parser splits on semicolons), `tar`'s warnings are swallowed
    /// (`|| true` — a flushing InnoDB may legitimately change files
    /// mid-archive), and the trailing `stat` is the verification:
    /// it only runs when the chain got there, and its output (the
    /// archive's byte size) is the snapshot's success contract.
    ///
    /// The archive runs *container-side on purpose*: the engines'
    /// entrypoints take ownership of the data directory (Postgres
    /// `chown`s + `chmod 700`s it, the MySQL family `chown -R`s it),
    /// so the host-side actor — non-root in CI, the engine user in
    /// production — cannot rely on reading through it. The
    /// container-side `tar` (as the root entrypoint user) reads any
    /// ownership, and the daemon's own file-transfer surface (the
    /// `docker cp` API) is how the archive ships between nodes.
    pub fn archive_shell(&self, data_dir: &str, archive_path: &str) -> String {
        format!(
            "mkdir -p {data_dir}/{} && (tar -czf {data_dir}/{archive_path} \
             --exclude={} -C {data_dir} . || true) && stat -c %s {data_dir}/{archive_path}",
            Self::SNAPSHOT_DIR,
            Self::SNAPSHOT_DIR,
        )
    }

    /// The exec argv for one running-state snapshot (per engine: the
    /// MySQL family folds the archive into the lock-held client
    /// session; the others run the shell line directly after their
    /// own quiesce command).
    pub fn snapshot_exec_argv(
        &self,
        creds: &DatabaseCredentials,
        data_dir: &str,
        archive_path: &str,
    ) -> Vec<String> {
        let shell = self.archive_shell(data_dir, archive_path);
        if self.uses_lock_held_snapshot() {
            let binary = if matches!(self, DatabaseEngine::MySql) {
                "mysql"
            } else {
                "mariadb"
            };
            let statement = format!("FLUSH TABLES WITH READ LOCK; SYSTEM {shell}; UNLOCK TABLES;");
            let mut argv = vec![
                binary.to_owned(),
                "--protocol=tcp".to_owned(),
                "-h".to_owned(),
                "127.0.0.1".to_owned(),
                "-u".to_owned(),
                creds.username.clone().unwrap_or_else(|| "root".to_owned()),
            ];
            if !creds.password.is_empty() {
                argv.push(format!("-p{}", creds.password));
            }
            argv.push("-e".to_owned());
            argv.push(statement);
            argv
        } else {
            vec!["/bin/sh".to_owned(), "-c".to_owned(), shell]
        }
    }

    /// The restore shell script (the one-shot restore container's
    /// body): clear the data directory except the snapshot store,
    /// then extract the snapshot archive into place.
    ///
    /// The clear uses the `ls | grep | xargs` pipeline — the most
    /// portable form across the images' shells (busybox ash on the
    /// alpine images, bash on the debian-family ones) — and `xargs
    /// -r` keeps an empty directory a no-op.
    pub fn restore_script(&self, data_dir: &str, archive_path: &str) -> String {
        format!(
            "cd {data_dir} && ls -A | grep -v '^{}$' | xargs -r rm -rf \
             && tar -xzf {data_dir}/{archive_path} -C {data_dir}",
            Self::SNAPSHOT_DIR,
        )
    }

    /// The one-shot restore container's command (the entrypoints
    /// pass non-server commands straight through to `exec`, as
    /// root, which is exactly what a data-directory rewrite needs).
    pub fn restore_exec_argv(&self, data_dir: &str, archive_path: &str) -> Vec<String> {
        vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            self.restore_script(data_dir, archive_path),
        ]
    }

    /// The migration-marker argv (the point-in-time statement in
    /// tuple-only form, so the handle's marker extraction can use
    /// the output verbatim).
    pub fn marker_argv(&self, creds: &DatabaseCredentials) -> Vec<String> {
        match self {
            DatabaseEngine::Postgres => {
                let mut argv = vec![
                    "psql".to_owned(),
                    "-h".to_owned(),
                    "/var/run/postgresql".to_owned(),
                    "-U".to_owned(),
                    creds
                        .username
                        .clone()
                        .unwrap_or_else(|| "postgres".to_owned()),
                    "-X".to_owned(),
                    "-At".to_owned(),
                    "-v".to_owned(),
                    "ON_ERROR_STOP=1".to_owned(),
                    "-c".to_owned(),
                ];
                argv.push(self.marker_statement().to_owned());
                argv
            }
            DatabaseEngine::MySql | DatabaseEngine::MariaDb => {
                let binary = if matches!(self, DatabaseEngine::MySql) {
                    "mysql"
                } else {
                    "mariadb"
                };
                let mut argv = vec![
                    binary.to_owned(),
                    "--protocol=tcp".to_owned(),
                    "-h".to_owned(),
                    "127.0.0.1".to_owned(),
                    "-u".to_owned(),
                    creds.username.clone().unwrap_or_else(|| "root".to_owned()),
                    "-N".to_owned(),
                    "-B".to_owned(),
                ];
                if !creds.password.is_empty() {
                    argv.push(format!("-p{}", creds.password));
                }
                argv.push("-e".to_owned());
                argv.push(self.marker_statement().to_owned());
                argv
            }
            DatabaseEngine::Redis => self.eval_argv(creds, self.marker_statement()),
        }
    }
}

impl fmt::Display for DatabaseEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            DatabaseEngine::Postgres => "postgres",
            DatabaseEngine::MySql => "mysql",
            DatabaseEngine::MariaDb => "mariadb",
            DatabaseEngine::Redis => "redis",
        };
        f.write_str(name)
    }
}

impl std::str::FromStr for DatabaseEngine {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "postgres" | "postgresql" => Ok(DatabaseEngine::Postgres),
            "mysql" => Ok(DatabaseEngine::MySql),
            "mariadb" => Ok(DatabaseEngine::MariaDb),
            "redis" => Ok(DatabaseEngine::Redis),
            other => Err(format!(
                "{other:?} is not a managed database engine \
                 (expected one of postgres, mysql, mariadb, redis)"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn postgres_creds() -> DatabaseCredentials {
        DatabaseCredentials {
            username: Some("postgres".to_owned()),
            password: "sekret".to_owned(),
            database: Some("app".to_owned()),
            app_password: None,
        }
    }

    #[test]
    fn the_engine_table_is_total_over_its_families() {
        assert_eq!(DatabaseEngine::Postgres.family(), "postgres");
        assert_eq!(DatabaseEngine::MySql.family(), "mysql");
        assert_eq!(DatabaseEngine::MariaDb.family(), "mysql");
        assert_eq!(DatabaseEngine::Redis.family(), "redis");
        assert_eq!(DatabaseEngine::ALL.len(), 4);
    }

    #[test]
    fn the_parsing_round_trips_both_ways() {
        use std::str::FromStr;
        assert_eq!(
            DatabaseEngine::from_str("postgres").unwrap(),
            DatabaseEngine::Postgres
        );
        assert_eq!(
            DatabaseEngine::from_str("PostgreSQL").unwrap(),
            DatabaseEngine::Postgres
        );
        assert_eq!(
            DatabaseEngine::from_str("mysql").unwrap(),
            DatabaseEngine::MySql
        );
        assert_eq!(
            DatabaseEngine::from_str("MariaDB").unwrap(),
            DatabaseEngine::MariaDb
        );
        assert_eq!(
            DatabaseEngine::from_str("redis").unwrap(),
            DatabaseEngine::Redis
        );
        assert!(DatabaseEngine::from_str("mongodb").is_err());
        for engine in DatabaseEngine::ALL {
            assert_eq!(
                DatabaseEngine::from_str(&engine.to_string()).unwrap(),
                engine
            );
        }
    }

    #[test]
    fn the_probe_argv_shapes_match_the_engines() {
        let creds = postgres_creds();
        let argv = DatabaseEngine::Postgres.probe_argv(&creds, 5432);
        assert_eq!(argv[0], "pg_isready");
        // The default socket directory (the entrypoint's own client
        // hard-codes it; the writable-rootfs posture keeps it real).
        assert!(argv.contains(&"/var/run/postgresql".to_owned()));
        assert!(argv.contains(&"5432".to_owned()));

        let mysql_creds = DatabaseCredentials {
            username: Some("root".to_owned()),
            password: "pw".to_owned(),
            database: None,
            app_password: None,
        };
        let argv = DatabaseEngine::MySql.probe_argv(&mysql_creds, 3306);
        assert_eq!(argv[0], "mysqladmin");
        assert!(argv.contains(&"--protocol=tcp".to_owned()));
        assert!(argv.iter().any(|a| a == "-ppw"));
        assert_eq!(argv.last().unwrap(), "ping");

        let argv = DatabaseEngine::MariaDb.probe_argv(&mysql_creds, 3306);
        assert_eq!(argv[0], "mariadb-admin");

        let redis_creds = DatabaseCredentials::default();
        let argv = DatabaseEngine::Redis.probe_argv(&redis_creds, 6379);
        assert_eq!(argv[0], "redis-cli");
        assert_eq!(argv.last().unwrap(), "ping");
        // A password rides the argv.
        let locked = DatabaseCredentials {
            password: "hunter2".to_owned(),
            ..DatabaseCredentials::default()
        };
        let argv = DatabaseEngine::Redis.probe_argv(&locked, 6379);
        assert!(argv.contains(&"hunter2".to_owned()));
    }

    #[test]
    fn the_mysql_family_snapshots_with_the_lock_held_archive() {
        let creds = DatabaseCredentials {
            username: Some("root".to_owned()),
            password: "pw".to_owned(),
            database: None,
            app_password: None,
        };
        let argv = DatabaseEngine::MariaDb.snapshot_exec_argv(
            &creds,
            "/var/lib/mysql",
            "snapshots/pre-migrate.tar.gz",
        );
        let statement = argv.last().unwrap();
        assert!(statement.starts_with("FLUSH TABLES WITH READ LOCK"));
        assert!(statement.contains("SYSTEM "));
        assert!(statement.ends_with("UNLOCK TABLES;"));
        // The shell line embedded in the SYSTEM command has no
        // semicolons (the client's -e parser would split them into
        // separate statements).
        let shell = statement
            .split("SYSTEM ")
            .nth(1)
            .unwrap()
            .split("; UNLOCK")
            .next()
            .unwrap();
        assert!(!shell.contains(';'), "shell command: {shell:?}");
        // The archive excludes the store and verifies with stat.
        assert!(shell.contains("--exclude=snapshots"));
        assert!(shell.contains("tar -czf /var/lib/mysql/snapshots/pre-migrate.tar.gz"));
        assert!(shell.contains("stat -c %s"));

        // The separate-quiesce engines run the shell line directly.
        let argv = DatabaseEngine::Postgres.snapshot_exec_argv(
            &postgres_creds(),
            "/pgdata",
            "snapshots/x.tar.gz",
        );
        assert_eq!(argv[0], "/bin/sh");
        assert_eq!(argv[1], "-c");
        assert!(argv[2].starts_with("mkdir -p /pgdata/snapshots"));
        assert!(argv[2].contains("|| true) && stat -c %s"));
    }

    #[test]
    fn the_restore_script_clears_and_extracts() {
        for engine in DatabaseEngine::ALL {
            let script = engine.restore_script("/data", "snapshots/pre.tar.gz");
            assert!(script.starts_with("cd /data && ls -A"), "{engine}");
            assert!(script.contains("grep -v '^snapshots$'"), "{engine}");
            assert!(script.contains("xargs -r rm -rf"), "{engine}");
            assert!(
                script.contains("tar -xzf /data/snapshots/pre.tar.gz -C /data"),
                "{engine}"
            );
            let argv = engine.restore_exec_argv("/data", "snapshots/pre.tar.gz");
            assert_eq!(argv[0], "/bin/sh");
            assert_eq!(argv[1], "-c");
        }
    }

    #[test]
    fn the_container_postures_match_the_image_requirements() {
        // Every engine's entrypoint needs the root start.
        for engine in DatabaseEngine::ALL {
            assert_eq!(engine.default_container_user(), "0:0");
        }
        // Redis keeps the read-only rootfs (its only writable
        // surface is the bind-mounted data directory).
        assert!(DatabaseEngine::Redis.default_readonly_rootfs());
        // The three SQL engines need the writable posture: the
        // MySQL family's /run/mysqld, Postgres's
        // hard-coded /var/run/postgresql socket.
        assert!(!DatabaseEngine::Postgres.default_readonly_rootfs());
        assert!(!DatabaseEngine::MySql.default_readonly_rootfs());
        assert!(!DatabaseEngine::MariaDb.default_readonly_rootfs());
        // And the engine table carries the reduced capability
        // drop list (the ownership trio restored).
        {
            let caps = DatabaseEngine::default_cap_drop();
            assert!(!caps.contains(&"dac_override"));
            assert!(!caps.contains(&"fowner"));
            assert!(!caps.contains(&"fsetid"));
            assert!(caps.contains(&"mknod"));
            assert!(caps.contains(&"sys_chroot"));
            assert_eq!(caps.len(), 7);
        }
    }

    #[test]
    fn the_default_command_gates_on_the_entrypoint_binary() {
        // The images' entrypoints branch on $1: the first argv
        // token must be the server binary they gate on.
        assert_eq!(
            DatabaseEngine::Postgres.default_command(),
            vec!["postgres".to_owned()]
        );
        assert_eq!(DatabaseEngine::MySql.default_command()[0], "mysqld");
        assert_eq!(DatabaseEngine::MariaDb.default_command()[0], "mariadbd");
        assert_eq!(DatabaseEngine::Redis.default_command()[0], "redis-server");
        // The MySQL family's socket relocations are in place
        // (Postgres keeps its stock argv — the entrypoint's own
        // client hard-codes the default socket directory).
        assert!(DatabaseEngine::MariaDb
            .default_command()
            .contains(&"--socket=/tmp/mysqld.sock".to_owned()));
    }

    #[test]
    fn the_bootstrap_env_shapes_match_the_images() {
        let env = DatabaseEngine::Postgres.bootstrap_env(&postgres_creds());
        assert!(env.contains(&("POSTGRES_USER".to_owned(), "postgres".to_owned())));
        assert!(env.contains(&("POSTGRES_PASSWORD".to_owned(), "sekret".to_owned())));
        assert!(env.contains(&("POSTGRES_DB".to_owned(), "app".to_owned())));

        let mysql_creds = DatabaseCredentials {
            username: Some("root".to_owned()),
            password: "pw".to_owned(),
            database: Some("shop".to_owned()),
            app_password: None,
        };
        let env = DatabaseEngine::MySql.bootstrap_env(&mysql_creds);
        assert!(env.contains(&("MYSQL_ROOT_PASSWORD".to_owned(), "pw".to_owned())));
        assert!(env.contains(&("MYSQL_DATABASE".to_owned(), "shop".to_owned())));
        let env = DatabaseEngine::MariaDb.bootstrap_env(&mysql_creds);
        assert!(env.contains(&("MARIADB_ROOT_PASSWORD".to_owned(), "pw".to_owned())));
        // Redis has no env bootstrap.
        assert!(DatabaseEngine::Redis
            .bootstrap_env(&DatabaseCredentials::default())
            .is_empty());
    }

    #[test]
    fn the_marker_statements_are_engine_shaped() {
        assert_eq!(
            DatabaseEngine::Postgres.marker_statement(),
            "SELECT pg_current_wal_lsn()"
        );
        assert_eq!(DatabaseEngine::Redis.marker_statement(), "LASTSAVE");
        assert!(DatabaseEngine::MySql.marker_statement().contains("NOW(6)"));
    }

    #[test]
    fn the_marker_argv_is_tuple_only() {
        let creds = postgres_creds();
        let argv = DatabaseEngine::Postgres.marker_argv(&creds);
        assert!(argv.contains(&"-At".to_owned()));
        assert_eq!(argv.last().unwrap(), "SELECT pg_current_wal_lsn()");

        let mysql_creds = DatabaseCredentials {
            username: Some("root".to_owned()),
            password: "pw".to_owned(),
            database: None,
            app_password: None,
        };
        let argv = DatabaseEngine::MariaDb.marker_argv(&mysql_creds);
        assert!(argv.contains(&"-N".to_owned()));
        assert!(argv.contains(&"-B".to_owned()));

        let argv = DatabaseEngine::Redis.marker_argv(&DatabaseCredentials::default());
        assert_eq!(argv.last().unwrap(), "LASTSAVE");
    }

    #[test]
    fn the_snapshot_modes_split_by_family() {
        assert!(!DatabaseEngine::Postgres.uses_lock_held_snapshot());
        assert!(!DatabaseEngine::Redis.uses_lock_held_snapshot());
        assert!(DatabaseEngine::MySql.uses_lock_held_snapshot());
        assert!(DatabaseEngine::MariaDb.uses_lock_held_snapshot());
    }
}
