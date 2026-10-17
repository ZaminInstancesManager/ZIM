/**
 * The live-MySQL panel migration (M27's E2E — the roadmap's
 * "live MySQL panel migration E2E" line): the SAME fixture data,
 * seeded into a real MySQL panel database, imported through the
 * importer's live DSN path (`--panel-mysql-dsn`), and compared
 * against the fixture path's import — the mapping layer cannot
 * tell the two sources apart by design, and now neither can the
 * E2E.
 *
 * The panel schema here is generated FROM the canonical fixture
 * (`panel_fixture.json`) — the column inventory is exactly what
 * `migrate-pterodactyl/src/mysql.rs` SELECTs — so the seed and the
 * fixture can never drift apart silently.
 *
 * Tier honesty: needs a MySQL server (CI's Linux docker leg runs a
 * `mysql` service container and exports `ZIM_E2E_MYSQL_DSN`);
 * without the env the test skips with the reason printed, like
 * every other tier gate in the suite.
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { join } from "node:path";
import { mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { launchEngine, runMigration } from "../lib/engine.js";
import { makeClient } from "../lib/api.js";
import { repoRoot, run, tempDir } from "../lib/util.js";

type Value = string | number | boolean | null | Array<unknown> | Record<string, unknown>;
type Row = Record<string, Value>;

/** A seeded table's shape: the columns the importer SELECTs, their
 * SQL kind, and the fixture-key override where MySQL's column name
 * and the fixture's key differ (`uuidShort`). */
interface Column {
  name: string;
  kind: "int" | "bool" | "text" | "longtext" | "datetime" | "json";
  key?: string;
}

