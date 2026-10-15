/**
 * `zim://app` — the ZIM application chrome (M22).
 *
 * The Chrome interaction model, replicated in the frontend's own
 * TypeScript (the roadmap's M22 goal): a tab strip across the top
 * (one tab per server/workload view, drag-reorder, close, tear-off,
 * overflow, Chrome's animation curves), the omnibox beneath it
 * (`zim://` navigation with a suggestion dropdown and engine-
 * identity indication, AD-23's visible-canonical-identifier
 * obligation), and one content pane per tab.
 *
 * Panes are same-origin iframes onto the engine's own page surface
 * (AD-24): every tab loads the same pages any browser can load at
 * `https://127.0.0.1:{port}/zim/…`, so tab content inherits the
 * pages' view-only semantics — closing a tab only unsubscribes,
 * never stops a server (invariants 3–6).
 *
 * The tab strip holds one §11 events stream per server tab (the
 * §9 "events stream" slot of a tab's budget): `instance.state_change`
 * drives the §4 indicator dot in real time, `instance.install_progress`
 * drives the progress ring, and a gap re-fetches state over HTTP.
 *
 * Session persistence (§7): the tab set is stored as `zim://` URLs
 * (stable identifiers, never indexes) under `zim.session.v1`, and
 * restoration re-opens views only — no lifecycle API is called as a
 * side effect of loading this page.
 */

import { ApiError, getInstance, getState, health, listInstances, power, visualStatus, whoami } from "./api";
import type { Instance, Me } from "./api";
import { ZimStream } from "./stream";
import "./components";
import { SESSION_KEY, TabModel, defaultSession, restoreSession } from "./tabs";
import type { Tab } from "./tabs";
import { buildSuggestions } from "./omnibox";
import type { SuggestibleInstance, Suggestion } from "./omnibox";
import { canonicalZim, httpForm, instanceIdOf, parseZimUrl, pathFromLocation, serverSurfaceOf, staticTitle } from "./zimurl";
// ── Tunables (Chrome-derived motion values; see §12's note) ──────

/** Tab open/close/drag animation: Chrome's tab-animation curve. */
const TAB_EASE = "cubic-bezier(0.2, 0, 0, 1)";
/** Chrome's tab animation duration (its canonical ~10 frames). */
const TAB_MS = 167;
/** Pointer travel before a press becomes a drag (Chrome's slop). */
const DRAG_SLOP_PX = 5;
/** Downward travel before a drag becomes a tear-off. */
const TEAR_OFF_PX = 90;
/** Session writes are debounced; the flush is eager on unload. */
const SAVE_DEBOUNCE_MS = 300;

// ── DOM refs ─────────────────────────────────────────────────────

const strip = document.getElementById("tabstrip");
const newTabButton = document.getElementById("newtab-btn");
const overflowBox = document.getElementById("strip-overflow");
const scrollLeftButton = document.getElementById("scroll-left");
const scrollRightButton = document.getElementById("scroll-right");
const panes = document.getElementById("panes");
const omniboxInput = document.getElementById("omnibox-input") as HTMLInputElement | null;
const suggestBox = document.getElementById("suggest");
const engineChip = document.getElementById("engine-chip");
const chipText = document.getElementById("chip-text");
const chipDot = document.getElementById("chip-dot");
const menuBox = document.getElementById("ctxmenu");
const toastBox = document.getElementById("toast");

// ── State ────────────────────────────────────────────────────────

/** One pane per tab: the iframe, its indicator stream, its status. */
interface Pane {
  tabId: number;
  frame: HTMLIFrameElement;
  stream: ZimStream | null;
  /** Last-known lifecycle status (`unknown` until proven). */
  status: string;
  /** The §4 progress ring value, while installing. */
  progress: number | undefined;
  /** Instance id this pane addresses, when it addresses one. */
  instanceId: string | null;
  /** The live stream is unusable (auth/permission) — HTTP fallback. */
  degraded: boolean;
}

const model = new TabModel();
const paneById = new Map<number, Pane>();
const instanceCache = new Map<string, Instance>();
let me: Me | null = null;
let suggestContext: SuggestibleInstance[] = [];
let saveTimer: number | undefined;

// ── Boot ─────────────────────────────────────────────────────────

function boot(): void {
  const params = new URLSearchParams(window.location.search);
  const deepLink = params.get("tab");
  const askNewTab = params.get("newtab") === "1";

  // §7: restore the persisted tab set — views only, never
  // lifecycle. A `?tab=` deep link (tear-off target) REPLACES the
  // session, like Chrome opening with a URL argument.
  let restored = defaultSession();
  if (deepLink !== null) {
    const parsed = parseZimUrl(deepLink);
    if ("url" in parsed) {
      restored = { paths: [parsed.url.path], active: 0 };
    }
  } else {
    const stored = readStoredSession();
    const revived = restoreSession(stored);
    if (revived !== null) {
      restored = revived;
    }
  }

  for (const path of restored.paths) {
    openTabSilent(path);
  }
  const tabs = model.tabs;
  const active = tabs[Math.min(Math.max(restored.active, 0), tabs.length - 1)];
  if (active !== undefined) {
    model.activate(active.id);
  }
  if (askNewTab) {
    // The tray's "New tab" lands here: session restored + one more.
    model.add("newtab");
  }

  syncStrip();
  syncPanes();
  refreshOmniboxDisplay();
  saveSessionSoon();
  void refreshIdentity();

  attachGlobalListeners();
}

