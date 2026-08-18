// Announcement barrier through the REAL worker-WS message handler. A respawn's
// durable append is still in flight when the new keeper's first PTY bytes and
// first cell grid arrive: nothing may publish before the commit binds
// (workerFp, new_channel) → session, and once it does, the buffered frames must
// publish in their exact arrival order on the socket lane — a respawn's first
// binary frame can carry the only copy of the new PTY's title/OSC mapping, and
// a fast-path frame must not overtake the drain.

import { afterEach, expect, test } from "bun:test";
import { create, toBinary } from "@bufbuild/protobuf";
import { PbCellGridFrameSchema } from "@roost/shared/proto/cell_pb";
import {
  CoordWorkerUpSchema,
  WBinarySchema,
  WCellGridSchema,
  WSessionEventSchema,
  type CoordWorkerDown,
  type CoordWorkerUp,
  type DViewportRequest,
} from "@roost/shared/proto/worker_transport_pb";
import { ResizeCause } from "@roost/shared/proto/coordinator_pb";
import { eventToProto } from "@roost/shared/wire/event-proto";
import { asChannelId, asSessionId, asWorkerFp } from "@roost/shared/wire";
import {
  _barrierRepairSnapshot,
  clearBarrierRepairForWorker,
  isBarrierRepairMarked,
} from "../src/byte-hub-barrier-repair.ts";
import {
  primeChannelMap,
  replaceWorkerChannelIndex,
} from "../src/byte-hub.ts";
import { ANNOUNCED_CHANNEL_MAX_FRAMES } from "../src/connect/announced-channel-barrier.ts";
import { mutateCellSubscription } from "../src/connect/cell-subscriptions.ts";
import { terminalViewerIdentity } from "../src/connect/session-control.ts";
import { _bumpViewer, _viewersBySession } from "../src/connect/viewer-tracker.ts";
import { __setConnectWorkerForTest } from "../src/connect/worker-registry.ts";
import {
  createAnnouncedChannelBarrier,
  makeWorkerWsHandler,
  type WorkerWsData,
} from "../src/connect/worker-ws-handler.ts";
import type { WorkerServiceDeps } from "../src/connect/worker-service.ts";

const WORKER_FP = "c".repeat(64);
const SESSION = "33333333-3333-4333-8333-333333333333";
const NEW_CHANNEL = 21;

// message() never reads deps — it decodes, buffers, and drives ws.data.conn.
const deps = {} as unknown as WorkerServiceDeps;

function respawnedFrame(): CoordWorkerUp {
  return create(CoordWorkerUpSchema, {
    frame: { case: "event", value: create(WSessionEventSchema, {
      event: eventToProto({
        kind: "respawned",
        session_id: asSessionId(SESSION),
        new_channel: asChannelId(NEW_CHANNEL),
        ts: Date.now(),
      }, 0)!,
      clientSeq: 1n,
    }) },
  });
}

function cellFrame(seq: number, full: boolean): CoordWorkerUp {
  return create(CoordWorkerUpSchema, {
    frame: { case: "cellGrid", value: create(WCellGridSchema, {
      channelId: NEW_CHANNEL,
      frame: create(PbCellGridFrameSchema, {
        seq: BigInt(seq),
        full,
        cols: 80,
        rows: 24,
        gridEpoch: "respawn",
      }),
    }) },
  });
}

function binaryFrame(text: string): CoordWorkerUp {
  return create(CoordWorkerUpSchema, {
    frame: { case: "binary", value: create(WBinarySchema, {
      channelId: NEW_CHANNEL,
      direction: 1,
      seq: 1n,
      data: new TextEncoder().encode(text),
    }) },
  });
}

function label(frame: CoordWorkerUp): string {
  if (frame.frame.case === "cellGrid") {
    return `cell:${frame.frame.value.frame?.seq}:${frame.frame.value.frame?.full ? "full" : "delta"}`;
  }
  if (frame.frame.case === "binary") return `binary:${new TextDecoder().decode(frame.frame.value.data)}`;
  return `event:${frame.frame.case}`;
}

interface Harness {
  ws: { data: WorkerWsData };
  deliver: (frame: CoordWorkerUp) => void;
  published: string[];
  commitAppend: () => void;
}

/** Drive the real handler with a WorkerConn whose event append commits only when
 *  the test says so — the exact window the barrier exists to cover. */
function harness(options: { bindOnCommit?: boolean } = {}): Harness {
  const published: string[] = [];
  const gate = Promise.withResolvers<void>();
  const conn = {
    isCurrentGeneration: () => true,
    handleUpstream: async (frame: CoordWorkerUp): Promise<void> => {
      if (frame.frame.case === "event") {
        await gate.promise;
        // appendEvent installs the channel index inside its post-commit durable
        // publication, before any subscriber can observe the event.
        if (options.bindOnCommit !== false) {
          primeChannelMap([{ id: SESSION, worker_fp: WORKER_FP, channel: NEW_CHANNEL }]);
        }
        published.push("event:respawned");
        return;
      }
      published.push(label(frame));
    },
    close: () => undefined,
  };
  const data: WorkerWsData = {
    kind: "worker",
    caller: { fingerprint: WORKER_FP, keyGeneration: 1 },
    fp: WORKER_FP,
    conn,
    tail: Promise.resolve(),
    announcedChannels: createAnnouncedChannelBarrier(WORKER_FP),
  };
  const ws = { data };
  const handler = makeWorkerWsHandler(deps);
  return {
    ws,
    published,
    commitAppend: gate.resolve,
    deliver: (frame) => {
      handler.message(
        ws as never,
        Buffer.from(toBinary(CoordWorkerUpSchema, frame)),
      );
    },
  };
}

