// Announcement barrier through the REAL worker-WS message handler. A respawn's
// durable append is still in flight when the new keeper's first PTY frames
// arrive: nothing may publish before the commit binds
// (workerFp, new_channel) → session, and once it does, the buffered frames must
// publish in their exact arrival order on the socket lane — a respawn's first
// binary frame can carry the only copy of the new PTY's title/OSC mapping, and
// a fast-path frame must not overtake the drain.

import { afterEach, expect, test } from "bun:test";
import { create, toBinary } from "@bufbuild/protobuf";
import {
  CoordWorkerUpSchema,
  WBinarySchema,
  WSessionEventSchema,
  type CoordWorkerUp,
} from "@roost/shared/proto/worker_transport_pb";
import { eventToProto } from "@roost/shared/wire/event-proto";
import { asChannelId, asSessionId, asWorkerFp } from "@roost/shared/wire";
import {
  primeChannelMap,
  replaceWorkerChannelIndex,
} from "../src/byte-hub.ts";
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

function binaryFrame(text: string, seq: number): CoordWorkerUp {
  return create(CoordWorkerUpSchema, {
    frame: { case: "binary", value: create(WBinarySchema, {
      channelId: NEW_CHANNEL,
      direction: 1,
      seq: BigInt(seq),
      data: new TextEncoder().encode(text),
    }) },
  });
}

function label(frame: CoordWorkerUp): string {
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
function harness(): Harness {
  const published: string[] = [];
  const gate = Promise.withResolvers<void>();
  const conn = {
    isCurrentGeneration: () => true,
    handleUpstream: async (frame: CoordWorkerUp): Promise<void> => {
      if (frame.frame.case === "event") {
        await gate.promise;
        // appendEvent installs the channel index inside its post-commit durable
        // publication, before any subscriber can observe the event.
        primeChannelMap([{ id: SESSION, worker_fp: WORKER_FP, channel: NEW_CHANNEL }]);
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
  // Drop this worker's live routes so the next test starts with no binding for
  // the respawned channel.
  replaceWorkerChannelIndex(asWorkerFp(WORKER_FP), []);
});

test("a respawn's binary frames publish after the commit in arrival order", async () => {
  const h = harness();
  h.deliver(respawnedFrame());
  h.deliver(binaryFrame("\u001b]0;fresh-title\u0007", 1));
  h.deliver(binaryFrame("prompt$ ", 2));
  h.deliver(binaryFrame("ready\n", 3));

  // The durable append has not committed: nothing crossed the barrier, and the
  // only-copy title bytes are still buffered rather than dropped.
  await Promise.resolve();
  expect(h.published).toEqual([]);

  h.commitAppend();
  await h.ws.data.tail;

  expect(h.published).toEqual([
    "event:respawned",
    "binary:\u001b]0;fresh-title\u0007",
    "binary:prompt$ ",
    "binary:ready\n",
  ]);
  expect(h.ws.data.announcedChannels.stats().channels).toBe(0);

  // Channel open: later frames take the fast path and still land after the drain.
  h.deliver(binaryFrame("after-drain", 4));
  await h.ws.data.tail;
  await Promise.resolve();
  expect(h.published[h.published.length - 1]).toBe("binary:after-drain");
});
