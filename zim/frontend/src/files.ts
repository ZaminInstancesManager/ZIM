/**
 * `zim://servers/{id}/files` — the file manager (M28's third
 * doorway).
 *
 * The real §10.3 file family over the real engine: breadcrumb
 * navigation through the instance's chrooted tree, listing with
 * honest metadata, upload (the atomic channel), download, new
 * file/folder, rename, delete, and archive/extract. Destructive
 * actions confirm; every mutation is an explicit user action (the
 * page never writes on load); errors surface as the structured
 * `error.message` text, never a silent failure. The same tree is
 * reachable over SFTP (`sftp://127.0.0.1:2022` — AD-33), which the
 * state line advertises.
 */

import {
  ApiError,
  archiveFiles,
  createFileEntry,
  deleteFileEntry,
  downloadFile,
  extractArchive,
  getInstance,
  getState,
  joinFilePath,
  listFiles,
  parentFilePath,
  renameFileEntry,
  uploadFile,
  type FileEntry,
} from "./api";
import { ZimStream } from "./stream";
import "./components";
import { initThemeFromStorage } from "./theme";

// The theme applies before anything renders (M23: live, everywhere).
initThemeFromStorage();

/** Where the instance id lives: `/zim/servers/{id}/files`. */
function instanceIdFromPath(): string | null {
  const match = window.location.pathname.match(/^\/zim\/servers\/([^/]+)\/files\/?$/);
  return match !== null ? decodeURIComponent(match[1]) : null;
}

const nameHeading = document.getElementById("server-name");
const idPill = document.getElementById("server-id");
const dot = document.getElementById("status-dot");
const stateLabel = document.getElementById("state-label");
const breadcrumb = document.getElementById("files-breadcrumb");
const listBody = document.getElementById("files-list");
const message = document.getElementById("files-message");
const uploadInput = document.getElementById("upload-input") as HTMLInputElement | null;
const newFolderButton = document.getElementById("btn-new-folder");
const newFileButton = document.getElementById("btn-new-file");
const archiveButton = document.getElementById("btn-archive");
const extractButton = document.getElementById("btn-extract");
const createForm = document.getElementById("create-form") as HTMLFormElement | null;
const createName = document.getElementById("create-name") as HTMLInputElement | null;
const authGate = document.getElementById("auth-gate");

/** The directory currently listed (`"."` = the instance root). */
let currentPath = ".";
/** What the inline create form is creating (`"dir"` | `"file"`). */
let createKind: "dir" | "file" = "file";
/** The instance id, once parsed. */
let instanceId = "";

function applyStatus(status: string): void {
  if (dot !== null) {
    dot.setAttribute("status", status);
    dot.setAttribute("data-label", status);
  }
  if (stateLabel !== null) {
    stateLabel.textContent = `state: ${status}`;
  }
}

/** Show one operation's outcome in the toolbar's status line. */
function say(text: string): void {
  if (message !== null) {
    message.textContent = text;
  }
}

/** Format one entry's size for humans (binary units, KB-free). */
function formatSize(size: number): string {
  if (size < 1024) {
    return `${size} B`;
  }
  const units = ["KiB", "MiB", "GiB", "TiB"];
  let value = size;
  let unit = -1;
  do {
    value /= 1024;
    unit += 1;
  } while (value >= 1024 && unit < units.length - 1);
  return `${value.toFixed(value >= 100 ? 0 : 1)} ${units[unit]}`;
}

/** The clickable breadcrumb for the current path. */
function renderBreadcrumb(): void {
  if (breadcrumb === null) {
    return;
  }
  breadcrumb.replaceChildren();
  const here = document.createElement("a");
  here.className = "quicklink";
  here.href = "#";
  here.textContent = "root";
  here.addEventListener("click", (event) => {
    event.preventDefault();
    void navigate(".");
  });
  breadcrumb.append(here);
  if (currentPath === ".") {
    return;
  }
  const parts = currentPath.split("/").filter((part) => part !== "" && part !== ".");
  let walked = "";
  for (const part of parts) {
    walked = walked === "" ? part : `${walked}/${part}`;
    const target = walked;
    const separator = document.createElement("span");
    separator.className = "pill pill-muted mono";
    separator.textContent = "/";
    breadcrumb.append(separator);
    const link = document.createElement("a");
    link.className = "quicklink";
    link.href = "#";
    link.textContent = part;
    link.addEventListener("click", (event) => {
      event.preventDefault();
      void navigate(target);
    });
    breadcrumb.append(link);
  }
}

