/**
 * ZIM default frontend — the tab model and session format (M22).
 *
 * Pure, DOM-free state for the tab strip: one tab per server or
 * workload view, addressed by `zim://` path (never tab index —
 * BROWSER_INTEGRATION.md §3's stable-identifier rule). The model
 * owns order, activation, and the serialized session; the DOM
 * layer (`app.ts`) renders it and never re-derives tab state.
 *
 * The session format (§7) is deliberately boring: an array of
 * `zim://` URLs plus the active index. Restoration re-opens views
 * only — no lifecycle API is ever called as a side effect of
 * restoring (architecture invariants 3–6, and the roadmap's M22
 * acceptance line).
 */
import { isSafePath } from "./zimurl.js";
/** localStorage key for the persisted tab set (§7). */
export const SESSION_KEY = "zim.session.v1";
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
    /** Serialize the session (§7: `zim://` URLs, stable identifiers). */
    serialize() {
        const activeIndex = this.activeIdInternal === null ? -1 : this.indexOf(this.activeIdInternal);
        return {
            v: 1,
            tabs: this.tabsInternal.map((tab) => tab.path),
            active: activeIndex,
        };
    }
}
/** The default session: one new-tab, active. */
export function defaultSession() {
    return { paths: ["newtab"], active: 0 };
}
/**
 * Validate and revive a persisted session. Corrupt input (wrong
 * shape, unsafe paths) yields `null` — the caller falls back to
 * the default session. Duplicated URLs are legitimate (Chrome
 * allows two tabs on one page; so does ZIM — invariant 6: tabs are
 * views, not processes).
 */
export function restoreSession(stored) {
    if (stored === null || typeof stored !== "object") {
        return null;
    }
    const snapshot = stored;
    if (snapshot.v !== 1 || !Array.isArray(snapshot.tabs) || snapshot.tabs.length === 0) {
        return null;
    }
    if (snapshot.tabs.length > 64) {
        // An absurd tab count is corruption, not a session.
        return null;
    }
    const paths = [];
    for (const entry of snapshot.tabs) {
        if (typeof entry !== "string" || entry.length === 0 || entry.length > 512) {
            return null;
        }
        // The session is machine-written (serialize()), but treat it
        // as untrusted anyway: one bad path voids the session rather
        // than being silently dropped (a dropped middle tab would
        // silently renumber `active`).
        if (!safeStoredPath(entry)) {
            return null;
        }
        paths.push(entry);
    }
    const active = typeof snapshot.active === "number" ? snapshot.active : 0;
    return {
        paths,
        active: Math.min(Math.max(active, 0), paths.length - 1),
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
