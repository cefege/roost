// Separates heartbeat timing so fake-timer state stays within one focused suite.
// Bun's test runner invokes this suite with an isolated worker transport fixture.
// It depends on the real worker connection, ordered frame queue, and ping deadlines.

import { afterAll, beforeAll, describe, expect, test, vi } from "bun:test";
import { create, toBinary } from "@bufbuild/protobuf";
import {
  CoordWorkerUpSchema,
  WPongSchema,
  WSessionEventSchema,
  type CoordWorkerDown,
} from "@roost/shared/proto/worker_transport_pb";
import {
  createAnnouncedChannelBarrier,
  makeWorkerWsHandler,
  type WorkerWsData,
} from "../src/connect/worker-ws-handler.ts";
import {
  makeWorkerConn,
  WORKER_PING_DELAY_MS,
  WORKER_PONG_TIMEOUT_MS,
  type WorkerConn,
} from "../src/connect/worker-conn.ts";
import { connectWorkers } from "../src/connect/worker-registry.ts";
import {
  helloFrame,
  startWorkerWsTransportFixture,
  type WorkerWsTransportFixture,
} from "./worker-ws-transport-fixture.ts";

let fixture: WorkerWsTransportFixture;
let dashboardId: string;
let deps: WorkerWsTransportFixture["deps"];

beforeAll(async () => {
  fixture = await startWorkerWsTransportFixture();
  ({ dashboardId, deps } = fixture);
});

afterAll(async () => { await fixture?.cleanup(); });