/** localStorage may throw (privacy modes); a session is a nicety. */
function readStoredSession(): unknown {
  try {
    const raw = window.localStorage.getItem(SESSION_KEY);
    return raw === null ? null : (JSON.parse(raw) as unknown);
  } catch {
    return null;
  }
}

// ── Identity (the omnibox's engine-identity indication) ──────────

/** Health + session + instance cache: the address bar's origin
 * chip and the suggestion context, in one round of calls. */
async function refreshIdentity(): Promise<void> {
  let version = "";
  try {
    const info = await health();
    version = `v${(info as { version?: string }).version ?? ""}`.replace(/^v$/, "");
  } catch {
    version = "unreachable";
  }
  try {
    me = await whoami();
  } catch {
    me = null;
  }
  if (me !== null) {
    try {
      const instances = await listInstances();
      instanceCache.clear();
      for (const instance of instances) {
        instanceCache.set(instance.id, instance);
      }
    } catch {
      instanceCache.clear();
    }
  }
  suggestContext = [...instanceCache.values()].map((instance) => ({ id: instance.id, name: instance.name }));
  renderChip(version);
  refreshTabTitlesAndStatuses();
  if (omniboxInput !== null && omniboxInput === document.activeElement) {
    renderSuggestions();
  }
}

/** The chip: engine identity where Chrome shows site identity. */
function renderChip(version: string): void {
  const host = window.location.host || "127.0.0.1";
  const who = me !== null ? me.username : "not signed in";
  if (chipText !== null) {
    chipText.textContent = version === "unreachable" ? "engine unreachable" : `engine ${version} · ${who}`;
  }
  if (chipDot !== null) {
    chipDot.dataset.state = version === "unreachable" ? "down" : me !== null ? "auth" : "anon";
  }
  if (engineChip !== null) {
    engineChip.setAttribute(
      "title",
      `ZIM engine at https://${host}\nsigned in: ${who}\nthe omnibox navigates zim:// surfaces`,
    );
  }
}

/** After identity/instance data lands: retitle and reseed the
 * per-tab indicators from the instance rows. */
function refreshTabTitlesAndStatuses(): void {
  for (const tab of model.tabs) {
    const pane = paneById.get(tab.id);
    if (pane === undefined) {
      continue;
    }
    if (pane.instanceId !== null) {
      const row = instanceCache.get(pane.instanceId);
      if (row !== undefined && pane.status === "unknown") {
        pane.status = row.status;
      }
    }
    renderTab(tab);
    renderPaneIndicator(pane);
  }
}

// ── Tab strip DOM ────────────────────────────────────────────────

/** Build (or reuse) one tab element. */
function tabElement(tab: Tab): HTMLElement {
  const existing = strip?.querySelector<HTMLElement>(`[data-tab-id="${tab.id}"]`);
  if (existing !== null && existing !== undefined) {
    return existing;
  }
  const element = document.createElement("div");
  element.className = "tab";
  element.dataset.tabId = String(tab.id);
  element.setAttribute("role", "tab");
  element.tabIndex = 0;

  const glyph = document.createElement("span");
  glyph.className = "tab-glyph";
  const title = document.createElement("span");
  title.className = "tab-title";
  const close = document.createElement("button");
  close.className = "tab-close";
  close.type = "button";
  close.textContent = "×";
  close.setAttribute("aria-label", "Close tab");
  element.append(glyph, title, close);

  element.addEventListener("click", (event) => {
    if ((event.target as HTMLElement).closest(".tab-close") !== null) {
      closeTab(tab.id);
      return;
    }
    if (!dragMoved) {
      model.activate(tab.id);
      syncStrip();
      syncPanes();
      refreshOmniboxDisplay();
      saveSessionSoon();
    }
  });
  element.addEventListener("keydown", (event) => {
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      model.activate(tab.id);
      syncStrip();
      syncPanes();
      refreshOmniboxDisplay();
      saveSessionSoon();
    }
  });
  element.addEventListener("auxclick", (event) => {
    if (event.button === 1) {
      closeTab(tab.id);
    }
  });
  element.addEventListener("contextmenu", (event) => {
    event.preventDefault();
    showContextMenu(tab, event.clientX, event.clientY);
  });
  element.addEventListener("pointerdown", (event) => {
    if (event.button === 0) {
      beginDrag(tab, element, event);
    }
  });
  return element;
}

