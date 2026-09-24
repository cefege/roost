// Requester ceremony coverage uses deferred RPCs and timers to prove persistence,
// recovery, and operation fencing. Each assertion observes storage, RPC inputs,
// redirect behavior, or visible controller state rather than implementation shape.

import { Code, ConnectError } from "@connectrpc/connect";
import { afterAll, afterEach, beforeEach, describe, expect, mock, test, vi } from "bun:test";
import type * as SolidApi from "solid-js";
import type { OnboardingPairingCeremony } from "../src/components/pairing/onboarding-pairing-ceremony.ts";

const REQUEST_ID_A = "0123456789abcdef0123456789abcdef";
const REQUEST_ID_B = "fedcba9876543210fedcba9876543210";
const REQUESTER_TOKEN_A = "ab".repeat(32);
const REQUESTER_TOKEN_B = "cd".repeat(32);
const sessionValues = new Map<string, string>();
const originalSessionStorage = Object.getOwnPropertyDescriptor(globalThis, "sessionStorage");

Object.defineProperty(globalThis, "sessionStorage", {
  configurable: true,
  value: {
    get length() { return sessionValues.size; },
    clear: () => sessionValues.clear(),
    getItem: (key: string) => sessionValues.get(key) ?? null,
    key: (index: number) => [...sessionValues.keys()][index] ?? null,
    removeItem: (key: string) => { sessionValues.delete(key); },
    setItem: (key: string, value: string) => { sessionValues.set(key, value); },
  } satisfies Storage,
});

type PairCreateInput = {
  ceremonyVersion: number;
  ephemeralId: string;
  requesterToken: string;
  sshPubkeyB64: string;
  label: string;
};
type PairPollInput = {
  ceremonyVersion: number;
  ephemeralId: string;
  requesterToken: string;
};
type PairConfirmInput = PairPollInput & { verificationCode: string };

let generatedIds: string[] = [];
let generatedTokens: string[] = [];
let pairCreateImplementation: (request: PairCreateInput) => Promise<{ ephemeralId: string }>;
let pairPollImplementation: (request: PairPollInput) => Promise<{ status: string; expiresAtMs: bigint }>;
let pairConfirmImplementation: (request: PairConfirmInput) => Promise<{ ok: boolean }>;
const pairCreate = mock((request: PairCreateInput) => pairCreateImplementation(request));
const pairPoll = mock((request: PairPollInput) => pairPollImplementation(request));
const pairConfirm = mock((request: PairConfirmInput) => pairConfirmImplementation(request));

mock.module("@roost/protocol/pairing", () => ({
  PAIRING_CEREMONY_VERSION: 1,
  PAIR_VERIFICATION_CODE_LENGTH: 6,
  generatePairRequestId: () => generatedIds.shift() ?? REQUEST_ID_A,
  generatePairRequesterToken: () => generatedTokens.shift() ?? REQUESTER_TOKEN_A,
  generatePairVerificationCode: () => "123456",
  normalizePairRequestId: (value: string) => /^[0-9a-f]{32}$/.test(value) ? value : null,
  normalizePairRequesterToken: (value: string) => /^[0-9a-f]{64}$/.test(value) ? value : null,
  normalizePairVerificationCode: (value: string) => /^\d{6}$/.test(value) ? value : null,
}));
mock.module("@roost/protocol/retry", () => ({ backoffDelayMs: () => 10 }));
mock.module("../src/client/rpc/connect.ts", () => ({
  coordClient: { pairCreate, pairPoll, pairConfirm },
}));
mock.module("../src/client/auth/web-key.ts", () => ({
  getPublicKeyB64: async () => "requester-public-key",
}));
mock.module("../src/browser/browserSelfLabel.ts", () => ({ browserSelfLabel: () => "Requester browser" }));
mock.module("../src/store/toastStore.ts", () => ({ addToast: () => undefined }));

// Load client Solid after the module mocks select the browser-safe runtime.
const Solid = await import(new URL("./solid.js", import.meta.resolve("solid-js")).href) as typeof SolidApi;
mock.module("solid-js", () => Solid);
// Dynamic imports follow the mocked protocol, timer, and browser boundaries.
const { PAIRING_CEREMONY_STORAGE_KEY } = await import("../src/client/auth/pairing-ceremony.ts");
const { createOnboardingPairingCeremony } = await import(
  "../src/components/pairing/onboarding-pairing-ceremony.ts"
);

