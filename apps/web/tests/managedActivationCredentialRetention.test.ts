// This suite pins owner-activation credentials across startup tenant cleanup.
// It models BroadcastChannel sender isolation, URL scrubbing, and the activation request.
// The real account transaction remains dependency-injected so no network or IndexedDB is needed.

import { afterAll, expect, test } from "bun:test";

const ROUTE_KEY = "c".repeat(64);
const ACTIVATION_TOKEN = "A".repeat(43);
const PASSWORD = "correct horse battery staple";

class MemoryStorage implements Storage {
  private readonly values = new Map<string, string>();
  get length(): number { return this.values.size; }
  clear(): void { this.values.clear(); }
  getItem(key: string): string | null { return this.values.get(key) ?? null; }
  key(index: number): string | null { return [...this.values.keys()][index] ?? null; }
  removeItem(key: string): void { this.values.delete(key); }
  setItem(key: string, value: string): void { this.values.set(key, value); }
}

class FakeBroadcastChannel {
  static readonly openChannels = new Set<FakeBroadcastChannel>();
  onmessage: ((event: MessageEvent<unknown>) => void) | null = null;
  readonly name: string;

  constructor(name: string) {
    this.name = name;
    FakeBroadcastChannel.openChannels.add(this);
  }

  postMessage(data: unknown): void {
    for (const peer of FakeBroadcastChannel.openChannels) {
      if (peer !== this && peer.name === this.name) {
        queueMicrotask(() => peer.onmessage?.({ data } as MessageEvent<unknown>));
      }
    }
  }

  close(): void {
    FakeBroadcastChannel.openChannels.delete(this);
  }
}

const browserGlobals = [
  "BroadcastChannel",
  "history",
  "localStorage",
  "location",
  "sessionStorage",
] as const;
const previousGlobals = new Map(
  browserGlobals.map((name) => [name, Object.getOwnPropertyDescriptor(globalThis, name)]),
);
const localStorage = new MemoryStorage();
const sessionStorage = new MemoryStorage();
const locationValue = {
  origin: "https://dashboard.roosttt.com",
  pathname: `/activate/${ROUTE_KEY}`,
  search: "",
  hash: `#${ACTIVATION_TOKEN}`,
  reloads: 0,
  reload() { this.reloads++; },
};
Object.defineProperty(globalThis, "BroadcastChannel", {
  configurable: true,
  value: FakeBroadcastChannel,
});
Object.defineProperty(globalThis, "localStorage", { configurable: true, value: localStorage });
Object.defineProperty(globalThis, "sessionStorage", { configurable: true, value: sessionStorage });
Object.defineProperty(globalThis, "location", { configurable: true, value: locationValue });
Object.defineProperty(globalThis, "history", {
  configurable: true,
  value: {
    replaceState(_data: unknown, _unused: string, url: string): void {
      const next = new URL(url, locationValue.origin);
      locationValue.pathname = next.pathname;
      locationValue.search = next.search;
      locationValue.hash = next.hash;
    },
  },
});

// These modules capture browser primitives at evaluation time, so the test
// must install its cross-tab and storage boundary before importing them.
const fragments = await import("../src/auth/fragment-credential.ts");
const { completePendingTenantRouteSwitch } = await import("../src/auth/managed-logout.ts");
const { activateManagedOwner } = await import("../src/auth/managed-account.ts");
const { _announceWebKeyChange } = await import("../src/auth/web-key.ts");

afterAll(() => {
  for (const name of browserGlobals) {
    const descriptor = previousGlobals.get(name);
    if (descriptor) Object.defineProperty(globalThis, name, descriptor);
    else Reflect.deleteProperty(globalThis, name);
  }
});

test("startup key cleanup retains the scrubbed activation through browser submit", async () => {
  expect(fragments.captureAndScrubFragmentCredential()).toEqual({
    kind: "activation",
    token: ACTIVATION_TOKEN,
    routeKey: ROUTE_KEY,
  });
  expect(`${locationValue.pathname}${locationValue.hash}`).toBe("/activate");

  const peerMessage = Promise.withResolvers<unknown>();
  const peer = new FakeBroadcastChannel("roost-web-key-v1");
  peer.onmessage = (event) => peerMessage.resolve(event.data);
  try {
    await completePendingTenantRouteSwitch({
      unsubscribePreviousPush: async () => {},
      revokePreviousDevice: async () => {},
      clearClientState: () => {},
      clearWebKeyMaterial: async () => { _announceWebKeyChange("logout"); },
    });
    expect(await peerMessage.promise).toBe("logout");

    const credential = fragments.peekCapturedFragmentCredential();
    expect(credential).toEqual({
      kind: "activation",
      token: ACTIVATION_TOKEN,
      routeKey: ROUTE_KEY,
    });
    if (credential?.kind !== "activation") throw new Error("activation credential was lost");

    const submissions: Array<{ routeKey: string; token: string }> = [];
    let confirmedDashboardId: string | null = null;
    await activateManagedOwner({
      routeKey: credential.routeKey,
      token: credential.token,
      password: PASSWORD,
      confirmation: PASSWORD,
    }, {
      publicKeyB64: async () => "browser-public-key",
      activateOwner: async (routeKey, request) => {
        submissions.push({ routeKey, token: request.token });
        return { dashboardId: "coordinator-id" };
      },
      browserLabel: () => "Owner browser",
      confirmDashboard: async (_routeKey, dashboardId) => {
        confirmedDashboardId = dashboardId;
        return true;
      },
      confirmedDashboardId: () => confirmedDashboardId,
      markKeyAuthorized: () => {},
      resumeBootstrap: () => {},
      clearCredential: fragments.clearCapturedFragmentCredential,
      replaceLocation: (path) => { locationValue.pathname = path; },
    });

    expect(submissions).toEqual([{ routeKey: ROUTE_KEY, token: ACTIVATION_TOKEN }]);
    expect(locationValue.pathname).toBe("/app");
    expect(locationValue.reloads).toBe(0);
  } finally {
    peer.close();
  }
});
