// Promotion sequencing is isolated from RTC and terminal-frame folding here.
// These fakes prove that a ready candidate cannot take input ownership before
// old-route drain and that cancellation transfers only a current held transition.

import { beforeEach, describe, expect, mock, test } from "bun:test";

interface Token {
  socketGeneration: number; socketId: string; processEpoch: string; domainGeneration: bigint;
  transportKind: "sync" | "webrtc"; workerFp: string | null;
}

interface Destination { token: Token; inputRouteSupported: boolean; workerEpoch: string; }

let authGeneration = 0;
let currentToken: Token | null = null;
let oldDestination: Destination | null = null;
let directDestination: Destination | null = null;
let requiresRouteClaim = false;
let demanded = true;
let ownerCurrent = true;
let candidateCancelReentry: (() => void) | null = null;
let drainOperation: (token: Token) => Promise<void> = () => Promise.resolve();
let claimOperation: (sessionId: string, destination: Destination) => Promise<{ accepted: boolean; reason?: string; inputRouteEpoch?: string }> = () => Promise.resolve({ accepted: true, inputRouteEpoch: "candidate-route" });
const drainCalls: Token[] = [];
const claimCalls: Destination[] = [];
const candidates: FakeCandidate[] = [];
const holds: FakeHold[] = [];

class FakeHold {
  current = true;
  readonly releases: Array<Destination | undefined> = [];
  isCurrent(): boolean { return this.current; }
  release(destination?: Destination): void {
    if (!this.current) return;
    this.releases.push(destination);
    this.current = false;
  }
}

class FakeCandidate {
  ready = false;
  cancelled: string | null = null;
  readonly readiness = Promise.withResolvers<boolean>();
  constructor(readonly options: { attemptId: string; connection: FakeConnection; token: Token; onCancelled?: (reason: string) => void }) {}
  get attemptId(): string { return this.options.attemptId; }
  isReady(): boolean { return this.ready && this.cancelled === null; }
  connection(): FakeConnection { return this.options.connection; }
  awaitReady(): Promise<boolean> { return this.readiness.promise; }
  prepare(claimEpoch: string, oldToken: Token | null): object | null {
    return this.isReady() ? { connection: this.options.connection, token: this.options.token, claimEpoch, oldToken } : null;
  }
  cancel(reason: string): void {
    if (this.cancelled !== null) return;
    this.cancelled = reason;
    candidateCancelReentry?.();
    this.options.onCancelled?.(reason);
  }
  resolveReady(): void { this.ready = true; this.readiness.resolve(true); }
}

class FakeConnection {
  constructor(readonly tokenValue: Token) {}
  readonly workerFp = "worker-a";
  readonly kind = "webrtc" as const;
  readonly workerEpoch = "worker-a-epoch";
  readonly inputRouteSupported = true;
  token(): Token { return this.tokenValue; }
  allowsSession(): boolean { return true; }
}

mock.module("../src/store/root.ts", () => ({ rootStore: { get auth_generation() { return authGeneration; } } }));
mock.module("../src/store/terminal-stream-publication.ts", () => ({ currentTerminalGenerationToken: () => currentToken }));
mock.module("../src/store/terminal-stream-promotion.ts", () => ({
  createTerminalSessionPromotion: (options: ConstructorParameters<typeof FakeCandidate>[0]) => {
    const candidate = new FakeCandidate(options);
    candidates.push(candidate);
    return candidate;
  },
}));
mock.module("../src/store/terminal-stream-types.ts", () => ({
  terminalGenerationTokenEquals: (left: Token | null, right: Token | null) => left === right || (
    !!left && !!right
    && left.socketGeneration === right.socketGeneration
    && left.socketId === right.socketId
    && left.processEpoch === right.processEpoch
    && left.domainGeneration === right.domainGeneration
    && left.transportKind === right.transportKind
    && left.workerFp === right.workerFp
  ),
}));
mock.module("../src/ws/terminal-direct-browser.ts", () => ({ createTerminalDirectRequestId: () => `attempt-${candidates.length + 1}` }));
mock.module("../src/ws/terminal-input-router.ts", () => ({
  holdTerminalInput: () => { const hold = new FakeHold(); holds.push(hold); return hold; },
  drainTerminalInput: (_sessionId: string, token: Token) => { drainCalls.push(token); return drainOperation(token); },
  claimTerminalInputRoute: (sessionId: string, destination: Destination) => { claimCalls.push(destination); return claimOperation(sessionId, destination); },
  terminalInputConnectionKey: (token: Token) => JSON.stringify([token.socketGeneration, token.socketId, token.processEpoch, token.transportKind, token.workerFp]),
  terminalInputRequiresRouteClaim: () => requiresRouteClaim,
}));
mock.module("../src/ws/sync-outbound.ts", () => ({
  terminalInputDestinationForDirectConnection: () => directDestination,
  terminalInputDestinationForSession: () => oldDestination,
}));
mock.module("../src/ws/terminal-peer-runtime.ts", () => ({ terminalPeerDeadline: <T>(promise: Promise<T>) => promise }));

