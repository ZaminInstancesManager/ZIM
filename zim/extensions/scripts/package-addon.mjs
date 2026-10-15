#!/usr/bin/env node
/**
 * package-addon.mjs — assemble, validate, and package ZIM addons.
 *
 * Usage:
 *   node scripts/package-addon.mjs validate <addon-dir>…
 *     Validate each addon dir's zim.addon.json against the §4.1
 *     shape this repo's engine enforces (the checks the Rust
 *     manifest validator performs; a fast author-time gate — the
 *     engine re-validates with full authority at install).
 *
 *   node scripts/package-addon.mjs build <addon-dir> <out.zimpkg>
 *     Vendor the SDK (zim/sdk/addon/dist/zim.js → assets/zim.js)
 *     and zip the whitelisted payload (manifest + pages/ + assets/)
 *     into an installable .zimpkg (POST /api/v1/extensions body).
 *
 * The whitelist mirrors the engine's build.rs embed list: manifest,
 * pages/, assets/ — sources and tooling never ship.
 */

import { createRequire } from "node:module";
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, statSync, writeFileSync, copyFileSync, rmSync, readdirSync } from "node:fs";
import { basename, join, resolve } from "node:path";
import { tmpdir } from "node:os";

const require = createRequire(import.meta.url);

/** The closed permission vocabulary (PermissionGrant::as_str). */
const PERMISSION_VOCABULARY = new Set([
  "websocket.connect",
  "control.console", "control.start", "control.stop", "control.restart",
  "file.read", "file.read_content", "file.write", "file.create", "file.delete", "file.archive",
  "backup.read", "backup.create", "backup.update", "backup.delete", "backup.restore", "backup.download",
  "allocation.read", "allocation.create", "allocation.update", "allocation.delete",
  "user.read", "user.create", "user.update", "user.delete",
  "schedule.read", "schedule.create", "schedule.update", "schedule.delete",
  "database.read", "database.create", "database.update", "database.delete", "database.view_password",
  "settings.read", "settings.update",
  "install", "transfer", "admin.instance",
  "workload.read", "workload.create", "workload.update", "workload.delete",
  "workload.migrate", "workload.snapshot", "workload.deploy",
  "network.expose", "network.unexpose",
]);

/** The closed event vocabulary (the streaming hub's set). */
const EVENT_VOCABULARY = new Set([
  "instance.state_change",
  "instance.resource_usage",
  "instance.console_output",
  "instance.install_progress",
  "job.completed",
  "job.progress",
]);

const RESERVED_ACCELERATORS = new Set([
  "ctrl+t", "ctrl+n", "ctrl+w", "ctrl+shift+t", "ctrl+shift+n", "ctrl+shift+w",
]);

const VALID_KEY = /^[A-Za-z0-9]$|^[Ff][1-9]$|^[Ff]1[0-2]$/;
const VALID_MODIFIER = new Set(["ctrl", "control", "cmd", "command", "meta", "super", "alt", "option", "shift"]);

/** Fail with a located message. */
function fail(where, message) {
  process.stderr.write(`✗ ${where}: ${message}\n`);
  process.exitCode = 1;
}

