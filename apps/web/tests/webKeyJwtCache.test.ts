// signCoordinatorJwt token cache — the perf-sweep fix for "one Ed25519 sign
// per RPC, including one per keystroke via input-channel". Contract: a minted
// token is reused for 240 s (80% of coord's default 300 s jwtMaxAgeSecs);
// past the TTL a fresh token (new iat) is minted and becomes the cached one.
// Ed25519 is deterministic (RFC 8032), so same-iat re-signs produce an
// identical token — the sign-call spy is what proves the cache short-circuits.
//
// bun has WebCrypto Ed25519 but no IndexedDB. The fake below models database
// versions, object-store upgrades, and the request/transaction surface used by
// web-key.ts so the v1→v2 migration exercises persisted CryptoKey records.

import { expect, test, describe, spyOn, setSystemTime } from "bun:test";

// ── focused fake IndexedDB ───────────────────────────────────────────────
class FakeReq<T> {
  onsuccess: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onupgradeneeded: (() => void) | null = null;
  result!: T;
  error: Error | null = null;
}

const _idbStore = new Map<string, unknown>();
const _trustStore = new Map<string, unknown>();

class FakeTx {
  oncomplete: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onabort: (() => void) | null = null;
  error: Error | null = null;

  constructor(private readonly records: Map<string, unknown>) {}

  objectStore(_name: string) {
    return {
      get: (key: string) => {
        const req = new FakeReq<unknown>();
        queueMicrotask(() => {
          req.result = this.records.get(key);
          req.onsuccess?.();
        });
        return req;
      },
      add: (value: unknown, key: string) => {
        const req = new FakeReq<undefined>();
        queueMicrotask(() => {
          if (this.records.has(key)) {
            this.error = new DOMException("key exists", "ConstraintError");
            req.error = this.error;
            req.onerror?.();
            this.onerror?.();
            this.onabort?.();
            return;
          }
          this.records.set(key, value);
          req.onsuccess?.();
          this.oncomplete?.();
        });
        return req;
      },
      put: (value: unknown, key: string) => {
        const req = new FakeReq<undefined>();
        queueMicrotask(() => {
          this.records.set(key, value);
          req.onsuccess?.();
          this.oncomplete?.();
        });
        return req;
      },
      delete: (key: string) => {
        const req = new FakeReq<undefined>();
        queueMicrotask(() => {
          this.records.delete(key);
          req.onsuccess?.();
          this.oncomplete?.();
        });
        return req;
      },
    };
  }

  abort(): void {
    this.error = new Error("aborted");
    queueMicrotask(() => this.onabort?.());
  }
}

class FakeDb {
  version = 1;
  private readonly stores = new Map<string, Map<string, unknown>>([
    ["keys", _idbStore],
    ["trust", _trustStore],
  ]);

  readonly objectStoreNames = {
    contains: (name: string) => this.stores.has(name),
  };

  createObjectStore(name: string): object {
    if (this.stores.has(name)) throw new DOMException("store exists", "ConstraintError");
    const records = name === "keys"
      ? _idbStore
      : name === "trust"
      ? _trustStore
      : new Map<string, unknown>();
    records.clear();
    this.stores.set(name, records);
    return {};
  }

  deleteObjectStore(name: string): void {
    const records = this.stores.get(name);
    if (!records) throw new DOMException("store missing", "NotFoundError");
    records.clear();
    this.stores.delete(name);
  }

  transaction(name: string, _mode?: string): FakeTx {
    const records = this.stores.get(name);
    if (!records) throw new DOMException("store missing", "NotFoundError");
    return new FakeTx(records);
  }

  resetFresh(): void {
    this.version = 0;
    this.stores.clear();
    _idbStore.clear();
    _trustStore.clear();
  }
}

const fakeDb = new FakeDb();
const requestedDbVersions: number[] = [];