/** One table row for one entry (dirs first, then names — the engine
 * already sorts by name; the dirs-first pass is the file-manager
 * convention the listing UI adds). */
function entryRow(entry: FileEntry): HTMLTableRowElement {
  const row = document.createElement("tr");
  row.dataset.name = entry.name;
  row.dataset.isDir = entry.is_dir ? "true" : "false";

  const checkCell = document.createElement("td");
  checkCell.className = "files-check";
  const check = document.createElement("input");
  check.type = "checkbox";
  check.dataset.path = joinFilePath(currentPath, entry.name);
  check.setAttribute("aria-label", `select ${entry.name}`);
  checkCell.append(check);

  const nameCell = document.createElement("td");
  nameCell.className = "files-name";
  if (entry.is_dir) {
    const link = document.createElement("a");
    link.href = "#";
    link.textContent = `▸ ${entry.name}`;
    link.addEventListener("click", (event) => {
      event.preventDefault();
      void navigate(joinFilePath(currentPath, entry.name));
    });
    nameCell.append(link);
  } else {
    nameCell.textContent = entry.name;
  }

  const sizeCell = document.createElement("td");
  sizeCell.className = "files-size mono";
  sizeCell.textContent = entry.is_dir ? "—" : formatSize(entry.size);

  const modifiedCell = document.createElement("td");
  modifiedCell.className = "files-modified";
  modifiedCell.textContent = entry.modified !== null ? new Date(entry.modified).toLocaleString() : "—";

  const actionsCell = document.createElement("td");
  actionsCell.className = "files-actions";
  if (!entry.is_dir) {
    const download = document.createElement("button");
    download.type = "button";
    download.className = "pill pill-action";
    download.textContent = "⤓";
    download.title = `Download ${entry.name}`;
    download.setAttribute("aria-label", `download ${entry.name}`);
    download.addEventListener("click", () => {
      void downloadEntry(entry.name);
    });
    actionsCell.append(download);
  }
  const rename = document.createElement("button");
  rename.type = "button";
  rename.className = "pill pill-action";
  rename.textContent = "✎";
  rename.title = `Rename ${entry.name}`;
  rename.setAttribute("aria-label", `rename ${entry.name}`);
  rename.addEventListener("click", () => {
    askRename(entry.name);
  });
  actionsCell.append(rename);
  const remove = document.createElement("button");
  remove.type = "button";
  remove.className = "pill pill-action";
  remove.textContent = "✕";
  remove.title = `Delete ${entry.name}`;
  remove.setAttribute("aria-label", `delete ${entry.name}`);
  remove.addEventListener("click", () => {
    void deleteEntry(entry.name, entry.is_dir);
  });
  actionsCell.append(remove);

  row.append(checkCell, nameCell, sizeCell, modifiedCell, actionsCell);
  return row;
}

/** Render one listing (dirs first, then files, each by name). */
function renderListing(entries: FileEntry[]): void {
  if (listBody === null) {
    return;
  }
  listBody.replaceChildren();
  if (entries.length === 0) {
    const row = document.createElement("tr");
    const cell = document.createElement("td");
    cell.colSpan = 5;
    cell.className = "empty";
    cell.textContent = "empty directory";
    row.append(cell);
    listBody.append(row);
    return;
  }
  const sorted = [...entries].sort((a, b) => {
    if (a.is_dir !== b.is_dir) {
      return a.is_dir ? -1 : 1;
    }
    return a.name.localeCompare(b.name);
  });
  for (const entry of sorted) {
    listBody.append(entryRow(entry));
  }
}

