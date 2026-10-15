/**
 * Unit tests for the `zim://` URL utilities (the omnibox, session
 * format, and pane navigation all speak this dialect).
 */

import test from "node:test";
import assert from "node:assert/strict";

import {
  canonicalZim,
  httpForm,
  instanceIdOf,
  isSafePath,
  parseZimUrl,
  pathFromLocation,
  serverSurfaceOf,
  staticTitle,
} from "../src/zimurl";

test("parse accepts scheme'd and bare forms", () => {
  assert.deepStrictEqual(parseZimUrl("zim://servers/abc/console"), { url: { path: "servers/abc/console" } });
  assert.deepStrictEqual(parseZimUrl("servers"), { url: { path: "servers" } });
  assert.deepStrictEqual(parseZimUrl("ZIM://Servers"), { url: { path: "Servers" } });
});

test("the bare namespace is the new-tab surface", () => {
  assert.deepStrictEqual(parseZimUrl("zim://"), { url: { path: "newtab" } });
  assert.deepStrictEqual(parseZimUrl("zim://newtab/"), { url: { path: "newtab" } });
  assert.deepStrictEqual(parseZimUrl(""), { url: { path: "newtab" } });
});

test("foreign schemes are rejected as not-zim", () => {
  assert.deepStrictEqual(parseZimUrl("https://127.0.0.1:8443/zim/"), { error: "not-zim" });
  assert.deepStrictEqual(parseZimUrl("http://example.test/"), { error: "not-zim" });
});

test("unsafe paths are rejected, never guessed", () => {
  for (const bad of [
    "zim://../etc/passwd",
    "zim://servers/../../x",
    "zim://servers?x=1",
    "zim://servers#f",
    "zim://servers/%2e%2e",
    "zim://servers/with space",
    "zim://servers/semi;colon",
  ]) {
    assert.deepStrictEqual(parseZimUrl(bad), { error: "unsafe-path" }, bad);
  }
});

test("isSafePath mirrors the shell's grammar", () => {
  assert.equal(isSafePath("servers"), true);
  assert.equal(isSafePath("servers/9f2c/console"), true);
  assert.equal(isSafePath("servers/a.b_c~d-e"), true);
  assert.equal(isSafePath(""), false);
  assert.equal(isSafePath("/leading"), false);
  assert.equal(isSafePath("trailing/"), false);
  assert.equal(isSafePath("a//b"), false);
  assert.equal(isSafePath("a/../b"), false);
  assert.equal(isSafePath("a?b"), false);
});

test("canonicalZim produces the display form", () => {
  assert.equal(canonicalZim("servers/9f2c/console"), "zim://servers/9f2c/console");
  assert.equal(canonicalZim("newtab"), "zim://newtab");
});

test("httpForm maps newtab to the page root, others verbatim", () => {
  assert.equal(httpForm("newtab"), "/zim/");
  assert.equal(httpForm("servers"), "/zim/servers");
  assert.equal(httpForm("servers/9f2c/console"), "/zim/servers/9f2c/console");
});

test("pathFromLocation derives the surface from a pane's address", () => {
  const fake = (pathname: string) => ({ pathname } as Location);
  assert.equal(pathFromLocation(fake("/zim/")), "newtab");
  assert.equal(pathFromLocation(fake("/zim/servers/abc/console")), "servers/abc/console");
  assert.equal(pathFromLocation(fake("/api/v1/instances")), null);
});

test("instanceIdOf and serverSurfaceOf extract the address parts", () => {
  assert.equal(instanceIdOf("servers/9f2c/console"), "9f2c");
  assert.equal(instanceIdOf("servers/9f2c/files"), "9f2c");
  assert.equal(instanceIdOf("servers"), null);
  assert.equal(instanceIdOf("newtab"), null);
  assert.equal(serverSurfaceOf("servers/9f2c/console"), "console");
  assert.equal(serverSurfaceOf("servers/9f2c"), null);
  assert.equal(serverSurfaceOf("newtab"), null);
});

test("staticTitle labels the surfaces", () => {
  assert.equal(staticTitle("newtab"), "New Tab");
  assert.equal(staticTitle("servers"), "Servers");
  assert.equal(staticTitle("servers/x/files"), "Files");
  assert.equal(staticTitle("servers/x/metrics"), "Metrics");
  assert.equal(staticTitle("unknown/path"), "unknown/path");
});
