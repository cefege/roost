// Direct promotion tests keep staged frames out of the canonical renderer until commit.
// The fixture supplies a real canonical Sync replica while this file supplies a bounded direct peer.
// Each case proves a route boundary, not adapter serialization or input-lane plumbing.
// Registry reset is owned by the shared fixture before and after every test.

import { create } from "@bufbuild/protobuf";
import { describe, expect, test } from "bun:test";
import type {
  PbCellGridFrame,
} from "@roost/shared/proto/cell_pb";
import {
  LocalTerminalServerFrameSchema,
  type LocalTerminalServerFrame,
} from "@roost/shared/proto/local_terminal_pb";
import {
  TerminalViewStateFrameSchema,
  TerminalViewStatus,
  type TerminalViewCommand,
} from "@roost/shared/proto/sync_pb";
import { terminalGenerationToken } from "../src/store/terminal-stream-liveness.ts";
import { sessionTerminalTransportPresentation } from "../src/store/local-transport-indicator.ts";
import {
  _terminalViewRenewalSchedulerSnapshotForTest,
} from "../src/store/terminal-stream-renewal-scheduler.ts";
import type { TerminalSessionPromotion } from "../src/store/terminal-stream-promotion.ts";
import {
  terminalDirectRegistry,
  type TerminalDirectConnection,
} from "../src/store/terminal-stream-transport.ts";
import type {
  TerminalGenerationToken,
  TerminalViewHandle,
} from "../src/store/terminal-stream-types.ts";
import {
  CURRENT_SYNC_OWNER,
  EPOCH_A,
  RecordingRenderer,
  SESSION_ID,
  STREAM_A,
  STREAM_B,
  WORKER_FP,
  acceptView,
  cellFrameToProto,
  delta,
  full,
  latestViewCommand,
  renderer,
  row,
  terminalStream,
  viewCommands,
} from "./helpers/terminalStreamFixture.ts";

const DIRECT_TOKEN: TerminalGenerationToken = {
  socketGeneration: 91,
  socketId: "direct-socket-91",
  processEpoch: "worker-epoch-91",
  domainGeneration: 0n,
  transportKind: "webrtc",
  workerFp: WORKER_FP,
};

function directServerFrame(
  frame: LocalTerminalServerFrame["frame"],
): LocalTerminalServerFrame {
  return create(LocalTerminalServerFrameSchema, { frame });
}

function createDirectConnection(livenessQualified = true): {
  connection: TerminalDirectConnection;
  published: TerminalViewCommand[];
  unregister: () => void;
} {
  const published: TerminalViewCommand[] = [];
  const connection: TerminalDirectConnection = {
    workerFp: WORKER_FP,
    kind: "webrtc",
    connectionId: "candidate-connection-91",
    workerEpoch: DIRECT_TOKEN.processEpoch,
    inputRouteSupported: true,
    token: () => DIRECT_TOKEN,
    allowsSession: (sessionId) => sessionId === SESSION_ID,
    publishView: (command) => {
      published.push(command);
      return true;
    },
    publishResync: () => true,
    sendInput: () => "refused",
    claimInputRoute: async () => { throw new Error("not exercised by frame promotion"); },
    requestScrollback: async () => { throw new Error("not exercised by frame promotion"); },
    probe: async () => undefined,
    telemetry: () => ({
      opaquePeerId: "opaque-peer-91",
      lastProbeAtMs: 0,
      rttMs: 17,
      livenessQualified,
      candidateType: "host",
      bufferedBytes: 23,
    }),
    close: () => undefined,
  };
  return { connection, published, unregister: terminalDirectRegistry.register(connection) };
}

function directViewState(viewId: string, revision: bigint, streamId: string) {
  return directServerFrame({
    case: "terminalViewState",
    value: create(TerminalViewStateFrameSchema, {
      viewId,
      sessionId: SESSION_ID,
      revision,
      active: true,
      streamId,
      status: TerminalViewStatus.ACCEPTED,
      effectiveCols: 1,
      effectiveRows: 1,
    }),
  });
}

