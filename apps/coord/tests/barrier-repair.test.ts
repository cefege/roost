// Coordinator-local barrier repair. When the announcement barrier abandons a
// channel's buffer, the cells it dropped can only come back as a full frame —
// but the browser still believes its held cell sequence is current and no
// protocol field tells it otherwise. So the coordinator marks the exact
// (workerFp, sessionId, channelId) route, overrides the worker-bound
// held_cell_seq to 0 while that mark stands, replays one refresh per active
// viewer so recovery does not wait for an unrelated delta or a reload, and
// clears the mark only when a full frame publishes for that same route.

import { afterEach, beforeEach, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import { PbCellGridFrameSchema } from "@roost/shared/proto/cell_pb";
import {
  TerminalViewportStatus,
  WViewportResultSchema,
} from "@roost/shared/proto/worker_transport_pb";
import type { CoordWorkerDown } from "@roost/shared/proto/worker_transport_pb";
import { ResizeCause } from "@roost/shared/proto/coordinator_pb";
import { asChannelId, asWorkerFp } from "@roost/shared/wire";
import {
  _announcedBarrierSnapshot,
  _barrierRepairSnapshot,
  clearBarrierRepairForWorker,
  dropStaleBarrierRepair,
  isBarrierRepairMarked,
  noteBarrierChannelLoss,
  noteBarrierRepairFullFrames,
  primeChannelMap,
  publishCellGrid,
} from "../src/byte-hub.ts";
import {
  processViewportControl,
  requestBarrierRepairFullFrame,
  terminalViewerIdentity,
} from "../src/connect/session-control.ts";
import { mutateCellSubscription } from "../src/connect/cell-subscriptions.ts";
import { _bumpViewer, _viewersBySession } from "../src/connect/viewer-tracker.ts";
import { __setConnectWorkerForTest } from "../src/connect/worker-registry.ts";
import { resolvePendingRpc } from "../src/router/pending-rpcs.ts";
import type { ConnectDeps } from "../src/connect/router.ts";

const WORKER_FP = "a".repeat(64);
const SESSION = "11111111-1111-4111-8111-111111111111";
const OTHER_SESSION = "22222222-2222-4222-8222-222222222222";
const CHANNEL = 12;
const CALLER_FP = "bb".repeat(16);

// A fresh tab per test: the viewer key owns the coordinator's installed
// watermark, its viewer projection, and its committed-replay entry, so reusing
// one would leak ordering state between tests.
let tabCounter = 0;
let identity = terminalViewerIdentity(CALLER_FP, "tab-0");

interface ViewportSend {
  sessionId: string;
  viewerId: string;
  clientSeq: bigint;
  cols: number;
  rows: number;
  cause: number;
  heldCellSeq: bigint;
}

let sends: ViewportSend[] = [];

// processViewportControl reads deps.db only when the live route cache misses,
// and every test here primes that cache; the rest of ConnectDeps is transport
// wiring this focused test never touches.
const deps = {} as unknown as ConnectDeps;

function cellFrame(seq: number, full: boolean) {
  return create(PbCellGridFrameSchema, {
    seq: BigInt(seq),
    full,
    cols: 80,
    rows: 24,
    gridEpoch: "repair",
  });
}

function markOverflow(channelId = CHANNEL): boolean {
  return noteBarrierChannelLoss({
    workerFp: WORKER_FP,
    sessionId: SESSION,
    channelId,
    reason: "overflow",
    phase: "pending",
    cellFrames: 3,
    binaryFrames: 2,
    binaryBytes: 64,
  });
}

beforeEach(() => {
  sends = [];
  identity = terminalViewerIdentity(CALLER_FP, `tab-${++tabCounter}`);
  clearBarrierRepairForWorker(WORKER_FP);
  _viewersBySession.delete(SESSION);
  primeChannelMap([{ id: SESSION, worker_fp: WORKER_FP, channel: CHANNEL }]);
  __setConnectWorkerForTest(WORKER_FP, {
    workerFp: WORKER_FP,
    send: (frame: CoordWorkerDown) => {
      if (frame.frame.case !== "viewportRequest") return 1;
      const request = frame.frame.value;
      sends.push({
        sessionId: request.sessionId,
        viewerId: request.viewerId,
        clientSeq: request.clientSeq,
        cols: request.cols,
        rows: request.rows,
        cause: request.cause,
        heldCellSeq: request.heldCellSeq,
      });
      // Reply the way a healthy worker does: committed at the requested size.
      queueMicrotask(() => {
        resolvePendingRpc(request.requestId, create(WViewportResultSchema, {
          requestId: request.requestId,
          sessionId: request.sessionId,
          clientSeq: request.clientSeq,
          status: TerminalViewportStatus.COMMITTED,
          cols: request.cols,
          rows: request.rows,
          resized: false,
          channelResizeSeq: 4n,
        }), WORKER_FP);
      });
      return 1;
    },
    close: () => undefined,
    bufferedAmount: () => 0,
  });
});

afterEach(() => {
  __setConnectWorkerForTest(WORKER_FP, null);
  clearBarrierRepairForWorker(WORKER_FP);
  _viewersBySession.delete(SESSION);
});

test("a marked route reaches the worker as held_cell_seq 0, then carries the browser value", async () => {
  expect(markOverflow()).toBe(true);
  expect(isBarrierRepairMarked(WORKER_FP, SESSION, CHANNEL)).toBe(true);

  const repaired = await processViewportControl(deps, {
    identity,
    sessionId: SESSION,
    clientSeq: 5n,
    cols: 100,
    rows: 30,
    cause: ResizeCause.TAB_VISIBLE,
    heldCellSeq: 41n,
  });
  expect(repaired.status).toBe("accepted");
  expect(sends).toHaveLength(1);
  // The browser's nonzero held sequence is provably stale for this route.
  expect(sends[0]!.heldCellSeq).toBe(0n);

  // A delta cannot end the override, and neither can a full frame published on
  // another channel of the same session.
  publishCellGrid(asWorkerFp(WORKER_FP), asChannelId(CHANNEL), cellFrame(42, false));
  primeChannelMap([{ id: SESSION, worker_fp: WORKER_FP, channel: CHANNEL + 1 }]);
  publishCellGrid(asWorkerFp(WORKER_FP), asChannelId(CHANNEL + 1), cellFrame(43, true));
  expect(isBarrierRepairMarked(WORKER_FP, SESSION, CHANNEL)).toBe(true);

  // The exact route's full frame clears it.
  publishCellGrid(asWorkerFp(WORKER_FP), asChannelId(CHANNEL), cellFrame(44, true));
  expect(isBarrierRepairMarked(WORKER_FP, SESSION, CHANNEL)).toBe(false);

  primeChannelMap([{ id: SESSION, worker_fp: WORKER_FP, channel: CHANNEL }]);
  const later = await processViewportControl(deps, {
    identity,
    sessionId: SESSION,
    clientSeq: 6n,
    cols: 100,
    rows: 30,
    cause: ResizeCause.HEARTBEAT,
    heldCellSeq: 44n,
  });
  expect(later.status).toBe("accepted");
  expect(sends).toHaveLength(2);
  expect(sends[1]!.heldCellSeq).toBe(44n);
});

test("barrier overflow with an active viewer produces an automatic full frame", async () => {
  mutateCellSubscription(identity.viewerKey, SESSION, true, 7n);
  _bumpViewer(SESSION, identity.viewerKey, 120, 40, 7n);

  const replay = requestBarrierRepairFullFrame({
    workerFp: WORKER_FP,
    sessionId: SESSION,
    channelId: CHANNEL,
  });
  expect(replay.enqueued).toBe(1);
  await replay.settled;

  expect(sends).toHaveLength(1);
  const refresh = sends[0]!;
  // A heartbeat at the installed watermark: the worker's stale-sequence path
  // answers with a snapshot without changing membership or geometry.
  expect(refresh.cause).toBe(ResizeCause.HEARTBEAT);
  expect(refresh.clientSeq).toBe(7n);
  expect(refresh.heldCellSeq).toBe(0n);
  expect(refresh.cols).toBe(120);
  expect(refresh.rows).toBe(40);
  expect(refresh.viewerId).toBe(identity.viewerKey);
  expect(refresh.sessionId).toBe(SESSION);
});

test("no active viewer means no automatic claim, only a standing mark", async () => {
  // A background pane at 0×0 stays subscribed but must never receive a
  // positive-cause refresh: the worker would read it as a withdraw.
  mutateCellSubscription(identity.viewerKey, SESSION, true, 3n);
  _bumpViewer(SESSION, identity.viewerKey, 0, 0, 3n);

  const replay = requestBarrierRepairFullFrame({
    workerFp: WORKER_FP,
    sessionId: SESSION,
    channelId: CHANNEL,
  });
  expect(replay.enqueued).toBe(0);
  await replay.settled;
  expect(sends).toHaveLength(0);

  // Its own next visible claim still picks up the override.
  expect(markOverflow()).toBe(true);
  const visible = await processViewportControl(deps, {
    identity,
    sessionId: SESSION,
    clientSeq: 4n,
    cols: 90,
    rows: 24,
    cause: ResizeCause.TAB_VISIBLE,
    heldCellSeq: 77n,
  });
  expect(visible.status).toBe("accepted");
  expect(sends).toHaveLength(1);
  expect(sends[0]!.heldCellSeq).toBe(0n);
});

test("draining loss of PTY bytes alone marks nothing, pending loss always does", () => {
  expect(noteBarrierChannelLoss({
    workerFp: WORKER_FP,
    sessionId: SESSION,
    channelId: CHANNEL,
    reason: "overflow",
    phase: "draining",
    cellFrames: 0,
    binaryFrames: 4,
    binaryBytes: 512,
  })).toBe(false);
  expect(isBarrierRepairMarked(WORKER_FP, SESSION, CHANNEL)).toBe(false);

  // Pending: the route was not bound yet, so cells arriving after the drop are
  // dropped as unmapped too — the loss is never limited to the buffer.
  expect(noteBarrierChannelLoss({
    workerFp: WORKER_FP,
    sessionId: SESSION,
    channelId: CHANNEL,
    reason: "timeout",
    phase: "pending",
    cellFrames: 0,
    binaryFrames: 0,
    binaryBytes: 0,
  })).toBe(true);
  expect(isBarrierRepairMarked(WORKER_FP, SESSION, CHANNEL)).toBe(true);
});

test("a rebind sweep drops only marks whose route now belongs to another session", () => {
  expect(markOverflow()).toBe(true);
  // Same worker, a route that has not bound yet: its durable append may still
  // be in flight and the mark is what forces that channel's first full frame.
  expect(markOverflow(CHANNEL + 5)).toBe(true);

  dropStaleBarrierRepair(WORKER_FP);
  expect(isBarrierRepairMarked(WORKER_FP, SESSION, CHANNEL)).toBe(true);
  expect(isBarrierRepairMarked(WORKER_FP, SESSION, CHANNEL + 5)).toBe(true);

  // The channel now routes to a different session: that mark can never be read
  // again, because every lookup goes through the live route.
  primeChannelMap([{ id: OTHER_SESSION, worker_fp: WORKER_FP, channel: CHANNEL }]);
  dropStaleBarrierRepair(WORKER_FP);
  expect(isBarrierRepairMarked(WORKER_FP, SESSION, CHANNEL)).toBe(false);
  expect(isBarrierRepairMarked(WORKER_FP, SESSION, CHANNEL + 5)).toBe(true);
});

test("the diagnostic snapshot names the dropped route and its overflow decision", () => {
  expect(markOverflow()).toBe(true);
  noteBarrierRepairFullFrames(WORKER_FP, SESSION, CHANNEL, 2);

  const marks = _barrierRepairSnapshot()[SESSION];
  expect(marks).toHaveLength(1);
  expect(marks![0]).toMatchObject({
    worker_fp: WORKER_FP,
    channel_id: CHANNEL,
    reason: "overflow",
    phase: "pending",
    dropped_cell_frames: 3,
    dropped_binary_frames: 2,
    dropped_binary_bytes: 64,
    full_frame_requests: 2,
  });

  const aggregate = _announcedBarrierSnapshot();
  expect(aggregate.repair_marks).toBeGreaterThanOrEqual(1);
  expect(aggregate.drops.overflow).toBeGreaterThanOrEqual(1);
  expect(aggregate.dropped_cell_frames).toBeGreaterThanOrEqual(3);
  expect(aggregate.dropped_binary_bytes).toBeGreaterThanOrEqual(64);
  expect(aggregate.full_frame_requests).toBeGreaterThanOrEqual(2);

  clearBarrierRepairForWorker(WORKER_FP);
  expect(_barrierRepairSnapshot()[SESSION]).toBeUndefined();
});