afterEach(() => {
  clearBarrierRepairForWorker(WORKER_FP);
  // Drop this worker's live routes so the next test starts with no binding for
  // the respawned channel.
  replaceWorkerChannelIndex(asWorkerFp(WORKER_FP), []);
});

test("a respawn's first binary frame publishes after the commit, in order with its first cell frame", async () => {
  const h = harness();
  h.deliver(respawnedFrame());
  h.deliver(binaryFrame("\u001b]0;fresh-title\u0007"));
  h.deliver(cellFrame(1, true));
  h.deliver(binaryFrame("prompt$ "));
  h.deliver(cellFrame(2, false));

  // The durable append has not committed: nothing crossed the barrier, and the
  // only-copy title bytes are still buffered rather than dropped.
  await Promise.resolve();
  expect(h.published).toEqual([]);

  h.commitAppend();
  await h.ws.data.tail;

  expect(h.published).toEqual([
    "event:respawned",
    "binary:\u001b]0;fresh-title\u0007",
    "cell:1:full",
    "binary:prompt$ ",
    "cell:2:delta",
  ]);
  expect(h.ws.data.announcedChannels.stats().channels).toBe(0);
  expect(isBarrierRepairMarked(WORKER_FP, SESSION, NEW_CHANNEL)).toBe(false);

  // Channel open: later frames take the fast path and still land after the drain.
  h.deliver(cellFrame(3, false));
  await h.ws.data.tail;
  await Promise.resolve();
  expect(h.published[h.published.length - 1]).toBe("cell:3:delta");
});

test("a commit whose binding never installed drops the buffer and marks the route for repair", async () => {
  const h = harness({ bindOnCommit: false });
  h.deliver(respawnedFrame());
  h.deliver(binaryFrame("only-copy-title"));
  h.deliver(cellFrame(1, true));

  h.commitAppend();
  await h.ws.data.tail;

  expect(h.published).toEqual(["event:respawned"]);
  expect(h.ws.data.announcedChannels.stats().channels).toBe(0);
  // Cells were lost, so the route carries the coordinator-local override until a
  // full frame publishes for it.
  expect(isBarrierRepairMarked(WORKER_FP, SESSION, NEW_CHANNEL)).toBe(true);
});

test("barrier overflow with an active viewer produces an automatic full frame", async () => {
  // A tab that is actually watching this session at a real size.
  const identity = terminalViewerIdentity("dd".repeat(16), "tab-overflow");
  mutateCellSubscription(identity.viewerKey, SESSION, true, 11n);
  _bumpViewer(SESSION, identity.viewerKey, 111, 33, 11n);
  const refresh = Promise.withResolvers<DViewportRequest>();
  __setConnectWorkerForTest(WORKER_FP, {
    workerFp: WORKER_FP,
    send: (frame: CoordWorkerDown) => {
      if (frame.frame.case === "viewportRequest") refresh.resolve(frame.frame.value);
      return 1;
    },
    close: () => undefined,
    bufferedAmount: () => 0,
  });

  try {
    const h = harness();
    h.deliver(respawnedFrame());
    // Flood the announcement window past its frame cap while the durable append
    // is still queued.
    for (let seq = 1; seq <= ANNOUNCED_CHANNEL_MAX_FRAMES + 1; seq++) {
      h.deliver(cellFrame(seq, seq === 1));
    }

    // Recovery cannot wait for an unrelated delta or a browser reload: the
    // coordinator replays a heartbeat with no held frame, which is what makes the
    // worker emit an authoritative snapshot.
    const request = await refresh.promise;
    expect(request.sessionId).toBe(SESSION);
    expect(request.viewerId).toBe(identity.viewerKey);
    expect(request.cause).toBe(ResizeCause.HEARTBEAT);
    expect(_barrierRepairSnapshot()[SESSION]?.[0]).toMatchObject({
      channel_id: NEW_CHANNEL,
      reason: "overflow",
      phase: "pending",
    });
    expect(request.heldCellSeq).toBe(0n);
    expect(request.clientSeq).toBe(11n);
    expect(request.cols).toBe(111);
    expect(request.rows).toBe(33);

    // Let the append finish so the socket lane settles before teardown.
    h.commitAppend();
    await h.ws.data.tail;
  } finally {
    __setConnectWorkerForTest(WORKER_FP, null);
    _viewersBySession.delete(SESSION);
  }
});
