// Keeps dialing, upgrade authentication, and RPC round trips in the core transport suite.
// Bun's test runner invokes this file with setup from worker-ws-transport-fixture.
// The assertions depend on the real coord worker hub and raw protobuf transport.

import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { create, toBinary } from "@bufbuild/protobuf";
import {
  CoordWorkerUpSchema,
  DHelloAckSchema,
  WRpcOkSchema,
} from "@roost/shared/proto/worker_transport_pb";
import { WORKER_AUTH_SUBPROTOCOL } from "@roost/shared/wire/coord-worker";
import { handleWorkerWsUpgrade } from "../src/connect/worker-ws-handler.ts";
import { getWorkerHubSocket } from "../src/connect/worker-service.ts";
import { listRoutableFps } from "../src/connect/worker-registry.ts";
import { createPendingRpc } from "../src/router/pending-rpcs.ts";
import {
  helloFrame,
  startWorkerWsTransportFixture,
  type WorkerWsTransportFixture,
} from "./worker-ws-transport-fixture.ts";

let fixture: WorkerWsTransportFixture;
let connectWorker: WorkerWsTransportFixture["connectWorker"];
let dashboardId: string;
let deps: WorkerWsTransportFixture["deps"];
let port: number;
let readyWorker: WorkerWsTransportFixture["readyWorker"];
let workerFp: string;
let workerJwt: string;

beforeAll(async () => {
  fixture = await startWorkerWsTransportFixture();
  ({
    connectWorker,
    dashboardId,
    deps,
    port,
    readyWorker,
    workerFp,
    workerJwt,
  } = fixture);
});

afterAll(async () => { await fixture?.cleanup(); });

describe("worker↔coord raw-WS transport", () => {
  test("dial + hello → helloAck, worker registered in the hub", async () => {
    const w = connectWorker(workerFp, workerJwt);
    await w.opened;
    w.sendUp(helloFrame(workerFp));
    const ack = await w.waitFor((f) => f.frame.case === "helloAck");
    expect(ack.frame.case).toBe("helloAck");
    if (ack.frame.case !== "helloAck") throw new Error("expected helloAck");
    expect(toBinary(DHelloAckSchema, ack.frame.value)).toEqual(new Uint8Array());
    // Hello owns the generation but remains unroutable until its exact
    // snapshot commits and publishes.
    expect(getWorkerHubSocket(workerFp)).toBeNull();
    expect(listRoutableFps(dashboardId)).not.toContain(workerFp);
    await readyWorker(w, workerFp);
    expect(getWorkerHubSocket(workerFp)).not.toBeNull();
    expect(listRoutableFps(dashboardId)).toContain(workerFp);
    w.close();
  });

  test("upgrade rejected (401) with no token", async () => {
    const res = await handleWorkerWsUpgrade(
      new Request(`http://127.0.0.1:${port}/ws/coord-worker/${workerFp}`),
      { upgrade: () => false } as never, deps,
    );
    expect(res).toBeInstanceOf(Response);
    expect((res as Response).status).toBe(401);
  });

  test("upgrade rejected (401) when URL fp != JWT caller", async () => {
    const otherFp = "0".repeat(64);
    const res = await handleWorkerWsUpgrade(
      new Request(`http://127.0.0.1:${port}/ws/coord-worker/${otherFp}`, {
        headers: {
          "sec-websocket-protocol": `${WORKER_AUTH_SUBPROTOCOL}, ${workerJwt}`,
        },
      }),
      { upgrade: () => false } as never, deps,
    );
    expect((res as Response).status).toBe(401);
  });

  test("upgrade rejects query credentials even with the exact auth protocols", async () => {
    const res = await handleWorkerWsUpgrade(
      new Request(`http://127.0.0.1:${port}/ws/coord-worker/${workerFp}?token=${encodeURIComponent(workerJwt)}`, {
        headers: {
          "sec-websocket-protocol": `${WORKER_AUTH_SUBPROTOCOL}, ${workerJwt}`,
        },
      }),
      { upgrade: () => false } as never,
      deps,
    );
    expect((res as Response).status).toBe(401);
  });

  test("upgrade rejects any protocol list other than exactly marker then JWT", async () => {
    for (const protocol of [
      workerJwt,
      `${workerJwt}, ${WORKER_AUTH_SUBPROTOCOL}`,
      `${WORKER_AUTH_SUBPROTOCOL}, ${workerJwt}, extra`,
    ]) {
      const res = await handleWorkerWsUpgrade(
        new Request(`http://127.0.0.1:${port}/ws/coord-worker/${workerFp}`, {
          headers: { "sec-websocket-protocol": protocol },
        }),
        { upgrade: () => false } as never,
        deps,
      );
      expect((res as Response).status).toBe(401);
    }
  });

  test("non-WS path returns null (caller continues to coord.fetch)", async () => {
    const res = await handleWorkerWsUpgrade(
      new Request(`http://127.0.0.1:${port}/roost.v1.CoordinatorService/SessionsList`),
      { upgrade: () => false } as never, deps,
    );
    expect(res).toBeNull();
  });

  // The regression that motivated the whole swap: a downstream browser-command
  // must reach the worker AND the worker's upstream rpc-ok must resolve coord's
  // pending RPC. Under the old Connect-bidi-on-Bun transport this round-trip
  // never completed (rpc-ok stalled in the buffered h1.1 upstream → spawn hung).
  test("downstream command + upstream rpc-ok resolves the pending RPC", async () => {
    const w = connectWorker(workerFp, workerJwt);
    await w.opened;
    w.sendUp(helloFrame(workerFp));
    await w.waitFor((f) => f.frame.case === "helloAck");
    await readyWorker(w, workerFp);

    // Coord side: create a pending RPC and dispatch a browser-command exactly
    // as router.sessionsSpawn does, via the hub socket.
    const pending = createPendingRpc<{ session_id: string; channel_id: number }>(3000);
    const sock = getWorkerHubSocket(workerFp);
    expect(sock).not.toBeNull();
    sock!.send(JSON.stringify({
      kind: "browser-command", browser_id: "b1", viewer_id: "v1",
      request_id: pending.request_id,
      frame: { kind: "spawn-shell", folder: "/tmp", cols: 80, rows: 24 },
    }));

    // Worker side: receive the command over WS, reply rpc-ok with the result.
    const cmd = await w.waitFor((f) => f.frame.case === "browserCommand");
    expect(cmd.frame.case).toBe("browserCommand");
    const bc = cmd.frame.value as { requestId: string; frameJson: string };
    expect(bc.requestId).toBe(pending.request_id);
    expect(JSON.parse(bc.frameJson)).toMatchObject({ kind: "spawn-shell", folder: "/tmp" });

    w.sendUp(create(CoordWorkerUpSchema, {
      frame: { case: "rpcOk", value: create(WRpcOkSchema, {
        requestId: pending.request_id,
        dataJson: JSON.stringify({ session_id: "sess-1", channel_id: 7 }),
      }) },
    }));

    // Coord side: the upstream rpc-ok must resolve the pending promise.
    const data = await pending.promise;
    expect(data).toEqual({ session_id: "sess-1", channel_id: 7 });
    w.close();
  });
});