// Module mocks must install before importing the promotion owner.
const promotions = await import("../src/ws/terminal-peer-promotions.ts");

class FakeRegistry {
  committed = 0;
  active: FakeConnection | null = null;
  activeForSession(): FakeConnection | null { return this.active; }
  commitSessionPromotion(_sessionId: string, _attemptId: string, prepared: { connection: FakeConnection }): boolean {
    this.committed += 1;
    this.active = prepared.connection;
    return true;
  }
}

class FakeRecovery {
  readonly calls: Array<{ sessionId: string; options: Record<string, unknown> }> = [];
  claim(sessionId: string, options: Record<string, unknown>): void { this.calls.push({ sessionId, options }); }
}

function syncToken(domainGeneration = 1n): Token {
  return { socketGeneration: 1, socketId: "sync-socket", processEpoch: "sync-epoch", domainGeneration, transportKind: "sync", workerFp: null };
}

function peerToken(): Token {
  return { socketGeneration: 2, socketId: "peer-socket", processEpoch: "worker-a-epoch", domainGeneration: 1n, transportKind: "webrtc", workerFp: "worker-a" };
}

async function settle(): Promise<void> { for (let turn = 0; turn < 10; turn += 1) await Promise.resolve(); }

function setup(): { owner: InstanceType<typeof promotions.TerminalPeerPromotions>; registry: FakeRegistry; recovery: FakeRecovery; connection: FakeConnection; committed: FakeConnection[] } {
  const registry = new FakeRegistry();
  const recovery = new FakeRecovery();
  const committed: FakeConnection[] = [];
  const connection = new FakeConnection(peerToken());
  const owner = new promotions.TerminalPeerPromotions("worker-a", registry as never, recovery as never, {
    hasDemand: () => demanded,
    isCurrent: () => ownerCurrent,
    committed: (candidate) => committed.push(candidate as unknown as FakeConnection),
    failed: () => undefined,
  });
  return { owner, registry, recovery, connection, committed };
}

beforeEach(() => {
  authGeneration = 0;
  currentToken = syncToken();
  oldDestination = { token: currentToken, inputRouteSupported: false, workerEpoch: "sync-epoch" };
  directDestination = { token: peerToken(), inputRouteSupported: true, workerEpoch: "worker-a-epoch" };
  requiresRouteClaim = false;
  demanded = true;
  ownerCurrent = true;
  candidateCancelReentry = null;
  drainOperation = () => Promise.resolve();
  claimOperation = () => Promise.resolve({ accepted: true, inputRouteEpoch: "candidate-route" });
  drainCalls.length = 0;
  claimCalls.length = 0;
  candidates.length = 0;
  holds.length = 0;
});

