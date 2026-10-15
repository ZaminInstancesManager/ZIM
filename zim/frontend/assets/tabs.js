/**
 * ZIM default frontend — the tab model and session formats (M22,
 * M23).
 *
 * Pure, DOM-free state for the tab strip: one tab per server or
 * workload view, addressed by `zim://` path (never tab index —
 * BROWSER_INTEGRATION.md §3's stable-identifier rule). The model
 * owns order, activation, and the serialized session; the DOM
 * layer (`app.ts`) renders it and never re-derives tab state.
 *
 * The session format (§7) is deliberately boring: an array of
 * tab entries (a `zim://` path, plus — since M23 — an optional
 * split-view pairing) and the active index. Restoration re-opens
 * views only — no lifecycle API is ever called as a side effect
 * of restoring (architecture invariants 3–6, and the roadmap's
 * acceptance lines).
 *
 * Format history: `zim.session.v1` (M22) stored bare path strings;
 * `zim.session.v2` (M23) stores `{ path, split? }` entries. The
 * v1 key is still read for upgrade; every write is v2.
 */
import { isSafePath } from "./zimurl.js";
import { clampRatio } from "./split.js";
/** localStorage key for the persisted tab set (v2, M23). */
export const SESSION_KEY = "zim.session.v2";
/** The M22 key, read once more on upgrade (then superseded). */
export const LEGACY_SESSION_KEY = "zim.session.v1";
/** The per-window session key: the chrome's main window uses the
 * primary key; a torn-off window (M23 multi-window) keeps its own
 * tab set under a window-scoped key, so windows never clobber each
 * other's sessions on the shared origin. Labels are shell-issued
 * (`chrome-1`, …); an untrusted/odd label falls back to the primary
 * key rather than reaching deeper into storage. */
export function sessionKeyForWindow(label) {
    if (label === null || !/^[A-Za-z0-9-]{1,64}$/.test(label) || label === "main") {
        return SESSION_KEY;
    }
    return `${SESSION_KEY}.w:${label}`;
}
/** The tab strip's state machine. */
export class TabModel {
    constructor() {
        this.tabsInternal = [];
        this.activeIdInternal = null;
        this.nextId = 1;
    }
    /** The tabs, in strip order (a fresh array each call). */
    get tabs() {
        return [...this.tabsInternal];
    }
    /** The active tab's id (`null` only before the first tab). */
    get activeId() {
        return this.activeIdInternal;
    }
    /** The active tab, if any. */
    get active() {
        return this.byId(this.activeIdInternal);
    }
    /** The number of tabs. */
    get size() {
        return this.tabsInternal.length;
    }
    /** Look one tab up by id. */
    byId(id) {
        if (id === null) {
            return null;
        }
        return this.tabsInternal.find((tab) => tab.id === id) ?? null;
    }
    /** The strip position of a tab, `-1` when absent. */
    indexOf(id) {
        return this.tabsInternal.findIndex((tab) => tab.id === id);
    }
    /**
     * Open a tab for a `zim://` path. Chrome semantics: the new tab
     * opens immediately right of the active tab (or at the end) and
     * becomes active unless told otherwise.
     */
    add(path, options = {}) {
        const activate = options.activate ?? true;
        const anchor = this.indexOf(this.activeIdInternal ?? -1);
        const at = options.index ?? (anchor >= 0 ? anchor + 1 : this.tabsInternal.length);
        const tab = { id: this.nextId++, path };
        if (options.split !== undefined) {
            tab.split = options.split;
        }
        this.tabsInternal.splice(Math.min(Math.max(at, 0), this.tabsInternal.length), 0, tab);
        if (activate) {
            this.activeIdInternal = tab.id;
        }
        return tab;
    }
    /**
     * Close a tab. Chrome semantics for the follow-up activation:
     * the right neighbor inherits focus; at the strip's right edge,
     * the left neighbor does. Closing the last tab is the caller's
     * policy (the app chrome keeps one `newtab` alive).
     *
     * Returns the id of the tab that should become active (the
     * caller activates it), or `null` when the strip is now empty.
     */
    close(id) {
        const index = this.indexOf(id);
        if (index < 0) {
            return this.activeIdInternal;
        }
        this.tabsInternal.splice(index, 1);
        if (this.activeIdInternal === id) {
            const neighbor = this.tabsInternal[Math.min(index, this.tabsInternal.length - 1)];
            this.activeIdInternal = neighbor !== undefined ? neighbor.id : null;
        }
        return this.activeIdInternal;
    }
    /** Move a tab (drag-reorder) from its current position to `to`. */
    move(id, to) {
        const from = this.indexOf(id);
        if (from < 0) {
            return;
        }
        const [tab] = this.tabsInternal.splice(from, 1);
        const clamped = Math.min(Math.max(to, 0), this.tabsInternal.length);
        this.tabsInternal.splice(clamped, 0, tab);
    }
    /** Activate a tab (click, keyboard, programmatic). */
    activate(id) {
        if (this.indexOf(id) >= 0) {
            this.activeIdInternal = id;
        }
    }
    /**
     * Update a tab's path (omnibox navigation, or pane resync after
     * an in-page navigation). Returns whether anything changed.
     */
    updatePath(id, path) {
        const tab = this.byId(id);
        if (tab === null || tab.path === path) {
            return false;
        }
        tab.path = path;
        return true;
    }
    /** The tab's split pairing, when it hosts two panes. */
    splitOf(id) {
        return this.byId(id)?.split ?? null;
    }
    /**
     * Set (or clear) a tab's split. Setting a split on an already
     * split tab re-pairs it (the old secondary pane is the caller's
     * business — it unsubscribes before this is called). Returns
     * whether anything changed.
     */
    setSplit(id, split) {
        const tab = this.byId(id);
        if (tab === null) {
            return false;
        }
        if (split === null) {
            if (tab.split === undefined) {
                return false;
            }
            delete tab.split;
            return true;
        }
        const clamped = clampRatio(split.ratio);
        if (tab.split !== undefined && tab.split.path === split.path && tab.split.ratio === clamped) {
            return false;
        }
        tab.split = { path: split.path, ratio: clamped };
        return true;
    }
    /**
     * Collapse a split to its secondary pane: the secondary surface
     * becomes the tab's whole content (Chrome's "the surviving pane
     * takes over"). Returns whether anything changed.
     */
    collapseSplitToSecondary(id) {
        const tab = this.byId(id);
        if (tab === null || tab.split === undefined) {
            return false;
        }
        tab.path = tab.split.path;
        delete tab.split;
        return true;
    }
    /**
     * Update the secondary pane's path (in-pane navigation inside a
     * split). Returns whether anything changed.
     */
    updateSplitPath(id, path) {
        const tab = this.byId(id);
        if (tab === null || tab.split === undefined || tab.split.path === path) {
            return false;
        }
        tab.split.path = path;
        return true;
    }
    /**
     * Update the divider position (drag / snap / double-click reset).
     * The ratio is clamped here — the drag math may overshoot. Returns
     * whether anything changed.
     */
    updateSplitRatio(id, ratio) {
        const tab = this.byId(id);
        if (tab === null || tab.split === undefined) {
            return false;
        }
        const clamped = clampRatio(ratio);
        if (tab.split.ratio === clamped) {
            return false;
        }
        tab.split.ratio = clamped;
        return true;
    }
    /** Serialize the session (§7: `zim://` URLs, stable identifiers). */
    serialize() {
        const activeIndex = this.activeIdInternal === null ? -1 : this.indexOf(this.activeIdInternal);
        return {
            v: 2,
            tabs: this.tabsInternal.map((tab) => {
                const entry = { path: tab.path };
                if (tab.split !== undefined) {
                    // Three decimals: divider positions are visual, and tidy
                    // numbers keep debounced writes diffable.
                    entry.split = { path: tab.split.path, ratio: Math.round(tab.split.ratio * 1000) / 1000 };
                }
                return entry;
            }),
            active: activeIndex,
        };
    }
}
/** The default session: one new-tab, active. */
export function defaultSession() {
    return { tabs: [{ path: "newtab" }], active: 0 };
}
/**
 * Validate and revive a persisted session (v2 or legacy v1).
 * Corrupt input (wrong shape, unsafe paths) yields `null` — the
 * caller falls back to the default session. Duplicated URLs are
 * legitimate (Chrome allows two tabs on one page; so does ZIM —
 * invariant 6: tabs are views, not processes).
 */
