// deepgramKey caches coord's Deepgram credential for the page session so the
// mic tap doesn't pay a WAN round-trip per recording. The contract that matters:
// exactly one RPC per cache generation (including under concurrent callers), a
// re-fetch after invalidation, and a rejection that does NOT poison the cache.

import { describe, test, expect, beforeEach, mock } from "bun:test";

let grantResult: () => Promise<{ accessToken: string }> = () =>
  Promise.resolve({ accessToken: "key-1" });
let grantCalls = 0;
const transcriptionGrantToken = mock(() => {
  grantCalls++;
  return grantResult();
});

mock.module("../src/connect.ts", () => ({ coordClient: { transcriptionGrantToken } }));

// Dynamic import keeps the production connect client behind the mock above.
const { getDeepgramKey, invalidateDeepgramKey, prefetchDeepgramKey } = await import(
  "../src/lib/deepgramKey.ts"
);

describe("deepgramKey cache", () => {
  beforeEach(() => {
    invalidateDeepgramKey();
    grantCalls = 0;
    grantResult = () => Promise.resolve({ accessToken: "key-1" });
  });

  test("a second call is served from the cache", async () => {
    expect(await getDeepgramKey()).toBe("key-1");
    expect(await getDeepgramKey()).toBe("key-1");
    expect(grantCalls).toBe(1);
  });

  test("concurrent callers share one in-flight fetch", async () => {
    const gate = Promise.withResolvers<{ accessToken: string }>();
    grantResult = () => gate.promise;

    const both = Promise.all([getDeepgramKey(), getDeepgramKey()]);
    prefetchDeepgramKey(); // the pointerdown warm-up must not add a request
    gate.resolve({ accessToken: "key-2" });

    expect(await both).toEqual(["key-2", "key-2"]);
    expect(grantCalls).toBe(1);
  });

  test("invalidate forces the next call to re-fetch", async () => {
    await getDeepgramKey();
    invalidateDeepgramKey();
    grantResult = () => Promise.resolve({ accessToken: "key-rotated" });

    expect(await getDeepgramKey()).toBe("key-rotated");
    expect(grantCalls).toBe(2);
  });

  test("a rejected fetch leaves the cache empty so the next call retries", async () => {
    grantResult = () => Promise.reject(new Error("coord restarting"));
    await expect(getDeepgramKey()).rejects.toThrow("coord restarting");

    grantResult = () => Promise.resolve({ accessToken: "key-after-retry" });
    expect(await getDeepgramKey()).toBe("key-after-retry");
    expect(grantCalls).toBe(2);
  });
});
