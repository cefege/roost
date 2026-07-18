// signCoordinatorJwt token cache — the perf-sweep fix for "one Ed25519 sign
// per RPC, including one per keystroke via input-channel". Contract: a minted
// token is reused for 240 s (80% of coord's default 300 s jwtMaxAgeSecs);
// past the TTL a fresh token (new iat) is minted and becomes the cached one.
// Ed25519 is deterministic (RFC 8032), so same-iat re-signs produce an
// identical token — the sign-call spy is what proves the cache short-circuits.
//
// bun has WebCrypto Ed25519 but no IndexedDB — a ~40-line fake (microtask-
// dispatched request events, exactly the surface web-key.ts touches) backs
// loadOrGenerate.

import { expect, test, describe, spyOn, setSystemTime } from "bun:test";

// ── minimal fake IndexedDB ───────────────────────────────────────────────
class FakeReq<T> {
  onsuccess: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onupgradeneeded: (() => void) | null = null;
  result!: T;
  error: Error | null = null;
}
const _idbStore = new Map<string, unknown>();
const fakeDb = {
  createObjectStore: (_name: string) => ({}),
  transaction: (_name: string, _mode?: string) => ({
    objectStore: (_n: string) => ({
      get(key: string) {
        const req = new FakeReq<unknown>();
        queueMicrotask(() => { req.result = _idbStore.get(key); req.onsuccess?.(); });
        return req;
      },
      put(val: unknown, key: string) {
        const req = new FakeReq<undefined>();
        queueMicrotask(() => { _idbStore.set(key, val); req.onsuccess?.(); });
        return req;
      },
    }),
  }),
};
// Named-cast global: test double for an API bun doesn't provide.
const g = globalThis as unknown as {
  indexedDB: { open: (name: string, ver: number) => FakeReq<typeof fakeDb> };
  localStorage?: Storage;
};
g.indexedDB = {
  open(_name: string, _ver: number) {
    const req = new FakeReq<typeof fakeDb>();
    queueMicrotask(() => { req.result = fakeDb; req.onupgradeneeded?.(); req.onsuccess?.(); });
    return req;
  },
};
if (typeof g.localStorage === "undefined") {
  const _ls: Record<string, string> = {};
  g.localStorage = {
    getItem: (k: string) => _ls[k] ?? null,
    setItem: (k: string, v: string) => { _ls[k] = v; },
    removeItem: (k: string) => { delete _ls[k]; },
    clear: () => { for (const k of Object.keys(_ls)) delete _ls[k]; },
    key: () => null, length: 0,
  } as Storage;
}

// Dynamic import on purpose: the module must initialize AFTER the stubs above.
const { signCoordinatorJwt } = await import("../src/auth/web-key.ts");

function iatOf(token: string): number {
  const payloadB64 = token.split(".")[1].replace(/-/g, "+").replace(/_/g, "/");
  const payload = JSON.parse(atob(payloadB64)) as { sub: string; aud: string; iat: number };
  expect(payload.sub).toBe("web");
  expect(payload.aud).toBe("roost-coordinator");
  return payload.iat;
}

describe("signCoordinatorJwt cache", () => {
  test("reuses one token within 240s, re-mints past the TTL", async () => {
    const T0 = new Date("2026-07-11T00:00:00Z");
    setSystemTime(T0);
    try {
      const signSpy = spyOn(crypto.subtle, "sign");

      const t1 = await signCoordinatorJwt();
      expect(t1.split(".").length).toBe(3);
      expect(iatOf(t1)).toBe(Math.floor(T0.getTime() / 1000));
      const signsForFirstMint = signSpy.mock.calls.length;
      expect(signsForFirstMint).toBe(1);

      // Within the TTL: identical token, ZERO additional sign calls.
      expect(await signCoordinatorJwt()).toBe(t1);
      setSystemTime(new Date(T0.getTime() + 239_000));
      expect(await signCoordinatorJwt()).toBe(t1);
      expect(signSpy.mock.calls.length).toBe(signsForFirstMint);

      // Past the TTL: a fresh token (new iat) is minted...
      setSystemTime(new Date(T0.getTime() + 241_000));
      const t2 = await signCoordinatorJwt();
      expect(t2).not.toBe(t1);
      expect(iatOf(t2)).toBe(Math.floor((T0.getTime() + 241_000) / 1000));
      expect(signSpy.mock.calls.length).toBe(signsForFirstMint + 1);

      // ...and becomes the cached one.
      expect(await signCoordinatorJwt()).toBe(t2);
      expect(signSpy.mock.calls.length).toBe(signsForFirstMint + 1);

      signSpy.mockRestore();
    } finally {
      setSystemTime(); // restore real clock for the rest of the suite
    }
  });
});
