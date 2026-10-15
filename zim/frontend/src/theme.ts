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

/** The themeable token set (FRONTEND_EXTENSION_SPEC §2 names). */
export interface ThemeTokens {
  "color.background": string;
  "color.surface": string;
  "color.surface2": string;
  "color.text.primary": string;
  "color.text.secondary": string;
  "color.accent": string;
  "color.success": string;
  "color.warning": string;
  "color.danger": string;
  "color.indicator.running": string;
  "color.indicator.starting": string;
  "color.indicator.crashed": string;
  "color.indicator.stopped": string;
  "color.indicator.unknown": string;
}

/** One preset: a name, a blurb, and the tokens it overrides. */
export interface ThemePreset {
  /** Stable id (persisted; never rename one). */
  id: string;
  /** Human name shown in the preset row. */
  name: string;
  /** One-line description under the name. */
  description: string;
  /** `light` presets flip the color-scheme hint. */
  scheme: "dark" | "light";
  /** Token overrides; anything omitted falls back to the default. */
  tokens: Partial<ThemeTokens>;
}

/** localStorage key for the chosen theme. */
export const THEME_KEY = "zim.theme.v1";

/** A `#rrggbb` color — the pip's only accepted shape. */
const HEX_COLOR = /^#[0-9a-fA-F]{6}$/;

/** The default (dark) tokens — exactly `main.css`'s `:root` block,
 * kept in lockstep with it (they are the same values; the CSS block
 * is the no-JS floor, this is the JS-applied floor). */
