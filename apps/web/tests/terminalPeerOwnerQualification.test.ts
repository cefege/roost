// Candidate qualification stays live while a direct promotion waits for Sync input.
// The owner drives content-free probes; the real registry remains the commit boundary.
// No browser ICE or terminal-frame folding is involved in this owner lifecycle seam.

import { TERMINAL_PEER_HEARTBEAT_INTERVAL_MS, TERMINAL_PEER_PROBE_QUALIFICATION_MS } from "@roost/shared/terminal-peer";
import { expect, mock, test, vi } from "bun:test";
import { TerminalDirectRegistry } from "../src/store/terminal-stream-transport.ts";

let authGeneration = 0;
let livenessNowMs = 0;
let promotionStageHandler: ((sessionId: string, connection: unknown) => void) | null = null;

mock.module("../src/ws/local-terminal-grants.ts", () => ({
  LOCAL_TERMINAL_GRANT_RETRY_MS: 30_000,
  currentTerminalGrant: () => null,
  dropTerminalGrant: () => undefined,
  isTerminalGrantWorkerRetired: () => false,
  refreshTerminalGrant: () => Promise.resolve(null),
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
    stage(sessionId: string, connection: unknown): void { promotionStageHandler?.(sessionId, connection); }
    cancelSession(): void {}
    retireConnection(): void {}
    dispose(): void {}
  },
}));
mock.module("../src/ws/terminal-peer-connection.ts", () => ({ TerminalPeerConnection: class {} }));
mock.module("../src/store/sync.ts", () => ({
  registerSyncV2ProbeResultHandler: () => () => undefined,
}));
mock.module("../src/connect.ts", () => ({ coordClient: {} }));
mock.module("../src/store/root.ts", () => ({ rootStore: { get auth_generation() { return authGeneration; } } }));

// Module mocks must install before the owner under test is loaded.
const peer = await import("../src/ws/terminal-peer.ts");

interface Token {
  socketGeneration: number;
  socketId: string;
  processEpoch: string;
  domainGeneration: bigint;
  transportKind: "webrtc";
  workerFp: string;
}

class FakeConnection {
  readonly connectionId = "candidate-connection";
  readonly workerEpoch = "worker-epoch";
  readonly inputRouteSupported = true;
  readonly kind = "webrtc" as const;
  readonly tokenValue: Token = {
    socketGeneration: 1,
    socketId: this.connectionId,
    processEpoch: this.workerEpoch,
    domainGeneration: 1n,
    transportKind: this.kind,
    workerFp: "worker-a",
  };
  probeCalls = 0;
  lastProbeAtMs: number | null = null;
  rttMs: number | null = null;
  readonly workerFp = "worker-a";

  token(): Token { return this.tokenValue; }
  allowsSession(): boolean { return true; }
  publishView(): boolean { return true; }
  publishResync(): boolean { return true; }
  sendInput(): "accepted" { return "accepted"; }
  claimInputRoute(): Promise<never> { return Promise.reject(new Error("not used")); }
  requestScrollback(): Promise<never> { return Promise.reject(new Error("not used")); }
  probe(): Promise<void> {
    this.probeCalls += 1;
    return Promise.resolve().then(() => { this.lastProbeAtMs = livenessNowMs; this.rttMs = this.probeCalls; });
  }
  telemetry() {
    const lastProbeAtMs = this.lastProbeAtMs;
    return {
      opaquePeerId: "candidate-peer",
      lastProbeAtMs,
      rttMs: this.rttMs,
      livenessQualified: lastProbeAtMs !== null
        && livenessNowMs - lastProbeAtMs <= TERMINAL_PEER_PROBE_QUALIFICATION_MS,
      bufferedBytes: 0,
    };
  }
  close(): void {}
}

interface PeerOwnerStateForTest {
  connection: FakeConnection | null;
  controller: AbortController | null;
  phase: string;
}
interface PeerOwnerInternals {
  states: Map<string, PeerOwnerStateForTest>;
  peerReady(state: PeerOwnerStateForTest, connection: FakeConnection, controller: AbortController, authGeneration: number): void;
}

async function settle(): Promise<void> {
  for (let turn = 0; turn < 8; turn += 1) await Promise.resolve();
}

test("renews candidate telemetry through a delayed Sync drain before committing promotion", async () => {
  vi.useFakeTimers();
  authGeneration = 0;
  livenessNowMs = 0;
  const registry = new TerminalDirectRegistry();
  const connection = new FakeConnection();
  const oldSyncDrain = Promise.withResolvers<void>();
  let committed: boolean | null = null;
  let handoffExpired = false;
  const handoffDeadline = setTimeout(() => { handoffExpired = true; }, 10_000);
  promotionStageHandler = (sessionId, staged) => {
    const candidate = staged as FakeConnection;
    void oldSyncDrain.promise.then(() => {
      if (handoffExpired) return;
      const token = candidate.token();
      committed = registry.commitSessionPromotion(sessionId, "qualified-promotion", {
        attemptId: "qualified-promotion",
        connection: candidate as never,
        token,
        oldToken: null,
        currentToken: null,
        claimEpoch: "route-epoch",
        candidateFrame: {} as never,
        expectedStreamId: "stream-a",
        prospectiveViews: new Map(),
        applyCanonical: () => true,
      });
    });
  };
  const owner = new peer.TerminalPeerOwner({
    registry: registry as never,
    secureContext: () => false,
    localDoor: () => null,
  });
  try {
    owner.start();
    registry.setViewDemand("worker-a", "session-a", "view-a", true);
    const internals = owner as unknown as PeerOwnerInternals;
    const state = internals.states.get("worker-a");
    if (!state) throw new Error("expected peer state");
    const controller = new AbortController();
    state.connection = connection;
    state.controller = controller;
    internals.peerReady(state, connection, controller, authGeneration);
    await settle();

    expect(state.phase).toBe("candidate");
    expect(connection.telemetry()).toMatchObject({ lastProbeAtMs: 0, rttMs: 1, livenessQualified: true });
    livenessNowMs += TERMINAL_PEER_HEARTBEAT_INTERVAL_MS;
    vi.advanceTimersByTime(TERMINAL_PEER_HEARTBEAT_INTERVAL_MS);
    await settle();
    expect(connection.probeCalls).toBe(2);
    expect(connection.telemetry()).toMatchObject({
      lastProbeAtMs: TERMINAL_PEER_HEARTBEAT_INTERVAL_MS,
      rttMs: 2,
      livenessQualified: true,
    });

    const delayedDrainMs = TERMINAL_PEER_PROBE_QUALIFICATION_MS + 1;
    expect(delayedDrainMs).toBeLessThan(10_000);
    livenessNowMs += delayedDrainMs - TERMINAL_PEER_HEARTBEAT_INTERVAL_MS;
    vi.advanceTimersByTime(delayedDrainMs - TERMINAL_PEER_HEARTBEAT_INTERVAL_MS);
    expect(connection.telemetry().livenessQualified).toBe(true);
    expect(handoffExpired).toBe(false);
    oldSyncDrain.resolve();
    await settle();

    expect(Boolean(committed)).toBe(true);
    expect(registry.activeForSession("session-a")).toBe(connection as never);
  } finally {
    promotionStageHandler = null;
    owner.dispose("test cleanup");
    clearTimeout(handoffDeadline);
    vi.useRealTimers();
  }
});
