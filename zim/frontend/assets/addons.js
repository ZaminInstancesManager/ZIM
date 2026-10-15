/**
 * Addon contributions for the chrome (M24, spec §4.5–§4.7).
 *
 * One read of `GET /api/v1/extensions` behind a promise cache; the
 * app chrome and the console page both consume it:
 *
 * - `addonContextMenuItems(instanceId)` — the §4.6 items with the
 *   manifest's `{id}` placeholder resolved for one instance.
 * - `addonCommands()` — the §4.7 accelerators, normalized for
 *   matching (`Ctrl+Shift+D` → `ctrl+shift+d`).
 * - `addonSidebarItems(instanceId)` — the §4.5 per-instance items.
 *
 * The cache is best-effort: signed-out or unreachable engines yield
 * empty lists (the chrome works exactly as before — addon surfaces
 * simply add nothing). A reload refetches after lifecycle changes.
 */
import { listExtensions } from "./api.js";
/** Enabled addons (empty until the first successful fetch). */
let contributions = [];
let loaded = Promise.resolve();
/** Fetch (once per load unless {@link reloadAddonContributions}). */
export function loadAddonContributions() {
    loaded = listExtensions()
        .then((envelope) => {
        contributions = envelope.data.filter((addon) => addon.enabled);
    })
        .catch(() => {
        // Signed out, or the engine is unreachable: no contributions.
        contributions = [];
    });
    return loaded;
}
/** Force a refetch (after a lifecycle change on the management page
 * in another tab, or on window focus). */
export function reloadAddonContributions() {
    return loadAddonContributions();
}
/** Substitute the manifest's `{id}` placeholder in one page path. */
function resolvePage(page, instanceId) {
    if (page.includes("{id}")) {
        if (instanceId === null) {
            return null; // A per-instance page with no instance: skip.
        }
        return page.replace(new RegExp("{id}", "g"), instanceId);
    }
    return page;
}
/** The §4.6 context-menu contributions for one instance tab. */
export function addonContextMenuItems(instanceId) {
    const items = [];
    for (const addon of contributions) {
        for (const item of addon.context_menu_items) {
            const page = resolvePage(item.page, instanceId);
            if (page !== null) {
                items.push({ label: item.label, page });
            }
        }
    }
    return items;
}
/** The §4.5 sidebar contributions for one instance. */
export function addonSidebarItems(instanceId) {
    const items = [];
    for (const addon of contributions) {
        for (const item of addon.sidebar_items) {
            const page = resolvePage(item.page, instanceId);
            if (page !== null) {
                items.push({ label: item.label, icon: item.icon, page });
            }
        }
    }
    return items;
}
/** The §4.7 commands (accelerators normalized for matching). */
export function addonCommands() {
    const commands = [];
    for (const addon of contributions) {
        const firstSurface = addon.page_surfaces[0];
        if (firstSurface === undefined) {
            continue; // A command with no surface to act on navigates nowhere.
        }
        for (const command of addon.commands) {
            if (command.accelerator === undefined) {
                continue; // No accelerator: the omnibox still reaches the page.
            }
            commands.push({
                addonId: addon.id,
                command,
                normalized: normalizeAccelerator(command.accelerator),
                page: firstSurface,
            });
        }
    }
    return commands;
}
/** Normalize an accelerator for keydown matching: `Ctrl+Shift+D` →
 * `ctrl+shift+d`, with Cmd/Command/Meta treated as the primary
 * modifier (the shell translates per-platform). */
export function normalizeAccelerator(accelerator) {
    const parts = accelerator.split("+").map((part) => part.trim());
    const normalized = parts.map((part, index) => {
        if (index === parts.length - 1) {
            return part.toLowerCase();
        }
        switch (part.toLowerCase()) {
            case "ctrl":
            case "control":
            case "cmd":
            case "command":
            case "meta":
            case "super":
                return "ctrl";
            case "alt":
            case "option":
                return "alt";
            case "shift":
                return "shift";
            default:
                return part.toLowerCase();
        }
    });
    return normalized.join("+");
}
/** The keydown event's own normalized form, in the same vocabulary. */
export function eventAccelerator(event) {
    const parts = [];
    if (event.ctrlKey || event.metaKey) {
        parts.push("ctrl");
    }
    if (event.altKey) {
        parts.push("alt");
    }
    if (event.shiftKey) {
        parts.push("shift");
    }
    const key = event.key.toLowerCase();
    if (key !== "control" && key !== "shift" && key !== "alt" && key !== "meta") {
        parts.push(key);
    }
    return parts.join("+");
}
