// Direct-peer election is driven through fake registry and direct connections.
// These tests isolate owner lifecycle fencing from browser ICE and promotion frame
// folding, including synchronous grant invalidation and removed-worker tombstones.

import { describe, expect, mock, test, vi } from "bun:test";

let authGeneration = 0;
let terminalGrantRefreshes = 0;
let droppedGrant: ((workerFp: string) => void) | null = null;
const retiredWorkers = new Set<string>();
const grantsByWorker = new Map<string, { workerEpoch: string; peerSupported: boolean; workerFp: string; stunUrls: string[] }>();
let probeResultHandler: ((result: { workerFp: string; workerEpoch: string }) => void) | null = null;
const promotionCommitHooks: Array<(connection: unknown) => void> = [];
const promotionStageCalls: Array<{ sessionId: string; connection: unknown }> = [];
mock.module("../src/ws/local-terminal-grants.ts", () => ({
  LOCAL_TERMINAL_GRANT_RETRY_MS: 30_000,
  currentTerminalGrant: (workerFp: string) => grantsByWorker.get(workerFp) ?? null,
  dropTerminalGrant: (workerFp: string) => { grantsByWorker.delete(workerFp); droppedGrant?.(workerFp); },
  isTerminalGrantWorkerRetired: (workerFp: string) => retiredWorkers.has(workerFp),
  refreshTerminalGrant: () => { terminalGrantRefreshes++; return Promise.resolve(null); },
  setTerminalGrantDemand: () => undefined,
  subscribeTerminalGrant: () => () => undefined,
}));
mock.module("../src/ws/terminal-input-router.ts", () => ({
  retireTerminalInput: () => undefined,
  settleTerminalInput: () => undefined,
  terminalInputPhase: () => null,
}));
mock.module("../src/ws/terminal-peer-fallback.ts", () => ({
  TerminalPeerFallbackClaims: class {
    claim(): void {}
    retire(): void {}
    dispose(): void {}
  },
}));
mock.module("../src/ws/terminal-peer-promotions.ts", () => ({
  TerminalPeerPromotions: class {
    constructor(_workerFp: unknown, _registry: unknown, _recovery: unknown, hooks: { committed(connection: unknown): void }) {
      promotionCommitHooks.push(hooks.committed);
    }
    stage(sessionId: string, connection: unknown): void { promotionStageCalls.push({ sessionId, connection }); }
    cancelSession(): void {}
    retireConnection(): void {}
    dispose(): void {}
  },
}));
mock.module("../src/ws/terminal-peer-connection.ts", () => ({ TerminalPeerConnection: class {} }));
mock.module("../src/store/sync.ts", () => ({
  registerSyncV2ProbeResultHandler: (handler: (result: { workerFp: string; workerEpoch: string }) => void) => {
    probeResultHandler = handler;
    return () => { if (probeResultHandler === handler) probeResultHandler = null; };
  },
}));
mock.module("../src/connect.ts", () => ({ coordClient: {} }));
mock.module("../src/store/root.ts", () => ({ rootStore: { get auth_generation() { return authGeneration; } } }));

// The mocks above must install before the owner imports its direct dependencies.
const peer = await import("../src/ws/terminal-peer.ts");

interface Token {
  socketGeneration: number; socketId: string; processEpoch: string; domainGeneration: bigint; transportKind: "loopback" | "webrtc"; workerFp: string;
}

type RegistryEvent = { kind: string; workerFp?: string; sessionId?: string; viewId?: string; active?: boolean; token?: Token; reason?: string };
type RegistryListener = (event: RegistryEvent) => void;