function directCell(frame: PbCellGridFrame): LocalTerminalServerFrame {
  return directServerFrame({ case: "cellGrid", value: frame });
}

function canonicalView(): { view: TerminalViewHandle; sink: RecordingRenderer } {
  const view = terminalStream.createTerminalView(SESSION_ID, WORKER_FP);
  const sink = new RecordingRenderer();
  view.subscribeRenderer(renderer(sink));
  view.setViewport({ cols: 1, rows: 1 });
  acceptView(view.viewId, latestViewCommand().value.revision as bigint);
  terminalStream.dispatchTerminalCellFrame(cellFrameToProto(full(), SESSION_ID));
  return { view, sink };
}

function stagedCandidate(streamId = STREAM_B, livenessQualified = true) {
  const direct = createDirectConnection(livenessQualified);
  const candidate = terminalStream.createTerminalSessionPromotion({
    sessionId: SESSION_ID,
    attemptId: "promotion-attempt-91",
    connection: direct.connection,
    token: DIRECT_TOKEN,
  });
  if (!candidate) throw new Error("candidate did not start");
  const command = direct.published.at(-1);
  if (!command) throw new Error("candidate did not publish a prospective view");
  terminalStream.dispatchDirectTerminalFrame(
    DIRECT_TOKEN,
    directViewState(command.viewId, command.revision, streamId),
  );
  return { candidate, direct };
}

function commitCandidate(candidate: TerminalSessionPromotion): boolean {
  const prepared = candidate.prepare(
    "route-epoch-91",
    terminalGenerationToken({ ...CURRENT_SYNC_OWNER, ready: true }),
  );
  if (!prepared) throw new Error("candidate did not prepare");
  return terminalDirectRegistry.commitSessionPromotion(SESSION_ID, "promotion-attempt-91", prepared);
}