/** One addon's manifest validation (§4.1 author-time gate). */
function validateManifest(addonDir) {
  const id = basename(addonDir);
  const manifestPath = join(addonDir, "zim.addon.json");
  if (!existsSync(manifestPath)) {
    return fail(id, "no zim.addon.json at the package root");
  }
  let manifest;
  try {
    manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
  } catch (error) {
    return fail(id, `zim.addon.json is not valid JSON: ${error.message}`);
  }

  const problems = [];
  const where = (field) => `${id} → ${field}`;

  if (!/^[A-Za-z0-9.-]+\.[A-Za-z0-9.-]+$/.test(manifest.id ?? "")) {
    problems.push(where("id") + " must be a reverse-DNS identifier");
  }
  // Convention: an addon directory is named by its full id
  // (org.zaminmc.extension.audit-log) or by the id's trailing
  // segment (samples/hello-zim). Anything else invites confusion.
  if (manifest.id !== id && !manifest.id?.endsWith("." + id) && !manifest.id?.endsWith("-" + id)) {
    problems.push(where("id") + ` (${JSON.stringify(manifest.id)}) does not match the directory name (${id})`);
  }
  if (manifest.manifest_version !== 1) {
    problems.push(where("manifest_version") + " must be 1");
  }
  if (!/^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$/.test(manifest.version ?? "")) {
    problems.push(where("version") + " must be semver");
  }
  if (typeof manifest.name !== "string" || manifest.name.trim() === "") {
    problems.push(where("name") + " must be a non-empty string");
  }
  if (manifest.api_version !== "v1") {
    problems.push(where("api_version") + ' must be "v1"');
  }
  const engines = manifest.engines ?? {};
  if (typeof engines.zim !== "string" || !/^>=?\s?\d/.test(engines.zim)) {
    problems.push(where("engines.zim") + " must be a semver range string (e.g. \">=0.0.1\")");
  }
  if (!["required", "optional", "none"].includes(manifest.instance_scope)) {
    problems.push(where("instance_scope") + ' must be one of "required", "optional", "none"');
  }

  const permissions = manifest.permissions ?? [];
  if (!Array.isArray(permissions) || new Set(permissions).size !== permissions.length) {
    problems.push(where("permissions") + " must be a duplicate-free array");
  } else {
    for (const permission of permissions) {
      if (!PERMISSION_VOCABULARY.has(permission)) {
        problems.push(where("permissions") + `: ${JSON.stringify(permission)} is outside the engine's vocabulary`);
      }
    }
  }

  const surfaces = manifest.page_surfaces ?? [];
  const surfaceSet = new Set(surfaces);
  for (const surface of surfaces) {
    if (!/^zim:\/\/[A-Za-z0-9{_}/-]+$/.test(surface)) {
      problems.push(where("page_surfaces") + `: ${JSON.stringify(surface)} is not a well-formed zim:// surface`);
    }
    if (surface.startsWith("zim://ext/")) {
      problems.push(where("page_surfaces") + `: ${JSON.stringify(surface)} collides with the engine's reserved addon-asset namespace (zim://ext/)`);
    }
  }

  const entrypoints = manifest.entrypoints ?? {};
  for (const [surface, path] of Object.entries(entrypoints)) {
    if (!/^pages\/[A-Za-z0-9._/-]+\.html?$/.test(path)) {
      problems.push(where("entrypoints") + `: ${JSON.stringify(surface)} → ${JSON.stringify(path)} must be a pages/…html path`);
    }
    if (!existsSync(join(addonDir, path))) {
      problems.push(where("entrypoints") + `: ${JSON.stringify(path)} does not exist in the package`);
    }
  }
  for (const surface of surfaces) {
    const key = surface.replace(/^zim:\/\//, "").split("/").filter((s) => !s.startsWith("{")).pop();
    if (!entrypoints[key]) {
      problems.push(where("page_surfaces") + `: ${JSON.stringify(surface)} routes to entrypoint ${JSON.stringify(key)}, which is not declared`);
    }
  }

  for (const item of manifest.sidebar_items ?? []) {
    if (typeof item.label !== "string" || item.label.trim() === "") {
      problems.push(where("sidebar_items") + ": every item needs a non-empty label");
    }
    if (!surfaceSet.has(item.page)) {
      problems.push(where("sidebar_items") + `: page ${JSON.stringify(item.page)} is not a declared surface`);
    }
  }

  for (const item of manifest.context_menu_items ?? []) {
    if (typeof item.label !== "string" || item.label.trim() === "") {
      problems.push(where("context_menu_items") + ": every item needs a non-empty label");
    }
    if (!surfaceSet.has((item.page ?? "").split("?")[0])) {
      problems.push(where("context_menu_items") + `: page ${JSON.stringify(item.page)} is not a declared surface (query strings are fine)`);
    }
  }

  const commandIds = new Set();
  for (const command of manifest.commands ?? []) {
    if (!/^[A-Za-z0-9_-]+$/.test(command.id ?? "")) {
      problems.push(where("commands") + ": ids must be slugs");
    }
    if (commandIds.has(command.id)) {
      problems.push(where("commands") + `: ${JSON.stringify(command.id)} is registered twice`);
    }
    commandIds.add(command.id);
    if (typeof command.accelerator === "string") {
      const parts = command.accelerator.split("+").map((p) => p.trim());
      if (parts.length < 2 || parts.length > 4) {
        problems.push(where("commands") + `: accelerator ${JSON.stringify(command.accelerator)} must be Modifier+…+Key`);
      } else {
        const key = parts[parts.length - 1];
        if (!VALID_KEY.test(key)) {
          problems.push(where("commands") + `: accelerator key ${JSON.stringify(key)} must be a letter, digit, or F1-F12`);
        }
        for (const modifier of parts.slice(0, -1)) {
          if (!VALID_MODIFIER.has(modifier.toLowerCase())) {
            problems.push(where("commands") + `: accelerator modifier ${JSON.stringify(modifier)} must be Ctrl, Cmd, Alt, or Shift`);
          }
        }
        const normalized = parts
          .map((p, index) =>
            index < parts.length - 1
              ? ["ctrl", "control", "cmd", "command", "meta", "super"].includes(p.toLowerCase())
                ? "Ctrl"
                : p[0].toUpperCase() + p.slice(1).toLowerCase()
              : p.toUpperCase(),
          )
          .join("+")
          .toLowerCase();
        if (RESERVED_ACCELERATORS.has(normalized)) {
          problems.push(where("commands") + `: accelerator ${JSON.stringify(command.accelerator)} is reserved by the shell (§4.12)`);
        }
      }
    }
  }

  const events = manifest.event_subscriptions ?? [];
  if (new Set(events).size !== events.length) {
    problems.push(where("event_subscriptions") + " must be duplicate-free");
  }
  for (const event of events) {
    if (!EVENT_VOCABULARY.has(event)) {
      problems.push(where("event_subscriptions") + `: ${JSON.stringify(event)} is not an event the engine publishes`);
    }
  }

  const seenDeps = new Set();
  for (const dependency of manifest.dependencies ?? []) {
    if (!/^[A-Za-z0-9.-]+\.[A-Za-z0-9.-]+@.+/.test(dependency)) {
      problems.push(where("dependencies") + `: ${JSON.stringify(dependency)} must be shaped id@range`);
    }
    if (dependency.split("@")[0] === manifest.id) {
      problems.push(where("dependencies") + ": an addon cannot depend on itself");
    }
    if (seenDeps.has(dependency)) {
      problems.push(where("dependencies") + `: ${JSON.stringify(dependency)} is declared twice`);
    }
    seenDeps.add(dependency);
  }

  for (const problem of problems) {
    process.stderr.write(`✗ ${problem}\n`);
  }
  if (problems.length > 0) {
    process.exitCode = 1;
  } else {
    process.stdout.write(`✓ ${id} ${manifest.version} — manifest OK\n`);
  }
  return problems.length === 0;
}

/** Whitelisted payload files for one addon dir (manifest + pages/ + assets/). */
function payloadFiles(addonDir) {
  const files = ["zim.addon.json"];
  for (const prefix of ["pages", "assets"]) {
    const dir = join(addonDir, prefix);
    if (!existsSync(dir)) {
      continue;
    }
    const walk = (current) => {
      for (const entry of readdirSync(current, { withFileTypes: true })) {
        const path = join(current, entry.name);
        if (entry.isDirectory()) {
          walk(path);
        } else {
          files.push(path.slice(addonDir.length + 1).split("\\").join("/"));
        }
      }
    };
    walk(dir);
  }
  return files;
}

/** Build a .zimpkg: vendor the SDK into a staging dir, then zip. */
function buildPackage(addonDir, outputPath) {
  // The repo's zim/ root is two levels above this script
  // (zim/extensions/scripts/), independent of the addon's depth.
  const zimRoot = resolve(new URL("..", import.meta.url).pathname, "..");
  const sdkDist = join(zimRoot, "sdk", "addon", "dist", "zim.js");
  if (!existsSync(sdkDist)) {
    return fail(basename(addonDir), `the SDK build is missing (${sdkDist}); run zim/sdk/addon's build first`);
  }

  const staging = join(tmpdir(), `zim-addon-pkg-${process.pid}-${Date.now()}`);
  mkdirSync(join(staging, "pages"), { recursive: true });
  mkdirSync(join(staging, "assets"), { recursive: true });

  const sdkAlreadyVendored = existsSync(join(addonDir, "assets", "zim.js"));
  const files = payloadFiles(addonDir);
  for (const file of files) {
    const target = join(staging, file);
    mkdirSync(join(target, ".."), { recursive: true });
    copyFileSync(join(addonDir, file), target);
  }
  if (!sdkAlreadyVendored) {
    copyFileSync(sdkDist, join(staging, "assets", "zim.js"));
    process.stdout.write(`… vendored the SDK into assets/zim.js\n`);
  }

  // Every HTML page must reference the vendored SDK or its own
  // assets — the honest author-time check is just: pages exist.
  mkdirSync(join(outputPath, ".."), { recursive: true });
  try {
    execFileSync("zip", ["-q", "-X", "-r", resolve(outputPath), "."], { cwd: staging, stdio: "inherit" });
    const size = statSync(outputPath).size;
    process.stdout.write(`✓ ${basename(addonDir)} → ${outputPath} (${size} bytes)\n`);
  } finally {
    rmSync(staging, { recursive: true, force: true });
  }
}

const [mode, ...args] = process.argv.slice(2);
if (mode === "validate") {
  if (args.length === 0) {
    process.stderr.write("usage: package-addon.mjs validate <addon-dir>…\n");
    process.exit(2);
  }
  for (const addonDir of args) {
    validateManifest(resolve(addonDir));
  }
} else if (mode === "build") {
  if (args.length !== 2) {
    process.stderr.write("usage: package-addon.mjs build <addon-dir> <out.zimpkg>\n");
    process.exit(2);
  }
  buildPackage(resolve(args[0]), resolve(args[1]));
} else {
  process.stderr.write("usage: package-addon.mjs <validate|build> …\n");
  process.exit(2);
}
