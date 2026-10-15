/**
 * ZIM default frontend — the theme system (M23).
 *
 * Chrome's theme model adapted to ZIM (§12's Themes row): preset
 * rows plus a color pip for the accent, applied live. Themes are
 * the token vocabulary of `FRONTEND_EXTENSION_SPEC.md` §2 — the
 * same dotted names `main.css` already consumes as CSS custom
 * properties — so live application is a CSSOM write of custom
 * properties onto the root element. The `zim://` CSP
 * (`style-src 'self'`) forbids inline `<style>` and markup style
 * attributes, but CSSOM property writes are not subject to it,
 * which is exactly how this stays CSP-clean.
 *
 * The engine-side counterpart of the vocabulary — the values-only
 * contract third-party theme packages validate against — is
 * `zim/contracts/theme-tokens.v1.json` (M23 adds the contract;
 * packaged-theme upload via `POST /api/v1/themes` remains the M24
 * addon milestone's work).
 */
/** localStorage key for the chosen theme. */
export const THEME_KEY = "zim.theme.v1";
/** A `#rrggbb` color — the pip's only accepted shape. */
const HEX_COLOR = /^#[0-9a-fA-F]{6}$/;
/** The default (dark) tokens — exactly `main.css`'s `:root` block,
 * kept in lockstep with it (they are the same values; the CSS block
 * is the no-JS floor, this is the JS-applied floor). */
export const DEFAULT_TOKENS = Object.freeze({
    "color.background": "#14161a",
    "color.surface": "#1d2026",
    "color.surface2": "#24282f",
    "color.text.primary": "#e6e8ec",
    "color.text.secondary": "#9aa1ac",
    "color.accent": "#3b82f6",
    "color.success": "#22c55e",
    "color.warning": "#f59e0b",
    "color.danger": "#ef4444",
    "color.indicator.running": "#22c55e",
    "color.indicator.starting": "#f59e0b",
    "color.indicator.crashed": "#ef4444",
    "color.indicator.stopped": "#3b82f6",
    "color.indicator.unknown": "#9ca3af",
});
/** The shipped presets (Chrome's preset rows, ZIM's palette). */
export const PRESETS = Object.freeze([
    Object.freeze({
        id: "zim-dark",
        name: "ZIM Dark",
        description: "The default: graphite surfaces, blue accent.",
        scheme: "dark",
        tokens: {},
    }),
    Object.freeze({
        id: "midnight",
        name: "Midnight",
        description: "Deeper blue-black with an indigo accent.",
        scheme: "dark",
        tokens: {
            "color.background": "#0c0e16",
            "color.surface": "#131728",
            "color.surface2": "#1b2036",
            "color.accent": "#6366f1",
            "color.indicator.stopped": "#818cf8",
        },
    }),
    Object.freeze({
        id: "forest",
        name: "Forest",
        description: "Green-tinted dark, emerald accent.",
        scheme: "dark",
        tokens: {
            "color.background": "#101613",
            "color.surface": "#16201a",
            "color.surface2": "#1d2a22",
            "color.accent": "#10b981",
            "color.indicator.stopped": "#34d399",
        },
    }),
    Object.freeze({
        id: "ember",
        name: "Ember",
        description: "Warm dark with an amber accent.",
        scheme: "dark",
        tokens: {
            "color.background": "#171310",
            "color.surface": "#201a15",
            "color.surface2": "#2a231c",
            "color.accent": "#f59e0b",
            "color.indicator.stopped": "#fbbf24",
        },
    }),
    Object.freeze({
        id: "paper",
        name: "Paper",
        description: "A light theme: warm paper, ink text.",
        scheme: "light",
        tokens: {
            "color.background": "#f7f5f0",
            "color.surface": "#ffffff",
            "color.surface2": "#eceae3",
            "color.text.primary": "#1c1b18",
            "color.text.secondary": "#5f5c55",
            "color.accent": "#2563eb",
            "color.success": "#16a34a",
            "color.warning": "#d97706",
            "color.danger": "#dc2626",
            "color.indicator.running": "#16a34a",
            "color.indicator.starting": "#d97706",
            "color.indicator.crashed": "#dc2626",
            "color.indicator.stopped": "#2563eb",
            "color.indicator.unknown": "#6b7280",
        },
    }),
]);
/** Look a preset up by id (`null` for unknown ids). */
export function presetById(id) {
    return PRESETS.find((preset) => preset.id === id) ?? null;
}
/** Validate stored input into a `StoredTheme`, or `null` when it is
 * corrupt (the caller falls back to the default preset — a theme is
 * a nicety, the same discipline as the session format). */
export function restoreTheme(stored) {
    if (stored === null || typeof stored !== "object") {
        return null;
    }
    const shape = stored;
    if (shape.v !== 1 || typeof shape.preset !== "string") {
        return null;
    }
    if (presetById(shape.preset) === null) {
        return null;
    }
    const accent = typeof shape.accent === "string" && HEX_COLOR.test(shape.accent)
        ? shape.accent.toLowerCase()
        : undefined;
    // The key is omitted (not `undefined`-valued) so round-trips stay
    // byte-stable for deep-equality checks and storage diffs.
    return accent === undefined ? { v: 1, preset: shape.preset } : { v: 1, preset: shape.preset, accent };
}
/** Resolve the effective tokens: preset overrides on the default
 * floor, then the user's accent pip last (it wins the accent game). */
export function resolveTokens(stored) {
    const preset = presetById(stored.preset) ?? PRESETS[0];
    const tokens = { ...DEFAULT_TOKENS, ...preset.tokens };
    if (stored.accent !== undefined) {
        tokens["color.accent"] = stored.accent;
    }
    return tokens;
}
/** Map a dotted token name onto its CSS custom property. */
export function cssPropertyOf(token) {
    // `color.surface2` → `--color-surface-2` (main.css's spelling).
    return `--${token.replace(/\./g, "-").replace("surface2", "surface-2")}`;
}
/** Apply a theme to the document root, live. CSSOM custom-property
 * writes (not markup style attributes) keep this inside the
 * `zim://` CSP. */
export function applyTokens(tokens, scheme) {
    const root = document.documentElement;
    for (const key of Object.keys(tokens)) {
        root.style.setProperty(cssPropertyOf(key), tokens[key]);
    }
    root.style.setProperty("color-scheme", scheme);
}
/** Apply a stored theme (or the default when absent/corrupt). */
export function applyStoredTheme(stored) {
    const effective = stored ?? { v: 1, preset: PRESETS[0].id };
    const preset = presetById(effective.preset) ?? PRESETS[0];
    applyTokens(resolveTokens(effective), preset.scheme);
}
/** Read + validate + apply, in one call: what every page's boot does. */
export function initThemeFromStorage() {
    let stored = null;
    try {
        const raw = window.localStorage.getItem(THEME_KEY);
        stored = raw === null ? null : JSON.parse(raw);
    }
    catch {
        stored = null;
    }
    const theme = restoreTheme(stored);
    applyStoredTheme(theme);
    return theme;
}
/** Persist a choice (nicety semantics: failures are swallowed). */
export function saveTheme(stored) {
    try {
        window.localStorage.setItem(THEME_KEY, JSON.stringify(stored));
    }
    catch {
        // Private modes / quota: the live application still applies.
    }
}
