/**
 * `zim://newtab` — the ZIM new-tab page (M23 completion).
 *
 * The Chrome NTP grammar adapted to ZIM (§9/§12): a centered
 * search/filter field over the caller's servers, recently- and
 * frequently-opened tiles (engine-backed recents, §11 — `GET
 * /me/recents`, never a browser-history database), create/import
 * actions, workload-type browsing, and the customization entry
 * point (theme presets + accent pip, applied live).
 *
 * No lifecycle side effects on load (the session-restoration rule,
 * BROWSER_INTEGRATION.md §6): opening a tile is navigation; the
 * create/import flows are explicit user actions against the
 * engine's admin-gated `POST /instances`.
 */

import {
  ApiError,
  createInstance,
  health,
  listInstances,
  listNodes,
  listRecents,
  visualStatus,
  whoami,
} from "./api";
import type { Instance, NodeRow, RecentEntry } from "./api";
import "./components";
import { applyStoredTheme, initThemeFromStorage, presetById, resolveTokens, saveTheme } from "./theme";
import type { StoredTheme, ThemePreset } from "./theme";
import { PRESETS, THEME_KEY } from "./theme";
import { formFromCreate, parseInstanceJson, serverMatchesQuery } from "./instanceForm";
import type { CreateInstanceRequest } from "./instanceForm";

const statusPill = document.getElementById("engine-status");
const whoPill = document.getElementById("who");
const countPill = document.getElementById("server-count");
const loginCard = document.getElementById("login-card");
const searchInput = document.getElementById("ntp-search") as HTMLInputElement | null;
const recentSection = document.getElementById("recent-section");
const recentTiles = document.getElementById("recent-tiles");
const allSection = document.getElementById("all-section");
const allTiles = document.getElementById("all-tiles");
const emptySearch = document.getElementById("empty-search");
const createButton = document.getElementById("ntp-create");
const importButton = document.getElementById("ntp-import");
const customizeButton = document.getElementById("ntp-customize");
const createDialog = document.getElementById("create-dialog");
const createForm = document.getElementById("create-form") as HTMLFormElement | null;
const createName = document.getElementById("create-name") as HTMLInputElement | null;
const createNode = document.getElementById("create-node") as HTMLSelectElement | null;
const createDescription = document.getElementById("create-description") as HTMLInputElement | null;
const createStartup = document.getElementById("create-startup") as HTMLInputElement | null;
const createProblem = document.getElementById("create-problem");
const createNote = document.getElementById("create-note");
const importDialog = document.getElementById("import-dialog");
const importForm = document.getElementById("import-form") as HTMLFormElement | null;
const importJson = document.getElementById("import-json") as HTMLTextAreaElement | null;
const importNode = document.getElementById("import-node") as HTMLSelectElement | null;
const importProblem = document.getElementById("import-problem");
const themeDialog = document.getElementById("theme-dialog");
const themePresetsBox = document.getElementById("theme-presets");
const themeAccent = document.getElementById("theme-accent") as HTMLInputElement | null;
const themeAccentReset = document.getElementById("theme-accent-reset");

/** The tile-cache: instance rows + recents + the search filter. */
let instances: Instance[] = [];
let recents: RecentEntry[] = [];
let meAdmin = false;
let nodes: NodeRow[] = [];
let currentTheme: StoredTheme | null = null;

// ── Boot ─────────────────────────────────────────────────────────

initThemeFromStorage();
currentTheme = readStoredTheme();

void refresh();

