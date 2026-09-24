// Fixture for the coordinator's owner-mode tests: a TerminalViewHub whose
// owner resolution, view relay and socket-closed notice are captured instead
// of dialling a worker, plus a call log shared by the screen expectation and
// the browser sink so ordering between them is assertable. Extends the legacy
// terminal-view-hub-harness rather than replacing it — the legacy stream
// controller is still exercised through the same hub.

import { create, type MessageInitShape } from "@bufbuild/protobuf";
import {
  TerminalViewStateFrameSchema,
  TerminalViewStatus,
  type TerminalViewStateFrame,
} from "@roost/protocol/proto/sync_pb";
import {
  TerminalViewHub,
  installTerminalViewHub,
} from "../src/connect/terminal-view-hub.ts";
import { _resetTerminalViewOwners } from "../src/connect/terminal-view-projection.ts";
import type {
  TerminalViewRelayCommand,
  TerminalViewRelayIdentity,
} from "../src/connect/worker-send-terminal-view.ts";
import { evictSessionWorker } from "../src/byte-hub.ts";
import { connectWorkers } from "../src/connect/worker-registry.ts";
import {
  SESSION,
  TestSink,
  VIEW_A,
  WORKER,
  admitted,
  resultFor,
} from "./terminal-view-hub-harness.ts";

export const OWNER_FP = "b".repeat(64);
export const SOCKET = "socket-a";
export const VIEWER_KEY = "viewer-a";
export const DEVICE_FP = "fingerprint-a";
export const STREAM_A = "40000000-0000-4000-8000-000000000001";
export const STREAM_B = "40000000-0000-4000-8000-000000000002";

export interface RelayRecord {
  workerFp: string;
  identity: TerminalViewRelayIdentity;
  command: TerminalViewRelayCommand;
}

export interface StreamRecord {
  workerFp: string;
  sessionId: string;
  streamId: string;
  cols: number;
  rows: number;
}

const liveHubs: TerminalViewHub[] = [];

export function makeOwnerHarness(options: {
  owner?: string | null;
  relayAdmitted?: boolean;
} = {}) {
  const ownerRef = {
    value: options.owner === undefined ? OWNER_FP : options.owner,
    bySession: new Map<string, string | null>(),
  };
  const relayed: RelayRecord[] = [];
  const socketClosed: Array<{ workerFp: string; socketId: string }> = [];
  const streamStates: StreamRecord[] = [];
  const snapshotRequests: Array<{ workerFp: string; sessionId: string; streamId: string }> = [];
  const calls: string[] = [];
  const hub = new TerminalViewHub({
    db: undefined as never,
    now: () => 1_000,
    resolveRoute: async () => ({ workerFp: WORKER, channel: 7 }),
    sendStreamState: (workerFp, state) => {
      streamStates.push({ workerFp, ...state });
      return admitted(Promise.resolve(resultFor(state)));
    },
    sendSnapshot: (workerFp, sessionId, streamId) => {
      snapshotRequests.push({ workerFp, sessionId, streamId });
      return true;
    },
    ownerForSession: (sessionId) => {
      const override = ownerRef.bySession.get(sessionId);
      return override === undefined ? ownerRef.value : override;
    },
    sendViewRelay: (workerFp, identity, command) => {
      relayed.push({ workerFp, identity, command });
      return options.relayAdmitted ?? true;
    },
    sendViewSocketClosed: (workerFp, socketId) => {
      socketClosed.push({ workerFp, socketId });
      return true;
    },
  });
  liveHubs.push(hub);
  const expectStream = hub.screen.expectStream.bind(hub.screen);
  hub.screen.expectStream = (sessionId, streamId, cols, rows): void => {
    calls.push(`expect_stream:${streamId}`);
    expectStream(sessionId, streamId, cols, rows);
  };
  return { hub, ownerRef, relayed, socketClosed, streamStates, snapshotRequests, calls };
}

export function registerOwnerSocket(
  hub: TerminalViewHub,
  calls: string[],
  overrides: {
    socketId?: string;
    viewerKey?: string | null;
    allowsSession?: (sessionId: string) => boolean;
  } = {},
): TestSink {
  const sink = new TestSink();
  const enqueue = sink.enqueueTerminalState.bind(sink);
  sink.enqueueTerminalState = (frame, sessionId): void => {
    calls.push("view_state");
    enqueue(frame, sessionId);
  };
  hub.registerSocket({
    socketId: overrides.socketId ?? SOCKET,
    viewerKey: overrides.viewerKey === undefined ? VIEWER_KEY : overrides.viewerKey,
    callerFingerprint: DEVICE_FP,
    allowsSession: overrides.allowsSession ?? (() => true),
    sink,
  });
  return sink;
}

export function ownerState(
  overrides: MessageInitShape<typeof TerminalViewStateFrameSchema> = {},
): TerminalViewStateFrame {
  return create(TerminalViewStateFrameSchema, {
    viewId: VIEW_A,
    sessionId: SESSION,
    revision: 1n,
    active: true,
    streamId: STREAM_A,
    status: TerminalViewStatus.ACCEPTED,
    effectiveCols: 80,
    effectiveRows: 24,
    ...overrides,
  });
}

/** Owner registration, published rows and the byte-hub route cache are all
 * process-global, so every owner-mode test starts from empty. */
export function resetOwnerMode(): void {
  for (const hub of liveHubs.splice(0)) hub.dispose();
  installTerminalViewHub(null);
  _resetTerminalViewOwners();
  evictSessionWorker(SESSION);
  connectWorkers.delete(OWNER_FP);
}
