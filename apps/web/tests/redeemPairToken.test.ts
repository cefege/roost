// redeemPairToken TOFU pin enforcement: a coord fingerprint contradicting the
// pinned one fails redemption CLOSED ({reason:"coord_fingerprint_mismatch"}),
// leaves the pin untouched, and emits the auth.pin_mismatch Tier-1 signal;
// explicit clearCoordTrust() consent wipes the pin so the next redeem re-runs
// TOFU and succeeds; a first pairing with no pin stored trusts silently.
// bun has no IndexedDB — a microtask-dispatched fake backs exactly the
// object-store surface trust.ts touches; coordClient and web-key are mocked.
import { describe, expect, test, mock } from "bun:test";
import { setSignalSink } from "@roost/shared/diag";

// ── minimal fake IndexedDB ───────────────────────────────────────────────
class FakeReq<T> {
  onsuccess: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onupgradeneeded: (() => void) | null = null;
  result!: T;
  error: Error | null = null;
}
const idbStore = new Map<string, unknown>();
const fakeDb = {
  objectStoreNames: { contains: () => true },
  transaction: (_name: string, _mode: string) => new FakeTx(),
};

class FakeTx {
  objectStore(_name: string) {
    return {
      get: (key: string) => {
        const req = new FakeReq<unknown>();
        queueMicrotask(() => {
          req.result = idbStore.get(key);
          req.onsuccess?.();
        });
        return req;
      },
      put: (value: unknown, key: string) => {
        const req = new FakeReq<string>();
        queueMicrotask(() => {
          idbStore.set(key, value);
          req.result = key;
          req.onsuccess?.();
        });
        return req;
      },
      delete: (key: string) => {
        const req = new FakeReq<undefined>();
        queueMicrotask(() => {
          idbStore.delete(key);
          req.result = undefined;
          req.onsuccess?.();
        });
        return req;
      },
    };
  }
}

// Named-cast global: test double for an API bun doesn't provide.
const g = globalThis as unknown as {
  indexedDB: { open: (name: string, version: number) => FakeReq<typeof fakeDb> };
};
g.indexedDB = {
  open(_name: string, _version: number) {
    const req = new FakeReq<typeof fakeDb>();
    queueMicrotask(() => {
      req.result = fakeDb;
      req.onupgradeneeded?.();
      req.onsuccess?.();
    });
    return req;
  },
};

// ── Tier-1 signal capture + coord/web-key doubles ────────────────────────
const signals: Array<Record<string, unknown>> = [];
setSignalSink((record) => {
  signals.push(record);
});

interface RedeemInput {
  token: string;
  sshPubkeyB64: string;
  label: string;
}
let nextFingerprint: string | null = "fp-unset";
const redeemCalls: string[] = [];

mock.module("../src/connect.ts", () => ({
  coordClient: {
    authRedeemBrowser: async (input: RedeemInput) => {
      redeemCalls.push(input.token);
      return nextFingerprint === null ? {} : { fingerprint: nextFingerprint };
    },
  },
}));
mock.module("../src/auth/web-key.ts", () => ({
  getPublicKeyB64: async () => "dGVzdC1wdWJsaWMta2V5",
}));

// Dynamic import on purpose: the modules must initialize AFTER the stubs above.
const { redeemPairToken } = await import("../src/auth/redeemPairToken.ts");
const { trustCoordFingerprint, clearCoordTrust } = await import("../src/auth/trust.ts");

const ORIGINAL_FP = "fp-original-0000";
const ROTATED_FP = "fp-rotated-99999";

function pinMismatchEvents(): Array<Record<string, unknown>> {
  return signals.filter((s) => s.evt === "auth.pin_mismatch");
}

describe("redeemPairToken TOFU pin enforcement", () => {
  test("mismatch against the pinned fingerprint blocks redemption and signals", async () => {
    idbStore.clear();
    redeemCalls.length = 0;
    expect(await trustCoordFingerprint(ORIGINAL_FP)).toBe(true);

    nextFingerprint = ROTATED_FP;
    const res = await redeemPairToken("tok-mismatch");

    expect(res).toEqual({ ok: false, reason: "coord_fingerprint_mismatch" });
    expect(redeemCalls).toEqual(["tok-mismatch"]);
    // Fail closed: the stale pin survives — nothing is trusted implicitly.
    expect(idbStore.get("coord-fingerprint")).toBe(ORIGINAL_FP);
    const pin = pinMismatchEvents().at(-1);
    if (!pin) throw new Error("auth.pin_mismatch signal was not emitted");
    expect("pinned8" in pin && pin.pinned8 === ORIGINAL_FP.slice(0, 8)).toBe(true);
    expect("seen8" in pin && pin.seen8 === ROTATED_FP.slice(0, 8)).toBe(true);
  });

  test("explicit trust clears the pin and the retry re-runs TOFU successfully", async () => {
    idbStore.clear();
    redeemCalls.length = 0;
    await trustCoordFingerprint(ORIGINAL_FP);
    nextFingerprint = ROTATED_FP;
    expect(await redeemPairToken("tok-rotate")).toEqual({
      ok: false,
      reason: "coord_fingerprint_mismatch",
    });

    await clearCoordTrust();
    expect(idbStore.has("coord-fingerprint")).toBe(false);
    const res = await redeemPairToken("tok-retry");
    expect(res).toEqual({ ok: true });
    expect(redeemCalls).toEqual(["tok-rotate", "tok-retry"]);
    // Re-running TOFU against the presented key pins it.
    expect(idbStore.get("coord-fingerprint")).toBe(ROTATED_FP);
  });

  test("first pairing with no pin stored keeps silent TOFU", async () => {
    idbStore.clear();
    redeemCalls.length = 0;
    nextFingerprint = "fp-brand-new-00";
    const mismatchesBefore = pinMismatchEvents().length;

    const res = await redeemPairToken("tok-first");

    expect(res).toEqual({ ok: true });
    expect(redeemCalls).toEqual(["tok-first"]);
    expect(idbStore.get("coord-fingerprint")).toBe("fp-brand-new-00");
    expect(pinMismatchEvents().length).toBe(mismatchesBefore);
  });
});