const TABLES: Array<{ table: string; columns: Column[] }> = [
  {
    table: "users",
    columns: [
      { name: "id", kind: "int" },
      { name: "external_id", kind: "text" },
      { name: "uuid", kind: "text" },
      { name: "username", kind: "text" },
      { name: "email", kind: "text" },
      { name: "password", kind: "text" },
      { name: "root_admin", kind: "bool" },
    ],
  },
  {
    table: "locations",
    columns: [
      { name: "id", kind: "int" },
      { name: "short", kind: "text" },
      { name: "long", kind: "text" },
    ],
  },
  {
    table: "nodes",
    columns: [
      { name: "id", kind: "int" },
      { name: "uuid", kind: "text" },
      { name: "name", kind: "text" },
      { name: "location_id", kind: "int" },
      { name: "fqdn", kind: "text" },
      { name: "scheme", kind: "text" },
      { name: "daemonListen", kind: "int" },
    ],
  },
  {
    // The importer SELECTs `name ... ORDER BY id` — the panel's
    // nests table carries the id even though the fixture's rows
    // only name the nest (the first CI seed taught this one too:
    // MySQL refuses to order by a column the DDL never declared).
    table: "nests",
    columns: [
      { name: "id", kind: "int" },
      { name: "name", kind: "text" },
    ],
  },
  {
    table: "eggs",
    columns: [
      { name: "id", kind: "int" },
      { name: "uuid", kind: "text" },
      { name: "name", kind: "text" },
      { name: "author", kind: "text" },
      { name: "description", kind: "text" },
      { name: "features", kind: "json" },
      { name: "docker_image", kind: "text" },
      { name: "docker_images", kind: "json" },
      { name: "config_files", kind: "longtext" },
      { name: "config_startup", kind: "longtext" },
      { name: "config_stop", kind: "text" },
      { name: "startup", kind: "longtext" },
      { name: "script_is_privileged", kind: "bool" },
      { name: "script_install", kind: "longtext" },
      { name: "script_entry", kind: "text" },
      { name: "script_container", kind: "text" },
      { name: "created_at", kind: "datetime" },
    ],
  },
  {
    table: "egg_variables",
    columns: [
      { name: "id", kind: "int" },
      { name: "egg_id", kind: "int" },
      { name: "name", kind: "text" },
      { name: "description", kind: "text" },
      { name: "env_variable", kind: "text" },
      { name: "default_value", kind: "text" },
      { name: "user_viewable", kind: "bool" },
      { name: "user_editable", kind: "bool" },
      { name: "rules", kind: "text" },
    ],
  },
  {
    table: "allocations",
    columns: [
      { name: "id", kind: "int" },
      { name: "node_id", kind: "int" },
      { name: "ip", kind: "text" },
      { name: "port", kind: "int" },
      { name: "server_id", kind: "int" },
      { name: "notes", kind: "text" },
    ],
  },
  {
    table: "servers",
    columns: [
      { name: "id", kind: "int" },
      { name: "external_id", kind: "text" },
      { name: "uuid", kind: "text" },
      { name: "uuidShort", kind: "text" },
      { name: "node_id", kind: "int" },
      { name: "name", kind: "text" },
      { name: "description", kind: "text" },
      { name: "status", kind: "text" },
      { name: "skip_scripts", kind: "bool" },
      { name: "owner_id", kind: "int" },
      { name: "memory", kind: "int" },
      { name: "swap", kind: "int" },
      { name: "disk", kind: "int" },
      { name: "io", kind: "int" },
      { name: "cpu", kind: "int" },
      { name: "threads", kind: "text" },
      { name: "oom_disabled", kind: "bool" },
      { name: "allocation_id", kind: "int" },
      { name: "egg_id", kind: "int" },
      { name: "startup", kind: "longtext" },
      { name: "image", kind: "text" },
      { name: "allocation_limit", kind: "int" },
      { name: "database_limit", kind: "int" },
      { name: "backup_limit", kind: "int" },
      { name: "installed_at", kind: "datetime" },
      { name: "created_at", kind: "datetime" },
      { name: "updated_at", kind: "datetime" },
    ],
  },
  {
    table: "server_variables",
    columns: [
      { name: "id", kind: "int" },
      { name: "server_id", kind: "int" },
      { name: "variable_id", kind: "int" },
      { name: "variable_value", kind: "text" },
    ],
  },
  {
    table: "subusers",
    columns: [
      { name: "id", kind: "int" },
      { name: "user_id", kind: "int" },
      { name: "server_id", kind: "int" },
      { name: "permissions", kind: "json" },
    ],
  },
  {
    table: "schedules",
    columns: [
      { name: "id", kind: "int" },
      { name: "server_id", kind: "int" },
      { name: "name", kind: "text" },
      { name: "cron_minute", kind: "text" },
      { name: "cron_hour", kind: "text" },
      { name: "cron_day_of_month", kind: "text" },
      { name: "cron_month", kind: "text" },
      { name: "cron_day_of_week", kind: "text" },
      { name: "is_active", kind: "bool" },
      { name: "only_when_online", kind: "bool" },
      { name: "last_run_at", kind: "datetime" },
      { name: "next_run_at", kind: "datetime" },
    ],
  },
  {
    table: "schedule_tasks",
    columns: [
      { name: "id", kind: "int" },
      { name: "schedule_id", kind: "int" },
      { name: "sequence_id", kind: "int" },
      { name: "action", kind: "text" },
      { name: "payload", kind: "text" },
      { name: "time_offset", kind: "int" },
      { name: "continue_on_failure", kind: "bool" },
    ],
  },
  {
    table: "backups",
    columns: [
      { name: "id", kind: "int" },
      { name: "server_id", kind: "int" },
      { name: "uuid", kind: "text" },
      { name: "is_successful", kind: "bool" },
      { name: "is_locked", kind: "bool" },
      { name: "name", kind: "text" },
      { name: "checksum", kind: "text" },
      { name: "bytes", kind: "int" },
      { name: "completed_at", kind: "datetime" },
      { name: "deleted_at", kind: "datetime" },
      { name: "created_at", kind: "datetime" },
    ],
  },
  {
    table: "activity_logs",
    columns: [
      { name: "id", kind: "int" },
      { name: "event", kind: "text" },
      { name: "ip", kind: "text" },
      { name: "description", kind: "text" },
      { name: "actor_type", kind: "text" },
      { name: "actor_id", kind: "int" },
      { name: "properties", kind: "json" },
      { name: "timestamp", kind: "datetime" },
    ],
  },
  {
    table: "activity_log_subjects",
    columns: [
      { name: "activity_log_id", kind: "int" },
      { name: "subject_type", kind: "text" },
      { name: "subject_id", kind: "int" },
    ],
  },
  {
    table: "server_transfers",
    columns: [
      { name: "id", kind: "int" },
      { name: "server_id", kind: "int" },
      { name: "old_node", kind: "int" },
      { name: "new_node", kind: "int" },
      { name: "successful", kind: "bool" },
      { name: "created_at", kind: "datetime" },
    ],
  },
];

