// Direct-route registry tests isolate election from adapter implementation.
// They cover worker slot isolation, exact identity fences, and commit visibility.
// The registry is deliberately instantiated per case so no document singleton
// state or websocket lifecycle leaks across these behavioral boundaries.

import { describe, expect, test } from "bun:test";
import type { CellGridFrame } from "@roost/protocol/cell";
import type { TerminalInputRouteResult } from "@roost/protocol/proto/sync_pb";
import {
  terminalGenerationKey,
  terminalGenerationMatches,
} from "../src/store/terminal-stream-liveness.ts";
import {
  TerminalDirectRegistry,
  type LocalScrollbackQuery,
  type TerminalDirectConnection,
  type TerminalDirectPromotionPrepared,
  type TerminalDirectPromotionPreparedView,
  type TerminalDirectRegistryEvent,
} from "../src/store/terminal-stream-transport.ts";
import type { TerminalGenerationToken } from "../src/store/terminal-stream-types.ts";

const SESSION_A = "session-a";
const SESSION_B = "session-b";

interface FakeDirectConnection extends TerminalDirectConnection {
  readonly closedReasons: string[];
  readonly sessions: Set<string>;
}


function fakeConnection(
  workerFp: string,
  kind: "loopback" | "webrtc",
  connectionId: string,
  sessionIds: string[],
): FakeDirectConnection {
  const token: TerminalGenerationToken = {
    socketGeneration: 7,
    socketId: `socket-${connectionId}`,
    processEpoch: `${workerFp}-epoch`,
    domainGeneration: 0n,
    transportKind: kind,
    workerFp,
  };
  const sessions = new Set(sessionIds);
  const closedReasons: string[] = [];
  return {
    workerFp,
    kind,
    connectionId,
    workerEpoch: token.processEpoch,
    inputRouteSupported: true,
    closedReasons,
    sessions,
    token: () => token,
    allowsSession: (sessionId) => sessions.has(sessionId),
    publishView: () => true,
    publishResync: () => true,
    sendInput: () => "accepted",
    claimInputRoute: () => Promise.resolve({} as TerminalInputRouteResult),
    requestScrollback: (_query: LocalScrollbackQuery) => Promise.reject(new Error("unused")),
    probe: () => Promise.resolve(),
    close: (reason) => { closedReasons.push(reason); },
  };
}

function preparedPromotion(
  connection: TerminalDirectConnection,
  attemptId: string,
  oldToken: TerminalGenerationToken | null,
  currentToken: TerminalGenerationToken | null,
  applyCanonical: () => boolean = () => true,
  prospectiveViews: ReadonlyMap<string, TerminalDirectPromotionPreparedView> = new Map(),
): TerminalDirectPromotionPrepared {
  const token = connection.token();
  if (!token) throw new Error("test connection must be ready");
  return {
    attemptId,
    connection,
    token,
    oldToken,
    currentToken,
    claimEpoch: "route-epoch",
    candidateFrame: {} as CellGridFrame,
    expectedStreamId: "stream-a",
    prospectiveViews,
    applyCanonical,
  };
}

function promote(
  registry: TerminalDirectRegistry,
  connection: TerminalDirectConnection,
  sessionId: string,
  currentToken: TerminalGenerationToken | null = null,
): void {
  const attemptId = `attempt-${connection.connectionId}-${sessionId}`;
  expect(registry.commitSessionPromotion(
    sessionId,
    attemptId,
    preparedPromotion(connection, attemptId, currentToken, currentToken),
  )).toBe(true);
}

