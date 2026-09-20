// Direct-peer election is driven through a fake registry and direct connections.
// The tests prove document caps and heartbeat activation without depending on
// browser ICE, coordinator signaling, or a terminal replica implementation.

import { describe, expect, mock, test, vi } from "bun:test";
let terminalGrantRefreshes = 0;

mock.module("../src/ws/local-terminal-grants.ts", () => ({
  LOCAL_TERMINAL_GRANT_RETRY_MS: 30_000,
  currentTerminalGrant: () => null,
  dropTerminalGrant: () => undefined,
  refreshTerminalGrant: () => {
    terminalGrantRefreshes += 1;
    return Promise.resolve(null);
  },
  setTerminalGrantDemand: () => undefined,
  subscribeTerminalGrant: () => () => undefined,
}));
mock.module("../src/ws/terminal-input-router.ts", () => ({
  claimTerminalInputRoute: () => Promise.resolve({ accepted: false, unsupported: true, reason: "unsupported" }),
  drainTerminalInput: () => Promise.resolve(),
  holdTerminalInput: () => () => undefined,
  retireTerminalInput: () => undefined,
  terminalInputPhase: () => null,
  settleTerminalInput: () => undefined,
}));
mock.module("../src/ws/sync-outbound.ts", () => ({
  terminalInputDestinationForDirectConnection: () => null,
  terminalInputDestinationForSession: () => null,
}));
mock.module("../src/ws/terminal-peer-connection.ts", () => ({ TerminalPeerConnection: class {} }));
mock.module("../src/store/terminal-stream-promotion.ts", () => ({ createTerminalSessionPromotion: () => null }));
mock.module("../src/store/terminal-stream-publication.ts", () => ({ currentTerminalGenerationToken: () => null }));
mock.module("../src/store/terminal-stream-types.ts", () => ({
  terminalGenerationTokenEquals: (
    left: { socketId?: string; processEpoch?: string } | null,
    right: { socketId?: string; processEpoch?: string } | null,
  ) => left?.socketId === right?.socketId && left?.processEpoch === right?.processEpoch,
}));
mock.module("../src/store/sync.ts", () => ({ registerSyncV2ProbeResultHandler: () => () => undefined }));
mock.module("../src/connect.ts", () => ({ coordClient: {} }));
mock.module("../src/store/root.ts", () => ({ rootStore: { auth_generation: 0 } }));

// These mocks isolate the orchestration state machine from browser-only adapters.
const peer = await import("../src/ws/terminal-peer.ts");

type RegistryListener = (event: { kind: string; workerFp?: string; sessionId?: string; viewId?: string; active?: boolean; token?: Token; reason?: string }) => void;
interface Token {
  socketGeneration: number; socketId: string; processEpoch: string; domainGeneration: bigint; transportKind: "loopback" | "webrtc"; workerFp: string;
}
class FakeRegistry {
  readonly listeners = new Set<RegistryListener>();
  readonly activeSessions = new Set<string>();
  subscribe(listener: RegistryListener): () => void { this.listeners.add(listener); return () => this.listeners.delete(listener); }
  emit(event: Parameters<RegistryListener>[0]): void {
    if (event.kind === "demand_changed" && event.workerFp && event.sessionId && typeof event.active === "boolean") {
      const key = JSON.stringify([event.workerFp, event.sessionId]);
      if (event.active) this.activeSessions.add(key);
      else this.activeSessions.delete(key);
    }
    for (const listener of this.listeners) listener(event);
  }
  hasViewDemand(workerFp: string, sessionId: string): boolean {
    return this.activeSessions.has(JSON.stringify([workerFp, sessionId]));
  }
}

class FakeConnection {
  readonly kind: "loopback" | "webrtc";
  readonly connectionId: string;
  readonly workerEpoch: string;
  readonly inputRouteSupported = true;
  readonly tokenValue: Token;
  probeCalls = 0;
  closed = false;
  constructor(
    readonly workerFp: string,
    kind: "loopback" | "webrtc" = "webrtc",
    private readonly probeOperation: () => Promise<void> = () => Promise.resolve(),
  ) {
    this.kind = kind;
    this.connectionId = `connection-${workerFp}`;
    this.workerEpoch = `epoch-${workerFp}`;
    this.tokenValue = {
      socketGeneration: 1,
      socketId: this.connectionId,
      processEpoch: this.workerEpoch,
      domainGeneration: 1n,
      transportKind: kind,
      workerFp,
    };
  }
  token(): Token { return this.tokenValue; }
  allowsSession(): boolean { return true; }
  publishView(): boolean { return true; }
  publishResync(): boolean { return true; }
  sendInput(): "accepted" { return "accepted"; }
  claimInputRoute(): Promise<never> { return Promise.reject(new Error("not used")); }
  requestScrollback(): Promise<never> { return Promise.reject(new Error("not used")); }
  probe(): Promise<void> { this.probeCalls += 1; return this.probeOperation(); }
  close(): void { this.closed = true; }
}

