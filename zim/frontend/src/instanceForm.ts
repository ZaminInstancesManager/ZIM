/**
 * ZIM default frontend — the create/import form model (M23 NTP).
 *
 * Pure parsing and validation for the new-tab page's two instance
 * flows, so the dialog DOM (`newtab.ts`) stays thin and the rules
 * stay unit-tested:
 *
 * - **Create** (`formFromCreate`): name + node + optional fields →
 *   the engine's `POST /api/v1/instances` request shape.
 * - **Import** (`parseInstanceJson`): paste or upload a ZIM server
 *   definition (JSON) → the same request shape. Importing a
 *   *Pterodactyl panel* is a different, CLI-shaped flow
 *   (`zim-migrate-pterodactyl`); the NTP says so honestly rather
 *   than pretending a paste box migrates a database.
 *
 * Everything here mirrors the engine's `CreateInstanceRequest`
 * handlers/instances.rs contract; unknown fields are ignored, and
 * validation failures are field-attributed so the form can mark
 * the offending input.
 */

/** The engine's `POST /instances` request body (M5 contract). */
export interface CreateInstanceRequest {
  name: string;
  node_id: string;
  owner_id?: string;
  egg_id?: string;
  allocation_id?: number;
  limits?: Record<string, unknown>;
  startup?: string;
  image?: string;
  description?: string;
  external_id?: string;
  allocation_limit?: number;
  database_limit?: number;
  backup_limit?: number;
  crash_detection_enabled?: boolean;
  kind?: string;
}

/** One field-level validation problem. */
export interface FormProblem {
  field: "name" | "node_id" | "json" | "limits";
  message: string;
}

/** The longest name/import payload we will even look at. */
export const MAX_JSON_BYTES = 64 * 1024;

/** Trim + validate the human fields shared by create and import. */
function validateCommon(request: CreateInstanceRequest): FormProblem[] {
  const problems: FormProblem[] = [];
  if (request.name.length === 0) {
    problems.push({ field: "name", message: "a name is required" });
  } else if (request.name.length > 190) {
    problems.push({ field: "name", message: "the engine caps names at 190 characters" });
  }
  if (request.node_id.length === 0) {
    problems.push({ field: "node_id", message: "pick a node — the engine needs one" });
  }
  return problems;
}

/** Build the create request from raw form input (all strings, as
 * DOM gives them); returns the request plus any problems. The
 * caller decides whether problems block submission. */
export function formFromCreate(input: {
  name: string;
  node_id: string;
  description?: string;
  startup?: string;
  image?: string;
}): { request: CreateInstanceRequest; problems: FormProblem[] } {
  const request: CreateInstanceRequest = {
    name: input.name.trim(),
    node_id: input.node_id.trim(),
  };
  if (input.description !== undefined && input.description.trim() !== "") {
    request.description = input.description.trim();
  }
  if (input.startup !== undefined && input.startup.trim() !== "") {
    request.startup = input.startup.trim();
  }
  if (input.image !== undefined && input.image.trim() !== "") {
    request.image = input.image.trim();
  }
  return { request, problems: validateCommon(request) };
}

/** Parse an imported server definition (JSON text) into the create
 * request shape. Only the engine-accepted fields survive; `limits`
 * must be an object when present. */
export function parseInstanceJson(
  text: string,
  defaultNodeId: string,
): { request: CreateInstanceRequest | null; problems: FormProblem[] } {
  if (text.trim().length === 0) {
    return { request: null, problems: [{ field: "json", message: "paste a server definition first" }] };
  }
  if (text.length > MAX_JSON_BYTES) {
    return { request: null, problems: [{ field: "json", message: "that definition is too large to be one server" }] };
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(text) as unknown;
  } catch (error) {
    return {
      request: null,
      problems: [{ field: "json", message: `not valid JSON: ${String(error instanceof Error ? error.message : error)}` }],
    };
  }
  if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) {
    return { request: null, problems: [{ field: "json", message: "the definition must be a JSON object" }] };
  }
  const source = parsed as Record<string, unknown>;
  const request: CreateInstanceRequest = {
    name: typeof source.name === "string" ? source.name.trim() : "",
    // An imported definition may omit the node; the dialog's picked
    // node stands in (the engine needs one either way).
    node_id: typeof source.node_id === "string" && source.node_id.trim() !== "" ? source.node_id.trim() : defaultNodeId.trim(),
  };
  if (typeof source.description === "string" && source.description.trim() !== "") {
    request.description = source.description.trim();
  }
  if (typeof source.egg_id === "string" && source.egg_id.trim() !== "") {
    request.egg_id = source.egg_id.trim();
  }
  if (typeof source.startup === "string" && source.startup.trim() !== "") {
    request.startup = source.startup.trim();
  }
  if (typeof source.image === "string" && source.image.trim() !== "") {
    request.image = source.image.trim();
  }
  if (typeof source.external_id === "string" && source.external_id.trim() !== "") {
    request.external_id = source.external_id.trim();
  }
  if (source.limits !== undefined) {
    if (source.limits === null || typeof source.limits !== "object" || Array.isArray(source.limits)) {
      return { request: null, problems: [{ field: "limits", message: "limits must be an object of resource values" }] };
    }
    request.limits = source.limits as Record<string, unknown>;
  }
  if (typeof source.allocation_limit === "number" && Number.isFinite(source.allocation_limit)) {
    request.allocation_limit = source.allocation_limit;
  }
  if (typeof source.database_limit === "number" && Number.isFinite(source.database_limit)) {
    request.database_limit = source.database_limit;
  }
  if (typeof source.backup_limit === "number" && Number.isFinite(source.backup_limit)) {
    request.backup_limit = source.backup_limit;
  }
  if (typeof source.crash_detection_enabled === "boolean") {
    request.crash_detection_enabled = source.crash_detection_enabled;
  }
  return { request, problems: validateCommon(request) };
}

/** The NTP search/filter predicate: a server matches when its name
 * or id contains the query (case-insensitive, trimmed). An empty
 * query matches everything. */
export function serverMatchesQuery(name: string, id: string, query: string): boolean {
  const q = query.trim().toLowerCase();
  if (q === "") {
    return true;
  }
  return name.toLowerCase().includes(q) || id.toLowerCase().includes(q);
}
