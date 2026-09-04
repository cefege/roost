// Separates socket lifetime regressions that exercise copying, revocation, and deletion.
// Bun's test runner invokes this suite with an isolated worker transport fixture.
// It depends on the real WebSocket handler, worker registry, and deletion handlers.

import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { create, toBinary } from "@bufbuild/protobuf";
import {
  CoordWorkerUpSchema,
  WBinarySchema,
} from "@roost/shared/proto/worker_transport_pb";
import { WorkersDeleteRequestSchema } from "@roost/shared/proto/coordinator_pb";
import { WORKER_AUTH_SUBPROTOCOL } from "@roost/shared/wire/coord-worker";
import { invalidateJwtKey, verifyJwt } from "../src/jwt.ts";
import {
  createAnnouncedChannelBarrier,
  handleWorkerWsUpgrade,
  makeWorkerWsHandler,
  type WorkerWsData,
} from "../src/connect/worker-ws-handler.ts";
import { getWorkerHubSocket } from "../src/connect/worker-service.ts";
import { connectWorkers, listRoutableFps } from "../src/connect/worker-registry.ts";
import { makeWorkerHandlers } from "../src/connect/handlers-workers.ts";
import {
  helloFrame,
  startWorkerWsTransportFixture,
  type WorkerWsTransportFixture,
} from "./worker-ws-transport-fixture.ts";

let fixture: WorkerWsTransportFixture;
let browserAuthContext: WorkerWsTransportFixture["browserAuthContext"];
let closeWorkerSockets: WorkerWsTransportFixture["closeWorkerSockets"];
let connectDeps: WorkerWsTransportFixture["connectDeps"];
let connectWorker: WorkerWsTransportFixture["connectWorker"];
let dashboardId: string;
let deps: WorkerWsTransportFixture["deps"];
let fenceWorkerSockets: WorkerWsTransportFixture["fenceWorkerSockets"];
let port: number;
let readyWorker: WorkerWsTransportFixture["readyWorker"];
let workerFp: string;
let workerJwt: string;

beforeAll(async () => {
  fixture = await startWorkerWsTransportFixture();
  ({
    browserAuthContext,
    closeWorkerSockets,
    connectDeps,
    connectWorker,
    dashboardId,
    deps,
    fenceWorkerSockets,
    port,
    readyWorker,
    workerFp,
    workerJwt,
  } = fixture);
});

afterAll(async () => { await fixture?.cleanup(); });