export function restoreSession(stored) {
    if (stored === null || typeof stored !== "object") {
        return null;
    }
    // Validated field-by-field on purpose: the snapshot is machine-
    // written but treated as untrusted, so the loose read comes
    // first and every field proves itself below.
    const snapshot = stored;
    const version = snapshot.v;
    if (version !== 1 && version !== 2) {
        return null;
    }
    if (!Array.isArray(snapshot.tabs) || snapshot.tabs.length === 0) {
        return null;
    }
    if (snapshot.tabs.length > 64) {
        // An absurd tab count is corruption, not a session.
        return null;
    }
    const tabs = [];
    for (const entry of snapshot.tabs) {
        if (version === 1) {
            // Legacy: bare path strings (no splits existed in M22).
            if (typeof entry !== "string" || !safeStoredPath(entry)) {
                return null;
            }
            tabs.push({ path: entry });
            continue;
        }
        if (entry === null || typeof entry !== "object" || Array.isArray(entry)) {
            return null;
        }
        const shape = entry;
        if (typeof shape.path !== "string" || !safeStoredPath(shape.path)) {
            return null;
        }
        const tab = { path: shape.path };
        if (shape.split !== undefined) {
            // Same discipline as paths: one bad split voids the session
            // (a missing pane would silently change what restores).
            if (shape.split === null || typeof shape.split !== "object") {
                return null;
            }
            const split = shape.split;
            if (typeof split.path !== "string" || !safeStoredPath(split.path)) {
                return null;
            }
            if (typeof split.ratio !== "number" || !Number.isFinite(split.ratio)) {
                return null;
            }
            // Ratio drift is not corruption — clamp, don't reject.
            tab.split = { path: split.path, ratio: clampRatio(split.ratio) };
        }
        tabs.push(tab);
    }
    const activeRaw = snapshot.active;
    const active = typeof activeRaw === "number" ? activeRaw : 0;
    return {
        tabs,
        active: Math.min(Math.max(active, 0), tabs.length - 1),
    };
}
/** Session entries carry bare paths (no scheme), so the predicate
 * trims and reuses the shared `zim://` path grammar. */
function safeStoredPath(path) {
    const trimmed = path.replace(/^\/+|\/+$/g, "");
    if (trimmed === "" || trimmed === "newtab") {
        return true;
    }
    return isSafePath(trimmed);
}
