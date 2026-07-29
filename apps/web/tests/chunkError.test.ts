// Stale-chunk recovery. A redeploy replaces dist/assets/* with fresh content
// hashes and deletes the old files, so a tab that outlived the deploy asks for a
// 404 and dies. This decides when to self-heal by reloading.
//
// The Safari string here is verbatim from a real production report on
// 2026-07-29 (a tab open across a coord redeploy): "Importing a module script
// failed." That case does NOT fire vite:preloadError, which is why matching on
// message text exists at all.

import { expect, test, describe } from "bun:test";
import {
  isChunkLoadError,
  shouldReloadForChunkError,
  effectiveAttempts,
  CHUNK_RELOAD_COOLDOWN_MS,
  CHUNK_RELOAD_MAX_ATTEMPTS,
  CHUNK_ATTEMPT_RESET_MS,
} from "../src/lib/chunkError.ts";

const NOW = 1_700_000_000_000;
const ok = (over: Partial<Parameters<typeof shouldReloadForChunkError>[0]> = {}) =>
  shouldReloadForChunkError({
    message: "Importing a module script failed.",
    now: NOW,
    lastReloadAt: 0,
    online: true,
    attempts: 0,
    ...over,
  });

describe("isChunkLoadError", () => {
  test("matches every engine's wording for a missing module", () => {
    for (const msg of [
      "Importing a module script failed.", // Safari — the production report
      "Failed to fetch dynamically imported module: https://host/assets/BrowsePage-j7kz8r8k.js",
      "error loading dynamically imported module",
      "Error loading a module script: expected a JavaScript module",
      "Loading chunk 42 failed",
      "Loading CSS chunk 7 failed",
    ]) {
      expect(isChunkLoadError(msg)).toBe(true);
    }
  });

  test("does not hijack ordinary application crashes", () => {
    // A reload would destroy the evidence for these, and loop on a real bug.
    for (const msg of [
      "undefined is not an object (evaluating 'session.kind')",
      "Cannot read properties of null (reading 'focus')",
      "NetworkError: Failed to fetch",
      "ResizeObserver loop completed with undelivered notifications",
      "",
      undefined,
      null,
    ]) {
      expect(isChunkLoadError(msg)).toBe(false);
    }
  });
});

describe("shouldReloadForChunkError", () => {
  test("reloads on a stale-chunk error in a healthy tab", () => {
    expect(ok()).toBe(true);
  });

  test("never reloads for a non-chunk error", () => {
    expect(ok({ message: "TypeError: x is not a function" })).toBe(false);
  });

  test("offline is a missing network, not a stale chunk", () => {
    // Reloading a no-cache index.html while offline just blanks the app.
    expect(ok({ online: false })).toBe(false);
  });

  test("respects the cooldown so one bad load can't spin", () => {
    expect(ok({ lastReloadAt: NOW - (CHUNK_RELOAD_COOLDOWN_MS - 1), attempts: 1 })).toBe(false);
    expect(ok({ lastReloadAt: NOW - (CHUNK_RELOAD_COOLDOWN_MS + 1), attempts: 1 })).toBe(true);
  });

  test("gives up after the attempt cap so a broken deploy surfaces", () => {
    expect(
      ok({ attempts: CHUNK_RELOAD_MAX_ATTEMPTS, lastReloadAt: NOW - CHUNK_RELOAD_COOLDOWN_MS * 10 }),
    ).toBe(false);
  });
});

describe("effectiveAttempts", () => {
  test("keeps a recent count (a reload that just failed still counts)", () => {
    expect(effectiveAttempts(2, NOW - 1_000, NOW)).toBe(2);
  });

  test("expires a stale count so a long-lived tab keeps self-healing", () => {
    // Two deploys days apart must not permanently disable recovery.
    expect(effectiveAttempts(2, NOW - (CHUNK_ATTEMPT_RESET_MS + 1), NOW)).toBe(0);
  });

  test("no prior reload means no attempts", () => {
    expect(effectiveAttempts(5, 0, NOW)).toBe(0);
  });

  test("an expired count re-enables the reload decision end to end", () => {
    const lastReloadAt = NOW - (CHUNK_ATTEMPT_RESET_MS + 1);
    const attempts = effectiveAttempts(CHUNK_RELOAD_MAX_ATTEMPTS, lastReloadAt, NOW);
    expect(ok({ attempts, lastReloadAt })).toBe(true);
  });
});
