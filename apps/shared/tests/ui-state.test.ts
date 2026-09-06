// Shared UI-state resource-contract tests.
// These cases pin cross-runtime UTF-8 measurement and exact report limits.
// Coordinator and CLI consumers rely on this module without allocating encodings.

import { describe, expect, test } from "bun:test";
import {
  UI_ACTIVE_PATH_MAX_UTF8_BYTES,
  UI_FOLDER_KEY_MAX_UTF8_BYTES,
  UI_STATE_IDENTITY_WINDOW_MS,
  UI_STATE_MAX_TABS_PER_DASHBOARD,
  UI_STATE_MAX_TABS_PER_FINGERPRINT,
  UI_STATE_NEW_IDENTITIES_PER_WINDOW,
  UI_TAB_ID_MAX_UTF8_BYTES,
  hasAtMostUtf8Bytes,
  utf8ByteLength,
} from "../src/ui-state.ts";

describe("UI state resource contract", () => {
  test("pins text, retained-cardinality, and identity-rate limits", () => {
    expect(UI_TAB_ID_MAX_UTF8_BYTES).toBe(256);
    expect(UI_ACTIVE_PATH_MAX_UTF8_BYTES).toBe(8_192);
    expect(UI_FOLDER_KEY_MAX_UTF8_BYTES).toBe(8_192);
    expect(UI_STATE_MAX_TABS_PER_FINGERPRINT).toBe(32);
    expect(UI_STATE_MAX_TABS_PER_DASHBOARD).toBe(256);
    expect(UI_STATE_NEW_IDENTITIES_PER_WINDOW).toBe(16);
    expect(UI_STATE_IDENTITY_WINDOW_MS).toBe(60_000);
  });

  test("measures ASCII, BMP, astral, and unpaired-surrogate strings", () => {
    expect(utf8ByteLength("roost")).toBe(5);
    expect(utf8ByteLength("é")).toBe(2);
    expect(utf8ByteLength("界")).toBe(3);
    expect(utf8ByteLength("🙂")).toBe(4);
    expect(utf8ByteLength("\ud800x\udc00")).toBe(7);
  });

  test("accepts every exact UTF-8 boundary and rejects the next byte", () => {
    for (const maxBytes of [
      UI_TAB_ID_MAX_UTF8_BYTES,
      UI_ACTIVE_PATH_MAX_UTF8_BYTES,
      UI_FOLDER_KEY_MAX_UTF8_BYTES,
    ]) {
      const exact = "🙂".repeat(maxBytes / 4);
      expect(utf8ByteLength(exact)).toBe(maxBytes);
      expect(hasAtMostUtf8Bytes(exact, maxBytes)).toBe(true);
      expect(hasAtMostUtf8Bytes(`${exact}x`, maxBytes)).toBe(false);
    }
    expect(hasAtMostUtf8Bytes("", 0)).toBe(true);
    expect(hasAtMostUtf8Bytes("x", 0)).toBe(false);
  });

  test("rejects invalid limits deterministically", () => {
    for (const limit of [-1, 0.5, Number.NaN, Number.POSITIVE_INFINITY]) {
      expect(() => hasAtMostUtf8Bytes("x", limit)).toThrow(RangeError);
    }
  });
});