/** Render one tab's visible state (title, indicator, selection). */
function renderTab(tab: Tab): void {
  const element = strip?.querySelector<HTMLElement>(`[data-tab-id="${tab.id}"]`);
  if (element === null || element === undefined) {
    return;
  }
  const pane = paneById.get(tab.id);
  const title = element.querySelector(".tab-title");
  const glyph = element.querySelector(".tab-glyph");
  const instanceId = instanceIdOf(tab.path);
  const surface = serverSurfaceOf(tab.path);
  const row = instanceId !== null ? instanceCache.get(instanceId) : undefined;

  let text = staticTitle(tab.path);
  if (instanceId !== null) {
    text = row !== undefined ? row.name : instanceId.slice(0, 8);
    if (surface !== null) {
      text = `${text} — ${staticTitle(tab.path)}`;
    }
  }
  if (title !== null) {
    title.textContent = text;
  }

  if (glyph !== null) {
    if (instanceId !== null) {
      let dot = glyph.querySelector("zim-status-dot");
      if (dot === null) {
        dot = document.createElement("zim-status-dot");
        dot.setAttribute("data-size", "sm");
        glyph.append(dot);
      }
      const status = pane !== undefined ? pane.status : "unknown";
      dot.setAttribute("status", status);
      dot.setAttribute("data-label", visualStatus(status).label);
      if (pane !== undefined && pane.status === "installing" && pane.progress !== undefined) {
        dot.setAttribute("progress", String(pane.progress));
      } else {
        dot.removeAttribute("progress");
      }
      glyph.classList.remove("tab-glyph-static");
    } else {
      glyph.replaceChildren();
      glyph.classList.add("tab-glyph-static");
    }
  }

  const selected = model.activeId === tab.id;
  element.setAttribute("aria-selected", String(selected));
  element.classList.toggle("tab-active", selected);
  element.classList.toggle("tab-degraded", pane !== undefined && pane.degraded);
  const label = pane !== undefined ? visualStatus(pane.status).label : text;
  element.setAttribute(
    "title",
    instanceId !== null && pane !== undefined
      ? `${text} — ${label}${pane.degraded ? " (live indicator unavailable)" : ""}`
      : `${text} — ${canonicalZim(tab.path)}`,
  );
}

/** Reconcile the strip with the model (order, presence, state). */
function syncStrip(): void {
  if (strip === null) {
    return;
  }
  const wanted = model.tabs;
  // Removal first (closing animations own their element).
  for (const element of [...strip.querySelectorAll<HTMLElement>(".tab")]) {
    const id = Number(element.dataset.tabId);
    if (model.byId(id) === null) {
      element.remove();
    }
  }
  const previous = new Map<string, DOMRect>();
  if (!dragState.active) {
    for (const element of strip.querySelectorAll<HTMLElement>(".tab")) {
      previous.set(element.dataset.tabId ?? "", element.getBoundingClientRect());
    }
  }
  let anchor: HTMLElement | null = null;
  for (const tab of wanted) {
    const element = tabElement(tab);
    const current = element.parentElement === strip ? element : null;
    const reference: ChildNode | null = anchor !== null ? anchor.nextSibling : strip.firstChild;
    if (current === null || element !== reference) {
      strip.insertBefore(element, reference);
    }
    anchor = element;
    renderTab(tab);
  }
  // FLIP: elements that moved get an inverse transform released on
  // the next frame, so reorders glide instead of teleporting.
  if (!dragState.active && previous.size > 0) {
    for (const element of strip.querySelectorAll<HTMLElement>(".tab")) {
      const before = previous.get(element.dataset.tabId ?? "");
      if (before === undefined) {
        continue;
      }
      const delta = before.left - element.getBoundingClientRect().left;
      if (Math.abs(delta) > 1) {
        element.style.transform = `translateX(${delta}px)`;
        window.requestAnimationFrame(() => {
          element.style.transition = `transform ${TAB_MS}ms ${TAB_EASE}`;
          element.style.transform = "";
          window.setTimeout(() => {
            element.style.transition = "";
          }, TAB_MS);
        });
      }
    }
  }
  updateOverflow();
}

// ── Panes (one same-origin iframe per tab, AD-24) ────────────────

function openTabSilent(path: string): Tab {
  const tab = model.add(path, { activate: false });
  createPane(tab);
  return tab;
}

function createPane(tab: Tab): Pane {
  const frame = document.createElement("iframe");
  frame.className = "pane-frame";
  frame.src = httpForm(tab.path);
  frame.title = `${staticTitle(tab.path)} — ${canonicalZim(tab.path)}`;
  const pane: Pane = {
    tabId: tab.id,
    frame,
    stream: null,
    status: "unknown",
    progress: undefined,
    instanceId: instanceIdOf(tab.path),
    degraded: false,
  };
  // Pane resync: the page inside may navigate itself (links); the
  // tab follows the pane, not the other way round.
  frame.addEventListener("load", () => {
    void resyncPanePath(pane);
  });
  paneById.set(tab.id, pane);
  if (panes !== null) {
    panes.append(frame);
  }
  renderPaneIndicator(pane);
  return pane;
}

/** After an in-pane navigation: adopt the new path (omnibox,
 * session, title, indicator subscription all follow). */