/** The SQL kind for one column.
 *
 * Nullable across the board (ids included): the real panel's
 * nullability per column is the panel's business — the fixture
 * carries NULLs wherever the panel does (free allocations' NULL
 * server_id taught this the hard way), and the importer's
 * opt_-typed readers handle them all. */
function sqlType(kind: Column["kind"]): string {
  switch (kind) {
    case "int":
      return "INT NULL";
    case "bool":
      return "TINYINT(1) NULL";
    case "text":
      return "VARCHAR(191) NULL";
    case "longtext":
      return "TEXT NULL";
    case "datetime":
      return "DATETIME NULL";
    case "json":
      return "JSON NULL";
  }
}

/** Render one fixture value as a SQL literal. */
function sqlLiteral(value: Value, kind: Column["kind"]): string {
  if (value === null || value === undefined) {
    return "NULL";
  }
  switch (kind) {
    case "int": {
      assert.ok(typeof value === "number", `int column got ${typeof value}`);
      return String(Math.trunc(value));
    }
    case "bool": {
      const truthy = value === true || value === 1;
      return truthy ? "1" : "0";
    }
    case "json":
      return quote(JSON.stringify(value));
    case "text":
    case "longtext":
    case "datetime":
    default: {
      if (typeof value === "number") {
        return String(value);
      }
      if (typeof value === "boolean") {
        return value ? "1" : "0";
      }
      return quote(String(value));
    }
  }
}

/** A single-quoted SQL string literal. */
function quote(text: string): string {
  return `'${text.replaceAll("\\", "\\\\").replaceAll("'", "''")}'`;
}

/** Generate the full seed script from the fixture's rows. */
function seedSql(fixture: Record<string, Row[]>): string {
  const statements: string[] = [];
  for (const spec of TABLES) {
    statements.push(
      `CREATE TABLE \`${spec.table}\` (\n  ${spec.columns
        .map((column) => `\`${column.name}\` ${sqlType(column.kind)}`)
        .join(",\n  ")}\n) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;`,
    );
    const rows = fixture[spec.table] ?? [];
    for (const row of rows) {
      const columns = spec.columns.map((column) => `\`${column.name}\``).join(", ");
      const literals = spec.columns
        .map((column) => sqlLiteral(row[column.key ?? column.name] ?? null, column.kind))
        .join(", ");
      statements.push(`INSERT INTO \`${spec.table}\` (${columns}) VALUES (${literals});`);
    }
  }
  return `${statements.join("\n")}\n`;
}

/** The known counts the fixture carries (asserted in both paths). */
const FIXTURE_COUNTS = { users: 2, servers: 3, eggs: 2, allocations: 4, schedules: 2 };

/** Pull `"<n> <word>"`-style counts out of an importer report. */
function reportCount(report: string, word: string): number | null {
  const match = report.match(new RegExp(`(\\d+)\\s+${word}`, "i"));
  return match === null ? null : Number(match[1]);
}