/** Load and render the current directory. */
async function refresh(): Promise<void> {
  say("loading…");
  try {
    const listing = await listFiles(instanceId, currentPath);
    renderListing(listing.entries);
    renderBreadcrumb();
    say(`${listing.entries.length} entr${listing.entries.length === 1 ? "y" : "ies"}`);
  } catch (error) {
    renderListing([]);
    renderBreadcrumb();
    say(error instanceof ApiError ? `${error.code} — ${error.message}` : String(error));
  }
}

/** Navigate to one directory. */
async function navigate(path: string): Promise<void> {
  currentPath = path;
  await refresh();
}

/** Download one file in this directory (a real browser download). */
async function downloadEntry(name: string): Promise<void> {
  const path = joinFilePath(currentPath, name);
  say(`downloading ${name}…`);
  try {
    const bytes = await downloadFile(instanceId, path);
    const blob = new Blob([bytes], { type: "application/octet-stream" });
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement("a");
    anchor.href = url;
    anchor.download = name;
    anchor.click();
    URL.revokeObjectURL(url);
    say(`downloaded ${name} (${formatSize(bytes.byteLength)})`);
  } catch (error) {
    say(error instanceof ApiError ? `${error.code} — ${error.message}` : String(error));
  }
}

/** Delete one entry in this directory (confirm-first). */
async function deleteEntry(name: string, isDir: boolean): Promise<void> {
  const path = joinFilePath(currentPath, name);
  const question = isDir
    ? `Delete the directory "${name}" and everything inside it?`
    : `Delete the file "${name}"?`;
  if (!window.confirm(question)) {
    return;
  }
  say(`deleting ${name}…`);
  try {
    await deleteFileEntry(instanceId, path);
    say(`deleted ${name}`);
    await refresh();
  } catch (error) {
    say(error instanceof ApiError ? `${error.code} — ${error.message}` : String(error));
  }
}

/** Rename one entry in this directory (prompt-first). */
function askRename(name: string): void {
  const next = window.prompt(`Rename "${name}" to:`, name);
  if (next === null || next.trim() === "" || next.trim() === name) {
    return;
  }
  void (async () => {
    say(`renaming ${name}…`);
    try {
      await renameFileEntry(instanceId, {
        old_path: joinFilePath(currentPath, name),
        new_path: joinFilePath(currentPath, next.trim()),
      });
      say(`renamed ${name} → ${next.trim()}`);
      await refresh();
    } catch (error) {
      say(error instanceof ApiError ? `${error.code} — ${error.message}` : String(error));
    }
  })();
}

/** The checked entry paths in this directory. */
function checkedPaths(): string[] {
  if (listBody === null) {
    return [];
  }
  const paths: string[] = [];
  for (const box of listBody.querySelectorAll<HTMLInputElement>(
    "input[type=checkbox]:checked",
  )) {
    if (box.dataset.path !== undefined) {
      paths.push(box.dataset.path);
    }
  }
  return paths;
}

/** Archive the checked entries into one archive here. */
async function archiveChecked(): Promise<void> {
  const paths = checkedPaths();
  if (paths.length === 0) {
    say("check at least one entry to archive");
    return;
  }
  const destination = window.prompt(
    `Archive ${paths.length} checked entr${paths.length === 1 ? "y" : "ies"} to:`,
    "archive.tar.gz",
  );
  if (destination === null || destination.trim() === "") {
    return;
  }
  say("archiving…");
  try {
    const result = await archiveFiles(instanceId, {
      paths,
      destination: destination.trim(),
      format: "tar.gz",
    });
    say(`archived ${result.entries} entries → ${result.destination} (${formatSize(result.bytes)})`);
    await refresh();
  } catch (error) {
    say(error instanceof ApiError ? `${error.code} — ${error.message}` : String(error));
  }
}

/** Extract one archive into a directory here. */
async function extractHere(): Promise<void> {
  const archive = window.prompt("Extract which archive (path in this tree)?", "");
  if (archive === null || archive.trim() === "") {
    return;
  }
  const destination = window.prompt(
    `Extract "${archive.trim()}" into:`,
    joinFilePath(parentFilePath(archive.trim()), "restored"),
  );
  if (destination === null || destination.trim() === "") {
    return;
  }
  say("extracting…");
  try {
    const result = await extractArchive(instanceId, {
      archive_path: archive.trim(),
      destination: destination.trim(),
    });
    say(`extracted ${result.entries} entries (${formatSize(result.bytes)})`);
    await refresh();
  } catch (error) {
    say(error instanceof ApiError ? `${error.code} — ${error.message}` : String(error));
  }
}

