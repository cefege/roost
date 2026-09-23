// Provider behavior coverage observes the tab record and RPC contract across
// discovery gating, ambiguous retry, and terminal cleanup. The JSX shim keeps
// the test independent of browser custom elements while preserving Solid state.

import { Code, ConnectError } from "@connectrpc/connect";
import { afterAll, afterEach, beforeEach, describe, expect, mock, test, vi } from "bun:test";
import type * as SolidApi from "solid-js";
import type { PairApprovalContextValue } from "../src/components/PairApprovalProvider.tsx";

const REQUEST_ID = "0123456789abcdef0123456789abcdef";
const REQUEST_CODE = "654321";
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

type PairApproveInput = {
  ceremonyVersion: number;
  ephemeralId: string;
  verificationCode: string;
};

let pairApproveImplementation: (request: PairApproveInput) => Promise<{ ok: boolean }>;
const pairApprove = mock((request: PairApproveInput) => pairApproveImplementation(request));
const deletePairRequest = mock(() => undefined);

mock.module("@roost/shared/pairing", () => ({
  PAIRING_CEREMONY_VERSION: 1,
  PAIR_VERIFICATION_CODE_LENGTH: 6,
  generatePairRequestId: () => "0123456789abcdef0123456789abcdef",
  generatePairRequesterToken: () => "0".repeat(64),
  generatePairVerificationCode: () => REQUEST_CODE,
  normalizePairRequestId: (value: string) => /^[0-9a-f]{32}$/.test(value) ? value : null,
  normalizePairRequesterToken: (value: string) => /^[0-9a-f]{64}$/.test(value) ? value : null,
  normalizePairVerificationCode: (value: string) => /^\d{6}$/.test(value) ? value : null,
}));
mock.module("@roost/shared/retry", () => ({ backoffDelayMs: () => 10 }));
mock.module("../src/connect.ts", () => ({ coordClient: { pairApprove } }));
mock.module("../src/store/mutations.ts", () => ({ deletePairRequest }));
mock.module("../src/store/toastStore.ts", () => ({ addToast: () => undefined }));
mock.module("../src/components/PairVerificationCodeDialog.tsx", () => ({
  PairVerificationCodeDialog: () => null,
}));

// Load client Solid after mocks select a renderer-free runtime for this DOM suite.
const Solid = await import(new URL("./solid.js", import.meta.resolve("solid-js")).href) as typeof SolidApi;
mock.module("solid-js", () => Solid);

type VNode = { tag: unknown; props: Record<string, unknown> };

function createElement(
  tag: unknown,
  props: Record<string, unknown> | null,
  ...children: unknown[]
): VNode {
  const merged = { ...(props ?? {}) };
  if (children.length > 0) merged.children = children.length === 1 ? children[0] : children;
  return { tag, props: merged };
}

const ReactShim = { Fragment: Symbol("Fragment"), createElement };
(globalThis as typeof globalThis & { React: unknown }).React = ReactShim;
mock.module("react/jsx-dev-runtime", () => ({
  Fragment: ReactShim.Fragment,
  jsxDEV(tag: unknown, props: Record<string, unknown> | null): VNode {
    const children = props?.children === undefined ? [] : [props.children];
    return createElement(tag, props, ...children);
  },
}));

// Dynamic imports follow the storage, protocol, and component mocks above.
const { PAIR_APPROVAL_STORAGE_KEY } = await import("../src/auth/pairing-approval.ts");
const { PairApprovalProvider } = await import("../src/components/PairApprovalProvider.tsx");

function mountProvider(enabled = true) {
  let context: PairApprovalContextValue | undefined;
  let dispose: (() => void) | undefined;
  Solid.createRoot((disposeRoot) => {
    dispose = disposeRoot;
    const tree = PairApprovalProvider({ enabled, children: null });
    context = (tree as unknown as VNode).props.value as PairApprovalContextValue;
  });
  return { context: context!, dispose: dispose! };
}

function mountDiscoveryGatedProvider() {
  let context: PairApprovalContextValue | undefined;
  let setEnabled: ((value: boolean) => void) | undefined;
  let dispose: (() => void) | undefined;
  Solid.createRoot((disposeRoot) => {
    dispose = disposeRoot;
    const [enabled, setEnabledSignal] = Solid.createSignal(false);
    setEnabled = (value) => { setEnabledSignal(value); };
    const tree = PairApprovalProvider({
      get enabled() { return enabled(); },
      children: null,
    });
    context = (tree as unknown as VNode).props.value as PairApprovalContextValue;
  });
  return { context: context!, dispose: dispose!, setEnabled: setEnabled! };
}

async function settleApprovalWork(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
}