async function refresh(): Promise<void> {
  // Engine health — unauthenticated.
  try {
    await health();
    setPill(statusPill, "ok", "ok", "pill pill-ok");
  } catch {
    setPill(statusPill, "unreachable", "unreachable", "pill pill-unknown");
  }

  // Session + server list (one round trip each; 401 is expected).
  let me = null;
  try {
    me = await whoami();
  } catch {
    me = null;
  }
  meAdmin = me !== null && (me.is_admin || me.root_admin);
  if (me !== null) {
    setPill(whoPill, me.username, me.username, "pill pill-ok");
    try {
      instances = await listInstances();
      setPill(countPill, String(instances.length), `${instances.length}`, "pill pill-ok");
    } catch {
      instances = [];
      setPill(countPill, "—", "—", "pill pill-unknown");
    }
    // Recents are nicety-grade: a failure just hides the section.
    try {
      recents = await listRecents();
    } catch {
      recents = [];
    }
  } else {
    instances = [];
    recents = [];
    setPill(whoPill, "not signed in", "not signed in", "pill pill-unknown");
    setPill(countPill, "—", "—", "pill pill-unknown");
  }

  if (loginCard !== null) {
    loginCard.hidden = me !== null;
  }
  if (createButton !== null) {
    // Creating servers is admin-gated engine-side; the NTP says so
    // instead of offering a button that can only fail.
    createButton.hidden = me === null;
  }
  if (importButton !== null) {
    importButton.hidden = me === null;
  }

  renderTiles();
}

// ── Tiles (recent + all, filtered by the search field) ───────────

/** Render the recent + all tile grids from the caches. */
function renderTiles(): void {
  const query = searchInput?.value ?? "";
  const byId = new Map(instances.map((row) => [row.id, row]));

  if (recentSection !== null && recentTiles !== null) {
    // Recent = most recently opened (engine order); only tiles the
    // caller can actually see (stale/hidden ids drop silently).
    const rows = recents
      .map((entry) => byId.get(entry.instance_id) === undefined ? null : { row: byId.get(entry.instance_id)!, entry })
      .filter((item): item is { row: Instance; entry: RecentEntry } => item !== null)
      .filter((item) => serverMatchesQuery(item.row.name, item.row.id, query));
    recentSection.hidden = rows.length === 0;
    replaceTiles(recentTiles, rows.map((item) => ({ row: item.row, badge: tileBadge(item.entry) })));
  }

  if (allTiles !== null && allSection !== null) {
    const rows = instances.filter((row) => serverMatchesQuery(row.name, row.id, query));
    allSection.hidden = instances.length === 0 && query === "";
    replaceTiles(allTiles, rows.map((row) => ({ row, badge: null })));
    if (emptySearch !== null) {
      emptySearch.hidden = !(query !== "" && rows.length === 0);
    }
  }
}

/** A frequent-server badge: how often it was opened, when that is
 * worth saying (Chrome's NTP tiles stay quiet about counts of one). */
function tileBadge(entry: RecentEntry): string | null {
  if (entry.open_count >= 3) {
    return `opened ${entry.open_count}×`;
  }
  return null;
}

/** Rebuild one tile grid. */
function replaceTiles(box: HTMLElement, tiles: { row: Instance; badge: string | null }[]): void {
  const fragment = document.createDocumentFragment();
  for (const { row, badge } of tiles) {
    const tile = document.createElement("a");
    tile.className = "ntp-tile";
    tile.setAttribute("role", "listitem");
    tile.href = `/zim/servers/${encodeURIComponent(row.id)}/console`;

    const glyph = document.createElement("span");
    glyph.className = "ntp-tile-dot";
    const visual = visualStatus(row.status);
    glyph.dataset.color = visual.color;
    glyph.setAttribute("aria-hidden", "true");
    glyph.title = visual.label;

    const name = document.createElement("span");
    name.className = "ntp-tile-name";
    name.textContent = row.name;

    const meta = document.createElement("span");
    meta.className = "ntp-tile-meta";
    meta.textContent = badge !== null ? badge : visual.label;

    tile.append(glyph, name, meta);
    fragment.append(tile);
  }
  box.replaceChildren(fragment);
}

// ── Search ───────────────────────────────────────────────────────

searchInput?.addEventListener("input", renderTiles);

// ── Create dialog ────────────────────────────────────────────────

createButton?.addEventListener("click", () => {
  void openCreateDialog();
});