function mountCeremony(redirects: { count: number }) {
  let controller: OnboardingPairingCeremony | undefined;
  let dispose: (() => void) | undefined;
  Solid.createRoot((disposeRoot) => {
    dispose = disposeRoot;
    controller = createOnboardingPairingCeremony({
      redirectAfterPairing: () => { redirects.count += 1; },
      reportRequestError: () => undefined,
    });
  });
  return { controller: controller!, dispose: dispose! };
}

async function settlePairingWork(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
}

beforeEach(() => {
  vi.useFakeTimers();
  sessionValues.clear();
  generatedIds = [REQUEST_ID_A, REQUEST_ID_B];
  generatedTokens = [REQUESTER_TOKEN_A, REQUESTER_TOKEN_B];
  pairCreate.mockClear();
  pairPoll.mockClear();
  pairConfirm.mockClear();
  pairCreateImplementation = async (request) => ({ ephemeralId: request.ephemeralId });
  pairPollImplementation = async () => ({ status: "pending", expiresAtMs: 0n });
  pairConfirmImplementation = async () => ({ ok: false });
});

afterEach(() => {
  vi.clearAllTimers();
  vi.useRealTimers();
});

afterAll(() => {
  if (originalSessionStorage) Object.defineProperty(globalThis, "sessionStorage", originalSessionStorage);
  else Reflect.deleteProperty(globalThis, "sessionStorage");
});

