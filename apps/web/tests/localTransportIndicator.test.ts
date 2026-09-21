// Terminal transport UI state is canonical-replica state, never merely a
// connection that happened to register. The selector must hide a candidate
// until a baseline is installed and then expose all elected carrier kinds.

import { afterEach, describe, expect, test } from "bun:test";
import {
  hasLivenessQualifiedDirectTerminal,
  sessionTerminalTransportKind,
  sessionTerminalTransportPresentation,
} from "../src/store/local-transport-indicator.ts";
import { terminalSessions } from "../src/store/terminal-stream-state.ts";
import { terminalDirectRegistry } from "../src/store/terminal-stream-transport.ts";
import type {
  TerminalGenerationToken,
  TerminalSessionReplica,
} from "../src/store/terminal-stream-types.ts";

const SESSION = "11111111-2222-4333-8444-555555555555";

const LOOPBACK_TOKEN: TerminalGenerationToken = {
  socketGeneration: 1,
  socketId: "loopback-socket",
  processEpoch: "worker-epoch",
  domainGeneration: 0n,
  transportKind: "loopback",
  workerFp: "worker-fp",
};

const SYNC_TOKEN: TerminalGenerationToken = {
  socketGeneration: 2,
  socketId: "sync-socket",
  processEpoch: "sync-epoch",
  domainGeneration: 3n,
  transportKind: "sync",
  workerFp: null,
};

const WAITING_PRESENTATION = {
  kind: null,
  label: "Waiting",
  description: "No transport is confirmed for the current terminal screen.",
} as const;
const LOOPBACK_PRESENTATION = {
  kind: "loopback",
  label: "Loopback",
  description: "Terminal cells and input use a direct connection on this device.",
} as const;
const WEBRTC_PRESENTATION = {
  kind: "webrtc",
  label: "WebRTC",
  description: "Terminal cells and input use a direct WebRTC connection to the worker.",
} as const;
const COORDINATOR_PRESENTATION = {
  kind: "sync",
  label: "Coordinator",
  description: "Terminal cells and input go through the coordinator over Sync.",
} as const;

afterEach(() => {
  terminalDirectRegistry.reset("transport indicator test cleanup");
  terminalSessions.delete(SESSION);
});

describe("sessionTerminalTransportKind", () => {
  test("reports only an elected carrier that owns a baseline", () => {
    const session = {
      baselineReady: false,
      generation: LOOPBACK_TOKEN,
    } as unknown as TerminalSessionReplica;
    terminalSessions.set(SESSION, session);

    expect(sessionTerminalTransportKind(SESSION)).toBeNull();
    expect(sessionTerminalTransportPresentation(SESSION)).toEqual(WAITING_PRESENTATION);
    session.baselineReady = true;

    expect(sessionTerminalTransportKind(SESSION)).toBeNull();
    expect(sessionTerminalTransportPresentation(SESSION)).toEqual(WAITING_PRESENTATION);

    session.generation = SYNC_TOKEN;
    expect(sessionTerminalTransportKind(SESSION)).toBe("sync");
    expect(sessionTerminalTransportPresentation(SESSION)).toEqual(COORDINATOR_PRESENTATION);

    session.generation = null;
    expect(sessionTerminalTransportPresentation(SESSION)).toEqual(WAITING_PRESENTATION);
  });
});