async function resyncPanePath(pane: Pane): Promise<void> {
  let location: Location | null = null;
  try {
    location = pane.frame.contentWindow?.location ?? null;
  } catch {
    location = null; // Cross-origin — cannot happen on this origin.
  }
  if (location === null) {
    return;
  }
  const path = pathFromLocation(location);
  const tab = model.byId(pane.tabId);
  if (path === null || tab === null) {
    return;
  }
  if (model.updatePath(tab.id, path)) {
    const nextInstance = instanceIdOf(path);
    if (nextInstance !== pane.instanceId) {
      pane.stream?.close();
      pane.stream = null;
      pane.degraded = false;
    }
    pane.instanceId = nextInstance;
    renderPaneIndicator(pane);
    renderTab(tab);
    if (model.activeId === tab.id) {
      refreshOmniboxDisplay();
    }
    saveSessionSoon();
  }
}

/** Show only the active pane; every pane keeps running (background
 * tabs stay loaded, like Chrome's — streams included). */
function syncPanes(): void {
  for (const pane of paneById.values()) {
    const active = model.activeId === pane.tabId;
    pane.frame.hidden = !active;
    if (active) {
      pane.frame.focus({ preventScroll: true });
    }
  }
  const tab = model.active;
  if (tab !== null) {
    document.title = `ZIM — ${staticTitle(tab.path)}`;
    renderTab(tab);
  }
}

// ── Per-tab indicator (the §4 dot, live over §11) ────────────────

/** (Re)subscribe the pane's indicator stream for its current
 * instance; non-instance panes carry no subscription. */
function renderPaneIndicator(pane: Pane): void {
  const tab = model.byId(pane.tabId);
  if (tab === null) {
    return;
  }
  const wantedInstance = pane.instanceId;
  if (wantedInstance === null) {
    pane.stream?.close();
    pane.stream = null;
    pane.degraded = false;
    renderTab(tab);
    return;
  }
  // A server not in the list cache (deep-linked console, a shared
  // instance): fetch its row once for the title; failure is silent
  // (the id-slice title is the honest fallback).
  if (!instanceCache.has(wantedInstance)) {
    void getInstance(wantedInstance)
      .then((row) => {
        instanceCache.set(row.id, row);
        const current = model.byId(pane.tabId);
        if (current !== null) {
          renderTab(current);
        }
      })
      .catch(() => undefined);
  }
  if (pane.stream !== null) {
    // The stream is instance-scoped; a surface change on the same
    // instance keeps it (console → files on one server).
    return;
  }
  const row = instanceCache.get(wantedInstance);
  if (row !== undefined && pane.status === "unknown") {
    pane.status = row.status;
  }
  pane.stream = new ZimStream(wantedInstance, {
    onEvent: (event) => {
      if (event.type === "instance.state_change" && event.data.to !== undefined) {
        pane.status = event.data.to;
        pane.progress = undefined;
        const current = model.byId(pane.tabId);
        if (current !== null) {
          renderTab(current);
        }
      } else if (event.type === "instance.install_progress") {
        const progress = event.data.progress;
        if (typeof progress === "number") {
          pane.progress = Math.min(Math.max(progress, 0), 1);
          pane.status = "installing";
          const current = model.byId(pane.tabId);
          if (current !== null) {
            renderTab(current);
          }
        }
      }
    },
    onGap: () => {
      // §11.3: replay could not cover a range — re-fetch state.
      void refetchPaneState(pane);
    },
    onFatal: () => {
      // Anonymous or permission-denied: the tab still renders; the
      // indicator falls back to one HTTP fetch and a muted dot.
      pane.degraded = true;
      pane.stream?.close();
      pane.stream = null;
      void refetchPaneState(pane);
    },
  });
  renderTab(tab);
}

/** One HTTP state fetch (gap recovery / degraded fallback). */
async function refetchPaneState(pane: Pane): Promise<void> {
  if (pane.instanceId === null) {
    return;
  }
  try {
    const snapshot = await getState(pane.instanceId);
    pane.status = snapshot.status;
    pane.degraded = false;
  } catch {
    // Unreachable instance or engine: keep the last known state.
  }
  const current = model.byId(pane.tabId);
  if (current !== null) {
    renderTab(current);
  }
}

// ── Tab lifecycle ────────────────────────────────────────────────

function closeTab(id: number): void {
  const pane = paneById.get(id);
  const element = strip?.querySelector<HTMLElement>(`[data-tab-id="${id}"]`);
  // Chrome keeps at least one tab; so does the ZIM chrome.
  if (model.size <= 1) {
    const replacement = model.active;
    if (replacement !== null) {
      navigateTab(replacement.id, "newtab");
      showToast("one tab stays open — closed the page instead");
    }
    return;
  }
  model.close(id);
  if (pane !== undefined) {
    // Closing a tab only unsubscribes (invariant 4).
    pane.stream?.close();
    pane.frame.remove();
    paneById.delete(id);
  }
  if (element !== null && element !== undefined) {
    element.classList.add("tab-closing");
    window.setTimeout(() => {
      element.remove();
    }, TAB_MS);
  }
  syncStrip();
  syncPanes();
  refreshOmniboxDisplay();
  saveSessionSoon();
}

