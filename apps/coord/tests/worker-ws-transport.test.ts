// Regression test for the raw-WebSocket coord↔worker transport
// (connect/worker-ws-handler.ts). Guards the failure that forced the swap
// OFF Connect-bidi: under Bun, connect-node can't hold a full-duplex bidi
// stream (no node:http2 for h2; h1.1 buffers the upstream), so the worker's
// rpc-ok reply never reached coord and every sessionsSpawn hung
// ("[internal] internal error").
//
// This drives a REAL Bun client WebSocket through the ACTUAL coord WS
// handler (handleWorkerWsUpgrade + makeWorkerWsHandler) + query-param JWT
// auth + CoordWorkerUp/Down proto framing, and asserts:
//   1. dial + hello → helloAck (auth ok, hello processed, proto both ways)
//   2. missing / fp-mismatched token → 401 (upgrade rejected)
//   3. downstream browser-command delivered AND upstream rpc-ok resolves a
//      pending RPC — the exact spawn round-trip that hung under Connect-bidi.

import { describe, test, expect, beforeAll, afterAll } from "bun:test";
import { existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { create, toBinary, fromBinary } from "@bufbuild/protobuf";
import {
  CoordWorkerUpSchema, CoordWorkerDownSchema, WHelloSchema, WRpcOkSchema, WBinarySchema,
} from "@roost/shared/proto/worker_transport_pb";
import type { CoordWorkerDown } from "@roost/shared/proto/worker_transport_pb";
import { openDb } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { loadOrCreateCoordKey } from "../src/coord-key.ts";
import { newJwtCache, signJwt, fingerprintOf } from "../src/jwt.ts";
import { handleWorkerWsUpgrade, makeWorkerWsHandler } from "../src/connect/worker-ws-handler.ts";
import { getWorkerHubSocket, type WorkerServiceDeps } from "../src/connect/worker-service.ts";
import { createPendingRpc } from "../src/router/pending-rpcs.ts";
import type { CoordConfig } from "@roost/shared/config";

let workdir: string;
let cleanup: () => void;
let server: ReturnType<typeof Bun.serve>;
let port: number;
let workerFp: string;
let workerJwt: string;
let deps: WorkerServiceDeps;

beforeAll(async () => {
  workdir = mkdtempSync(join(tmpdir(), "roost-ws-transport-"));
  const dbPath = join(workdir, "test.db");
  const keyPath = join(workdir, "test.key");
  const authPath = join(workdir, "authorized_keys");
  writeFileSync(authPath, "");

  const opened = openDb(dbPath);
  const db = opened.db;
  const sqlite = opened.sqlite;
  await runMigrations(sqlite);
  const coordKey = await loadOrCreateCoordKey(keyPath);
  const jwtCache = newJwtCache();
  const cfg: CoordConfig = {
    bind: "127.0.0.1:0",
    dbPath, coordKeyPath: keyPath, authorizedKeysPath: authPath,
    webDistPath: "",
    tlsCertPath: undefined, tlsKeyPath: undefined,
    jwtMaxAgeSecs: 300,
    auditRetentionDays: 90,
    relaxedCsp: false,
    corsAllowedOrigins: [],
    logDir: workdir,
    publicUrl: undefined,
    handoffPath: join(workdir, "coord-handoff.json"),
  };
  deps = { db, coordKey, jwtCache, cfg };

  // Mint a worker keypair, authorize it, sign a worker JWT (sub == fp).
  const workerKeys = await crypto.subtle.generateKey({ name: "Ed25519" }, true, ["sign", "verify"]);
  const rawPub = new Uint8Array(await crypto.subtle.exportKey("raw", workerKeys.publicKey));
  workerFp = await fingerprintOf(rawPub);
  await db.insertInto("authorized_keys").values({
    fingerprint: workerFp, public_key: rawPub, label: "test-worker", added_at: Date.now(),
  }).execute();
  const now = Math.floor(Date.now() / 1000);
  workerJwt = await signJwt(
    { aud: "roost-coordinator", sub: workerFp, iat: now, exp: now + 60 },
    workerKeys.privateKey, workerFp,
  );

  // Boot the real WS endpoint exactly as main.ts wires it.
  const wsHandler = makeWorkerWsHandler(deps);
  server = Bun.serve({
    hostname: "127.0.0.1", port: 0,
    async fetch(req, srv) {
      const up = await handleWorkerWsUpgrade(req, srv, deps);
      if (up !== null) return up; // undefined = hijacked, Response = reject
      return new Response("not found", { status: 404 });
    },
    websocket: wsHandler,
  });
  port = server.port!;

  cleanup = () => {
    try { server.stop(true); } catch { /* ignore */ }
    try { sqlite.close(); } catch { /* ignore */ }
    if (existsSync(workdir)) rmSync(workdir, { recursive: true, force: true });
  };
});

afterAll(() => cleanup?.());

// Bun client WebSocket wrapper: decodes downstream proto frames + lets a
// test await a frame matching a predicate.
function connectWorker(fp: string, token: string) {
  const ws = new WebSocket(`ws://127.0.0.1:${port}/ws/coord-worker/${fp}?token=${encodeURIComponent(token)}`);
  ws.binaryType = "arraybuffer";
  const down: CoordWorkerDown[] = [];
  const waiters: Array<{ pred: (f: CoordWorkerDown) => boolean; resolve: (f: CoordWorkerDown) => void }> = [];
  ws.onmessage = (ev) => {
    const f = fromBinary(CoordWorkerDownSchema, new Uint8Array(ev.data as ArrayBuffer));
    down.push(f);
    for (let i = waiters.length - 1; i >= 0; i--) {
      if (waiters[i]!.pred(f)) { waiters[i]!.resolve(f); waiters.splice(i, 1); }
    }
  };
  const opened = new Promise<void>((res, rej) => {
    ws.onopen = () => res();
    ws.onerror = () => rej(new Error("ws error/closed"));
  });
  return {
    ws, opened,
    sendUp(frame: ReturnType<typeof create>) { ws.send(toBinary(CoordWorkerUpSchema, frame as never)); },
    waitFor(pred: (f: CoordWorkerDown) => boolean, ms = 3000): Promise<CoordWorkerDown> {
      const hit = down.find(pred);
      if (hit) return Promise.resolve(hit);
      return new Promise((resolve, reject) => {
        const t = setTimeout(() => reject(new Error("waitFor timeout")), ms);
        waiters.push({ pred, resolve: (f) => { clearTimeout(t); resolve(f); } });
      });
    },
    close() { try { ws.close(); } catch { /* ignore */ } },
  };
}

const helloFrame = (fp: string) => create(CoordWorkerUpSchema, {
  frame: { case: "hello", value: create(WHelloSchema, { workerFp: fp, version: "test" }) },
});

describe("worker↔coord raw-WS transport", () => {
  test("dial + hello → helloAck, worker registered in the hub", async () => {
    const w = connectWorker(workerFp, workerJwt);
    await w.opened;
    w.sendUp(helloFrame(workerFp));
    const ack = await w.waitFor((f) => f.frame.case === "helloAck");
    expect(ack.frame.case).toBe("helloAck");
    // Registry now resolves the fp → downstream commands can route to it.
    expect(getWorkerHubSocket(workerFp)).not.toBeNull();
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
      new Request(`http://127.0.0.1:${port}/ws/coord-worker/${otherFp}?token=${encodeURIComponent(workerJwt)}`),
      { upgrade: () => false } as never, deps,
    );
    expect((res as Response).status).toBe(401);
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

  // Regression: message() MUST copy off Bun's pooled ServerWebSocket buffer.
  // fromBinary returns a subarray VIEW for `bytes` fields (WBinary.data) and
  // handleUpstream runs DEFERRED via ws.data.tail — a view into Bun's recyclable
  // buffer is dereferenced as freed memory after the sync handler returns: the
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
    };
    const fakeWs = { data: { caller: { fingerprint: workerFp }, fp: workerFp, conn: fakeConn, tail: Promise.resolve() } };
    const payload = new Uint8Array([0x1b, 0x5b, 0x41, 0x99]); // ESC [ A + high byte
    const frame = create(CoordWorkerUpSchema, {
      frame: { case: "binary", value: create(WBinarySchema, { channelId: 3, direction: 1, seq: 42n, data: payload }) },
    });
    const buf = Buffer.from(toBinary(CoordWorkerUpSchema, frame));
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    wsHandler.message(fakeWs as any, buf);
    buf.fill(0); // simulate Bun recycling the pooled buffer after the sync handler
    await fakeWs.data.tail;
    expect(seen).toHaveLength(1);
    expect(Array.from(seen[0]!)).toEqual([0x1b, 0x5b, 0x41, 0x99]);
  });
});