test("requires an elected route with a current terminal proof before direct availability", () => {
  const session = {
    baselineReady: true,
    generation: LOOPBACK_TOKEN,
    lastAcceptedFrameAtMs: null,
    lastAcceptedFrameGeneration: null,
  } as unknown as TerminalSessionReplica;
  terminalSessions.set(SESSION, session);
  const connection = {
    workerFp: "worker-fp",
    kind: "loopback" as const,
    connectionId: "loopback-connection",
    workerEpoch: "worker-epoch",
    inputRouteSupported: true,
    token: () => LOOPBACK_TOKEN,
    allowsSession: (sessionId: string) => sessionId === SESSION,
    publishView: () => true,
    publishResync: () => true,
    sendInput: () => "accepted" as const,
    claimInputRoute: async () => { throw new Error("unused"); },
    requestScrollback: async () => { throw new Error("unused"); },
    probe: async () => undefined,
    close: () => undefined,
  };
  terminalDirectRegistry.register(connection);
  terminalDirectRegistry.setViewDemand("worker-fp", SESSION, "view-direct", true);
  expect(terminalDirectRegistry.commitSessionPromotion(SESSION, "attempt-direct", {
    attemptId: "attempt-direct",
    connection,
    token: LOOPBACK_TOKEN,
    oldToken: null,
    currentToken: null,
    claimEpoch: "",
    candidateFrame: {} as never,
    expectedStreamId: "stream-direct",
    prospectiveViews: new Map(),
    applyCanonical: () => true,
  })).toBe(true);

  expect(sessionTerminalTransportKind(SESSION)).toBe("loopback");
  expect(sessionTerminalTransportPresentation(SESSION)).toEqual(LOOPBACK_PRESENTATION);

  expect(hasLivenessQualifiedDirectTerminal()).toBe(false);

  session.lastAcceptedFrameAtMs = 1;
  session.lastAcceptedFrameGeneration = LOOPBACK_TOKEN;
  expect(hasLivenessQualifiedDirectTerminal()).toBe(true);

  session.lastAcceptedFrameGeneration = { ...LOOPBACK_TOKEN, socketId: "stale" };
  expect(hasLivenessQualifiedDirectTerminal()).toBe(false);
  terminalDirectRegistry.retireSessionRoute(SESSION, LOOPBACK_TOKEN, "test route retirement");
  expect(sessionTerminalTransportPresentation(SESSION)).toEqual(WAITING_PRESENTATION);
});

test("requires a current WebRTC probe before reporting direct outage continuity", () => {
  const token: TerminalGenerationToken = { ...LOOPBACK_TOKEN, transportKind: "webrtc" };
  const session = {
    baselineReady: true,
    generation: token,
    lastAcceptedFrameAtMs: 1,
    lastAcceptedFrameGeneration: token,
  } as unknown as TerminalSessionReplica;
  terminalSessions.set(SESSION, session);
  let probeQualified = false;
  const connection = {
    workerFp: "worker-fp",
    kind: "webrtc" as const,
    connectionId: "peer-connection",
    workerEpoch: "worker-epoch",
    inputRouteSupported: true,
    token: () => token,
    allowsSession: (sessionId: string) => sessionId === SESSION,
    publishView: () => true,
    publishResync: () => true,
    sendInput: () => "accepted" as const,
    claimInputRoute: async () => { throw new Error("unused"); },
    requestScrollback: async () => { throw new Error("unused"); },
    probe: async () => undefined,
    telemetry: () => ({
      opaquePeerId: "peer-id",
      lastProbeAtMs: probeQualified ? 1 : null,
      rttMs: probeQualified ? 1 : null,
      livenessQualified: probeQualified,
      bufferedBytes: 0,
    }),
    close: () => undefined,
  };
  terminalDirectRegistry.register(connection);
  terminalDirectRegistry.setViewDemand("worker-fp", SESSION, "view-peer", true);
  const prepared = {
    attemptId: "attempt-peer",
    connection,
    token,
    oldToken: null,
    currentToken: null,
    claimEpoch: "route-peer",
    candidateFrame: {} as never,
    expectedStreamId: "stream-peer",
    prospectiveViews: new Map(),
    applyCanonical: () => true,
  };
  expect(terminalDirectRegistry.commitSessionPromotion(
    SESSION,
    "attempt-peer",
    prepared,
  )).toBe(false);
  expect(sessionTerminalTransportPresentation(SESSION)).toEqual(WAITING_PRESENTATION);
  expect(hasLivenessQualifiedDirectTerminal()).toBe(false);

  probeQualified = true;
  expect(terminalDirectRegistry.commitSessionPromotion(
    SESSION,
    "attempt-peer",
    prepared,
  )).toBe(true);
  expect(sessionTerminalTransportPresentation(SESSION)).toEqual(WEBRTC_PRESENTATION);
  expect(hasLivenessQualifiedDirectTerminal()).toBe(true);
});