export const DEFAULT_TOKENS: Readonly<ThemeTokens> = Object.freeze({
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
export const PRESETS: readonly ThemePreset[] = Object.freeze([
  Object.freeze({
    id: "zim-dark",
    name: "ZIM Dark",
    description: "The default: graphite surfaces, blue accent.",
    scheme: "dark" as const,
    tokens: {},
  }),
  Object.freeze({
    id: "midnight",
    name: "Midnight",
    description: "Deeper blue-black with an indigo accent.",
    scheme: "dark" as const,
    tokens: {
      "color.background": "#0c0e16",
      "color.surface": "#131728",
      "color.surface2": "#1b2036",
      "color.accent": "#6366f1",
      "color.indicator.stopped": "#818cf8",
    } as Partial<ThemeTokens>,
  }),
  Object.freeze({
    id: "forest",
    name: "Forest",
    description: "Green-tinted dark, emerald accent.",
    scheme: "dark" as const,
    tokens: {
      "color.background": "#101613",
      "color.surface": "#16201a",
      "color.surface2": "#1d2a22",
      "color.accent": "#10b981",
      "color.indicator.stopped": "#34d399",
    } as Partial<ThemeTokens>,
  }),
  Object.freeze({
    id: "ember",
    name: "Ember",
    description: "Warm dark with an amber accent.",
    scheme: "dark" as const,
    tokens: {
      "color.background": "#171310",
      "color.surface": "#201a15",
      "color.surface2": "#2a231c",
      "color.accent": "#f59e0b",
      "color.indicator.stopped": "#fbbf24",
    } as Partial<ThemeTokens>,
  }),
  Object.freeze({
    id: "paper",
    name: "Paper",
    description: "A light theme: warm paper, ink text.",
    scheme: "light" as const,
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
    } as Partial<ThemeTokens>,
  }),
]);

/** Look a preset up by id (`null` for unknown ids). */
export function presetById(id: string): ThemePreset | null {
  return PRESETS.find((preset) => preset.id === id) ?? null;
}

/** The persisted shape: a preset choice, or an engine-theme
 * choice (M24: uploaded packages, with a validated token snapshot
 * so every page's synchronous boot applies them without a network
 * round-trip), plus the optional accent pip. */
export interface StoredTheme {
  v: 1;
  preset?: string;
  /** An uploaded engine theme's id (mutually exclusive with
   * `preset`). */
  engine?: string;
  /** The engine theme's token snapshot, validated on restore (the
   * engine validated the package against theme-tokens.v1 at
   * upload; this validates the persisted copy). */
  engineTokens?: Record<string, string>;
  accent?: string;
}

/** Validate stored input into a `StoredTheme`, or `null` when it is
 * corrupt (the caller falls back to the default preset — a theme is
 * a nicety, the same discipline as the session format). */
export function restoreTheme(stored: unknown): StoredTheme | null {
  if (stored === null || typeof stored !== "object") {
    return null;
  }
  const shape = stored as Partial<StoredTheme>;
  if (shape.v !== 1) {
    return null;
  }
  const accent =
    typeof shape.accent === "string" && HEX_COLOR.test(shape.accent)
      ? shape.accent.toLowerCase()
      : undefined;
  // Either an engine theme (id + a valid token snapshot) or one of
  // the shipped presets — never both, never neither.
  if (typeof shape.engine === "string" && shape.engine !== "") {
    if (typeof shape.preset === "string") {
      return null; // Both at once is a contradiction, not a choice.
    }
    const tokens = validateEngineTokens(shape.engineTokens);
    if (tokens === null) {
      return null;
    }
    return accent === undefined
      ? { v: 1, engine: shape.engine, engineTokens: tokens }
      : { v: 1, engine: shape.engine, engineTokens: tokens, accent };
  }
  if (typeof shape.preset !== "string" || presetById(shape.preset) === null) {
    return null;
  }
  // The key is omitted (not `undefined`-valued) so round-trips stay
  // byte-stable for deep-equality checks and storage diffs.
  return accent === undefined ? { v: 1, preset: shape.preset } : { v: 1, preset: shape.preset, accent };
}

/** Validate an engine-theme token snapshot: string keys of
 * `[a-z0-9.]+`, string values, sane lengths (the engine's own
 * caps mirrored — a tampered localStorage blob is corruption, not
 * a theme). */
function validateEngineTokens(tokens: unknown): Record<string, string> | null {
  if (tokens === null || typeof tokens !== "object" || Array.isArray(tokens)) {
    return null;
  }
  const result: Record<string, string> = {};
  for (const [key, value] of Object.entries(tokens as Record<string, unknown>)) {
    if (!/^[a-z0-9.]+$/.test(key) || key.length > 64) {
      return null;
    }
    if (typeof value !== "string" || value.length > 256) {
      return null;
    }
    result[key] = value;
  }
  return result;
}

/** Build the stored form of one uploaded engine theme (the NTP
 * panel's choose-an-engine-theme path). */
export function storedFromEngineTheme(id: string, tokens: Record<string, string>): StoredTheme {
  return { v: 1, engine: id, engineTokens: { ...tokens } };
}

/** Resolve the effective tokens: preset overrides (or the engine
 * snapshot's) on the default floor, then the user's accent pip last
 * (it wins the accent game). The engine path may carry tokens the
 * typed preset floor does not name — they pass through (the CSS
 * custom-property mapping is generic). */
export function resolveTokens(stored: StoredTheme): ThemeTokens {
  const tokens: ThemeTokens =
    stored.engine !== undefined && stored.engineTokens !== undefined
      ? { ...DEFAULT_TOKENS, ...(stored.engineTokens as Partial<ThemeTokens>) }
      : { ...DEFAULT_TOKENS, ...(presetById(stored.preset ?? "") ?? PRESETS[0]).tokens };
  if (stored.accent !== undefined) {
    tokens["color.accent"] = stored.accent;
  }
  return tokens;
}

/** Every token an engine theme carries (the generic map, for
 * applying beyond the typed color floor). */
export function resolveAllTokens(stored: StoredTheme): Record<string, string> {
  if (stored.engine !== undefined && stored.engineTokens !== undefined) {
    const all: Record<string, string> = {
      ...(DEFAULT_TOKENS as unknown as Record<string, string>),
      ...stored.engineTokens,
    };
    if (stored.accent !== undefined) {
      all["color.accent"] = stored.accent;
    }
    return all;
  }
  const tokens = resolveTokens(stored);
  return { ...(tokens as unknown as Record<string, string>) };
}

/** Map a dotted token name onto its CSS custom property. */
export function cssPropertyOf(token: keyof ThemeTokens): string;
export function cssPropertyOf(token: string): string;
export function cssPropertyOf(token: string): string {
  // `color.surface2` → `--color-surface-2` (main.css's spelling).
  return `--${token.replace(/\./g, "-").replace("surface2", "surface-2")}`;
}

/** Apply a theme to the document root, live. CSSOM custom-property
 * writes (not markup style attributes) keep this inside the
 * `zim://` CSP. */
export function applyTokens(tokens: ThemeTokens, scheme: "dark" | "light"): void {
  const root = document.documentElement;
  for (const key of Object.keys(tokens) as (keyof ThemeTokens)[]) {
    root.style.setProperty(cssPropertyOf(key), tokens[key]);
  }
  root.style.setProperty("color-scheme", scheme);
}

/** Apply a full token map (the engine-theme path — every token the
 * package carried, generically mapped, with the scheme inferred
 * from the background's luminance). */
export function applyAllTokens(tokens: Record<string, string>): void {
  const root = document.documentElement;
  for (const key of Object.keys(tokens)) {
    root.style.setProperty(cssPropertyOf(key), tokens[key]);
  }
  root.style.setProperty("color-scheme", schemeOfBackground(tokens["color.background"] ?? ""));
}

/** Infer the color-scheme hint from a background's luminance — an
 * engine theme declares no scheme of its own, but a light
 * background under `dark` scrolling chrome is worse than the
 * inference. */
function schemeOfBackground(background: string): "dark" | "light" {
  const match = /^#([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(background);
  if (match === null) {
    return "dark";
  }
  const [r, g, b] = [Number.parseInt(match[1], 16), Number.parseInt(match[2], 16), Number.parseInt(match[3], 16)];
  const luminance = (0.299 * r + 0.587 * g + 0.114 * b) / 255;
  return luminance > 0.6 ? "light" : "dark";
}

/** Apply a stored theme (or the default when absent/corrupt). */
export function applyStoredTheme(stored: StoredTheme | null): void {
  const effective = stored ?? { v: 1 as const, preset: PRESETS[0].id };
  if (effective.engine !== undefined && effective.engineTokens !== undefined) {
    applyAllTokens(resolveAllTokens(effective));
    return;
  }
  const preset = presetById(effective.preset ?? "") ?? PRESETS[0];
  applyTokens(resolveTokens(effective), preset.scheme);
}

/** Read + validate + apply, in one call: what every page's boot does. */
export function initThemeFromStorage(): StoredTheme | null {
  let stored: unknown = null;
  try {
    const raw = window.localStorage.getItem(THEME_KEY);
    stored = raw === null ? null : (JSON.parse(raw) as unknown);
  } catch {
    stored = null;
  }
  const theme = restoreTheme(stored);
  applyStoredTheme(theme);
  return theme;
}

/** Persist a choice (nicety semantics: failures are swallowed). */
export function saveTheme(stored: StoredTheme): void {
  try {
    window.localStorage.setItem(THEME_KEY, JSON.stringify(stored));
  } catch {
    // Private modes / quota: the live application still applies.
  }
}