// Named-cast global: test double for an API bun doesn't provide.
const g = globalThis as unknown as {
  indexedDB: { open: (name: string, ver: number) => FakeReq<FakeDb> };
  localStorage?: Storage;
};
g.indexedDB = {
  open(_name: string, version: number) {
    requestedDbVersions.push(version);
    const req = new FakeReq<FakeDb>();
    queueMicrotask(() => {
      req.result = fakeDb;
      if (version > fakeDb.version) {
        fakeDb.version = version;
        try {
          req.onupgradeneeded?.();
        } catch (error) {
          req.error = error instanceof Error ? error : new Error(String(error));
          req.onerror?.();
          return;
        }
      }
      req.onsuccess?.();
    });
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
const migrationCurrentKey = await crypto.subtle.generateKey(
  { name: "Ed25519" },
  false,
  ["sign", "verify"],
);
const migrationStagedKey = await crypto.subtle.generateKey(
  { name: "Ed25519" },
  false,
  ["sign", "verify"],
);
const migrationRotation = {
  operationId: "persisted-rotation",
  keyPair: migrationStagedKey,
};
_idbStore.set("ed25519", migrationCurrentKey);
_idbStore.set("ed25519-rotation-v1", migrationRotation);
_trustStore.set("coord-fingerprint", "obsolete-fingerprint");

// Dynamic import on purpose: the module must initialize AFTER the stubs above.
const {
  clearWebKeyMaterialForLogout,
  getPublicKeyB64,
  markCurrentWebKeyAuthorized,
  isResetWebKeyEligible,
  resetWebKey,
  probeCurrentWebKey,
  signCoordinatorJwt,
} = await import("../src/auth/web-key.ts");

describe("roost-auth IndexedDB migration", () => {
  test("v1 key material survives trust-store removal and a fresh database creates keys", async () => {
    const originalFetch = globalThis.fetch;
    const originalLocation = Object.getOwnPropertyDescriptor(globalThis, "location");
    Object.defineProperty(globalThis, "location", {
      configurable: true,
      value: {
        origin: "https://coord.example",
        hash: "",
        reload: () => {},
      },
    });
    globalThis.fetch = Object.assign(
      async () => new Response(null, { status: 503 }),
      { preconnect: originalFetch.preconnect },
    );

    try {
      expect(await probeCurrentWebKey()).toBe("ambiguous");
    } finally {
      globalThis.fetch = originalFetch;
      if (originalLocation) Object.defineProperty(globalThis, "location", originalLocation);
      else Reflect.deleteProperty(globalThis, "location");
    }

    expect(requestedDbVersions).toEqual([2]);
    expect(fakeDb.version).toBe(2);
    expect(fakeDb.objectStoreNames.contains("keys")).toBe(true);
    expect(fakeDb.objectStoreNames.contains("trust")).toBe(false);
    expect(_idbStore.get("ed25519")).toBe(migrationCurrentKey);
    expect(migrationCurrentKey.privateKey.extractable).toBe(false);
    expect(_idbStore.get("ed25519-rotation-v1")).toBe(migrationRotation);
    expect(migrationRotation.keyPair.privateKey.extractable).toBe(false);

    fakeDb.resetFresh();
    requestedDbVersions.length = 0;
    localStorage.clear();

    expect((await getPublicKeyB64()).length).toBeGreaterThan(0);
    expect(requestedDbVersions).toEqual([2]);
    expect(fakeDb.version).toBe(2);
    expect(fakeDb.objectStoreNames.contains("keys")).toBe(true);
    expect(fakeDb.objectStoreNames.contains("trust")).toBe(false);
    const freshKey = _idbStore.get("ed25519") as CryptoKeyPair | undefined;
    expect(freshKey).toBeDefined();
    expect(freshKey?.privateKey.extractable).toBe(false);
  });
});

function iatOf(token: string): number {
  const [headerB64, payloadB64] = token.split(".");
  const header = JSON.parse(atob(headerB64!.replace(/-/g, "+").replace(/_/g, "/"))) as {
    kid: string;
  };
  const payload = JSON.parse(atob(payloadB64!.replace(/-/g, "+").replace(/_/g, "/"))) as {
    sub: string;
    aud: string;
    iat: number;
    exp: number;
  };
  expect(payload.sub).toBe(header.kid);
  expect(payload.aud).toBe("roost-coordinator");
  expect(payload.exp).toBe(payload.iat + 300);
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

describe("managed rejected-key recovery", () => {
  test("deletes only after a marked dashboard-access rejection and generates a new key", async () => {
    const originalFetch = globalThis.fetch;
    const originalLocation = Object.getOwnPropertyDescriptor(globalThis, "location");
    const originalLocks = Object.getOwnPropertyDescriptor(navigator, "locks");
    let responseStatus = 503;
    let reloads = 0;
    Object.defineProperty(globalThis, "location", {
      configurable: true,
      value: {
        origin: "https://managed.example",
        hash: "",
        reload: () => { reloads++; },
      },
    });
    Object.defineProperty(navigator, "locks", {
      configurable: true,
      value: {
        request: async (...args: unknown[]) => {
          const action = args.at(-1);
          if (typeof action !== "function") throw new Error("missing lock action");
          return action();
        },
      },
    });
    globalThis.fetch = Object.assign(
      async () => new Response(null, {
        status: responseStatus,
        headers: responseStatus === 401
          ? { "x-roost-auth-layer": "device" }
          : undefined,
      }),
      { preconnect: originalFetch.preconnect },
    );

    try {
      markCurrentWebKeyAuthorized();
      const originalKey = await getPublicKeyB64();
      expect(await isResetWebKeyEligible("managed")).toBe(false);
      await expect(resetWebKey("managed")).rejects.toThrow("explicitly rejected");
      expect(await getPublicKeyB64()).toBe(originalKey);

      responseStatus = 401;
      expect(await isResetWebKeyEligible("managed")).toBe(true);
      await resetWebKey("managed");
      expect(reloads).toBe(1);
      expect(await getPublicKeyB64()).not.toBe(originalKey);
    } finally {
      globalThis.fetch = originalFetch;
      if (originalLocation) Object.defineProperty(globalThis, "location", originalLocation);
      else Reflect.deleteProperty(globalThis, "location");
      if (originalLocks) Object.defineProperty(navigator, "locks", originalLocks);
      else Reflect.deleteProperty(navigator, "locks");
    }
  });
});

describe("managed logout key cleanup", () => {
  test("removes committed and staged keys, flags, and cached identity without reloading", async () => {
    await getPublicKeyB64();
    markCurrentWebKeyAuthorized();
    _idbStore.set("ed25519-rotation-v1", {
      operationId: "staged-operation",
      keyPair: _idbStore.get("ed25519"),
    });
    expect(_idbStore.has("ed25519")).toBe(true);
    expect(_idbStore.has("ed25519-rotation-v1")).toBe(true);
    expect(localStorage.getItem("roostKeyMinted")).toBe("1");
    expect(localStorage.getItem("roostKeyAuthorized")).toBe("1");

    await clearWebKeyMaterialForLogout();

    expect(_idbStore.has("ed25519")).toBe(false);
    expect(_idbStore.has("ed25519-rotation-v1")).toBe(false);
    expect(localStorage.getItem("roostKeyMinted")).toBeNull();
    expect(localStorage.getItem("roostKeyAuthorized")).toBeNull();
  });
});