describe("TerminalDirectRegistry", () => {
  test("keeps worker candidates and elected sessions isolated", () => {
    const registry = new TerminalDirectRegistry();
    const first = fakeConnection("worker-a", "loopback", "a", [SESSION_A]);
    const second = fakeConnection("worker-b", "webrtc", "b", [SESSION_B]);
    registry.register(first);
    registry.register(second);
    registry.setViewDemand("worker-a", SESSION_A, "view-a", true);
    registry.setViewDemand("worker-b", SESSION_B, "view-b", true);

    promote(registry, first, SESSION_A);
    promote(registry, second, SESSION_B);

    expect(registry.activeForSession(SESSION_A)).toBe(first);
    expect(registry.activeForSession(SESSION_B)).toBe(second);
    expect(registry.targetForToken(first.token()!)).toBe(first);
    expect(registry.targetForToken(second.token()!)).toBe(second);
    expect(registry.hasRoutesForConnection(first)).toBe(true);
    expect(registry.hasRoutesForConnection(second)).toBe(true);
  });

  test("registration is inert until an active demanded session commits", () => {
    const registry = new TerminalDirectRegistry();
    const connection = fakeConnection("worker-a", "loopback", "candidate", [SESSION_A]);
    const events: TerminalDirectRegistryEvent[] = [];
    registry.subscribe((event) => events.push(event));

    registry.register(connection);

    expect(events).toEqual([]);
    expect(registry.candidateForWorker("worker-a")).toBe(connection);
    expect(registry.activeForSession(SESSION_A)).toBeNull();
  });

  test("requires every token field to match before targeting or retiring", () => {
    const registry = new TerminalDirectRegistry();
    const connection = fakeConnection("worker-a", "webrtc", "exact", [SESSION_A]);
    registry.register(connection);
    registry.setViewDemand("worker-a", SESSION_A, "view-a", true);
    promote(registry, connection, SESSION_A);
    const token = connection.token()!;
    const mismatchedWorker = { ...token, workerFp: "worker-b" };
    const mismatchedCarrier = { ...token, transportKind: "loopback" as const };
    const events: TerminalDirectRegistryEvent[] = [];
    registry.subscribe((event) => events.push(event));

    expect(terminalGenerationMatches(token, mismatchedWorker)).toBe(false);
    expect(terminalGenerationMatches(token, mismatchedCarrier)).toBe(false);
    expect(terminalGenerationKey(token)).not.toBe(terminalGenerationKey(mismatchedWorker));
    expect(terminalGenerationKey(token)).not.toBe(terminalGenerationKey(mismatchedCarrier));
    expect(registry.targetForToken(mismatchedWorker)).toBeNull();
    expect(registry.targetForToken(mismatchedCarrier)).toBeNull();
    registry.retireSessionRoute(SESSION_A, mismatchedWorker, "stale token");

    expect(registry.activeForSession(SESSION_A)).toBe(connection);
    expect(events).toEqual([]);
  });

  test("commits canonical state before one promotion notification", () => {
    const registry = new TerminalDirectRegistry();
    const connection = fakeConnection("worker-a", "loopback", "atomic", [SESSION_A]);
    registry.register(connection);
    registry.setViewDemand("worker-a", SESSION_A, "view-a", true);
    const events: TerminalDirectRegistryEvent[] = [];
    let canonicalCommitted = false;
    registry.subscribe((event) => {
      expect(canonicalCommitted).toBe(true);
      expect(registry.activeForSession(SESSION_A)).toBe(connection);
      events.push(event);
    });
    const attemptId = "attempt-atomic";

    expect(registry.commitSessionPromotion(
      SESSION_A,
      attemptId,
      preparedPromotion(connection, attemptId, null, null, () => {
        expect(events).toEqual([]);
        expect(registry.activeForSession(SESSION_A)).toBeNull();
        canonicalCommitted = true;
        return true;
      }),
    )).toBe(true);

    expect(events).toEqual([{
      kind: "promotion_committed",
      sessionId: SESSION_A,
      token: connection.token()!,
    }]);
  });

  test("retires one exact route without disturbing another worker", () => {
    const registry = new TerminalDirectRegistry();
    const first = fakeConnection("worker-a", "loopback", "lost", [SESSION_A]);
    const second = fakeConnection("worker-b", "webrtc", "kept", [SESSION_B]);
    registry.register(first);
    registry.register(second);
    registry.setViewDemand("worker-a", SESSION_A, "view-a", true);
    registry.setViewDemand("worker-b", SESSION_B, "view-b", true);
    promote(registry, first, SESSION_A);
    promote(registry, second, SESSION_B);
    const events: TerminalDirectRegistryEvent[] = [];
    registry.subscribe((event) => events.push(event));

    registry.retireSessionRoute(SESSION_A, first.token()!, "peer closed");

    expect(registry.activeForSession(SESSION_A)).toBeNull();
    expect(registry.activeForSession(SESSION_B)).toBe(second);
    expect(events).toEqual([{
      kind: "route_lost",
      sessionId: SESSION_A,
      token: first.token()!,
      reason: "peer closed",
    }]);
  });

  test("retires every route and candidate for only the removed worker", () => {
    const registry = new TerminalDirectRegistry();
    const removed = fakeConnection("worker-a", "webrtc", "removed", [SESSION_A]);
    const retained = fakeConnection("worker-b", "webrtc", "retained", [SESSION_B]);
    registry.register(removed);
    registry.register(retained);
    registry.setViewDemand("worker-a", SESSION_A, "view-a", true);
    registry.setViewDemand("worker-b", SESSION_B, "view-b", true);
    promote(registry, removed, SESSION_A);
    promote(registry, retained, SESSION_B);
    const events: TerminalDirectRegistryEvent[] = [];
    registry.subscribe((event) => events.push(event));

    registry.retireWorker("worker-a", "worker removed");

    expect(removed.closedReasons).toEqual(["worker removed"]);
    expect(retained.closedReasons).toEqual([]);
    expect(registry.activeForSession(SESSION_A)).toBeNull();
    expect(registry.activeForSession(SESSION_B)).toBe(retained);
    expect(events.at(-1)).toEqual({
      kind: "worker_retired",
      workerFp: "worker-a",
      reason: "worker removed",
    });
  });

  test("moves active demand to the fresh view id during promotion", () => {
    const registry = new TerminalDirectRegistry();
    const connection = fakeConnection("worker-a", "loopback", "migrate", [SESSION_A]);
    registry.register(connection);
    registry.setViewDemand("worker-a", SESSION_A, "old-view", true);
    const attemptId = "attempt-migrate";
    const prospectiveViews = new Map([[
      "old-view",
      { viewId: "new-view", intent: {} as never, acknowledged: true },
    ]]);

    expect(registry.commitSessionPromotion(
      SESSION_A,
      attemptId,
      preparedPromotion(connection, attemptId, null, null, () => true, prospectiveViews),
    )).toBe(true);
    expect(registry.hasViewDemand("worker-a", SESSION_A)).toBe(true);
    registry.retireSessionRoute(SESSION_A, connection.token()!, "route retired");

    const replacement = fakeConnection("worker-a", "loopback", "replacement", [SESSION_A]);
    registry.register(replacement);
    registry.setViewDemand("worker-a", SESSION_A, "new-view", false);
    expect(registry.hasViewDemand("worker-a", SESSION_A)).toBe(false);

    expect(registry.commitSessionPromotion(
      SESSION_A,
      "attempt-after-demand-removal",
      preparedPromotion(replacement, "attempt-after-demand-removal", null, null),
    )).toBe(false);
  });

  test("reset closes routes and discards residual view demand", () => {
    const registry = new TerminalDirectRegistry();
    const connection = fakeConnection("worker-a", "loopback", "reset", [SESSION_A]);
    registry.register(connection);
    registry.setViewDemand("worker-a", SESSION_A, "view-a", true);
    promote(registry, connection, SESSION_A);
    registry.subscribe((event) => {
      if (event.kind === "route_lost") {
        registry.setViewDemand("worker-a", SESSION_A, "resurrected-view", true);
      }
    });

    registry.reset("logout");

    expect(connection.closedReasons).toEqual(["logout"]);
    expect(registry.activeForSession(SESSION_A)).toBeNull();
    expect(registry.candidateForWorker("worker-a")).toBeNull();
    registry.register(connection);
    expect(registry.commitSessionPromotion(
      SESSION_A,
      "attempt-after-reset",
      preparedPromotion(connection, "attempt-after-reset", null, null),
    )).toBe(false);
  });
});
