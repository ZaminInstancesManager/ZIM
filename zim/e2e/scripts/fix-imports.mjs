/**
 * Build post-step: give every relative import an explicit `.js`
 * extension.
 *
 * Why: `tsc` emits module specifiers verbatim, and the sources use
 * extensionless specifiers (`import { x } from "./api"`), which is
 * fine for the type checker but NOT for real ES-module hosts — a
 * browser resolving `import "./api"` inside `<script type="module">`
 * requests `/zim/assets/api` and 404s. The compiled assets are what
 * the engine embeds and serves, so the emitted files must carry
 * loadable specifiers. (Surfaced by M22's first real page-load of
 * the app chrome; the fix is mechanical and auditable.)
 *
 * Usage: node scripts/fix-imports.mjs <dir>...
 */

import { readdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { join } from "node:path";

for (const root of process.argv.slice(2)) {
  for (const file of walk(root)) {
    if (!file.endsWith(".js")) {
      continue;
    }
    const before = readFileSync(file, "utf8");
    const after = before.replace(
      /(\bfrom\s*|\bimport\s*)(["'])(\.\.?\/[^"']+)\2/g,
      (match, keyword, quote, specifier) => {
        if (specifier.endsWith(".js")) {
          return match;
        }
        return `${keyword}${quote}${specifier}.js${quote}`;
      },
    );
    if (after !== before) {
      writeFileSync(file, after);
      console.log(`fixed specifiers: ${file}`);
    }
  }
}

/** Depth-first file list (stdlib-only, so the frontend stays
 * dependency-free even in its tooling). */
function walk(dir) {
  const out = [];
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) {
      out.push(...walk(full));
    } else {
      out.push(full);
    }
  }
  return out;
}