/** Omnibox navigation (and the context menu's "Open …" items):
 * the ACTIVE-tab form navigates in place. */
function navigateTab(id: number, path: string): void {
  const tab = model.byId(id);
  const pane = paneById.get(id);
  if (tab === null || pane === undefined) {
    return;
  }
  // A different instance means a different subscription — the old
  // stream closes before the new one attaches (§4: one stream per
  // tab, following the tab's instance).
  const nextInstance = instanceIdOf(path);
  if (nextInstance !== pane.instanceId) {
    pane.stream?.close();
    pane.stream = null;
    pane.degraded = false;
  }
  model.updatePath(id, path);
  pane.instanceId = nextInstance;
  pane.status = "unknown";
  pane.progress = undefined;
  pane.frame.src = httpForm(path);
  pane.frame.title = `${staticTitle(path)} — ${canonicalZim(path)}`;
  renderPaneIndicator(pane);
  renderTab(tab);
  if (model.activeId === id) {
    refreshOmniboxDisplay();
  }
  saveSessionSoon();
}

function newTab(path = "newtab"): void {
  const tab = model.add(path);
  const element = tabElement(tab);
  element.classList.add("tab-opening");
  window.requestAnimationFrame(() => {
    element.classList.remove("tab-opening");
    window.setTimeout(() => {
      element.classList.remove("tab-opening");
    }, TAB_MS);
  });
  createPane(tab);
  syncStrip();
  syncPanes();
  refreshOmniboxDisplay();
  saveSessionSoon();
  omniboxInput?.blur();
}

// ── Drag-reorder + tear-off (pointer events) ─────────────────────

interface DragState {
  active: boolean;
  tabId: number;
  element: HTMLElement | null;
  startX: number;
  startY: number;
  pointerId: number;
  insertAt: number;
}

const dragState: DragState = {
  active: false,
  tabId: -1,
  element: null,
  startX: 0,
  startY: 0,
  pointerId: -1,
  insertAt: -1,
};
let dragMoved = false;

function beginDrag(tab: Tab, element: HTMLElement, event: PointerEvent): void {
  if ((event.target as HTMLElement).closest(".tab-close") !== null) {
    return;
  }
  dragMoved = false;
  dragState.tabId = tab.id;
  dragState.element = element;
  dragState.startX = event.clientX;
  dragState.startY = event.clientY;
  dragState.pointerId = event.pointerId;
  element.setPointerCapture(event.pointerId);
  element.addEventListener("pointermove", onDragMove);
  element.addEventListener("pointerup", onDragEnd, { once: true });
  element.addEventListener("pointercancel", onDragCancel, { once: true });
}

function onDragMove(event: PointerEvent): void {
  const element = dragState.element;
  if (element === null) {
    return;
  }
  const dx = event.clientX - dragState.startX;
  const dy = event.clientY - dragState.startY;
  if (!dragState.active) {
    if (Math.abs(dx) < DRAG_SLOP_PX && Math.abs(dy) < DRAG_SLOP_PX) {
      return;
    }
    if (Math.abs(dy) >= TEAR_OFF_PX) {
      dragMoved = true;
      element.classList.add("tab-tearing");
      return;
    }
    dragState.active = true;
    dragMoved = true;
    element.classList.add("tab-dragging");
  }
  element.style.transform = `translateX(${dx}px)`;
  element.classList.toggle("tab-tearing", dy >= TEAR_OFF_PX);
  // Which slot are we hovering over? Shift the in-between siblings.
  const index = hoverIndex(event.clientX);
  if (index >= 0 && index !== dragState.insertAt) {
    dragState.insertAt = index;
    shiftSiblings(index);
  }
}

/** The strip index the pointer currently hovers. */
function hoverIndex(clientX: number): number {
  if (strip === null) {
    return -1;
  }
  const tabs = [...strip.querySelectorAll<HTMLElement>(".tab:not(.tab-closing)")];
  for (let i = 0; i < tabs.length; i += 1) {
    const rect = tabs[i].getBoundingClientRect();
    if (clientX < rect.left + rect.width / 2) {
      return i;
    }
  }
  return tabs.length;
}

/** Visually open a gap at `index` by translating the siblings
 * between the dragged tab's old slot and the new one. */
function shiftSiblings(index: number): void {
  if (strip === null) {
    return;
  }
  const tabs = [...strip.querySelectorAll<HTMLElement>(".tab:not(.tab-closing)")];
  const from = tabs.findIndex((t) => t.dataset.tabId === String(dragState.tabId));
  if (from < 0) {
    return;
  }
  tabs.forEach((element, i) => {
    if (element.dataset.tabId === String(dragState.tabId)) {
      return;
    }
    const between = (i > from && i <= index) || (i < from && i >= index);
    const shift = between ? (from < index ? -1 : 1) * (element.getBoundingClientRect().width + 2) : 0;
    element.style.transition = `transform ${TAB_MS}ms ${TAB_EASE}`;
    element.style.transform = shift === 0 ? "" : `translateX(${shift}px)`;
  });
}

