// Regression: CoordLink stale-link watchdog. When the coord process dies
// behind tailscale serve the worker-side TCP stays ESTABLISHED and ws.send
// black-holes — onerror/onclose never fire, so the worker re-dial logic that
// keys off those events never runs (2026-07-11: 7h zombie link, every spawn
// failed [failed_precondition] worker not connected). The watchdog watches
// downstream silence and force-closes + re-dials. These are integration tests
// against a live Bun.serve WebSocket: the behavior under test is real-clock
// silence detection driving CoordLink's internal setInterval alongside the
// real socket I/O loop, which fake timers cannot advance coherently — the
// ts-no-test-timers exception (real platform-clock timer behavior) applies.

import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

// Isolate durable session-event stores from production worker state.
// Set at module evaluation before any test opens a store.
process.env.ROOST_WORKER_DATA_DIR = mkdtempSync(join(tmpdir(), "coordlink-test-"));
process.env.ROOST_KEEPER_QUIET = "1";

import { expect, test } from "bun:test";
import { create, toBinary } from "@bufbuild/protobuf";
import { CoordWorkerDownSchema, DHelloAckSchema, DPingSchema } from "@roost/shared/proto/worker_transport_pb";
import { startCoordLink } from "../src/transport/coord-link.ts";
import { openSessionEventStore } from "../src/transport/session-event-store.ts";
import type { WorkerFp } from "@roost/shared/wire";

function pingBytes(): Uint8Array {
  return toBinary(
    CoordWorkerDownSchema,
    create(CoordWorkerDownSchema, {
      frame: { case: "ping", value: create(DPingSchema, { ts: BigInt(Date.now()) }) },
    }),
  );
}

function helloAckBytes(): Uint8Array {
  return toBinary(
    CoordWorkerDownSchema,
    create(CoordWorkerDownSchema, {
      frame: {
        case: "helloAck",
        value: create(DHelloAckSchema, {}),
      },
    }),
  );
}

test("silent server → watchdog force-closes and re-dials", async () => {
  let opens = 0;
  const { promise: reDialed, resolve: gotReDial } = Promise.withResolvers<void>();
  const reopenFlags: boolean[] = [];
  const helloAckFlags: boolean[] = [];
  const { promise: reconnectHook, resolve: gotReconnectHook } = Promise.withResolvers<void>();
  const { promise: reconnectAck, resolve: gotReconnectAck } = Promise.withResolvers<void>();
  const server = Bun.serve({
    port: 0,
    fetch(req, s) {
      if (s.upgrade(req)) return undefined;
      return new Response("ws only", { status: 400 });
    },
    websocket: {
      open() {
        opens += 1;
        if (opens >= 2) gotReDial();
      },
      message(ws) {
        // Coordinator primes its channel map from hello before helloAck. Reply
        // once per dial; then stay silent so the watchdog still forces re-dial.
        ws.send(helloAckBytes());
      },
      close() {},
    },
  });
  const eventStore = openSessionEventStore({
    dbPath: join(process.env.ROOST_WORKER_DATA_DIR!, "silent-outbox.sqlite"),
    legacySequencePath: join(process.env.ROOST_WORKER_DATA_DIR!, "silent-client-seq.txt"),
  });
  const link = startCoordLink({
    coordHttpUrl: `http://127.0.0.1:${server.port}`,
    workerFp: "test-fp" as WorkerFp,
    workerVersion: "test",
    sessionEventStore: eventStore,
    mintJwt: async () => "jwt",
    staleLinkTimeoutMs: 300,
    staleCheckIntervalMs: 50,
    onOpen(reconnected) {
      reopenFlags.push(reconnected);
      if (reconnected) gotReconnectHook();
    },
    onHelloAck({ reconnected }) {
      helloAckFlags.push(reconnected);
      if (reconnected) gotReconnectAck();
    },
  });
  // Watchdog fires ≤350ms after open; reconnect backoff starts at 500ms, so
  // the second open lands well inside the bun:test deadline. If the watchdog
  // regresses (no re-dial), this await hangs and bun:test fails on timeout.
  await Promise.all([reDialed, reconnectHook, reconnectAck]);
  expect(opens).toBeGreaterThanOrEqual(2);
  expect(reopenFlags.slice(0, 2)).toEqual([false, true]);
  expect(helloAckFlags.slice(0, 2)).toEqual([false, true]);
  link.dispose();
  eventStore.close();
  server.stop(true);
}, 10_000);

test("pinged link stays up — no false-positive reconnect", async () => {
  let opens = 0;
  let ticks = 0;
  let pingTimer: Timer | undefined;
  const { promise: sawEnoughPings, resolve: enough } = Promise.withResolvers<void>();
  const server = Bun.serve({
    port: 0,
    fetch(req, s) {
      if (s.upgrade(req)) return undefined;
      return new Response("ws only", { status: 400 });
    },
    websocket: {
      open(ws) {
        opens += 1;
        // Real interval — this IS the behavior under test: a live coord's
        // 30s ping keepalive keeps lastDownstreamAtMs fresh so the watchdog
        // never false-positives. Faster cadence (100ms) scaled to the test's
        // 300ms stale timeout. Cleared on close and after the assertion.
        pingTimer = setInterval(() => {
          try {
            ws.send(pingBytes());
          } catch {
            /* socket closed */
          }
          ticks += 1;
          // ~1.5s of pings = 5× the 300ms stale timeout: ample window for a
          // false-positive reconnect to have surfaced if the watchdog ignored
          // downstream frames.
          if (ticks >= 15) enough();
        }, 100);
      },
      message() {
        /* ignore worker hello/upstream frames */
      },
      close() {
        clearInterval(pingTimer);
      },
    },
  });
  const eventStore = openSessionEventStore({
    dbPath: join(process.env.ROOST_WORKER_DATA_DIR!, "pinged-outbox.sqlite"),
    legacySequencePath: join(process.env.ROOST_WORKER_DATA_DIR!, "pinged-client-seq.txt"),
  });
  const link = startCoordLink({
    coordHttpUrl: `http://127.0.0.1:${server.port}`,
    workerFp: "test-fp" as WorkerFp,
    workerVersion: "test",
    sessionEventStore: eventStore,
    mintJwt: async () => "jwt",
    staleLinkTimeoutMs: 300,
    staleCheckIntervalMs: 50,
  });
  await sawEnoughPings;
  expect(opens).toBe(1);
  clearInterval(pingTimer);
  link.dispose();
  eventStore.close();
  server.stop(true);
}, 10_000);