class FakeRegistry {
  readonly listeners = new Set<RegistryListener>();
  readonly activeSessions = new Set<string>();
  readonly routes = new Map<string, FakeConnection>();
  candidate: FakeConnection | null = null;
  subscribe(listener: RegistryListener): () => void { this.listeners.add(listener); return () => this.listeners.delete(listener); }
  emit(event: RegistryEvent): void {
    if (event.kind === "demand_changed" && event.workerFp && event.sessionId && typeof event.active === "boolean") {
      const key = JSON.stringify([event.workerFp, event.sessionId]);
      if (event.active) this.activeSessions.add(key); else this.activeSessions.delete(key);
    }
    for (const listener of this.listeners) listener(event);
  }
  hasViewDemand(workerFp: string, sessionId: string): boolean { return this.activeSessions.has(JSON.stringify([workerFp, sessionId])); }
  activeForSession(sessionId: string): FakeConnection | null { return this.routes.get(sessionId) ?? null; }
  candidateForWorker(): FakeConnection | null { return this.candidate; }
  targetForToken(token: Token): FakeConnection | null {
    return this.candidate?.token() === token ? this.candidate : null;
  }
  hasRoutesForConnection(connection: FakeConnection): boolean { return [...this.routes.values()].includes(connection); }
  register(connection: FakeConnection): () => void {
    this.candidate = connection;
    return () => { if (this.candidate === connection) this.candidate = null; };
  }
}

class FakeConnection {
  readonly connectionId: string;
  readonly workerEpoch: string;
  readonly inputRouteSupported = true;
  readonly tokenValue: Token;
  probeCalls = 0;
  closeCalls = 0;
  closed = false;
  onClose: (() => void) | null = null;
  constructor(
    readonly workerFp: string,
    readonly kind: "loopback" | "webrtc" = "webrtc",
    private readonly probeOperation: () => Promise<void> = () => Promise.resolve(),
  ) {
    this.connectionId = `connection-${workerFp}-${kind}`;
    this.workerEpoch = `epoch-${workerFp}`;
    this.tokenValue = { socketGeneration: 1, socketId: this.connectionId, processEpoch: this.workerEpoch, domainGeneration: 1n, transportKind: kind, workerFp };
  }
  token(): Token { return this.tokenValue; }
  allowsSession(): boolean { return true; }
  publishView(): boolean { return true; }
  publishResync(): boolean { return true; }
  sendInput(): "accepted" { return "accepted"; }
  claimInputRoute(): Promise<never> { return Promise.reject(new Error("not used")); }
  requestScrollback(): Promise<never> { return Promise.reject(new Error("not used")); }
  probe(): Promise<void> { this.probeCalls += 1; return this.probeOperation(); }
  close(): void { this.closeCalls += 1; this.closed = true; this.onClose?.(); }
}

interface PeerOwnerStateForTest { connection: FakeConnection | null; controller: AbortController | null; phase: string; }
interface PeerOwnerInternals { states: Map<string, PeerOwnerStateForTest>; peerReady(state: PeerOwnerStateForTest, connection: FakeConnection, controller: AbortController, authGeneration: number): void; }
async function settle(): Promise<void> { for (let turn = 0; turn < 8; turn += 1) await Promise.resolve(); }

function resetState(): void {
  authGeneration = 0;
  terminalGrantRefreshes = 0;
  droppedGrant = null;
  retiredWorkers.clear();
  grantsByWorker.clear();
  probeResultHandler = null;
  promotionCommitHooks.length = 0;
  promotionStageCalls.length = 0;
}

