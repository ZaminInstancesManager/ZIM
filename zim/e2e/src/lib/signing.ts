/**
 * Development-manifest signing, in Node, bit-for-bit compatible
 * with the engine's `dev_sign` (frontend-loader/src/signature.rs):
 * ed25519 over the manifest's canonical JSON — the serde_json
 * serialization of the manifest with the `signature` member
 * removed, which for `serde_json::Value` means recursively
 * *sorted* object keys and compact separators.
 *
 * The development keypair is committed by design (see
 * zim/engine/crates/frontend-loader/keys/README.md): it carries no
 * release authority and exists so CI exercises the trust path
 * end-to-end. Node's crypto signs raw ed25519 from a PKCS8-wrapped
 * 32-byte seed — the wrap is the standard 16-byte prefix.
 */

import { createPrivateKey, sign as cryptoSign } from "node:crypto";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { repoRoot } from "./util.js";

/** The signature block's shape (FRONTEND_EXTENSION_SPEC §3.4/§4.1). */
export interface SignatureBlock {
  algorithm: "ed25519";
  public_key: string;
  value: string;
}

/** The committed development seed (base64, 32 bytes). */
function devSeed(): string {
  return readFileSync(
    join(repoRoot(), "zim", "engine", "crates", "frontend-loader", "keys", "zaminmc-dev.seed"),
    "utf8",
  ).trim();
}

/** The committed development public key (base64, 32 bytes). */
export function devPublicKey(): string {
  return readFileSync(
    join(repoRoot(), "zim", "engine", "crates", "frontend-loader", "keys", "zaminmc-dev.pub"),
    "utf8",
  ).trim();
}

/**
 * Canonicalize like serde_json's default `to_string`: object keys
 * sorted recursively (serde_json::Value maps are BTreeMaps), arrays
 * in order, `JSON.stringify`-style compact separators. Numbers keep
 * their JS string form, which matches serde for the integers the
 * manifests carry.
 */
export function canonicalJson(value: unknown): string {
  return JSON.stringify(sortDeep(value));
}

function sortDeep(value: unknown): unknown {
  if (Array.isArray(value)) {
    return value.map(sortDeep);
  }
  if (value !== null && typeof value === "object") {
    const record = value as Record<string, unknown>;
    const out: Record<string, unknown> = {};
    for (const key of Object.keys(record).sort()) {
      out[key] = sortDeep(record[key]);
    }
    return out;
  }
  return value;
}

/** Sign a manifest object (WITHOUT its signature member) and return the block. */
export function devSign(manifest: Record<string, unknown>): SignatureBlock {
  const unsigned: Record<string, unknown> = { ...manifest };
  delete unsigned.signature;
  const value = signCanonical(canonicalJson(unsigned));
  return { algorithm: "ed25519", public_key: devPublicKey(), value };
}

/** ed25519 over a UTF-8 canonical string, base64 raw 64-byte output. */
function signCanonical(canonical: string): string {
  const seed = Buffer.from(devSeed(), "base64");
  if (seed.length !== 32) {
    throw new Error("dev seed is not 32 bytes");
  }
  // PKCS8 wrap for a raw ed25519 seed: version + algorithm id + OCTET STRING(seed).
  const der = Buffer.concat([
    Buffer.from("302e020100300506032b657004220420", "hex"),
    seed,
  ]);
  const key = createPrivateKey({ key: der, format: "der", type: "pkcs8" });
  const signature = cryptoSign(null, Buffer.from(canonical, "utf8"), key);
  return signature.toString("base64");
}
