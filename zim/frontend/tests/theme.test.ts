/**
 * Unit tests for the theme system (M23): preset resolution, stored-
 * theme validation, the accent pip, and the token→CSS-property map.
 */

import test from "node:test";
import assert from "node:assert/strict";

import {
  DEFAULT_TOKENS,
  PRESETS,
  THEME_KEY,
  cssPropertyOf,
  presetById,
  resolveTokens,
  restoreTheme,
} from "../src/theme";

test("the default preset resolves to exactly the default tokens", () => {
  const tokens = resolveTokens({ v: 1, preset: "zim-dark" });
  assert.deepStrictEqual(tokens, { ...DEFAULT_TOKENS });
});

test("presets override on the default floor, never under it", () => {
  const midnight = presetById("midnight");
  assert.notEqual(midnight, null);
  const tokens = resolveTokens({ v: 1, preset: "midnight" });
  assert.equal(tokens["color.background"], midnight!.tokens["color.background"]);
  assert.equal(tokens["color.success"], DEFAULT_TOKENS["color.success"]); // untouched
});

test("every preset's ids are unique and every one resolves", () => {
  const ids = PRESETS.map((preset) => preset.id);
  assert.equal(new Set(ids).size, ids.length);
  for (const id of ids) {
    assert.notEqual(presetById(id), null);
  }
  assert.equal(presetById("nope"), null);
});

test("the accent pip wins the accent game and only that", () => {
  const tokens = resolveTokens({ v: 1, preset: "midnight", accent: "#ff00ff" });
  assert.equal(tokens["color.accent"], "#ff00ff");
  assert.equal(tokens["color.background"], presetById("midnight")!.tokens["color.background"]);
});

test("restoreTheme accepts honest shapes and normalizes the pip", () => {
  assert.deepStrictEqual(restoreTheme({ v: 1, preset: "zim-dark" }), { v: 1, preset: "zim-dark" });
  assert.deepStrictEqual(restoreTheme({ v: 1, preset: "paper", accent: "#ABCDEF" }), {
    v: 1,
    preset: "paper",
    accent: "#abcdef",
  });
});

test("restoreTheme rejects corruption (a theme is a nicety, not a guess)", () => {
  assert.equal(restoreTheme(null), null);
  assert.equal(restoreTheme("dark"), null);
  assert.equal(restoreTheme({ v: 2, preset: "zim-dark" }), null);
  assert.equal(restoreTheme({ v: 1 }), null);
  assert.equal(restoreTheme({ v: 1, preset: "not-a-preset" }), null);
  // A malformed pip is dropped, not fatal — the preset still applies.
  assert.deepStrictEqual(restoreTheme({ v: 1, preset: "zim-dark", accent: "blue" }), {
    v: 1,
    preset: "zim-dark",
  });
  assert.deepStrictEqual(restoreTheme({ v: 1, preset: "zim-dark", accent: "#12345" }), {
    v: 1,
    preset: "zim-dark",
  });
});

test("dotted token names map onto the CSS custom properties", () => {
  assert.equal(cssPropertyOf("color.background"), "--color-background");
  assert.equal(cssPropertyOf("color.surface2"), "--color-surface-2");
  assert.equal(cssPropertyOf("color.text.primary"), "--color-text-primary");
  assert.equal(cssPropertyOf("color.indicator.running"), "--color-indicator-running");
});

test("the storage key and the default preset are the documented ones", () => {
  assert.equal(THEME_KEY, "zim.theme.v1");
  assert.equal(PRESETS[0].id, "zim-dark");
});

test("light presets say so (the color-scheme hint flips with them)", () => {
  const paper = presetById("paper");
  assert.notEqual(paper, null);
  assert.equal(paper!.scheme, "light");
  for (const preset of PRESETS) {
    if (preset.id !== "paper") {
      assert.equal(preset.scheme, "dark");
    }
  }
});
