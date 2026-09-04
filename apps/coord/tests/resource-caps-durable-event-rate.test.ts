/**
 * Covers the per-socket durable-event cap that protects worker WebSocket handling.
 * Bun discovers this suite directly and sends encoded events through the real handler.
 * Narrow socket doubles supply worker state while the frame queue preserves ordering.
 */
import { describe, expect, test } from "bun:test";
import { create, toBinary } from "@bufbuild/protobuf";
import {
  CoordWorkerUpSchema,
  WSessionEventSchema,
  type CoordWorkerUp,
} from "@roost/shared/proto/worker_transport_pb";
import { asSessionId, asWorkerFp } from "@roost/shared/wire";
import { eventToProto } from "@roost/shared/wire/event-proto";
import {
  WORKER_DURABLE_EVENT_LIMIT,
  createAnnouncedChannelBarrier,
  makeWorkerWsHandler,
  type WorkerWsData,
} from "../src/connect/worker-ws-handler.ts";
import type {
  WorkerConn,
  WorkerServiceDeps,
} from "../src/connect/worker-service.ts";

const WORKER_FP = asWorkerFp("d".repeat(64));
const DASHBOARD_ID = "resource-caps-dashboard";

function durableEventFrame(): CoordWorkerUp {
  return create(CoordWorkerUpSchema, {
    frame: {
      case: "event",
      value: create(WSessionEventSchema, {
        event: eventToProto({
          kind: "closed",
          session_id: asSessionId("77777777-7777-4777-8777-777777777777"),
          exit_code: 0,
          ts: Date.now(),
        }, 0)!,
        clientSeq: 1n,
      }),
    },
  });
}

function rateLimitedSocket(): {
  data: WorkerWsData;
  closes: Array<[number | undefined, string | undefined]>;
  handled: { value: number };
} {
  const handled = { value: 0 };
  const conn: WorkerConn = {
    async handleUpstream() { handled.value += 1; },
    close() {},
    revoke() {},
    isReady: () => true,
    isCurrentGeneration: () => true,
  };
  const data: WorkerWsData = {
    kind: "worker",
    caller: {
      fingerprint: WORKER_FP,
      label: "rate-worker",
      keyGeneration: 0,
      validUntilMs: Date.now() + 60_000,
    },
    fp: WORKER_FP,
    dashboardId: DASHBOARD_ID,
    authDeadlineAtMs: null,
    authDeadlineTimer: null,
    conn,
    queue: null,
    eventRate: { startedAtMs: null, events: 0 },
    announcedChannels: createAnnouncedChannelBarrier(WORKER_FP),
  };
  const closes: Array<[number | undefined, string | undefined]> = [];
  return {
    data,
    closes,
    handled,
    close(code?: number, reason?: string) { closes.push([code, reason]); },
  } as never;
}

describe("per-socket durable event rate", () => {
  test("admits 600/min, closes the 601st socket, and leaves another socket usable", async () => {
    const clock = { now: () => 10_000 };
    const handler = makeWorkerWsHandler(
      {} as WorkerServiceDeps,
      { deadlineClock: clock as never },
    );
    const frame = Buffer.from(toBinary(CoordWorkerUpSchema, durableEventFrame()));
    const first = rateLimitedSocket();
    for (let count = 0; count < WORKER_DURABLE_EVENT_LIMIT; count += 1) {
      handler.message(first as never, frame);
      await first.data.queue!.whenIdle();
    }
    expect(first.handled.value).toBe(WORKER_DURABLE_EVENT_LIMIT);
    expect(first.closes).toEqual([]);

    handler.message(first as never, frame);
    expect(first.closes).toEqual([[1008, "worker event rate exceeded"]]);
    expect(first.handled.value).toBe(WORKER_DURABLE_EVENT_LIMIT);

    const second = rateLimitedSocket();
    handler.message(second as never, frame);
    await second.data.queue!.whenIdle();
    expect(second.handled.value).toBe(1);
    expect(second.closes).toEqual([]);

  });
});
