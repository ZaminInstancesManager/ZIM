/**
 * The frontend-package smoke-test page (§3.5, step 5).
 *
 * Loads the package's API client and makes exactly one authenticated
 * endpoint call (`GET /api/v1/users/me`), rendering the structured
 * result. The engine's activation gate verifies this page references
 * both; actually executing it in a renderer is the browser side of
 * the gate (M13+).
 */

import { whoami } from "./api";

const result = document.getElementById("smoke-result");

async function run(): Promise<void> {
  if (result === null) {
    return;
  }
  try {
    const me = await whoami();
    result.textContent =
      me !== null
        ? `OK — authenticated as ${me.username} (GET /api/v1/users/me → 200)`
        : "OK — anonymous (GET /api/v1/users/me → 401, structured)";
  } catch (error) {
    result.textContent = `FAIL — ${String(error)}`;
  }
}

void run();
