/**
 * Test package builders: theme archives, custom frontend packages
 * (signed with the development key, exercising the real trust
 * path), and broken variants for the negative paths. The addon
 * `.zimpkg` used by the extensions test is built by the repo's own
 * authoring script (zim/extensions/scripts/package-addon.mjs), not
 * here — that pipeline is part of what the test verifies.
 */

import { buildZip } from "./zip.js";
import { canonicalJson, devSign } from "./signing.js";

/** A theme package: `zim.theme.json` at the archive root. */
export function themeZip(name: string, tokens: Record<string, unknown>): Buffer {
  const manifest = {
    manifest_version: 1,
    name,
    tokens,
  };
  return buildZip([
    { path: "zim.theme.json", data: JSON.stringify(manifest, null, 2) + "\n" },
  ]);
}

export interface CustomFrontendOptions {
  /** Package id (becomes part of the install identity). */
  id: string;
  /** Version string. */
  version?: string;
  /** Break the package so the upload gate refuses it. */
  broken?: "missing-entrypoint" | "failing-smoke";
}

export interface CustomFrontendPackage {
  /** The ZIP bytes (POST /api/v1/frontend/upload body). */
  bytes: Buffer;
  /** The manifest as uploaded (for assertions). */
  manifest: Record<string, unknown>;
}

/**
 * A complete, gate-passing custom frontend package: one new-tab
 * surface, a CSP-clean page, an API client that calls the identity
 * endpoint, and a dev-key signature over the canonical manifest.
 */
export function customFrontendZip(options: CustomFrontendOptions): CustomFrontendPackage {
  const version = options.version ?? "0.1.0";
  const entrypoint =
    options.broken === "missing-entrypoint" ? "pages/absent.html" : "pages/newtab.html";

  const manifest: Record<string, unknown> = {
    manifest_version: 1,
    id: options.id,
    version,
    name: `Custom ${options.id}`,
    description: "an e2e-test package",
    author: "ZIM E2E",
    license: "Apache-2.0",
    api_version: "v1",
    engines: { zim: ">=0.0.1" },
    full_replacement: false,
    smoke_test: "pages/smoke.html",
    page_surfaces: ["zim://newtab"],
    entrypoints: { newtab: entrypoint, smoke: "pages/smoke.html" },
    assets: ["assets/api.js"],
    permissions: [],
    // serde serializes the absent Option as null — the engine's
    // canonical JSON (the signature's payload) carries it, so the
    // Node-side manifest must too.
    accessibility: null,
  };
  manifest.signature = devSign(manifest);

  const apiClient = options.broken === "failing-smoke"
    ? "export async function ping() { return 1; }\n" // never calls the identity endpoint
    : [
        "export async function whoami() {",
        "  const response = await fetch('/api/v1/users/me', { credentials: 'include' });",
        "  return response.ok ? response.json() : null;",
        "}",
        "",
      ].join("\n");

  const page = `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Custom ${options.id}</title>
</head>
<body>
  <main class="page">
    <h1>Custom frontend ${options.id}</h1>
    <p id="custom-marker">e2e custom package</p>
  </main>
</body>
</html>
`;

  const smoke = `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Custom ${options.id} smoke</title>
  <script type="module" src="assets/api.js"></script>
</head>
<body>
  <main class="page">
    <p id="smoke-result">running</p>
  </main>
</body>
</html>
`;

  const entries = [
    { path: "zim.frontend.json", data: JSON.stringify(manifest, null, 2) + "\n" },
    { path: "pages/newtab.html", data: page },
    { path: "pages/smoke.html", data: smoke },
    { path: "assets/api.js", data: apiClient },
  ];
  if (options.broken === "missing-entrypoint") {
    // The declared entrypoint file is simply not in the archive.
    return { bytes: buildZip(entries.filter((e) => e.path !== "pages/newtab.html")), manifest };
  }
  return { bytes: buildZip(entries), manifest };
}

/** The canonical JSON of a manifest (diagnostics + parity asserts). */
export function manifestCanonicalJson(manifest: Record<string, unknown>): string {
  return canonicalJson(manifest);
}