beforeEach(() => {
  vi.useFakeTimers();
  sessionValues.clear();
  pairApprove.mockClear();
  deletePairRequest.mockClear();
  pairApproveImplementation = async () => ({ ok: true });
});

afterEach(() => {
  vi.clearAllTimers();
  vi.useRealTimers();
});

afterAll(() => {
  if (originalSessionStorage) Object.defineProperty(globalThis, "sessionStorage", originalSessionStorage);
  else Reflect.deleteProperty(globalThis, "sessionStorage");
});

describe("PairApprovalProvider", () => {
  test("persists a generated code before approval and only completes after acknowledgement", async () => {
    const response = Promise.withResolvers<{ ok: boolean }>();
    pairApproveImplementation = () => response.promise;
    const mounted = mountProvider();
    try {
      const approval = mounted.context.approve({
        ephemeralId: REQUEST_ID,
        requesterLabel: "Kitchen tablet",
        expiresAtMs: Date.now() + 60_000,
      });
      await settleApprovalWork();
      expect(pairApprove).toHaveBeenCalledWith({
        ceremonyVersion: 1,
        ephemeralId: REQUEST_ID,
        verificationCode: REQUEST_CODE,
      });
      expect(JSON.parse(sessionValues.get(PAIR_APPROVAL_STORAGE_KEY)!)).toMatchObject({
        ceremonyVersion: 1,
        ephemeralId: REQUEST_ID,
        verificationCode: REQUEST_CODE,
        requesterLabel: "Kitchen tablet",
      });
      expect(deletePairRequest).not.toHaveBeenCalled();
      expect(mounted.context.busyRequestId()).toBe(REQUEST_ID);

      response.resolve({ ok: true });
      await approval;
      expect(deletePairRequest).toHaveBeenCalledWith(REQUEST_ID);
      expect(sessionValues.has(PAIR_APPROVAL_STORAGE_KEY)).toBe(true);
      expect(mounted.context.busyRequestId()).toBe(REQUEST_ID);
    } finally {
      mounted.dispose();
    }
  });

  test("defers persisted approval until discovery and retries an ambiguous result exactly", async () => {
    sessionValues.set(PAIR_APPROVAL_STORAGE_KEY, JSON.stringify({
      ceremonyVersion: 1,
      ephemeralId: REQUEST_ID,
      verificationCode: REQUEST_CODE,
      requesterLabel: "Kitchen tablet",
      expiresAtMs: Date.now() + 60_000,
    }));
    let attempts = 0;
    pairApproveImplementation = async () => {
      attempts += 1;
      if (attempts === 1) throw new ConnectError("offline", Code.Unavailable);
      return { ok: true };
    };
    const mounted = mountDiscoveryGatedProvider();
    try {
      await settleApprovalWork();
      expect(pairApprove).not.toHaveBeenCalled();
      mounted.setEnabled(true);
      await settleApprovalWork();
      expect(pairApprove).toHaveBeenCalledTimes(1);
      vi.advanceTimersByTime(10);
      await settleApprovalWork();
      expect(pairApprove).toHaveBeenCalledTimes(2);
      expect(pairApprove.mock.calls[0]?.[0]).toEqual(pairApprove.mock.calls[1]?.[0]);
      expect(pairApprove.mock.calls[1]?.[0]).toEqual({
        ceremonyVersion: 1,
        ephemeralId: REQUEST_ID,
        verificationCode: REQUEST_CODE,
      });
      expect(deletePairRequest).toHaveBeenCalledWith(REQUEST_ID);
    } finally {
      mounted.dispose();
    }
  });

  test("clears expired and terminally rejected approval records", async () => {
    sessionValues.set(PAIR_APPROVAL_STORAGE_KEY, JSON.stringify({
      ceremonyVersion: 1,
      ephemeralId: REQUEST_ID,
      verificationCode: REQUEST_CODE,
      requesterLabel: "Kitchen tablet",
      expiresAtMs: Date.now() - 1,
    }));
    const expired = mountProvider();
    try {
      await settleApprovalWork();
      expect(pairApprove).not.toHaveBeenCalled();
      expect(sessionValues.has(PAIR_APPROVAL_STORAGE_KEY)).toBe(false);
    } finally {
      expired.dispose();
    }

    pairApproveImplementation = async () => {
      throw new ConnectError("not found", Code.NotFound);
    };
    const rejected = mountProvider();
    try {
      await rejected.context.approve({
        ephemeralId: REQUEST_ID,
        requesterLabel: "Kitchen tablet",
        expiresAtMs: Date.now() + 60_000,
      });
      expect(sessionValues.has(PAIR_APPROVAL_STORAGE_KEY)).toBe(false);
      expect(rejected.context.busyRequestId()).toBeNull();
    } finally {
      rejected.dispose();
    }
  });
});
