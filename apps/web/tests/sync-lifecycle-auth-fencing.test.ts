// Regression coverage for dashboard scope and authentication ownership across Sync links.
//
// Drives the production singleton loop against a fake WebSocket. Deferred signing and
// close events model dashboard cutovers and retired-link callbacks; a replacement
// may open only in the released scope, and only the current link may revoke auth.

import { afterAll, beforeAll, describe, expect, mock, test, vi } from "bun:test";
import { setRootStore } from "../src/store/root.ts";

class FakeSyncSocket extends EventTarget {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSING = 2;
  static readonly CLOSED = 3;
  readyState = FakeSyncSocket.CONNECTING;
  binaryType = "blob";
  readonly closes: Array<{ code: number; reason: string }> = [];
  deferCloseEvent = false;
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: ArrayBuffer }) => void) | null = null;
  onerror: ((event: unknown) => void) | null = null;
  onclose: ((event: { code: number; reason: string }) => void) | null = null;

  constructor(readonly url: string, readonly protocols?: string[]) {
    super();
    dialed.push(this);
  }

  send(_data: Uint8Array): void {}

  close(code = 1000, reason = ""): void {
    if (this.readyState === FakeSyncSocket.CLOSED) return;
    this.closes.push({ code, reason });
    this.readyState = this.deferCloseEvent
      ? FakeSyncSocket.CLOSING
      : FakeSyncSocket.CLOSED;
    if (!this.deferCloseEvent) this.onclose?.({ code, reason });
  }

  serverClose(code = 1000, reason = ""): void {
    this.readyState = FakeSyncSocket.CLOSED;
    this.onclose?.({ code, reason });
  }
}

const dialed: FakeSyncSocket[] = [];
const storage = new Map<string, string>();
const fakeDocument = Object.assign(new EventTarget(), { visibilityState: "visible" });
const fakeWindow = new EventTarget();

let pendingJwtSign: Promise<string> | null = null;
let noteJwtSignStarted: (() => void) | null = null;

function signTestJwt(): Promise<string> {
  const notifySignStarted = noteJwtSignStarted;
  noteJwtSignStarted = null;
  notifySignStarted?.();
  const pendingSign = pendingJwtSign;
  pendingJwtSign = null;
  return pendingSign ?? Promise.resolve("test-jwt");
}

Object.assign(globalThis, {
  WebSocket: FakeSyncSocket,
  document: fakeDocument,
  window: fakeWindow,
  location: {
    origin: "http://127.0.0.1:65000",
    protocol: "http:",
    host: "127.0.0.1:65000",
    hostname: "127.0.0.1",
    href: "http://127.0.0.1:65000/",
    pathname: "/",
    search: "",
    hash: "",
  },
  localStorage: {
    getItem: (key: string) => storage.get(key) ?? null,
    setItem: (key: string, value: string) => { storage.set(key, value); },
    removeItem: (key: string) => { storage.delete(key); },
  },
});

mock.module("../src/auth/web-key.ts", () => ({
  signCoordinatorJwt: signTestJwt,
  getPublicKeyB64: async () => "test-key",
}));

// The browser fakes and JWT mock must exist before the Sync singleton evaluates.
const sync = await import("../src/store/sync.ts");
const { SYNC_OPEN_TIMEOUT_MS } = await import("../src/store/sync-watchdog.ts");
const { setForceHidden, setForceVisible } = await import("../src/lib/pageVisible.ts");

async function flush(turns = 24): Promise<void> {
  for (let turn = 0; turn < turns; turn++) await Promise.resolve();
}

async function advance(ms: number): Promise<void> {
  vi.advanceTimersByTime(ms);
  await flush();
}

beforeAll(() => {
  vi.useFakeTimers();
  setForceVisible(true);
  setRootStore("selected_dashboard_id", "dashboard-old");
  void sync._runConnectSync();
});

afterAll(async () => {
  setForceHidden(true);
  storage.set("roostSmoke", "1");
  sync.pauseSyncTransport();
  await flush();
  vi.useRealTimers();
});

describe("Sync link scope and authentication ownership", () => {
  test("an in-flight reconnect opens only the dashboard selected when its hold releases", async () => {
    await flush();
    const currentUrl = new URL(dialed.at(-1)!.url);
    expect(currentUrl.searchParams.get("dashboard")).toBe("dashboard-old");

    const jwtSignStarted = Promise.withResolvers<void>();
    const jwtSignFinished = Promise.withResolvers<string>();
    noteJwtSignStarted = jwtSignStarted.resolve;
    pendingJwtSign = jwtSignFinished.promise;

    const dialCount = dialed.length;
    sync._requestSyncRedial();
    await jwtSignStarted.promise;

    sync.holdSyncForDashboardSwitch();
    jwtSignFinished.resolve("test-jwt");
    await flush();
    expect(dialed).toHaveLength(dialCount);

    setRootStore("selected_dashboard_id", "dashboard-new");
    sync.releaseSyncAfterDashboardSwitch();
    await flush();

    expect(dialed).toHaveLength(dialCount + 1);
    const reconnectUrl = new URL(dialed.at(-1)!.url);
    expect(reconnectUrl.searchParams.get("dashboard")).toBe("dashboard-new");
  });

  test("retired and intentionally closed links cannot revoke replacement auth", async () => {
    await flush();
    let authResets = 0;
    const unregister = sync.registerSyncAuthRejectionHandler(() => {
      authResets += 1;
    });

    const dashboardSwitchLink = dialed.at(-1)!;
    dashboardSwitchLink.deferCloseEvent = true;
    sync.closeSyncForDashboardSwitch();
    dashboardSwitchLink.serverClose(4001, "authentication revoked");
    await flush();
    expect(authResets).toBe(0);

    const retiringLink = dialed.at(-1)!;
    const dialsBeforeRetirement = dialed.length;
    retiringLink.deferCloseEvent = true;
    await advance(SYNC_OPEN_TIMEOUT_MS);
    expect(retiringLink.readyState).toBe(FakeSyncSocket.CLOSING);
    await advance(5_000);
    await advance(sync.syncRedialStatus().nextDelayMs);
    expect(dialed).toHaveLength(dialsBeforeRetirement + 1);

    retiringLink.serverClose(4001, "authentication revoked");
    expect(authResets).toBe(0);

    const currentLink = dialed.at(-1)!;
    currentLink.serverClose(4001, "authentication revoked");
    expect(authResets).toBe(1);
    unregister();
    await flush();
  });
});