function onDragEnd(event: PointerEvent): void {
  const element = dragState.element;
  cleanupDragListeners(element);
  const dy = event.clientY - dragState.startY;
  const tearing = dragState.active && dy >= TEAR_OFF_PX;
  if (dragState.active && !tearing) {
    const to = dragState.insertAt >= 0 ? dragState.insertAt : model.indexOf(dragState.tabId);
    model.move(dragState.tabId, to);
  }
  const tab = model.byId(dragState.tabId);
  if (element !== null) {
    element.classList.remove("tab-dragging", "tab-tearing");
    if (dragState.active) {
      element.style.transition = `transform ${TAB_MS}ms ${TAB_EASE}`;
    }
    element.style.transform = "";
    window.setTimeout(() => {
      if (element !== null) {
        element.style.transition = "";
      }
    }, TAB_MS);
  }
  if (strip !== null) {
    for (const sibling of strip.querySelectorAll<HTMLElement>(".tab")) {
      sibling.style.transform = "";
      sibling.style.transition = "";
    }
  }
  dragState.active = false;
  dragState.insertAt = -1;
  if (tearing && tab !== null) {
    tearOff(tab);
  } else {
    syncStrip();
    saveSessionSoon();
  }
}

function onDragCancel(): void {
  const element = dragState.element;
  cleanupDragListeners(element);
  if (element !== null) {
    element.classList.remove("tab-dragging", "tab-tearing");
    element.style.transform = "";
  }
  if (strip !== null) {
    for (const sibling of strip.querySelectorAll<HTMLElement>(".tab")) {
      sibling.style.transform = "";
      sibling.style.transition = "";
    }
  }
  dragState.active = false;
  dragMoved = false;
}

function cleanupDragListeners(element: HTMLElement | null): void {
  element?.removeEventListener("pointermove", onDragMove);
}

/** Tear-off: open the tab in its own window. In a plain browser
 * this is a `window.open` of the chrome with a `?tab=` deep link;
 * inside the desktop shell, popups are not available — the M23
 * multi-window milestone completes this natively (the event hook
 * below is what M22 ships for the shell side). */
function tearOff(tab: Tab): void {
  const url = `/zim/app?tab=${encodeURIComponent(canonicalZim(tab.path))}`;
  const opened = window.open(url, "_blank", "noopener");
  if (opened === null) {
    window.dispatchEvent(new CustomEvent("zim:tear-off", { detail: { path: tab.path } }));
    showToast("tear-off needs a window the page may open — the desktop shell's multi-window lands with M23");
    syncStrip();
    return;
  }
  closeTab(tab.id);
}

// ── Overflow ─────────────────────────────────────────────────────

function updateOverflow(): void {
  if (strip === null || overflowBox === null) {
    return;
  }
  const overflowing = strip.scrollWidth > strip.clientWidth + 1;
  overflowBox.hidden = !overflowing;
}

// ── Omnibox ──────────────────────────────────────────────────────

let suggestRows: Suggestion[] = [];
let suggestHighlight = -1;

function refreshOmniboxDisplay(): void {
  const tab = model.active;
  if (omniboxInput === null || tab === null) {
    return;
  }
  if (document.activeElement !== omniboxInput) {
    omniboxInput.value = tab.path;
  }
}

function renderSuggestions(): void {
  if (suggestBox === null || omniboxInput === null) {
    return;
  }
  suggestRows = buildSuggestions(omniboxInput.value, {
    instances: suggestContext,
    signedIn: me !== null,
  });
  suggestHighlight = -1;
  const fragment = document.createDocumentFragment();
  suggestRows.forEach((row, index) => {
    const element = document.createElement("div");
    element.className = `suggest-row suggest-${row.kind}`;
    element.setAttribute("role", "option");
    element.dataset.index = String(index);
    const title = document.createElement("span");
    title.className = "suggest-title";
    title.textContent = row.title;
    const subtitle = document.createElement("span");
    subtitle.className = "suggest-subtitle mono";
    subtitle.textContent = row.subtitle;
    element.append(title, subtitle);
    element.addEventListener("mousedown", (event) => {
      event.preventDefault(); // Keep omnibox focus until navigation.
      chooseSuggestion(index);
    });
    fragment.append(element);
  });
  suggestBox.replaceChildren(fragment);
  suggestBox.hidden = suggestRows.length === 0;
  omniboxInput.setAttribute("aria-expanded", String(suggestRows.length > 0));
}

function chooseSuggestion(index: number): void {
  const row = suggestRows[index];
  if (row === undefined || row.path === null) {
    return;
  }
  navigateFromOmnibox(row.path);
}

function navigateFromOmnibox(path: string): void {
  const tab = model.active;
  if (tab !== null) {
    navigateTab(tab.id, path);
  }
  omniboxInput?.blur();
  hideSuggestions();
}

function hideSuggestions(): void {
  if (suggestBox !== null) {
    suggestBox.hidden = true;
  }
  omniboxInput?.setAttribute("aria-expanded", "false");
}