async function openCreateDialog(): Promise<void> {
  if (createDialog === null) {
    return;
  }
  if (createNote !== null) {
    createNote.textContent = meAdmin
      ? "The engine needs a node (and a free allocation on it) — pick one below."
      : "Creating servers needs an administrator account; the engine enforces this.";
  }
  // Nodes load per-dialog (fresh list, admin-only route, honest 403).
  nodes = [];
  if (createNode !== null) {
    createNode.replaceChildren(new Option("loading…", ""));
    try {
      nodes = await listNodes();
      createNode.replaceChildren(
        ...nodes.map((node) => new Option(`${node.name} (${node.id.slice(0, 8)})`, node.id)),
      );
    } catch (error) {
      const hint =
        error instanceof ApiError && error.isPermission
          ? "your account cannot list nodes — an admin creates servers"
          : "nodes could not be listed";
      createNode.replaceChildren(new Option(hint, ""));
    }
  }
  if (createProblem !== null) {
    createProblem.textContent = "";
  }
  showDialog(createDialog);
  createName?.focus();
}

createForm?.addEventListener("submit", (event) => {
  event.preventDefault();
  void submitCreate();
});

async function submitCreate(): Promise<void> {
  if (createForm === null) {
    return;
  }
  const { request, problems } = formFromCreate({
    name: createName?.value ?? "",
    node_id: createNode?.value ?? "",
    description: createDescription?.value,
    startup: createStartup?.value,
  });
  if (problems.length > 0) {
    if (createProblem !== null) {
      createProblem.textContent = problems.map((p) => p.message).join(" · ");
    }
    return;
  }
  await submitInstance(request);
}

// ── Import dialog ────────────────────────────────────────────────

importButton?.addEventListener("click", () => {
  void openImportDialog();
});

async function openImportDialog(): Promise<void> {
  if (importDialog === null) {
    return;
  }
  if (importNode !== null) {
    importNode.replaceChildren(new Option("loading…", ""));
    try {
      nodes = await listNodes();
      importNode.replaceChildren(
        ...nodes.map((node) => new Option(`${node.name} (${node.id.slice(0, 8)})`, node.id)),
      );
    } catch {
      importNode.replaceChildren(new Option("nodes unavailable", ""));
    }
  }
  if (importProblem !== null) {
    importProblem.textContent = "";
  }
  showDialog(importDialog);
  importJson?.focus();
}

importForm?.addEventListener("submit", (event) => {
  event.preventDefault();
  void submitImport();
});

async function submitImport(): Promise<void> {
  if (importJson === null) {
    return;
  }
  const { request, problems } = parseInstanceJson(importJson.value, importNode?.value ?? "");
  if (request === null || problems.length > 0) {
    if (importProblem !== null) {
      importProblem.textContent = problems.map((p) => p.message).join(" · ");
    }
    return;
  }
  await submitInstance(request);
}

/** The shared submit path: create, surface the engine's structured
 * errors honestly, and open the new server's console on success
 * (§9's "NTP flows create/open a server end-to-end"). */
async function submitInstance(request: CreateInstanceRequest): Promise<void> {
  const button = document.activeElement as HTMLElement | null;
  if (button !== null) {
    button.setAttribute("aria-busy", "true");
  }
  try {
    const created = await createInstance(request);
    window.location.assign(`/zim/servers/${encodeURIComponent(created.id)}/console`);
  } catch (error) {
    if (createDialog !== null && createDialog.hidden) {
      if (importProblem !== null) {
        importProblem.textContent = describeError(error);
      }
    } else if (createProblem !== null) {
      createProblem.textContent = describeError(error);
    }
  } finally {
    if (button !== null) {
      button.removeAttribute("aria-busy");
    }
  }
}

/** The engine's structured error, humanized. */
function describeError(error: unknown): string {
  if (error instanceof ApiError) {
    const missing =
      error.missingPermissions.length > 0 ? ` (needs ${error.missingPermissions.join(", ")})` : "";
    return `${error.code}: ${error.message}${missing}`;
  }
  return String(error);
}

// ── Customize (theme presets + accent pip) ───────────────────────

customizeButton?.addEventListener("click", () => {
  if (themeDialog === null) {
    return;
  }
  renderThemePanel();
  showDialog(themeDialog);
});

/** The preset rows: a pip of the preset's accent, its name, a check
 * when selected (Chrome's theme rows, §12). */