describe("TerminalPeerPromotions", () => {
  test("coalesces one candidate and replaces it after its demand ends", () => {
    const { owner, connection } = setup();
    owner.stage("session-a", connection as never);
    owner.stage("session-a", connection as never);
    expect(candidates).toHaveLength(1);
    owner.cancelSession("session-a", "terminal view demand ended");
    expect(candidates[0]?.cancelled).toBe("terminal view demand ended");
    owner.stage("session-a", connection as never);
    expect(candidates).toHaveLength(2);
  });
  test("releases a held promotion when demand disappears during drain", async () => {
    const draining = Promise.withResolvers<void>();
    drainOperation = () => draining.promise;
    const { owner, connection } = setup();
    owner.stage("session-a", connection as never);
    candidates[0]!.resolveReady();
    await settle();
    demanded = false;
    draining.resolve();
    await settle();
    expect(candidates[0]?.cancelled).toBe("candidate promotion did not commit");
    expect(holds[0]?.releases).toEqual([oldDestination!]);
    demanded = true;
    owner.stage("session-a", connection as never);
    expect(candidates).toHaveLength(2);
  });

  test("waits for old-route drain and cancels without a claim on a same-connection domain change", async () => {
    const draining = Promise.withResolvers<void>();
    drainOperation = () => draining.promise;
    const { owner, connection } = setup();
    owner.stage("session-a", connection as never);
    candidates[0]!.resolveReady();
    await settle();
    expect(drainCalls).toEqual([syncToken()]);
    expect(claimCalls).toEqual([]);
    const refreshed = syncToken(2n);
    currentToken = refreshed;
    oldDestination = { token: refreshed, inputRouteSupported: false, workerEpoch: "sync-epoch" };
    draining.resolve();
    await settle();
    expect(claimCalls).toEqual([]);
    expect(candidates[0]?.cancelled).toBe("terminal input route changed");
    expect(holds[0]?.releases).toEqual([oldDestination]);
    expect(candidates).toHaveLength(2);
  });

  test("does not claim or commit when the old-route drain is ambiguous", async () => {
    drainOperation = () => Promise.reject(new Error("terminal input route cannot drain an ambiguous batch"));
    const { owner, registry, recovery, connection } = setup();
    owner.stage("session-a", connection as never);
    candidates[0]!.resolveReady();
    await settle();
    expect(claimCalls).toEqual([]);
    expect(registry.committed).toBe(0);
    expect(recovery.calls).toEqual([]);
    expect(holds[0]?.releases).toEqual([oldDestination!]);
  });

  test("does not commit or release after ownership changes while a candidate claim awaits", async () => {
    const claimed = Promise.withResolvers<{ accepted: boolean; inputRouteEpoch: string }>();
    claimOperation = () => claimed.promise;
    const { owner, registry, connection, committed } = setup();
    owner.stage("session-a", connection as never);
    candidates[0]!.resolveReady();
    await settle();
    expect(claimCalls).toHaveLength(1);
    authGeneration += 1;
    claimed.resolve({ accepted: true, inputRouteEpoch: "candidate-route" });
    await settle();
    expect(registry.committed).toBe(0);
    expect(committed).toEqual([]);
    expect(holds[0]?.releases).toEqual([]);
  });

  test("cancels an unclaimed candidate back to the current healthy old route", async () => {
    const draining = Promise.withResolvers<void>();
    drainOperation = () => draining.promise;
    const { owner, recovery, connection } = setup();
    owner.stage("session-a", connection as never);
    candidates[0]!.resolveReady();
    await settle();
    candidates[0]!.cancel("candidate baseline failed");
    draining.resolve();
    await settle();
    expect(recovery.calls).toEqual([]);
    expect(holds[0]?.releases).toEqual([oldDestination!]);
    expect(claimCalls).toEqual([]);
  });

  test("assigns uncertain recovery before candidate cancellation reenters", async () => {
    const claimed = Promise.withResolvers<{ accepted: boolean; inputRouteEpoch: string }>();
    claimOperation = () => claimed.promise;
    const { owner, recovery, connection } = setup();
    owner.stage("session-a", connection as never);
    candidates[0]!.resolveReady();
    await settle();
    expect(claimCalls).toHaveLength(1);
    candidateCancelReentry = () => {
      expect(recovery.calls).toHaveLength(1);
      owner.retireConnection(connection as never, "reentrant route loss");
    };
    owner.retireConnection(connection as never, "candidate connection closed");
    await settle();
    expect(recovery.calls).toHaveLength(1);
    expect(recovery.calls[0]?.options.hold).toBe(holds[0]);
    expect(recovery.calls[0]?.options.expectedOldConnection).toEqual(syncToken());
    claimed.resolve({ accepted: true, inputRouteEpoch: "obsolete" });
    await settle();
    expect(holds[0]?.releases).toEqual([]);
  });
});
