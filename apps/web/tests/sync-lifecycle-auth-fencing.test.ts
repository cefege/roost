// Regression coverage for Sync link authentication ownership.
//
// Drives the production singleton loop against a fake WebSocket. Deferred close events
// model intentionally retired links: only the current, unretired link may report that
// this browser's credential was revoked.

import { afterAll, beforeAll, describe, expect, mock, test, vi } from "bun:test";

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

const signTestJwt = async (): Promise<string> => "test-jwt";

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
  void sync._runConnectSync();
});

afterAll(async () => {
  setForceHidden(true);
  storage.set("roostSmoke", "1");
  sync.pauseSyncTransport();
  await flush();
  vi.useRealTimers();
});

describe("Sync link authentication ownership", () => {
  test("retired and intentionally closed links cannot revoke replacement auth", async () => {
    await flush();
    let authResets = 0;
    const unregister = sync.registerSyncAuthRejectionHandler(() => {
      authResets += 1;
    });

    const intentionallyClosedLink = dialed.at(-1)!;
    intentionallyClosedLink.deferCloseEvent = true;
    sync._requestSyncRedial();
    intentionallyClosedLink.serverClose(4001, "authentication revoked");
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