test("retries a transient initial grant failure at the bounded retry deadline", async () => {
  vi.useFakeTimers(); resetState();
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
    resetState();
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

  test("keeps a candidate qualified while a prior input route drains", async () => {
    vi.useFakeTimers(); resetState();
    const registry = new FakeRegistry();
    const owner = new peer.TerminalPeerOwner({ registry: registry as never, secureContext: () => false, localDoor: () => null });
    const connection = new FakeConnection("worker-a");
    try {
      owner.start();
      registry.emit({ kind: "demand_changed", workerFp: "worker-a", sessionId: "session-a", viewId: "view-a", active: true });
      const internals = owner as unknown as PeerOwnerInternals;
      const state = internals.states.get("worker-a");
      if (!state) throw new Error("expected peer state");
      const controller = new AbortController();
      state.connection = connection;
      state.controller = controller;
      internals.peerReady(state, connection, controller, authGeneration);
      await settle();
      expect(state.phase).toBe("candidate");
      expect(connection.probeCalls).toBe(1);
      vi.advanceTimersByTime(5_000);
      await settle();
      expect(connection.probeCalls).toBe(2);
      expect(promotionStageCalls).toHaveLength(2);
    } finally {
      owner.dispose("test cleanup");
      vi.useRealTimers();
    }
  });

  test("closes a failed qualification after its last view disappears", async () => {
    resetState();
    const registry = new FakeRegistry();
    const pendingProbe = Promise.withResolvers<void>();
    const owner = new peer.TerminalPeerOwner({ registry: registry as never, secureContext: () => false, localDoor: () => null });
    const connection = new FakeConnection("worker-a", "webrtc", () => pendingProbe.promise);
    owner.start();
    registry.emit({ kind: "demand_changed", workerFp: "worker-a", sessionId: "session-a", viewId: "view-a", active: true });
    const internals = owner as unknown as PeerOwnerInternals;
    const state = internals.states.get("worker-a");
    if (!state) throw new Error("expected peer state");
    const controller = new AbortController();
    state.connection = connection;
    state.controller = controller;
    internals.peerReady(state, connection, controller, authGeneration);
    registry.emit({ kind: "demand_changed", workerFp: "worker-a", sessionId: "session-a", viewId: "view-a", active: false });
    pendingProbe.reject(new Error("qualification failed"));
    await settle();
    expect(connection.closed).toBe(true);
    owner.dispose("test cleanup");
  });

  test("keeps elected RTC live while an unelected loopback candidate arrives", async () => {
    resetState();
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
    expect(rtc.closed).toBe(false);
    expect(loopback.closed).toBe(false);
    owner.dispose("test cleanup");
  });

  test("fences synchronous grant invalidation with the captured connection", () => {
    resetState();
    grantsByWorker.set("worker-a", { workerFp: "worker-a", workerEpoch: "epoch-worker-a", peerSupported: true, stunUrls: [] });
    const registry = new FakeRegistry();
    const owner = new peer.TerminalPeerOwner({ registry: registry as never, secureContext: () => false, localDoor: () => null });
    const affected = new FakeConnection("worker-a");
    const unaffected = new FakeConnection("worker-b");
    owner.start();
    registry.emit({ kind: "demand_changed", workerFp: "worker-a", sessionId: "session-a", viewId: "view-a", active: true });
    registry.emit({ kind: "demand_changed", workerFp: "worker-b", sessionId: "session-b", viewId: "view-b", active: true });
    owner.stage(affected as never); owner.stage(unaffected as never);
    droppedGrant = (workerFp) => { if (workerFp === "worker-a") owner.retire(affected as never, "grant cleared"); };
    probeResultHandler?.({ workerFp: "worker-a", workerEpoch: "new-worker-epoch" });
    expect(affected.closeCalls).toBe(1);
    expect(owner.snapshot("worker-a")?.hasConnection).toBe(false);
    expect(owner.snapshot("worker-b")?.hasConnection).toBe(true);
    owner.dispose("test cleanup");
  });

  test("keeps RTC alive until the final loopback route replacement", () => {
    resetState();
    const registry = new FakeRegistry();
    const owner = new peer.TerminalPeerOwner({ registry: registry as never, secureContext: () => false, localDoor: () => null });
    const rtc = new FakeConnection("worker-a", "webrtc");
    const loopback = new FakeConnection("worker-a", "loopback");
    owner.start();
    registry.emit({ kind: "demand_changed", workerFp: "worker-a", sessionId: "session-a", viewId: "view-a", active: true });
    registry.emit({ kind: "demand_changed", workerFp: "worker-a", sessionId: "session-b", viewId: "view-b", active: true });
    owner.stage(rtc as never);
    owner.stage(loopback as never);
    const committed = promotionCommitHooks[0];
    if (!committed) throw new Error("promotion hook was not installed");
    registry.routes.set("session-a", rtc);
    registry.routes.set("session-b", rtc);
    committed(loopback);
    expect(rtc.closeCalls).toBe(0);
    registry.routes.clear();
    rtc.onClose = () => owner.retire(rtc as never, "loopback replaced RTC");
    committed(loopback);
    expect(rtc.closeCalls).toBe(1);
    expect(owner.snapshot("worker-a")?.hasConnection).toBe(true);
    owner.dispose("test cleanup");
  });

  test("does not demote an elected loopback route back to retained RTC", () => {
    resetState();
    const registry = new FakeRegistry();
    const owner = new peer.TerminalPeerOwner({ registry: registry as never, secureContext: () => false, localDoor: () => null });
    const rtc = new FakeConnection("worker-a", "webrtc");
    const loopback = new FakeConnection("worker-a", "loopback");
    owner.start();
    registry.emit({ kind: "demand_changed", workerFp: "worker-a", sessionId: "session-a", viewId: "view-a", active: true });
    owner.stage(rtc as never);
    registry.routes.set("session-a", loopback);
    promotionStageCalls.length = 0;
    registry.emit({ kind: "demand_changed", workerFp: "worker-a", sessionId: "session-a", viewId: "view-b", active: true });
    expect(promotionStageCalls).toEqual([]);
    owner.dispose("test cleanup");
  });

  test("stages new demand on a registered loopback candidate before retained RTC", () => {
    resetState();
    const registry = new FakeRegistry();
    const owner = new peer.TerminalPeerOwner({ registry: registry as never, secureContext: () => false, localDoor: () => null });
    const rtc = new FakeConnection("worker-a", "webrtc");
    const loopback = new FakeConnection("worker-a", "loopback");
    owner.start();
    registry.emit({ kind: "demand_changed", workerFp: "worker-a", sessionId: "session-a", viewId: "view-a", active: true });
    owner.stage(rtc as never);
    registry.candidate = loopback;
    owner.stage(loopback as never);
    promotionStageCalls.length = 0;
    registry.emit({ kind: "demand_changed", workerFp: "worker-a", sessionId: "session-b", viewId: "view-b", active: true });
    expect(promotionStageCalls).toEqual([{ sessionId: "session-b", connection: loopback }]);
    owner.dispose("test cleanup");
  });

  test("adopts a surviving loopback candidate when RTC retires mid-handoff", () => {
    resetState();
    const registry = new FakeRegistry();
    const owner = new peer.TerminalPeerOwner({ registry: registry as never, secureContext: () => false, localDoor: () => null });
    const rtc = new FakeConnection("worker-a", "webrtc");
    const loopback = new FakeConnection("worker-a", "loopback");
    owner.start();
    registry.emit({ kind: "demand_changed", workerFp: "worker-a", sessionId: "session-a", viewId: "view-a", active: true });
    registry.emit({ kind: "demand_changed", workerFp: "worker-a", sessionId: "session-b", viewId: "view-b", active: true });
    owner.stage(rtc as never);
    registry.candidate = loopback;
    owner.stage(loopback as never);
    registry.routes.set("session-a", loopback);
    registry.routes.set("session-b", rtc);
    promotionStageCalls.length = 0;
    owner.retire(rtc as never, "RTC connection closed");
    expect(owner.snapshot("worker-a")?.hasConnection).toBe(true);
    expect(promotionStageCalls).toEqual([{ sessionId: "session-b", connection: loopback }]);
    registry.emit({ kind: "demand_changed", workerFp: "worker-a", sessionId: "session-c", viewId: "view-c", active: true });
    expect(promotionStageCalls.at(-1)).toEqual({ sessionId: "session-c", connection: loopback });
    owner.dispose("test cleanup");
  });

  test("does not recreate a peer owner after explicit worker retirement", async () => {
    vi.useFakeTimers(); resetState();
    const registry = new FakeRegistry();
    const owner = new peer.TerminalPeerOwner({ registry: registry as never, secureContext: () => true, localDoor: () => null });
    try {
      owner.start();
      registry.emit({ kind: "demand_changed", workerFp: "worker-a", sessionId: "session-a", viewId: "view-a", active: true });
      await settle();
      retiredWorkers.add("worker-a");
      registry.emit({ kind: "worker_retired", workerFp: "worker-a", reason: "worker removed" });
      terminalGrantRefreshes = 0;
      registry.emit({ kind: "demand_changed", workerFp: "worker-a", sessionId: "session-a", viewId: "view-a", active: true });
      vi.advanceTimersByTime(120_000);
      await settle();
      expect(owner.snapshot("worker-a")).toBeNull();
      expect(terminalGrantRefreshes).toBe(0);
    } finally {
      owner.dispose("test cleanup");
      vi.useRealTimers();
    }
  });

  test("retains a multiplexed peer after its inactive session route retires", () => {
    resetState();
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
    resetState();
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