/** Upload the chosen file into the current directory. */
async function uploadChosen(file: File): Promise<void> {
  const path = joinFilePath(currentPath, file.name);
  say(`uploading ${file.name}…`);
  try {
    const bytes = await file.arrayBuffer();
    const result = await uploadFile(instanceId, path, bytes);
    say(`uploaded ${result.path} (${formatSize(result.size)})`);
    await refresh();
  } catch (error) {
    say(error instanceof ApiError ? `${error.code} — ${error.message}` : String(error));
  }
}

/** Show the inline create form for one kind. */
function showCreate(kind: "dir" | "file"): void {
  createKind = kind;
  if (createForm !== null) {
    createForm.hidden = false;
  }
  if (createName !== null) {
    createName.value = "";
    createName.placeholder = kind === "dir" ? "directory name…" : "file name…";
    createName.focus();
  }
}

/** Hide the inline create form. */
function hideCreate(): void {
  if (createForm !== null) {
    createForm.hidden = true;
  }
}

/** Create the named entry of the current kind. */
async function createNamed(name: string): Promise<void> {
  say(`creating ${name}…`);
  try {
    await createFileEntry(instanceId, name, createKind === "dir");
    say(`created ${name}`);
    hideCreate();
    await refresh();
  } catch (error) {
    say(error instanceof ApiError ? `${error.code} — ${error.message}` : String(error));
  }
}

function wireToolbar(): void {
  if (uploadInput !== null) {
    uploadInput.addEventListener("change", () => {
      const file = uploadInput.files !== null ? uploadInput.files[0] : undefined;
      if (file !== undefined) {
        void uploadChosen(file);
      }
      uploadInput.value = "";
    });
  }
  newFolderButton?.addEventListener("click", () => showCreate("dir"));
  newFileButton?.addEventListener("click", () => showCreate("file"));
  archiveButton?.addEventListener("click", () => {
    void archiveChecked();
  });
  extractButton?.addEventListener("click", () => {
    void extractHere();
  });
  createForm?.addEventListener("submit", (event) => {
    event.preventDefault();
    const name = createName?.value.trim() ?? "";
    if (name === "") {
      return;
    }
    void createNamed(joinFilePath(currentPath, name));
  });
  document.getElementById("create-cancel")?.addEventListener("click", hideCreate);
}

async function boot(): Promise<void> {
  const parsed = instanceIdFromPath();
  if (parsed === null) {
    say(`cannot parse a server id from ${window.location.pathname}`);
    return;
  }
  instanceId = parsed;
  if (idPill !== null) {
    idPill.textContent = instanceId;
  }

  let instanceName = instanceId.slice(0, 8);
  try {
    const instance = await getInstance(instanceId);
    instanceName = instance.name;
  } catch (error) {
    if (error instanceof ApiError && error.isAuth) {
      if (authGate !== null) {
        authGate.hidden = false;
      }
      document.addEventListener("zim:authenticated", () => {
        window.location.reload();
      });
    } else {
      say(error instanceof ApiError ? `${error.code} — ${error.message}` : String(error));
    }
  }
  if (nameHeading !== null) {
    nameHeading.textContent = `${instanceName} — files`;
  }
  document.title = `ZIM — ${instanceName} files`;

  try {
    const snapshot = await getState(instanceId);
    applyStatus(snapshot.status);
  } catch {
    // The dot stays `unknown` — the honest state.
  }

  wireToolbar();
  await refresh();

  const stream = new ZimStream(instanceId, {
    onEvent: (event) => {
      if (event.type === "instance.state_change" && event.data.to !== undefined) {
        applyStatus(event.data.to);
      }
    },
    onGap: () => {
      void getState(instanceId)
        .then((snapshot) => {
          applyStatus(snapshot.status);
        })
        .catch(() => undefined);
    },
    onFatal: () => {
      stream.close();
    },
  });
}

void boot();