describe("terminal direct promotion", () => {
  test("keeps Sync elected when no direct candidate exists", () => {
    canonicalView();

    expect(terminalStream.terminalStreamDiagnosticSnapshot(SESSION_ID).route).toMatchObject({
      active: {
        kind: "sync",
        worker_epoch: null,
        peer_id: null,
        phase: "active",
        probe_age_ms: null,
        rtt_ms: null,
        worker_control_rtt_ms: null,
        buffered_bytes: null,
        candidate_type: "none",
      },
      candidate: null,
      pending_input_count: 0,
    });
  });

  test("keeps unqualified WebRTC behind ready Sync", async () => {
    canonicalView();
    const { candidate, direct } = stagedCandidate(STREAM_B, false);
    const ready = candidate.awaitReady();
    terminalStream.dispatchDirectTerminalFrame(
      DIRECT_TOKEN,
      directCell(cellFrameToProto(full(STREAM_B, [row(0, "U")]), SESSION_ID)),
    );
    expect(await ready).toBe(true);
    expect(commitCandidate(candidate)).toBe(false);
    expect(sessionTerminalTransportPresentation(SESSION_ID)).toMatchObject({
      kind: "sync",
      label: "Coordinator",
    });
    candidate.cancel("unqualified peer test complete");
    direct.unregister();
  });

  test("keeps a candidate isolated and defers canonical notifications until its atomic commit", async () => {
    const { view, sink } = canonicalView();
    const { candidate, direct } = stagedCandidate();
    expect(terminalStream.terminalStreamDiagnosticSnapshot(SESSION_ID).route).toMatchObject({
      active: {
        kind: "sync",
        worker_epoch: null,
        peer_id: null,
        phase: "active",
      },
      candidate: {
        kind: "webrtc",
        worker_epoch: DIRECT_TOKEN.processEpoch,
        peer_id: "opaque-peer-91",
        phase: "candidate",
        probe_age_ms: expect.any(Number),
        rtt_ms: 17,
        worker_control_rtt_ms: 17,
        buffered_bytes: 23,
        candidate_type: "host",
      },
      pending_input_count: 0,
    });
    const ready = candidate.awaitReady();
    terminalStream.dispatchDirectTerminalFrame(
      DIRECT_TOKEN,
      directCell(cellFrameToProto(full(STREAM_B, [row(0, "P")]), SESSION_ID)),
    );
    expect(candidate.isReady()).toBe(true);
    expect(await ready).toBe(true);
    expect(sessionTerminalTransportPresentation(SESSION_ID)).toMatchObject({
      kind: "sync",
      label: "Coordinator",
    });
    expect(sink.fullFrames).toHaveLength(1);
    let notifications = 0;
    view.subscribeStatus(() => { notifications++; });
    notifications = 0;

    expect(commitCandidate(candidate)).toBe(true);
    expect(sessionTerminalTransportPresentation(SESSION_ID)).toMatchObject({
      kind: "webrtc",
      label: "WebRTC",
    });
    expect(terminalStream.terminalStreamDiagnosticSnapshot(SESSION_ID).route).toMatchObject({
      active: {
        kind: "webrtc",
        worker_epoch: DIRECT_TOKEN.processEpoch,
        peer_id: "opaque-peer-91",
        phase: "active",
        probe_age_ms: expect.any(Number),
        rtt_ms: 17,
        worker_control_rtt_ms: 17,
        buffered_bytes: 23,
        candidate_type: "host",
      },
      candidate: null,
      pending_input_count: 0,
    });
    expect(notifications).toBe(0);
    await Promise.resolve();

    expect(notifications).toBe(1);
    expect(sink.fullFrames).toHaveLength(2);
    expect(sink.fullFrames.at(-1)?.viewportRows[0]?.spans[0]?.text).toBe("P");
    const laterSessionCandidate = terminalStream.createTerminalSessionPromotion({
      sessionId: SESSION_ID,
      attemptId: "promotion-attempt-active-peer",
      connection: direct.connection,
      token: DIRECT_TOKEN,
    });
    if (!laterSessionCandidate) throw new Error("active peer did not stage another session candidate");
    expect(terminalStream.terminalStreamDiagnosticSnapshot(SESSION_ID).route.candidate).toMatchObject({
      kind: "webrtc",
      phase: "candidate",
      peer_id: "opaque-peer-91",
    });
    laterSessionCandidate.cancel("diagnostic candidate test complete");
    expect(sessionTerminalTransportPresentation(SESSION_ID)).toMatchObject({
      kind: "webrtc",
      label: "WebRTC",
    });
    expect(viewCommands().some((command) => command.value.active === false)).toBe(true);
    direct.unregister();
  });

  test("ignores a stale token and cancels a malformed candidate delta without touching canonical state", () => {
    const { sink } = canonicalView();
    let cancelled = "";
    const direct = createDirectConnection();
    const candidate = terminalStream.createTerminalSessionPromotion({
      sessionId: SESSION_ID,
      attemptId: "promotion-attempt-91",
      connection: direct.connection,
      token: DIRECT_TOKEN,
      onCancelled: (reason) => { cancelled = reason; },
    });
    if (!candidate) throw new Error("candidate did not start");
    const command = direct.published.at(-1)!;
    terminalStream.dispatchDirectTerminalFrame(DIRECT_TOKEN, directViewState(command.viewId, command.revision, STREAM_B));
    terminalStream.dispatchDirectTerminalFrame(
      { ...DIRECT_TOKEN, socketId: "stale-direct-socket" },
      directCell(cellFrameToProto(full(STREAM_B, [row(0, "S")]), SESSION_ID)),
    );
    expect(candidate.isReady()).toBe(false);
    terminalStream.dispatchDirectTerminalFrame(
      DIRECT_TOKEN,
      directCell(cellFrameToProto(delta(2, "X", STREAM_B), SESSION_ID)),
    );

    expect(cancelled).toBe("candidate frame did not continue its baseline");
    expect(sink.fullFrames).toHaveLength(1);
    expect(terminalStream.terminalStreamDiagnosticSnapshot(SESSION_ID).replica).toMatchObject({
      grid_epoch: EPOCH_A,
      seq: 1,
    });
    direct.unregister();
  });

  test("folds candidate deltas while input handoff waits to commit", async () => {
    const { sink } = canonicalView();
    const { candidate, direct } = stagedCandidate();
    terminalStream.dispatchDirectTerminalFrame(
      DIRECT_TOKEN,
      directCell(cellFrameToProto(full(STREAM_B, [row(0, "B")]), SESSION_ID)),
    );
    terminalStream.dispatchDirectTerminalFrame(
      DIRECT_TOKEN,
      directCell(cellFrameToProto(delta(2, "D", STREAM_B), SESSION_ID)),
    );

    expect(commitCandidate(candidate)).toBe(true);
    await Promise.resolve();
    expect(sink.fullFrames.at(-1)?.viewportRows[0]?.spans[0]?.text).toBe("D");
    expect(terminalStream.terminalStreamDiagnosticSnapshot(SESSION_ID).replica.seq).toBe(2);
    direct.unregister();
  });

  test("returns to Waiting after a committed direct route retires", () => {
    canonicalView();
    const { candidate, direct } = stagedCandidate();
    terminalStream.dispatchDirectTerminalFrame(
      DIRECT_TOKEN,
      directCell(cellFrameToProto(full(STREAM_B, [row(0, "R")]), SESSION_ID)),
    );

    expect(commitCandidate(candidate)).toBe(true);
    expect(sessionTerminalTransportPresentation(SESSION_ID)).toMatchObject({
      kind: "webrtc",
      label: "WebRTC",
    });
    terminalDirectRegistry.retireSessionRoute(SESSION_ID, DIRECT_TOKEN, "test route retirement");
    expect(sessionTerminalTransportPresentation(SESSION_ID)).toMatchObject({
      kind: null,
      label: "Waiting",
    });
    direct.unregister();
  });

  test("refuses a same-stream candidate that trails the canonical sequence", () => {
    canonicalView();
    terminalStream.dispatchTerminalCellFrame(cellFrameToProto(delta(2, "B"), SESSION_ID));
    terminalStream.dispatchTerminalCellFrame(cellFrameToProto(delta(3, "C"), SESSION_ID));
    const { candidate, direct } = stagedCandidate(STREAM_A);
    terminalStream.dispatchDirectTerminalFrame(
      DIRECT_TOKEN,
      directCell(cellFrameToProto(full(STREAM_A, [row(0, "H")], 2), SESSION_ID)),
    );

    expect(commitCandidate(candidate)).toBe(false);
    expect(terminalStream.terminalStreamDiagnosticSnapshot(SESSION_ID).replica.seq).toBe(3);
    candidate.cancel("same stream sequence guard");
    direct.unregister();
  });

  test("cancels prospective leases and source ownership when staging is abandoned", async () => {
    canonicalView();
    const { candidate, direct } = stagedCandidate();
    const ready = candidate.awaitReady();
    expect(sessionTerminalTransportPresentation(SESSION_ID)).toMatchObject({
      kind: "sync",
      label: "Coordinator",
    });
    expect(_terminalViewRenewalSchedulerSnapshotForTest().scheduledViewCount).toBe(2);
    const activeRevision = direct.published.at(-1)?.revision;

    candidate.cancel("attempt abandoned");
    expect(await ready).toBe(false);
    expect(sessionTerminalTransportPresentation(SESSION_ID)).toMatchObject({
      kind: "sync",
      label: "Coordinator",
    });

    expect(_terminalViewRenewalSchedulerSnapshotForTest().scheduledViewCount).toBe(1);
    expect(direct.published.at(-1)?.active).toBe(false);
    expect(direct.published.at(-1)?.revision).toBe((activeRevision ?? 0n) + 1n);
    direct.unregister();
  });
});
