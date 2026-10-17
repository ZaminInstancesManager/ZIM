/**
 * ZIM default frontend — the omnibox suggestion engine (M22).
 *
 * Pure ranking logic for the address bar's dropdown: static
 * surfaces from the §3 inventory plus the caller's instances
 * (permission-gated upstream — an anonymous session simply has an
 * empty instance list). The DOM layer owns focus, selection, and
 * navigation; this module only answers "what matches this query".
 *
 * Ranking is deliberately boring and explainable, like Chrome's:
 * prefix beats substring, surface beats instance, and a fully
 * typed `zim://` address always wins so the user can navigate by
 * keystroke without touching the mouse.
 */

import { isSafePath } from "./zimurl";

/** One dropdown row. */
export interface Suggestion {
  /** The row's flavor (drives icon + keyboard semantics). */
  kind: "surface" | "instance" | "address" | "info" | "error";
  /** The primary line (row title). */
  title: string;
  /** The secondary line (the `zim://` address, or a hint). */
  subtitle: string;
  /** The `zim://` surface path this row navigates to (`null` for
   * informational and error rows). */
  path: string | null;
}

/** The static surface rows (the §3 inventory the default package
 * actually serves — suggesting an unserved surface would send the
 * user to the engine's 404 page). */
const STATIC_SURFACES: { path: string; title: string; blurb: string }[] = [
  { path: "newtab", title: "New Tab", blurb: "engine overview and quick links" },
  { path: "servers", title: "Servers", blurb: "every instance you can see" },
  { path: "about", title: "About", blurb: "versions and the update channel" },
];

/** The instances the omnibox may suggest (id, name, status — the
 * minimal projection of `api.Instance` this module needs). */
export interface SuggestibleInstance {
  id: string;
  name: string;
}

/** Build the dropdown rows for a query.
 *
 * - Empty query: the default page set (Chrome's NTP-style list)
 *   plus the first few instance consoles.
 * - Non-empty: prefix/substring matches against surfaces and
 *   instance names, the best `zim://` address completions first.
 * - A query that cannot be a `zim://` address at all (unsafe
 *   characters, or a foreign scheme) shows the error row — the
 *   omnibox never guesses (§3).
 */
export function buildSuggestions(
  query: string,
  context: { instances: SuggestibleInstance[]; signedIn: boolean },
): Suggestion[] {
  const trimmed = query.trim();
  if (trimmed === "") {
    return defaultRows(context);
  }

  // A foreign scheme is a hard error row (Chrome shows "search
  // instead"; ZIM has no search engine, so it shows the reason).
  const schemeSplit = trimmed.split("://");
  if (schemeSplit.length >= 2 && schemeSplit[0].toLowerCase() !== "zim") {
    return [errorRow(`${schemeSplit[0]}:// is not a ZIM address — ZIM navigates zim:// surfaces only`)];
  }

  // Unsafe characters (outside the §3 path grammar) likewise.
  const body = (schemeSplit.length >= 2 ? schemeSplit.slice(1).join("://") : trimmed)
    .replace(/^\/+|\/+$/g, "");
  if (body !== "" && !safeForQuery(body)) {
    return [errorRow("not a valid zim:// address — the surface path carries unsupported characters")];
  }

  const needle = body.toLowerCase();
  const rows: Suggestion[] = [];

  // The typed address itself, first (Chrome's "URL-first" rule) —
  // only when it is a complete, well-formed surface path; a
  // half-typed query ("ser") still matches rows below instead.
  if (body !== "" && isSafePath(body)) {
    rows.push({
      kind: "address",
      title: body === "newtab" ? "New Tab" : body,
      subtitle: `zim://${body}`,
      path: body,
    });
  }

  // Static surfaces.
  for (const surface of STATIC_SURFACES) {
    if (matches(surface.title, needle) || matches(surface.path, needle)) {
      rows.push({
        kind: "surface",
        title: surface.title,
        subtitle: `zim://${surface.path} — ${surface.blurb}`,
        path: surface.path,
      });
    }
  }

  // Instances (name matches; the console is the primary surface).
  for (const instance of context.instances) {
    if (matches(instance.name, needle) || matches(instance.id, needle)) {
      rows.push({
        kind: "instance",
        title: instance.name,
        subtitle: `zim://servers/${instance.id}/console`,
        path: `servers/${instance.id}/console`,
      });
    }
  }

  if (rows.length === 0) {
    if (!context.signedIn) {
      return [
        {
          kind: "info",
          title: "no matches",
          subtitle: "sign in on the new-tab page to search your servers",
          path: null,
        },
      ];
    }
    return [
      {
        kind: "info",
        title: "no matches",
        subtitle: "press Enter to open the address anyway",
        path: body !== "" && isSafePath(body) ? body : null,
      },
    ];
  }
  return rows;
}

/** The empty-query rows: surfaces first, then a handful of
 * consoles (Chrome shows most-visited tiles; ZIM's equivalent is
 * the instance list — the NTP's recents rank the tiles since M23). */
function defaultRows(context: { instances: SuggestibleInstance[]; signedIn: boolean }): Suggestion[] {
  const rows: Suggestion[] = STATIC_SURFACES.map((surface) => ({
    kind: "surface" as const,
    title: surface.title,
    subtitle: `zim://${surface.path} — ${surface.blurb}`,
    path: surface.path,
  }));
  for (const instance of context.instances.slice(0, 5)) {
    rows.push({
      kind: "instance",
      title: instance.name,
      subtitle: `zim://servers/${instance.id}/console`,
      path: `servers/${instance.id}/console`,
    });
  }
  if (context.instances.length === 0 && !context.signedIn) {
    rows.push({
      kind: "info",
      title: "sign in to search your servers",
      subtitle: "the new-tab page has the sign-in form",
      path: "newtab",
    });
  }
  return rows;
}

/** Case-insensitive prefix-or-substring match. */
function matches(haystack: string, needle: string): boolean {
  if (needle === "") {
    return true;
  }
  return haystack.toLowerCase().includes(needle);
}

/** The query-side grammar check (mirrors `zimurl.isSafePath`, minus
 * the segment-emptiness strictness the omnibox tolerates while
 * typing). */
function safeForQuery(body: string): boolean {
  for (const char of body) {
    const ok =
      (char >= "a" && char <= "z") ||
      (char >= "A" && char <= "Z") ||
      (char >= "0" && char <= "9") ||
      char === "." ||
      char === "_" ||
      char === "~" ||
      char === "/" ||
      char === "-";
    if (!ok) {
      return false;
    }
  }
  return !body.split("/").includes("..");
}

function errorRow(message: string): Suggestion {
  return { kind: "error", title: message, subtitle: "", path: null };
}