async function settle(): Promise<void> { for (let turn = 0; turn < 8; turn += 1) await Promise.resolve(); }


test("retries a transient initial grant failure at the bounded retry deadline", async () => {
  vi.useFakeTimers();
  terminalGrantRefreshes = 0;
  const previousRtc = globalThis.RTCPeerConnection;
  globalThis.RTCPeerConnection = class {} as typeof RTCPeerConnection;
  const registry = new FakeRegistry();
  const owner = new peer.TerminalPeerOwner({ registry: registry as never, secureContext: () => true, localDoor: () => null });
  try {
    owner.start();
    registry.emit({ kind: "demand_changed", workerFp: "worker-a", sessionId: "session-a", viewId: "view-a", active: true });
    await settle();
    expect(owner.snapshot("worker-a")?.phase).toBe("grant");
    expect(terminalGrantRefreshes).toBe(1);
    vi.advanceTimersByTime(30_000);
    await settle();
    expect(terminalGrantRefreshes).toBe(2);
  } finally {
    owner.dispose("test cleanup");
    globalThis.RTCPeerConnection = previousRtc;
    vi.useRealTimers();
  }
});

describe("TerminalPeerOwner", () => {
  test("starts a content-free heartbeat after a direct promotion commits", async () => {
    const registry = new FakeRegistry();
    const owner = new peer.TerminalPeerOwner({ registry: registry as never, secureContext: () => false, localDoor: () => null });
    const connection = new FakeConnection("worker-a");
    owner.start();
    registry.emit({ kind: "demand_changed", workerFp: "worker-a", sessionId: "session-a", viewId: "view-a", active: true });
    owner.stage(connection as never);
    registry.emit({ kind: "promotion_committed", sessionId: "session-a", token: connection.token() });
    await settle();
    expect(connection.probeCalls).toBe(1);
    owner.dispose("test cleanup");
    expect(connection.closed).toBe(true);
  });

  test("ignores a retired RTC heartbeat after loopback replaces its state", async () => {
    const registry = new FakeRegistry();
    const pendingProbe = Promise.withResolvers<void>();
    const owner = new peer.TerminalPeerOwner({ registry: registry as never, secureContext: () => false, localDoor: () => null });
    const rtc = new FakeConnection("worker-a", "webrtc", () => pendingProbe.promise);
    const loopback = new FakeConnection("worker-a", "loopback");
    owner.start();
    registry.emit({ kind: "demand_changed", workerFp: "worker-a", sessionId: "session-a", viewId: "view-a", active: true });
    owner.stage(rtc as never);
    registry.emit({ kind: "promotion_committed", sessionId: "session-a", token: rtc.token() });
    await settle();

    owner.stage(loopback as never);
    pendingProbe.reject(new Error("retired probe"));
    await settle();

    expect(loopback.closed).toBe(false);
    owner.dispose("test cleanup");
  });

  test("retains a multiplexed peer after its inactive session route retires", () => {
    const registry = new FakeRegistry();
    const owner = new peer.TerminalPeerOwner({ registry: registry as never, secureContext: () => false, localDoor: () => null });
    const connection = new FakeConnection("worker-a");
    owner.start();
    registry.emit({ kind: "demand_changed", workerFp: "worker-a", sessionId: "session-a", viewId: "view-a", active: true });
    owner.stage(connection as never);
    registry.emit({ kind: "demand_changed", workerFp: "worker-a", sessionId: "session-a", viewId: "view-a", active: false });
    registry.emit({ kind: "route_lost", sessionId: "session-a", token: connection.token(), reason: "view retired" });

    expect(owner.snapshot("worker-a")?.hasConnection).toBe(true);
    expect(connection.closed).toBe(false);
    owner.dispose("test cleanup");
  });

  test("refuses a ninth simultaneously demanded browser peer", () => {
    const registry = new FakeRegistry();
    const owner = new peer.TerminalPeerOwner({ registry: registry as never, secureContext: () => false, localDoor: () => null });
    owner.start();
    const connections: FakeConnection[] = [];
    for (let index = 0; index < 9; index += 1) {
      const workerFp = `worker-${index}`;
      registry.emit({ kind: "demand_changed", workerFp, sessionId: `session-${index}`, viewId: `view-${index}`, active: true });
      const connection = new FakeConnection(workerFp);
      connections.push(connection);
      owner.stage(connection as never);
    }
    expect(connections.slice(0, 8).every((connection) => !connection.closed)).toBe(true);
    expect(connections[8]?.closed).toBe(true);
    owner.dispose("test cleanup");
  });
});
