/**
 * ZIM default frontend — the `zim://` URL utilities (M22).
 *
 * The TypeScript mirror of the shell's `zim/shell/src/zim_url.rs`
 * (AD-23): `zim://` URLs are stable identifiers, never loadable
 * URLs. This module owns the identifier side — parsing,
 * canonicalizing, validating, and deriving the loadable `/zim/…`
 * HTTP form — so the address bar, the tab model, session
 * persistence, and tab-to-pane navigation all speak one dialect.
 *
 * The path-safety predicate mirrors the shell's exactly
 * (`[A-Za-z0-9._~/-]`, no dot-dot traversal): the omnibox must
 * reject a bad address with a visible error, never guess
 * (BROWSER_INTEGRATION.md §3).
 */
/** Parse user input into a `zim://` identifier.
 *
 * Accepts `zim://servers` and bare `servers` (the omnibox's
 * scheme-less convenience, like Chrome accepting an origin without
 * `https://`). The scheme match is case-insensitive. Returns the
 * failure reason for omnibox error states.
 */
export function parseZimUrl(input) {
    let rest = input.trim();
    const schemeSplit = rest.split("://");
    if (schemeSplit.length >= 2) {
        const scheme = schemeSplit[0];
        if (scheme.toLowerCase() !== "zim") {
            return { error: "not-zim" };
        }
        rest = schemeSplit.slice(1).join("://");
    }
    const path = rest.replace(/^\/+|\/+$/g, "");
    if (path === "") {
        // The bare namespace is the new-tab page (§3).
        return { url: { path: "newtab" } };
    }
    if (!isSafePath(path)) {
        return { error: "unsafe-path" };
    }
    return { url: { path } };
}
/** The canonical display form (what the address bar shows, AD-23):
 * `zim://` + the path, trailing slashes trimmed. */
export function canonicalZim(path) {
    return `zim://${path.replace(/^\/+|\/+$/g, "")}`;
}
/** The loadable same-origin HTTP form (the loader's URL space):
 * `zim://newtab` → `/zim/` (the NTP-replacement rule, §8); every
 * other surface → `/zim/{path}` verbatim. */
export function httpForm(path) {
    const trimmed = path.replace(/^\/+|\/+$/g, "");
    if (trimmed === "" || trimmed === "newtab") {
        return "/zim/";
    }
    return `/zim/${trimmed}`;
}
/** Derive the `zim://` path from a page's own location (pane
 * resync: an iframe that navigated internally reports where it
 * ended up). Returns `null` for non-`/zim` locations. */
export function pathFromLocation(location) {
    if (!location.pathname.startsWith("/zim")) {
        return null;
    }
    let rest = location.pathname.slice("/zim".length);
    rest = rest.replace(/^\/+|\/+$/g, "");
    return rest === "" ? "newtab" : rest;
}
/** The instance id a server surface addresses, when it addresses
 * one (`servers/{id}/console` → `{id}`); `null` otherwise. */
export function instanceIdOf(path) {
    const match = path.match(/^servers\/([^/]+)(?:\/|$)/);
    return match !== null ? match[1] : null;
}
/** The per-instance surface name (`servers/{id}/files` → `files`);
 * `null` for the bare row surface. */
export function serverSurfaceOf(path) {
    const match = path.match(/^servers\/[^/]+\/([^/]+)$/);
    return match !== null ? match[1] : null;
}
/** The path-safety predicate (mirrors `zim_url.rs`): path segments
 * confined to `[A-Za-z0-9._~/-]`, no empty-leading-trailing slash
 * weirdness after trimming, no `..` segment. */
export function isSafePath(path) {
    if (path.length === 0 || path.startsWith("/") || path.endsWith("/")) {
        return false;
    }
    for (const segment of path.split("/")) {
        if (segment === ".." || segment === "") {
            return false;
        }
        for (const char of segment) {
            const ok = (char >= "a" && char <= "z") ||
                (char >= "A" && char <= "Z") ||
                (char >= "0" && char <= "9") ||
                char === "." ||
                char === "_" ||
                char === "~" ||
                char === "-";
            if (!ok) {
                return false;
            }
        }
    }
    return true;
}
/** The display title for a surface path before instance names
 * resolve (and forever, for the static surfaces). */
export function staticTitle(path) {
    const surface = serverSurfaceOf(path);
    switch (path) {
        case "newtab":
            return "New Tab";
        case "servers":
            return "Servers";
        case "about":
            return "About";
        case "app":
            return "ZIM";
        default:
            break;
    }
    if (surface !== null) {
        const labels = {
            console: "Console",
            files: "Files",
            settings: "Settings",
            backups: "Backups",
            schedules: "Schedules",
            subusers: "Subusers",
            metrics: "Metrics",
        };
        return labels[surface] ?? surface;
    }
    return path;
}