function omniboxKeydown(event: KeyboardEvent): void {
  if (suggestBox === null || omniboxInput === null) {
    return;
  }
  const open = !suggestBox.hidden;
  if (event.key === "Escape") {
    if (open) {
      event.preventDefault();
      hideSuggestions();
    } else {
      omniboxInput.value = model.active?.path ?? "";
    }
    omniboxInput.select();
    return;
  }
  if (event.key === "ArrowDown" || event.key === "ArrowUp") {
    if (!open) {
      renderSuggestions();
      return;
    }
    event.preventDefault();
    const count = suggestRows.length;
    const direction = event.key === "ArrowDown" ? 1 : -1;
    suggestHighlight = (suggestHighlight + direction + count) % Math.max(count, 1);
    for (const row of suggestBox.querySelectorAll<HTMLElement>(".suggest-row")) {
      row.classList.toggle("suggest-active", Number(row.dataset.index) === suggestHighlight);
    }
    return;
  }
  if (event.key === "Enter") {
    event.preventDefault();
    const highlighted = suggestRows[suggestHighlight];
    if (highlighted !== undefined && highlighted.path !== null) {
      navigateFromOmnibox(highlighted.path);
      return;
    }
    const parsed = parseZimUrl(omniboxInput.value);
    if ("url" in parsed) {
      navigateFromOmnibox(parsed.url.path);
    }
    // Invalid input: the dropdown already shows the error row.
  }
}

// ── Context menu (§5) ────────────────────────────────────────────

interface MenuItem {
  label: string;
  hint?: string;
  disabled?: boolean;
  danger?: boolean;
  action?: () => void;
}

function contextItems(tab: Tab): MenuItem[] {
  const instanceId = instanceIdOf(tab.path);
  const items: MenuItem[] = [];
  items.push({
    label: "Reload",
    action: () => {
      const pane = paneById.get(tab.id);
      if (pane !== undefined) {
        pane.frame.src = httpForm(tab.path);
      }
    },
  });
  items.push({
    label: "Duplicate tab",
    action: () => {
      newTab(tab.path);
    },
  });
  if (instanceId === null) {
    items.push({
      label: "Copy address",
      action: () => {
        void copyText(canonicalZim(tab.path));
      },
    });
    return items;
  }
  const pane = paneById.get(tab.id);
  const status = pane !== undefined ? pane.status : "unknown";
  const row = instanceCache.get(instanceId);
  const name = row !== undefined ? row.name : instanceId.slice(0, 8);

  for (const surface of ["console", "files", "settings", "backups", "metrics"] as const) {
    items.push({
      label: `Open ${surface} — ${name}`,
      disabled: surface !== "console" && serverSurfaceOf(tab.path) === surface,
      action: () => {
        navigateTab(tab.id, `servers/${instanceId}/${surface}`);
      },
    });
  }
  items.push({ label: "—", disabled: true });
  // §5's lifecycle items, state-gated; permission enforcement stays
  // engine-side — a denied item surfaces the engine's structured
  // error (missing grants) in the toast.
  items.push({
    label: "Start server",
    hint: "control.start",
    disabled: status !== "offline",
    action: () => {
      void powerAction(instanceId, "start", name);
    },
  });
  items.push({
    label: "Stop server",
    hint: "control.stop",
    disabled: status !== "running" && status !== "starting",
    action: () => {
      void powerAction(instanceId, "stop", name);
    },
  });
  items.push({
    label: "Restart server",
    hint: "control.restart",
    disabled: status !== "running",
    action: () => {
      void powerAction(instanceId, "restart", name);
    },
  });
  items.push({ label: "—", disabled: true });
  items.push({
    label: "Open in split view",
    hint: "arrives with M23",
    disabled: true,
  });
  items.push({
    label: "Copy server URL",
    action: () => {
      void copyText(`zim://servers/${instanceId}/console`);
    },
  });
  return items;
}

async function powerAction(instanceId: string, operation: string, name: string): Promise<void> {
  showToast(`${operation} requested for ${name}…`);
  try {
    await power(instanceId, operation);
    showToast(`${operation} accepted for ${name}`);
  } catch (error) {
    if (error instanceof ApiError) {
      const missing = error.missingPermissions.length > 0 ? ` (needs ${error.missingPermissions.join(", ")})` : "";
      showToast(`${error.code}: ${error.message}${missing}`);
    } else {
      showToast(String(error));
    }
  }
}

async function copyText(text: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
    showToast(`copied ${text}`);
  } catch {
    showToast("could not copy — the clipboard needs a focused document");
  }
}