function renderThemePanel(): void {
  if (themePresetsBox === null) {
    return;
  }
  const selected = currentTheme?.preset ?? PRESETS[0].id;
  const fragment = document.createDocumentFragment();
  for (const preset of PRESETS) {
    const row = document.createElement("button");
    row.type = "button";
    row.className = "theme-preset";
    row.setAttribute("role", "radio");
    row.setAttribute("aria-checked", String(preset.id === selected));
    const tokens = resolveTokens({ v: 1, preset: preset.id });
    const pip = document.createElement("span");
    pip.className = "theme-pip";
    pip.style.backgroundColor = tokens["color.accent"];
    pip.style.borderColor = tokens["color.surface2"];
    const label = document.createElement("span");
    label.className = "theme-preset-name";
    label.textContent = preset.name;
    const note = document.createElement("span");
    note.className = "theme-preset-note";
    note.textContent = preset.description;
    row.append(pip, label, note);
    row.addEventListener("click", () => {
      choosePreset(preset);
    });
    fragment.append(row);
  }
  themePresetsBox.replaceChildren(fragment);
  if (themeAccent !== null && currentTheme !== null && currentTheme.accent !== undefined) {
    themeAccent.value = currentTheme.accent;
  }
}

/** Apply + persist a preset choice, live. */
function choosePreset(preset: ThemePreset): void {
  currentTheme = { v: 1, preset: preset.id };
  applyCurrentTheme();
  saveTheme(currentTheme);
  renderThemePanel();
}

themeAccent?.addEventListener("input", () => {
  if (currentTheme === null) {
    return;
  }
  const value = themeAccent.value.toLowerCase();
  if (/^#[0-9a-f]{6}$/.test(value)) {
    currentTheme = { ...currentTheme, accent: value };
    applyCurrentTheme();
    saveTheme(currentTheme);
  }
});

themeAccentReset?.addEventListener("click", () => {
  if (currentTheme === null) {
    return;
  }
  const { accent, ...rest } = currentTheme;
  void accent;
  currentTheme = rest;
  applyCurrentTheme();
  saveTheme(currentTheme);
  renderThemePanel();
});

/** Apply the current choice through the shared theme module. */
function applyCurrentTheme(): void {
  applyStoredTheme(currentTheme);
}

/** Read + validate the stored theme (null when absent/corrupt). */
function readStoredTheme(): StoredTheme | null {
  try {
    const raw = window.localStorage.getItem(THEME_KEY);
    if (raw === null) {
      return null;
    }
    const parsed = JSON.parse(raw) as unknown;
    const shape = parsed as Partial<StoredTheme>;
    if (shape.v !== 1 || typeof shape.preset !== "string" || presetById(shape.preset) === null) {
      return null;
    }
    return {
      v: 1,
      preset: shape.preset,
      accent: typeof shape.accent === "string" ? shape.accent.toLowerCase() : undefined,
    };
  } catch {
    return null;
  }
}

// ── Dialog plumbing ──────────────────────────────────────────────

/** Show a dialog (and wire its backdrop + Esc dismissal once). */
function showDialog(dialog: HTMLElement): void {
  dialog.hidden = false;
}

/** Backdrop click dismisses each dialog (Esc handled globally). */
for (const dialog of [createDialog, importDialog, themeDialog]) {
  if (dialog === null) {
    continue;
  }
  dialog.addEventListener("pointerdown", (event) => {
    if (event.target === dialog) {
      dialog.hidden = true;
    }
  });
}
document.getElementById("create-cancel")?.addEventListener("click", () => {
  if (createDialog !== null) {
    createDialog.hidden = true;
  }
});
document.getElementById("import-cancel")?.addEventListener("click", () => {
  if (importDialog !== null) {
    importDialog.hidden = true;
  }
});
window.addEventListener("keydown", (event) => {
  if (event.key === "Escape") {
    for (const dialog of [createDialog, importDialog, themeDialog]) {
      if (dialog !== null && !dialog.hidden) {
        dialog.hidden = true;
        break;
      }
    }
  }
});

// ── Pills + identity refresh relay ───────────────────────────────

function setPill(
  element: HTMLElement | null,
  text: string,
  aria: string,
  className: string,
): void {
  if (element === null) {
    return;
  }
  element.textContent = text;
  element.setAttribute("aria-label", aria);
  element.className = className;
}

document.addEventListener("zim:authenticated", () => {
  void refresh();
});
document.addEventListener("zim:signed-out", () => {
  void refresh();
});