test("migration (live MySQL): the same fixture data through a real panel database imports and boots identically", async (t) => {
  const dsn = process.env.ZIM_E2E_MYSQL_DSN;
  if (dsn === undefined || dsn === "") {
    t.skip("no ZIM_E2E_MYSQL_DSN — needs the MySQL service container (the CI Linux docker leg provides one)");
    return;
  }

  // ── The panel database: seeded from the canonical fixture.
  const fixturePath = join(
    repoRoot(),
    "zim",
    "engine",
    "crates",
    "migrate-pterodactyl",
    "tests",
    "fixtures",
    "panel_fixture.json",
  );
  const fixture = JSON.parse(
    await (await import("node:fs/promises")).readFile(fixturePath, "utf8"),
  ) as Record<string, Row[]>;

  const url = new URL(dsn);
  const database = url.pathname.replace(/^\//, "");
  assert.ok(database.length > 0, "the DSN must select a database");
  const cliArgs = [
    "-h",
    url.hostname,
    "-P",
    url.port || "3306",
    "-u",
    decodeURIComponent(url.username),
  ];
  const password = url.password ? decodeURIComponent(url.password) : "";
  const cliEnv: Record<string, string> = {};
  if (password !== "") {
    // MYSQL_PWD spares the CLI's argv (and the runner's logs) the
    // password; the DSN itself is already secret-bearing.
    cliEnv.MYSQL_PWD = password;
  }

  const createResult = await run("mysql", [...cliArgs, "-e", `CREATE DATABASE IF NOT EXISTS \`${database}\` CHARACTER SET utf8mb4;`], {
    timeoutMs: 30_000,
    env: cliEnv,
  });
  if (createResult.code !== 0) {
    t.skip(`the MySQL server behind the DSN did not answer (mysql exited ${createResult.code}: ${createResult.stderr.slice(0, 200)})`);
    return;
  }

  const scratch = await mkdtemp(join(tmpdir(), "zim-mysql-seed-"));
  const seedPath = join(scratch, "seed.sql");
  await writeFile(seedPath, seedSql(fixture), "utf8");
  // SOURCE (not stdin redirection): the run helper spawns without a
  // stdin pipe, so the SQL file is named on the client's own
  // command line — the first CI run taught this the quiet way (an
  // empty stdin seeded nothing and the importer answered "Table
  // 'panel.users' doesn't exist").
  const seed = await run(
    "mysql",
    [...cliArgs, database, "-e", `SOURCE ${seedPath.replaceAll("\\", "/")}`],
    { timeoutMs: 60_000, env: cliEnv },
  );
  assert.equal(
    seed.code,
    0,
    `seeding the panel failed:\n${seed.stderr.slice(0, 400)}\n${seed.stdout.slice(0, 200)}`,
  );

  try {
    // ── The real import through the live DSN.
    const mysqlDataDir = tempDir("migrated-mysql");
    const mysqlDb = join(mysqlDataDir, "engine.db");
    const report = await runMigration({ panelMysqlDsn: dsn, zimDb: mysqlDb });
    assert.match(report, /instances/i, "the report names the imported instances");

    // ── The fixture-path import (the equivalence oracle).
    const fixtureDataDir = tempDir("migrated-fixture");
    const fixtureDb = join(fixtureDataDir, "engine.db");
    const fixtureReport = await runMigration({ panelFixture: fixturePath, zimDb: fixtureDb });

    for (const [word, expected] of Object.entries(FIXTURE_COUNTS)) {
      const viaMysql = reportCount(report, word);
      const viaFixture = reportCount(fixtureReport, word);
      assert.notEqual(viaMysql, null, `the MySQL report mentions ${word}`);
      assert.notEqual(viaFixture, null, `the fixture report mentions ${word}`);
      assert.equal(
        viaMysql,
        viaFixture,
        `${word}: the live-MySQL path (${viaMysql}) and the fixture path (${viaFixture}) must agree (expected ${expected})`,
      );
      assert.equal(viaMysql, expected, `${word}: the fixture's known count`);
    }

    // ── The engine boots on the MySQL-migrated database.
    const engine = await launchEngine({ dataDir: mysqlDataDir });
    try {
      const anonymous = makeClient(engine);
      const health = await anonymous.tryGet("/api/v1/health");
      assert.equal(health.status, 200);
      assert.match(health.body, /version/);
    } finally {
      await engine.stop();
    }
  } finally {
    await run("mysql", [...cliArgs, "-e", `DROP DATABASE IF EXISTS \`${database}\`;`], {
      timeoutMs: 30_000,
      env: cliEnv,
    });
    await (await import("node:fs/promises")).rm(scratch, { recursive: true, force: true });
  }
});