describe("worker↔coord raw-WS transport", () => {
  // Regression: message() MUST copy off Bun's pooled ServerWebSocket buffer.
  // fromBinary returns a subarray VIEW for `bytes` fields (WBinary.data) and
  // handleUpstream runs after this synchronous handler — a view into Bun's
  // recyclable buffer would be dereferenced as freed memory:
  // coord segfault whose fault address is terminal bytes (ESC[, session ids).
  // Reverting `Uint8Array.from(message)` → a `.buffer` view fails this test.
  test("message() copies off the pooled WS buffer (no borrowed-view UAF)", async () => {
    const wsHandler = makeWorkerWsHandler(deps);
    const seen: Uint8Array[] = [];
    const fakeConn = {
      handleUpstream: async (f: { frame: { value: { data: Uint8Array } } }) => {
        seen.push(new Uint8Array(f.frame.value.data));
      },
      close() { /* noop */ },
      // message() fences superseded generations before decoding; this stub
      // stands in for the fingerprint's current handle.
      isCurrentGeneration: () => true,
      isReady: () => true,
    };
    const fakeWs = {
      data: {
        caller: {
          fingerprint: workerFp,
          label: "test-worker",
          keyGeneration: 0,
          validUntilMs: Date.now() + 60_000,
        },
        fp: workerFp,
        dashboardId,
        authDeadlineAtMs: null,
        authDeadlineTimer: null,
        conn: fakeConn,
        queue: null as WorkerWsData["queue"],
        eventRate: { startedAtMs: null, events: 0 },
        // The channel is never announced, so both fast-path lanes fall straight
        // through to handleUpstream.
        announcedChannels: createAnnouncedChannelBarrier(workerFp),
      },
    };
    const payload = new Uint8Array([0x1b, 0x5b, 0x41, 0x99]); // ESC [ A + high byte
    const frame = create(CoordWorkerUpSchema, {
      frame: { case: "binary", value: create(WBinarySchema, { channelId: 3, direction: 1, seq: 42n, data: payload }) },
    });
    const buf = Buffer.from(toBinary(CoordWorkerUpSchema, frame));
    wsHandler.message(
      fakeWs as unknown as Parameters<typeof wsHandler.message>[0],
      buf,
    );
    buf.fill(0); // simulate Bun recycling the pooled buffer after the sync handler
    const queue = fakeWs.data.queue;
    if (queue === null) throw new Error("expected worker frame queue");
    await queue.whenIdle();
    expect(seen).toHaveLength(1);
    const [firstSeen] = seen;
    if (firstSeen === undefined) throw new Error("expected one handled frame");
    expect(Array.from(firstSeen)).toEqual([0x1b, 0x5b, 0x41, 0x99]);
  });

  test("revocation between accepted upgrade and open closes before connection registration", async () => {
    const caller = await verifyJwt(workerJwt, {
      db: deps.db,
      cache: deps.jwtCache,
      jwtMaxAgeSecs: deps.cfg.jwtMaxAgeSecs,
    });
    const data: WorkerWsData = {
      kind: "worker",
      caller,
      fp: workerFp,
      dashboardId,
      authDeadlineAtMs: null,
      authDeadlineTimer: null,
      conn: null,
      queue: null,
      eventRate: { startedAtMs: null, events: 0 },
      announcedChannels: createAnnouncedChannelBarrier(workerFp),
    };
    invalidateJwtKey(deps.jwtCache, workerFp);
    let closed: [number, string] | undefined;
    const ws = {
      data,
      close: (code: number, reason: string) => { closed = [code, reason]; },
    };
    makeWorkerWsHandler(deps).open(ws as never);
    expect(closed).toEqual([4001, "revoked"]);
    expect(data.conn).toBeNull();
  });

  test("online deletion fences and closes the worker before old-key reconnect", async () => {
    const w = connectWorker(workerFp, workerJwt);
    await w.opened;
    w.sendUp(helloFrame(workerFp));
    await w.waitFor((frame) => frame.frame.case === "helloAck");
    await readyWorker(w, workerFp);
    expect(listRoutableFps(dashboardId)).toContain(workerFp);

    const closed = new Promise<{ code: number; reason: string }>((resolve) => {
      w.ws.addEventListener("close", (event) => {
        resolve({ code: event.code, reason: event.reason });
      }, { once: true });
    });
    const handlers = makeWorkerHandlers({
      ...connectDeps,
      onWorkerDeletedFence: fenceWorkerSockets,
      onWorkerDeletedSocketClose: closeWorkerSockets,
    });
    const response = await handlers.workersDelete(
      create(WorkersDeleteRequestSchema, { fp: workerFp }),
      browserAuthContext(),
    );

    expect(response.ok).toBe(true);
    expect(connectWorkers.has(workerFp)).toBe(false);
    expect(getWorkerHubSocket(workerFp)).toBeNull();
    expect(listRoutableFps(dashboardId)).not.toContain(workerFp);
    expect(await closed).toEqual({ code: 4001, reason: "revoked" });

    const reconnect = await handleWorkerWsUpgrade(
      new Request(`http://127.0.0.1:${port}/ws/coord-worker/${workerFp}`, {
        headers: {
          "sec-websocket-protocol": `${WORKER_AUTH_SUBPROTOCOL}, ${workerJwt}`,
        },
      }),
      { upgrade: () => false } as never,
      deps,
    );
    expect((reconnect as Response).status).toBe(401);
  });
});
