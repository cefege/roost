// PairingRequesterProvider sits above the access gate, so a restored tab
// ceremony must resume and finish from the provider alone — even when the gate
// never renders the pairing page because bootstrap proved authorization first.
// Uses the client-Solid virtual renderer with the coordinator client mocked.

import { Code, ConnectError } from "@connectrpc/connect";
import { afterAll, afterEach, beforeEach, describe, expect, mock, test, vi } from "bun:test";
import type * as SolidApi from "solid-js";
import type { PairingRequester } from "../src/components/pairing/PairingRequesterProvider.tsx";

const REQUEST_ID = "0123456789abcdef0123456789abcdef";
const REQUESTER_TOKEN = "ab".repeat(32);
const sessionValues = new Map<string, string>();
const originalSessionStorage = Object.getOwnPropertyDescriptor(globalThis, "sessionStorage");
const originalWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
const redirects: string[] = [];

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
Object.defineProperty(globalThis, "window", {
  configurable: true,
  value: { location: { replace: (url: string) => { redirects.push(url); } } },
});

type PairCreateInput = { ephemeralId: string; requesterToken: string };
let pairCreateImplementation: (request: PairCreateInput) => Promise<{ ephemeralId: string }>;
let pairPollImplementation: () => Promise<{ status: string }>;
const pairCreate = mock((request: PairCreateInput) => pairCreateImplementation(request));
const pairPoll = mock(() => pairPollImplementation());

mock.module("@roost/protocol/retry", () => ({ backoffDelayMs: () => 10 }));
mock.module("../src/client/rpc/connect.ts", () => ({ coordClient: { pairCreate, pairPoll } }));
mock.module("../src/client/auth/web-key.ts", () => ({ getPublicKeyB64: async () => "requester-public-key" }));
mock.module("../src/browser/browserSelfLabel.ts", () => ({ browserSelfLabel: () => "Requester browser" }));
mock.module("../src/store/toastStore.ts", () => ({ addToast: () => undefined }));

// Load client Solid after the module mocks select the browser-safe runtime.
const Solid = await import(new URL("./solid.js", import.meta.resolve("solid-js")).href) as typeof SolidApi;
mock.module("solid-js", () => Solid);

type VNode = { tag: unknown; props: Record<string, unknown> };
const ReactShim = {
  Fragment: Symbol("Fragment"),
  createElement: (tag: unknown, props: Record<string, unknown> | null): VNode => ({ tag, props: { ...(props ?? {}) } }),
};
(globalThis as typeof globalThis & { React: unknown }).React = ReactShim;
mock.module("react/jsx-dev-runtime", () => ({
  Fragment: ReactShim.Fragment,
  jsxDEV: (tag: unknown, props: Record<string, unknown> | null) => ReactShim.createElement(tag, props),
}));

// Dynamic imports follow the mocked protocol, storage, and renderer boundaries.
const { PAIRING_CEREMONY_STORAGE_KEY, savePairingCeremony } = await import("../src/client/auth/pairing-ceremony.ts");
const { PairingRequesterProvider } = await import("../src/components/pairing/PairingRequesterProvider.tsx");

const disposers: Array<() => void> = [];

function mountProvider(): PairingRequester {
  let requester: PairingRequester | undefined;
  Solid.createRoot((dispose) => {
    disposers.push(dispose);
    const tree = PairingRequesterProvider({ children: null }) as unknown as VNode;
    requester = tree.props.value as PairingRequester;
  });
  return requester!;
}

async function settle(): Promise<void> {
  for (let tick = 0; tick < 6; tick += 1) await Promise.resolve();
}

beforeEach(() => {
  vi.useFakeTimers();
  sessionValues.clear();
  redirects.length = 0;
  pairCreate.mockClear();
  pairPoll.mockClear();
  pairCreateImplementation = async (request) => ({ ephemeralId: request.ephemeralId });
  pairPollImplementation = async () => ({ status: "pending" });
});

afterEach(() => {
  while (disposers.length > 0) disposers.pop()?.();
  vi.clearAllTimers();
  vi.useRealTimers();
});

afterAll(() => {
  if (originalSessionStorage) Object.defineProperty(globalThis, "sessionStorage", originalSessionStorage);
  else Reflect.deleteProperty(globalThis, "sessionStorage");
  if (originalWindow) Object.defineProperty(globalThis, "window", originalWindow);
  else Reflect.deleteProperty(globalThis, "window");
});

describe("PairingRequesterProvider", () => {
  test("resumes a restored tab ceremony and finalizes it without a pairing page", async () => {
    savePairingCeremony({ ceremonyVersion: 1, ephemeralId: REQUEST_ID, requesterToken: REQUESTER_TOKEN });
    const requester = mountProvider();
    await settle();

    expect(pairCreate).toHaveBeenCalledTimes(1);
    expect(pairCreate.mock.calls[0]![0]).toMatchObject({
      ephemeralId: REQUEST_ID,
      requesterToken: REQUESTER_TOKEN,
    });
    expect(requester.ephemeralId()).toBe(REQUEST_ID);

    pairPollImplementation = async () => ({ status: "completed" });
    vi.advanceTimersByTime(5_000);
    await settle();

    expect(redirects).toEqual(["/"]);
    expect(sessionValues.has(PAIRING_CEREMONY_STORAGE_KEY)).toBe(false);
  });

  test("exposes a terminal request failure until the next request clears it", async () => {
    pairCreateImplementation = async () => {
      throw new ConnectError("device key rejected", Code.PermissionDenied);
    };
    const requester = mountProvider();
    await requester.start();

    expect(requester.pollStatus()).toBe("error");
    expect(requester.requestError()).toContain("device key rejected");
    expect(sessionValues.has(PAIRING_CEREMONY_STORAGE_KEY)).toBe(false);

    requester.clearRequestError();
    expect(requester.requestError()).toBeNull();
  });
});
