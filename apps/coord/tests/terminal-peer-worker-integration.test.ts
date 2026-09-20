// Focused worker-link coverage for terminal-peer capability acknowledgement and
// typed result dispatch. The dispatcher receives the exact WorkerHandle object
// after its existing current-generation gate, never a generic JSON completion.
// No frame contains SDP content in logs or test diagnostics.

import { afterEach, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import {
  CoordWorkerUpSchema,
  WHelloSchema,
  WLocalTerminalPeerAnswerSchema,
  WLocalTerminalPeerErrorSchema,
  type CoordWorkerDown,
  type WLocalTerminalPeerAnswer,
  type WLocalTerminalPeerError,
} from "@roost/shared/proto/worker_transport_pb";
import {
  TERMINAL_INPUT_ROUTE_CAPABILITY,
  TERMINAL_PEER_WEBRTC_CAPABILITY,
} from "@roost/shared/terminal-peer";
import { makeWorkerConn, type WorkerConn } from "../src/connect/worker-conn.ts";
import { makeWorkerFrameDispatcher } from "../src/connect/worker-frame-dispatch.ts";
import type { WorkerServiceDeps } from "../src/connect/worker-conn-types.ts";
import type { WorkerHandle } from "../src/connect/worker-registry.ts";
import {
  __setConnectWorkerForTest,
  fenceWorkerCredential,
} from "../src/connect/worker-registry.ts";

const WORKER_FP = "f".repeat(64);
const WORKER_EPOCH = "terminal-peer-worker-epoch";
let connection: WorkerConn | null = null;

afterEach(() => {
  connection?.close();
  connection = null;
  __setConnectWorkerForTest(WORKER_FP, null);
});

test("acknowledges peer capability only with its result owner and route capability only with its owner", async () => {
  const sent: CoordWorkerDown[] = [];
  const peerSink = {
    acceptAnswer: () => false,
    acceptError: () => false,
    cancelForWorkerHandle: () => {},
  };
  connection = makeWorkerConn({
    cfg: { terminalPeerEnabled: true },
    terminalPeerNegotiations: peerSink,
  } as unknown as WorkerServiceDeps, { fingerprint: WORKER_FP }, (frame) => {
    sent.push(frame);
    return 1;
  }, () => {});
  await connection.handleUpstream(create(CoordWorkerUpSchema, {
    frame: {
      case: "hello",
      value: create(WHelloSchema, {
        workerFp: WORKER_FP,
        version: "test",
        processEpoch: WORKER_EPOCH,
        capabilities: [TERMINAL_PEER_WEBRTC_CAPABILITY, TERMINAL_INPUT_ROUTE_CAPABILITY],
      }),
    },
  }));

  const acknowledgement = sent.at(-1);
  expect(acknowledgement?.frame.case).toBe("helloAck");
  if (acknowledgement?.frame.case !== "helloAck") throw new Error("expected hello acknowledgement");
  expect(acknowledgement.frame.value.capabilities).toEqual([TERMINAL_PEER_WEBRTC_CAPABILITY]);
});

test("acknowledges input route only when its typed result owner is installed", async () => {
  const sent: CoordWorkerDown[] = [];
  connection = makeWorkerConn({
    cfg: { terminalPeerEnabled: true },
    terminalPeerNegotiations: {
      acceptAnswer: () => false,
      acceptError: () => false,
      cancelForWorkerHandle: () => {},
    },
    terminalInputRouteResults: {
      acceptInputRouteResult: () => false,
      acceptTransportProbeResult: () => false,
      cancelForWorkerHandle: () => {},
    },
  } as unknown as WorkerServiceDeps, { fingerprint: WORKER_FP }, (frame) => {
    sent.push(frame);
    return 1;
  }, () => {});
  await connection.handleUpstream(create(CoordWorkerUpSchema, {
    frame: {
      case: "hello",
      value: create(WHelloSchema, {
        workerFp: WORKER_FP,
        version: "test",
        processEpoch: WORKER_EPOCH,
        capabilities: [TERMINAL_PEER_WEBRTC_CAPABILITY, TERMINAL_INPUT_ROUTE_CAPABILITY],
      }),
    },
  }));
  const acknowledgement = sent.at(-1);
  if (acknowledgement?.frame.case !== "helloAck") throw new Error("expected hello acknowledgement");
  expect(acknowledgement.frame.value.capabilities).toEqual([
    TERMINAL_INPUT_ROUTE_CAPABILITY,
    TERMINAL_PEER_WEBRTC_CAPABILITY,
  ]);
});

test("registry fencing still cancels typed route waiters after premarking the handle", async () => {
  const cancelled: string[] = [];
  connection = makeWorkerConn({
    cfg: { terminalPeerEnabled: true },
    terminalInputRouteResults: {
      acceptInputRouteResult: () => false,
      acceptTransportProbeResult: () => false,
      cancelForWorkerHandle: (_worker: WorkerHandle, reason: string) => cancelled.push(reason),
    },
  } as unknown as WorkerServiceDeps, { fingerprint: WORKER_FP }, () => 1, () => {});
  await connection.handleUpstream(create(CoordWorkerUpSchema, {
    frame: {
      case: "hello",
      value: create(WHelloSchema, {
        workerFp: WORKER_FP,
        version: "test",
        processEpoch: WORKER_EPOCH,
        capabilities: [TERMINAL_INPUT_ROUTE_CAPABILITY],
      }),
    },
  }));

  expect(fenceWorkerCredential(WORKER_FP)?.revoked).toBe(true);
  expect(cancelled).toEqual(["worker_revoked"]);
});

test("dispatches typed peer answer and error through the exact current handle", () => {
  const worker: WorkerHandle = {
    workerFp: WORKER_FP,
    processEpoch: WORKER_EPOCH,
    connectionGeneration: "terminal-peer-test-connection",
    capabilities: new Set([TERMINAL_PEER_WEBRTC_CAPABILITY]),
    revoked: false,
    ready: true,
    send: () => 1,
  };
  __setConnectWorkerForTest(WORKER_FP, worker);
  const answers: string[] = [];
  const errors: string[] = [];
  let fenced = false;
  const dispatcher = makeWorkerFrameDispatcher({
    deps: {
      terminalPeerNegotiations: {
        acceptAnswer(source: WorkerHandle, answer: WLocalTerminalPeerAnswer) {
          expect(source).toBe(worker);
          answers.push(answer.requestId);
          return true;
        },
        acceptError(source: WorkerHandle, error: WLocalTerminalPeerError) {
          expect(source).toBe(worker);
          errors.push(error.requestId);
          return true;
        },
        cancelForWorkerHandle() {},
      },
    } as unknown as WorkerServiceDeps,
    callerFingerprint: WORKER_FP,
    requestClose() {},
    getWorkerFp: () => WORKER_FP,
    getWorkerHandle: () => worker,
    isSnapshotReady: () => true,
    isCurrentGeneration: () => !fenced,
    fenced: () => fenced,
    sendBestEffort: () => true,
    markSnapshotReady: () => false,
    scheduleRespawn() {},
  });
  const answer = create(CoordWorkerUpSchema, {
    frame: {
      case: "localTerminalPeerAnswer",
      value: create(WLocalTerminalPeerAnswerSchema, {
        requestId: "answer-request",
        connectionGeneration: worker.connectionGeneration,
        workerEpoch: WORKER_EPOCH,
        peerId: "00000000-0000-4000-8000-000000000020",
        answerSdp: "bounded-by-owner",
      }),
    },
  });
  const error = create(CoordWorkerUpSchema, {
    frame: {
      case: "localTerminalPeerError",
      value: create(WLocalTerminalPeerErrorSchema, {
        requestId: "error-request",
        connectionGeneration: worker.connectionGeneration,
        workerEpoch: WORKER_EPOCH,
        peerId: "00000000-0000-4000-8000-000000000021",
        reason: "ice_failed",
      }),
    },
  });

  expect(dispatcher.handleLiveFrame(answer)).toBe(true);
  expect(dispatcher.handleLiveFrame(error)).toBe(true);
  fenced = true;
  expect(dispatcher.handleLiveFrame(answer)).toBe(true);
  expect(answers).toEqual(["answer-request"]);
  expect(errors).toEqual(["error-request"]);
});