describe("worker↔coord raw-WS transport", () => {
  test("sends one ping and lets only its exact pong beat the deadline", async () => {
    vi.useFakeTimers();
    const fp = "f".repeat(64);
    const sent: CoordWorkerDown[] = [];
    let closeRequests = 0;
    const conn = makeWorkerConn(
      deps,
      { fingerprint: fp },
      (frame) => { sent.push(frame); return 1; },
      () => { closeRequests++; },
      undefined,
      dashboardId,
    );
    try {
      await conn.handleUpstream(helloFrame(fp));
      vi.advanceTimersByTime(WORKER_PING_DELAY_MS);
      const firstPing = sent.findLast((frame) => frame.frame.case === "ping");
      if (firstPing?.frame.case !== "ping") throw new Error("missing worker ping");
      expect(sent.filter((frame) => frame.frame.case === "ping")).toHaveLength(1);

      await conn.handleUpstream(create(CoordWorkerUpSchema, {
        frame: {
          case: "pong",
          value: create(WPongSchema, { ts: firstPing.frame.value.ts + 1n }),
        },
      }));
      vi.advanceTimersByTime(WORKER_PONG_TIMEOUT_MS - 1);
      expect(closeRequests).toBe(0);
      expect(sent.filter((frame) => frame.frame.case === "ping")).toHaveLength(1);
      vi.advanceTimersByTime(1);
      expect(closeRequests).toBe(1);
      expect(connectWorkers.has(fp)).toBe(false);
    } finally {
      conn.close();
      vi.useRealTimers();
    }
  });

  test("an exact late pong cancels its deadline and starts a fresh 30s delay", async () => {
    vi.useFakeTimers();
    const fp = "d".repeat(64);
    const sent: CoordWorkerDown[] = [];
    let closeRequests = 0;
    const conn = makeWorkerConn(
      deps,
      { fingerprint: fp },
      (frame) => { sent.push(frame); return 1; },
      () => { closeRequests++; },
      undefined,
      dashboardId,
    );
    try {
      await conn.handleUpstream(helloFrame(fp));
      vi.advanceTimersByTime(WORKER_PING_DELAY_MS);
      const firstPing = sent.findLast((frame) => frame.frame.case === "ping");
      if (firstPing?.frame.case !== "ping") throw new Error("missing worker ping");
      vi.advanceTimersByTime(70_000);
      await conn.handleUpstream(create(CoordWorkerUpSchema, {
        frame: {
          case: "pong",
          value: create(WPongSchema, { ts: firstPing.frame.value.ts }),
        },
      }));

      vi.advanceTimersByTime(20_000);
      expect(closeRequests).toBe(0);
      expect(sent.filter((frame) => frame.frame.case === "ping")).toHaveLength(1);
      vi.advanceTimersByTime(WORKER_PING_DELAY_MS - 20_000 - 1);
      expect(sent.filter((frame) => frame.frame.case === "ping")).toHaveLength(1);
      vi.advanceTimersByTime(1);
      expect(sent.filter((frame) => frame.frame.case === "ping")).toHaveLength(2);
      expect(closeRequests).toBe(0);

      const secondPing = sent.findLast((frame) => frame.frame.case === "ping");
      if (secondPing?.frame.case !== "ping") throw new Error("missing second worker ping");
      await conn.handleUpstream(create(CoordWorkerUpSchema, {
        frame: {
          case: "pong",
          value: create(WPongSchema, { ts: secondPing.frame.value.ts }),
        },
      }));
      conn.close();
      vi.advanceTimersByTime(WORKER_PONG_TIMEOUT_MS + WORKER_PING_DELAY_MS);
      expect(closeRequests).toBe(0);
    } finally {
      conn.close();
      vi.useRealTimers();
    }
  });

  test("a superseded socket's captured deadline cannot close its replacement", async () => {
    vi.useFakeTimers();
    const fp = "e".repeat(64);
    let oldCloseRequests = 0;
    let newCloseRequests = 0;
    const oldConn = makeWorkerConn(
      deps,
      { fingerprint: fp },
      () => 1,
      () => { oldCloseRequests++; },
      undefined,
      dashboardId,
    );
    const newConn = makeWorkerConn(
      deps,
      { fingerprint: fp },
      () => 1,
      () => { newCloseRequests++; },
      undefined,
      dashboardId,
    );
    try {
      await oldConn.handleUpstream(helloFrame(fp));
      vi.advanceTimersByTime(WORKER_PING_DELAY_MS);
      await newConn.handleUpstream(helloFrame(fp));
      expect(oldCloseRequests).toBe(1);

      vi.advanceTimersByTime(WORKER_PONG_TIMEOUT_MS);
      expect(oldCloseRequests).toBe(1);
      expect(newCloseRequests).toBe(0);
      expect(connectWorkers.get(fp)?.workerFp).toBe(fp);
    } finally {
      oldConn.close();
      newConn.close();
      vi.useRealTimers();
    }
  });

  test("an exact pong clears the deadline only after the ordered frame lane reaches it", async () => {
    vi.useFakeTimers();
    const fp = "c".repeat(64);
    const sent: CoordWorkerDown[] = [];
    let closeRequests = 0;
    const conn = makeWorkerConn(
      deps,
      { fingerprint: fp },
      (frame) => { sent.push(frame); return 1; },
      () => { closeRequests++; },
      undefined,
      dashboardId,
    );
    const gate = Promise.withResolvers<void>();
    const order: string[] = [];
    const queuedConn: WorkerConn = {
      async handleUpstream(frame) {
        order.push(frame.frame.case ?? "unknown");
        if (frame.frame.case === "event") await gate.promise;
        await conn.handleUpstream(frame);
      },
      close: () => conn.close(),
      revoke: () => conn.revoke(),
      isCurrentGeneration: () => conn.isCurrentGeneration(),
      isReady: () => conn.isReady(),
    };
    const data: WorkerWsData = {
      kind: "worker",
      caller: {} as never,
      fp,
      dashboardId,
      authDeadlineAtMs: null,
      authDeadlineTimer: null,
      conn: queuedConn,
      queue: null,
      eventRate: { startedAtMs: null, events: 0 },
      announcedChannels: createAnnouncedChannelBarrier(fp),
    };
    const ws = { data, close: () => undefined };
    const handler = makeWorkerWsHandler(deps);
    try {
      await conn.handleUpstream(helloFrame(fp));
      vi.advanceTimersByTime(WORKER_PING_DELAY_MS);
      const ping = sent.findLast((frame) => frame.frame.case === "ping");
      if (ping?.frame.case !== "ping") throw new Error("missing worker ping");
      const held = create(CoordWorkerUpSchema, {
        frame: { case: "event", value: create(WSessionEventSchema, {}) },
      });
      const pong = create(CoordWorkerUpSchema, {
        frame: { case: "pong", value: create(WPongSchema, { ts: ping.frame.value.ts }) },
      });
      handler.message(ws as never, Buffer.from(toBinary(CoordWorkerUpSchema, held)));
      handler.message(ws as never, Buffer.from(toBinary(CoordWorkerUpSchema, pong)));
      expect(order).toEqual(["event"]);

      vi.advanceTimersByTime(WORKER_PONG_TIMEOUT_MS - 1);
      expect(closeRequests).toBe(0);
      const idle = data.queue?.whenIdle();
      gate.resolve();
      await idle;
      expect(order).toEqual(["event", "pong"]);
      vi.advanceTimersByTime(1);
      expect(closeRequests).toBe(0);
    } finally {
      data.queue?.close();
      conn.close();
      vi.useRealTimers();
    }
  });
});