describe("requester pairing verification", () => {
  test("saves generated versioned capability before create and polls only after acknowledgement", async () => {
    const createResponse = Promise.withResolvers<{ ephemeralId: string }>();
    pairCreateImplementation = () => createResponse.promise;
    const redirects = { count: 0 };
    const mounted = mountCeremony(redirects);
    const started = mounted.controller.start();
    await settlePairingWork();
    try {
      expect(pairCreate).toHaveBeenCalledWith({
        ceremonyVersion: 1,
        ephemeralId: REQUEST_ID_A,
        requesterToken: REQUESTER_TOKEN_A,
        sshPubkeyB64: "requester-public-key",
        label: "Requester browser",
      });
      expect(JSON.parse(sessionValues.get(PAIRING_CEREMONY_STORAGE_KEY)!)).toEqual({
        ceremonyVersion: 1,
        ephemeralId: REQUEST_ID_A,
        requesterToken: REQUESTER_TOKEN_A,
      });
      expect(pairPoll).not.toHaveBeenCalled();
      createResponse.resolve({ ephemeralId: REQUEST_ID_A });
      await started;
      vi.advanceTimersByTime(5_000);
      await settlePairingWork();
      expect(pairPoll).toHaveBeenCalledWith({
        ceremonyVersion: 1,
        ephemeralId: REQUEST_ID_A,
        requesterToken: REQUESTER_TOKEN_A,
      });
      expect(redirects.count).toBe(0);
    } finally {
      mounted.dispose();
    }
  });

  test("restores the exact capability and retries a transport-failed create", async () => {
    sessionValues.set(PAIRING_CEREMONY_STORAGE_KEY, JSON.stringify({
      ceremonyVersion: 1,
      ephemeralId: REQUEST_ID_A,
      requesterToken: REQUESTER_TOKEN_A,
    }));
    let attempts = 0;
    pairCreateImplementation = async (request) => {
      attempts += 1;
      if (attempts === 1) throw new Error("offline");
      return { ephemeralId: request.ephemeralId };
    };
    const mounted = mountCeremony({ count: 0 });
    try {
      await settlePairingWork();
      expect(pairCreate).toHaveBeenCalledTimes(1);
      expect(pairPoll).not.toHaveBeenCalled();
      vi.advanceTimersByTime(10);
      await settlePairingWork();
      expect(pairCreate).toHaveBeenCalledTimes(2);
      expect(pairCreate.mock.calls[0]?.[0]).toMatchObject({
        ceremonyVersion: 1,
        ephemeralId: REQUEST_ID_A,
        requesterToken: REQUESTER_TOKEN_A,
      });
      expect(pairCreate.mock.calls[1]?.[0]).toMatchObject({
        ceremonyVersion: 1,
        ephemeralId: REQUEST_ID_A,
        requesterToken: REQUESTER_TOKEN_A,
      });
      expect(mounted.controller.ephemeralId()).toBe(REQUEST_ID_A);
    } finally {
      mounted.dispose();
    }
  });

  test("recovers a completed confirmation through token-bound poll after reload", async () => {
    sessionValues.set(PAIRING_CEREMONY_STORAGE_KEY, JSON.stringify({
      ceremonyVersion: 1,
      ephemeralId: REQUEST_ID_A,
      requesterToken: REQUESTER_TOKEN_A,
    }));
    pairCreateImplementation = async () => {
      throw new ConnectError("completed", Code.FailedPrecondition);
    };
    pairPollImplementation = async () => ({ status: "completed", expiresAtMs: 0n });
    const redirects = { count: 0 };
    const mounted = mountCeremony(redirects);
    try {
      await settlePairingWork();
      vi.advanceTimersByTime(0);
      await settlePairingWork();
      expect(pairPoll).toHaveBeenCalledWith({
        ceremonyVersion: 1,
        ephemeralId: REQUEST_ID_A,
        requesterToken: REQUESTER_TOKEN_A,
      });
      expect(redirects.count).toBe(1);
      expect(sessionValues.has(PAIRING_CEREMONY_STORAGE_KEY)).toBe(false);
    } finally {
      mounted.dispose();
    }
  });

  test("drops stale create and out-of-order poll results after Start over", async () => {
    const firstCreate = Promise.withResolvers<{ ephemeralId: string }>();
    pairCreateImplementation = (request) => request.ephemeralId === REQUEST_ID_A
      ? firstCreate.promise
      : Promise.resolve({ ephemeralId: request.ephemeralId });
    const redirects = { count: 0 };
    const mounted = mountCeremony(redirects);
    try {
      const firstStart = mounted.controller.start();
      await settlePairingWork();
      await mounted.controller.start();
      firstCreate.resolve({ ephemeralId: REQUEST_ID_A });
      await firstStart;
      expect(mounted.controller.ephemeralId()).toBe(REQUEST_ID_B);
      expect(JSON.parse(sessionValues.get(PAIRING_CEREMONY_STORAGE_KEY)!)).toMatchObject({
        ephemeralId: REQUEST_ID_B,
        requesterToken: REQUESTER_TOKEN_B,
      });

      mounted.controller.clear();
      generatedIds = [REQUEST_ID_A, REQUEST_ID_B];
      generatedTokens = [REQUESTER_TOKEN_A, REQUESTER_TOKEN_B];
      const firstPoll = Promise.withResolvers<{ status: string; expiresAtMs: bigint }>();
      pairCreateImplementation = async (request) => ({ ephemeralId: request.ephemeralId });
      pairPollImplementation = () => firstPoll.promise;
      await mounted.controller.start();
      vi.advanceTimersByTime(5_000);
      await settlePairingWork();
      await mounted.controller.start();
      firstPoll.resolve({ status: "completed", expiresAtMs: 0n });
      await settlePairingWork();
      expect(mounted.controller.ephemeralId()).toBe(REQUEST_ID_B);
      expect(redirects.count).toBe(0);
    } finally {
      mounted.dispose();
    }
  });

  test("drops a stale confirmation success after a replacement request", async () => {
    const confirmation = Promise.withResolvers<{ ok: boolean }>();
    pairPollImplementation = async () => ({ status: "verification_required", expiresAtMs: 0n });
    pairConfirmImplementation = () => confirmation.promise;
    const redirects = { count: 0 };
    const mounted = mountCeremony(redirects);
    try {
      await mounted.controller.start();
      vi.advanceTimersByTime(5_000);
      await settlePairingWork();
      mounted.controller.updateVerificationCode("123 456");
      const firstConfirmation = mounted.controller.confirm();
      await settlePairingWork();
      await mounted.controller.start();
      confirmation.resolve({ ok: true });
      await firstConfirmation;
      expect(redirects.count).toBe(0);
      expect(mounted.controller.ephemeralId()).toBe(REQUEST_ID_B);
      expect(sessionValues.has(PAIRING_CEREMONY_STORAGE_KEY)).toBe(true);
    } finally {
      mounted.dispose();
    }
  });

  test("recovers a detached confirmation that commits after an interim poll", async () => {
    const originalCommit = Promise.withResolvers<void>();
    let originalCommitted = false;
    void originalCommit.promise.then(() => { originalCommitted = true; });
    let polls = 0;
    pairPollImplementation = async () => {
      polls += 1;
      return {
        status: polls === 1 || !originalCommitted ? "verification_required" : "completed",
        expiresAtMs: 0n,
      };
    };
    let confirmations = 0;
    pairConfirmImplementation = async () => {
      confirmations += 1;
      if (confirmations === 1) throw new Error("transport interrupted");
      throw new ConnectError("confirmation replayed", Code.FailedPrecondition);
    };
    const redirects = { count: 0 };
    const mounted = mountCeremony(redirects);
    try {
      await mounted.controller.start();
      vi.advanceTimersByTime(5_000);
      await settlePairingWork();
      mounted.controller.updateVerificationCode("123456");
      await mounted.controller.confirm();
      expect(pairConfirm).toHaveBeenCalledTimes(1);
      expect(mounted.controller.busy()).toBe(true);
      await mounted.controller.confirm();
      expect(pairConfirm).toHaveBeenCalledTimes(1);

      vi.advanceTimersByTime(10);
      await settlePairingWork();
      expect(mounted.controller.busy()).toBe(false);
      originalCommit.resolve();
      await settlePairingWork();
      await mounted.controller.confirm();
      expect(pairConfirm).toHaveBeenCalledTimes(2);
      expect(mounted.controller.busy()).toBe(true);
      vi.advanceTimersByTime(0);
      await settlePairingWork();
      expect(redirects.count).toBe(1);
      expect(sessionValues.has(PAIRING_CEREMONY_STORAGE_KEY)).toBe(false);
    } finally {
      mounted.dispose();
    }
  });

  test("re-arms confirmation recovery behind an older in-flight poll", async () => {
    const olderPoll = Promise.withResolvers<{ status: string; expiresAtMs: bigint }>();
    let polls = 0;
    pairPollImplementation = () => {
      polls += 1;
      if (polls === 1) return Promise.resolve({ status: "verification_required", expiresAtMs: 0n });
      if (polls === 2) return olderPoll.promise;
      return Promise.resolve({ status: "completed", expiresAtMs: 0n });
    };
    pairConfirmImplementation = async () => { throw new Error("transport interrupted"); };
    const redirects = { count: 0 };
    const mounted = mountCeremony(redirects);
    try {
      await mounted.controller.start();
      vi.advanceTimersByTime(5_000);
      await settlePairingWork();
      vi.advanceTimersByTime(5_000);
      await settlePairingWork();
      mounted.controller.updateVerificationCode("123456");
      await mounted.controller.confirm();
      vi.advanceTimersByTime(10);
      await settlePairingWork();
      olderPoll.resolve({ status: "verification_required", expiresAtMs: 0n });
      await settlePairingWork();
      vi.advanceTimersByTime(5_000);
      await settlePairingWork();
      expect(redirects.count).toBe(1);
    } finally {
      mounted.dispose();
    }
  });
  test("clears terminal failures and fences a completion after unmount", async () => {
    pairCreateImplementation = async () => {
      throw new ConnectError("pairing client must reload", Code.FailedPrecondition);
    };
    const terminal = mountCeremony({ count: 0 });
    try {
      await terminal.controller.start();
      expect(terminal.controller.pollStatus()).toBe("error");
      expect(sessionValues.has(PAIRING_CEREMONY_STORAGE_KEY)).toBe(false);
    } finally {
      terminal.dispose();
    }

    const pendingPoll = Promise.withResolvers<{ status: string; expiresAtMs: bigint }>();
    pairCreateImplementation = async (request) => ({ ephemeralId: request.ephemeralId });
    pairPollImplementation = () => pendingPoll.promise;
    const redirects = { count: 0 };
    const unmounted = mountCeremony(redirects);
    await unmounted.controller.start();
    vi.advanceTimersByTime(5_000);
    await settlePairingWork();
    unmounted.dispose();
    pendingPoll.resolve({ status: "completed", expiresAtMs: 0n });
    await settlePairingWork();
    expect(redirects.count).toBe(0);
  });
});