function showContextMenu(tab: Tab, x: number, y: number): void {
  if (menuBox === null) {
    return;
  }
  const fragment = document.createDocumentFragment();
  for (const item of contextItems(tab)) {
    if (item.label === "—") {
      const separator = document.createElement("div");
      separator.className = "menu-separator";
      fragment.append(separator);
      continue;
    }
    const button = document.createElement("button");
    button.type = "button";
    button.className = "menu-item";
    button.setAttribute("role", "menuitem");
    if (item.disabled === true) {
      button.disabled = true;
    }
    if (item.danger === true) {
      button.classList.add("menu-item-danger");
    }
    button.textContent = item.label;
    if (item.hint !== undefined) {
      const hint = document.createElement("span");
      hint.className = "menu-hint";
      hint.textContent = item.hint;
      button.append(hint);
    }
    if (item.action !== undefined && item.disabled !== true) {
      button.addEventListener("click", () => {
        hideMenu();
        item.action?.();
      });
    }
    fragment.append(button);
  }
  menuBox.replaceChildren(fragment);
  // Keep the menu on screen (Chrome clamps to the viewport too).
  const width = 300;
  const left = Math.min(x, window.innerWidth - width - 8);
  const top = Math.min(y, window.innerHeight - 16);
  menuBox.style.left = `${left}px`;
  menuBox.style.top = `${top}px`;
  menuBox.hidden = false;
}

function hideMenu(): void {
  if (menuBox !== null) {
    menuBox.hidden = true;
  }
}

// ── Toast ────────────────────────────────────────────────────────

let toastTimer: number | undefined;

function showToast(text: string): void {
  if (toastBox === null) {
    return;
  }
  toastBox.textContent = text;
  toastBox.hidden = false;
  if (toastTimer !== undefined) {
    window.clearTimeout(toastTimer);
  }
  toastTimer = window.setTimeout(() => {
    toastBox.hidden = true;
  }, 4000);
}

// ── Session persistence (§7) ─────────────────────────────────────

function saveSessionSoon(): void {
  if (saveTimer !== undefined) {
    window.clearTimeout(saveTimer);
  }
  saveTimer = window.setTimeout(() => {
    saveTimer = undefined;
    saveSessionNow();
  }, SAVE_DEBOUNCE_MS);
}

function saveSessionNow(): void {
  try {
    window.localStorage.setItem(SESSION_KEY, JSON.stringify(model.serialize()));
  } catch {
    // Private modes / quota: a session is a nicety, never a crash.
  }
}

// ── Global listeners ─────────────────────────────────────────────

function attachGlobalListeners(): void {
  newTabButton?.addEventListener("click", () => {
    newTab();
  });
  omniboxInput?.addEventListener("focus", () => {
    omniboxInput.select();
    renderSuggestions();
  });
  omniboxInput?.addEventListener("input", renderSuggestions);
  omniboxInput?.addEventListener("keydown", omniboxKeydown);
  omniboxInput?.addEventListener("blur", () => {
    // A click on a suggestion row defers navigation via
    // mousedown + preventDefault, so blur only hides.
    window.setTimeout(hideSuggestions, 120);
    refreshOmniboxDisplay();
  });
  scrollLeftButton?.addEventListener("click", () => {
    strip?.scrollBy({ left: -160, behavior: "smooth" });
  });
  scrollRightButton?.addEventListener("click", () => {
    strip?.scrollBy({ left: 160, behavior: "smooth" });
  });
  strip?.addEventListener("wheel", (event) => {
    if (Math.abs(event.deltaY) > Math.abs(event.deltaX)) {
      strip.scrollLeft += event.deltaY;
      event.preventDefault();
    }
  });
  strip?.addEventListener("keydown", (event) => {
    const target = event.target as HTMLElement;
    if (!target.classList.contains("tab")) {
      return;
    }
    if (event.key === "ArrowRight" || event.key === "ArrowLeft") {
      event.preventDefault();
      const tabs = [...strip.querySelectorAll<HTMLElement>(".tab")];
      const index = tabs.indexOf(target);
      const next = tabs[(index + (event.key === "ArrowRight" ? 1 : -1) + tabs.length) % tabs.length];
      next?.focus();
    }
  });

  // The auth relay from inside panes (login happens on the new-tab
  // page, which is itself tab content): refresh the identity.
  window.addEventListener("message", (event) => {
    if (event.origin !== window.location.origin) {
      return;
    }
    const data = event.data as { type?: string } | null;
    if (data !== null && (data.type === "zim:authenticated" || data.type === "zim:signed-out")) {
      void refreshIdentity();
    }
  });

  // The tray's "New tab" navigates here with ?newtab=1; while the
  // chrome is already loaded, a same-URL navigation reloads — the
  // desktop shell's event channel (when available) avoids that.
  window.addEventListener("zim-new-tab", () => {
    newTab();
  });

  // Menu dismissal + strip overflow live on window/document.
  window.addEventListener("pointerdown", (event) => {
    if (menuBox !== null && !menuBox.hidden && !menuBox.contains(event.target as Node)) {
      hideMenu();
    }
  }, true);
  window.addEventListener("resize", updateOverflow);
  window.addEventListener("keydown", (event) => {
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "t") {
      event.preventDefault();
      newTab();
    } else if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "w") {
      event.preventDefault();
      const active = model.activeId;
      if (active !== null) {
        closeTab(active);
      }
    } else if (event.key === "Escape") {
      hideMenu();
    }
  });
  window.addEventListener("pagehide", saveSessionNow);
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "hidden") {
      saveSessionNow();
    }
  });
}

// ── go ───────────────────────────────────────────────────────────

boot();
