// Bounded fallback claims own the transition hold, not input bytes. These tests
// prove route-claim retries remain acknowledged and cannot use a legacy shortcut
// after a worker has demonstrated input-route capability.

import { afterEach, beforeEach, describe, expect, mock, test, vi } from "bun:test";

interface Token {
  socketGeneration: number; socketId: string; processEpoch: string; domainGeneration: bigint;
  transportKind: "sync"; workerFp: null;
}

interface Destination { token: Token; inputRouteSupported: boolean; workerEpoch: string; }

let authGeneration = 0;
let requiresRouteClaim = true;
let currentDestination: Destination | null = null;
let claimOperation: () => Promise<{ accepted: boolean; reason: string; inputRouteEpoch?: string }> = () => Promise.resolve({ accepted: true, reason: "", inputRouteEpoch: "route" });
const holds: FakeHold[] = [];
const claims: Destination[] = [];

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

mock.module("../src/store/root.ts", () => ({ rootStore: { get auth_generation() { return authGeneration; } } }));
mock.module("../src/store/terminal-stream-types.ts", () => ({
  terminalGenerationTokenEquals: (left: Token | null, right: Token | null) => left === right || (
    !!left && !!right
    && left.socketGeneration === right.socketGeneration
    && left.socketId === right.socketId
    && left.processEpoch === right.processEpoch
    && left.domainGeneration === right.domainGeneration
  ),
}));
mock.module("../src/store/transport/terminal-input-router.ts", () => ({
  holdTerminalInput: () => { const hold = new FakeHold(); holds.push(hold); return hold; },
  claimTerminalInputRoute: (_sessionId: string, destination: Destination) => { claims.push(destination); return claimOperation(); },
  terminalInputConnectionKey: (token: Token) => JSON.stringify([token.socketGeneration, token.socketId, token.processEpoch, token.transportKind]),
  terminalInputRequiresRouteClaim: () => requiresRouteClaim,
}));
mock.module("../src/store/transport/sync-outbound.ts", () => ({
  readySyncTerminalInputDestinationForSession: () => currentDestination,
  terminalInputDestinationForSession: () => currentDestination,
}));

// Module mocks must precede the fallback owner import.
const fallback = await import("../src/store/transport/terminal-peer-fallback.ts");

function token(domainGeneration = 1n): Token {
  return { socketGeneration: 1, socketId: "sync", processEpoch: "epoch", domainGeneration, transportKind: "sync", workerFp: null };
}

async function settle(): Promise<void> { for (let turn = 0; turn < 10; turn += 1) await Promise.resolve(); }

beforeEach(() => {
  vi.useFakeTimers();
  authGeneration = 0;
  requiresRouteClaim = true;
  currentDestination = { token: token(), inputRouteSupported: true, workerEpoch: "epoch" };
  claimOperation = () => Promise.resolve({ accepted: true, reason: "", inputRouteEpoch: "route" });
  holds.length = 0;
  claims.length = 0;
});
afterEach(() => vi.useRealTimers());

describe("TerminalPeerFallbackClaims", () => {
  test("retries route_claim_busy and releases only after the current acknowledgement", async () => {
    let attempts = 0;
    claimOperation = () => Promise.resolve(++attempts === 1
      ? { accepted: false, reason: "route_claim_busy" }
      : { accepted: true, reason: "", inputRouteEpoch: "route-two" });
    const reports: Array<string | null> = [];
    const owner = new fallback.TerminalPeerFallbackClaims((reason) => reports.push(reason), () => true);
    owner.claim("session-a");
    await settle();
    expect(claims).toHaveLength(1);
    expect(holds[0]?.releases).toEqual([]);
    vi.advanceTimersByTime(250);
    await settle();
    expect(claims).toHaveLength(2);
    expect(holds[0]?.releases).toEqual([currentDestination!]);
    expect(reports).toEqual([null]);

    owner.dispose();
  });

  test("does not use an unsupported destination after a claim requirement was established", async () => {
    currentDestination = { token: token(), inputRouteSupported: false, workerEpoch: "epoch" };
    const reports: Array<string | null> = [];
    const owner = new fallback.TerminalPeerFallbackClaims((reason) => reports.push(reason), () => true);
    owner.claim("session-a");
    await settle();
    expect(claims).toEqual([]);
    expect(holds[0]?.releases).toEqual([]);
    for (let attempt = 0; attempt < 120; attempt += 1) {
      vi.advanceTimersByTime(250);
      await settle();
    }
    expect(holds[0]?.releases).toEqual([undefined]);
    expect(reports).toEqual(["terminal Sync fallback input-route capability is unavailable"]);
    owner.dispose();
  });

  test("cannot let a superseded claim release its replacement hold", async () => {
    const firstResponse = Promise.withResolvers<{ accepted: boolean; reason: string; inputRouteEpoch?: string }>();
    const secondResponse = Promise.withResolvers<{ accepted: boolean; reason: string; inputRouteEpoch?: string }>();
    let attempts = 0;
    claimOperation = () => ++attempts === 1 ? firstResponse.promise : secondResponse.promise;
    const owner = new fallback.TerminalPeerFallbackClaims(() => undefined, () => true);
    owner.claim("session-a");
    await settle();
    const first = holds[0]!;
    owner.claim("session-a", { expectedOldConnection: token() as never });
    await settle();
    const second = holds[1]!;
    firstResponse.resolve({ accepted: true, reason: "", inputRouteEpoch: "obsolete" });
    await settle();
    expect(first.releases).toEqual([undefined]);
    expect(second.releases).toEqual([]);
    secondResponse.resolve({ accepted: true, reason: "", inputRouteEpoch: "current" });
    await settle();
    expect(second.releases).toEqual([currentDestination!]);
    owner.dispose();
  });
});
