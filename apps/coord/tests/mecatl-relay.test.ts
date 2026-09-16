// Mecatl BFF contract through the real coordinator fetch stack: authority,
// worker existence, the concurrency cap, streaming order, caller cancellation,
// and the pre-head refusal mapping. The worker link is a fake send sink, so no
// worker, keeper, or daemon process participates.

import { afterAll, afterEach, beforeAll, expect, test } from "bun:test";
import { create, type MessageInitShape } from "@bufbuild/protobuf";
import { existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fingerprintOf } from "@roost/shared/fingerprint";
import type { CoordConfig } from "@roost/shared/config";
import {
  WMecatlRelayChunkSchema,
  type CoordWorkerDown,
  type DMecatlRelayRequest,
  type WMecatlRelayChunk,
} from "@roost/shared/proto/worker_transport_pb";
import { openDb, type KyselyDB } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { CoordinatorWriteGate } from "../src/coordinator-write-gate.ts";
import { createCoord, type CoordHandle } from "../src/coord-factory.ts";
import { ensureSelfHostedTenant } from "../src/self-hosted-tenant.ts";
import { newJwtCache, signJwt } from "../src/jwt.ts";
import { __setConnectWorkerForTest } from "../src/connect/worker-registry.ts";
import { deliverMecatlRelayChunk } from "../src/mecatl-relay.ts";

const UNKNOWN_WORKER_FP = "0".repeat(64);
const RELAY_MAX_PER_WORKER = 16;

let workdir: string;
let db: KyselyDB;
let coord: CoordHandle;
let closeDb: () => Promise<void>;
let workerFp: string;
let deletedWorkerFp: string;
let deviceJwt: string;
let workerJwt: string;
/** Every frame the coordinator handed to the fake worker link, plus the
 * waiters released on each send: tests await that signal, never a clock. */
let sentFrames: CoordWorkerDown[] = [];
let linkWaiters: Array<() => void> = [];

beforeAll(async () => {
  workdir = mkdtempSync(join(tmpdir(), "roost-mecatl-relay-"));
  const dbPath = join(workdir, "test.db");
  const authorizedKeysPath = join(workdir, "authorized_keys.roost");
  writeFileSync(authorizedKeysPath, "");

  const opened = openDb(dbPath);
  db = opened.db;
  closeDb = opened.close;
  await runMigrations(opened.sqlite);
  const tenant = ensureSelfHostedTenant(opened.sqlite, { backfillLegacyScopes: false });
  const cfg: CoordConfig = {
    trustProxy: false,
    bind: "127.0.0.1:0",
    pushAllowedOrigins: [],
    dbPath,
    authorizedKeysPath,
    webDistPath: "",
    jwtMaxAgeSecs: 300,
    auditRetentionDays: 90,
    relaxedCsp: false,
    corsAllowedOrigins: [],
    logDir: workdir,
    publicUrl: undefined,
  };
  coord = createCoord({
    db,
    sqlite: opened.sqlite,
    writeGate: new CoordinatorWriteGate(),
    cfg,
    jwtCache: newJwtCache(),
    selfHostedTenant: tenant,
  });

  const workerKeys = await crypto.subtle.generateKey({ name: "Ed25519" }, true, ["sign", "verify"]);
  const deviceKeys = await crypto.subtle.generateKey({ name: "Ed25519" }, true, ["sign", "verify"]);
  const rawWorkerKey = new Uint8Array(await crypto.subtle.exportKey("raw", workerKeys.publicKey));
  const rawDeviceKey = new Uint8Array(await crypto.subtle.exportKey("raw", deviceKeys.publicKey));
  workerFp = await fingerprintOf(rawWorkerKey);
  const deviceFp = await fingerprintOf(rawDeviceKey);
  deletedWorkerFp = await fingerprintOf(new Uint8Array(32).fill(7));
  const now = Date.now();

  await db.insertInto("authorized_keys").values([
    { fingerprint: workerFp, public_key: rawWorkerKey, label: "test worker", added_at: now },
    { fingerprint: deviceFp, public_key: rawDeviceKey, label: "test device", added_at: now },
  ]).execute();
  await db.insertInto("account_devices").values({
    fingerprint: deviceFp,
    account_id: tenant.accountId,
    added_at_ms: now,
    last_seen_at_ms: now,
  }).execute();
  await db.insertInto("workers").values([
    {
      fp: workerFp,
      dashboard_id: tenant.dashboardId,
      label: "test worker",
      os: "linux",
      git_sha: null,
      host_metrics_json: null,
      registered_at_ms: now,
      last_seen_ms: now,
      reachable_addr: null,
    },
    {
      fp: deletedWorkerFp,
      dashboard_id: tenant.dashboardId,
      label: "retired worker",
      os: "linux",
      git_sha: null,
      host_metrics_json: null,
      registered_at_ms: now,
      last_seen_ms: now,
      reachable_addr: null,
      deleted_at_ms: now,
    },
  ]).execute();

  const issuedAt = Math.floor(now / 1_000);
  deviceJwt = await signJwt(
    { aud: "roost-coordinator", sub: deviceFp, iat: issuedAt, exp: issuedAt + 60 },
    deviceKeys.privateKey,
    deviceFp,
  );
  workerJwt = await signJwt(
    { aud: "roost-coordinator", sub: workerFp, iat: issuedAt, exp: issuedAt + 60 },
    workerKeys.privateKey,
    workerFp,
  );
});

afterEach(() => {
  __setConnectWorkerForTest(workerFp, null);
});

afterAll(async () => {
  coord?.dispose();
  await closeDb?.();
  if (existsSync(workdir)) rmSync(workdir, { recursive: true, force: true });
});

/** Installs the fake worker link and starts a fresh frame capture. */
function attachFakeWorker(): void {
  sentFrames = [];
  linkWaiters = [];
  __setConnectWorkerForTest(workerFp, {
    workerFp,
    send: (frame: CoordWorkerDown) => {
      sentFrames.push(frame);
      for (const release of linkWaiters.splice(0)) release();
      return 1;
    },
  });
}

function relayFetch(
  jwt: string,
  path: string,
  init: { signal?: AbortSignal } = {},
): Promise<Response> {
  return coord.fetch(
    new Request(`http://coord${path}`, {
      method: "GET",
      headers: { authorization: `Bearer ${jwt}`, accept: "text/event-stream" },
      signal: init.signal,
    }),
  );
}

function relayRequests(): DMecatlRelayRequest[] {
  const requests: DMecatlRelayRequest[] = [];
  for (const frame of sentFrames) {
    if (frame.frame.case === "mecatlRelayRequest") requests.push(frame.frame.value);
  }
  return requests;
}

function cancelledRequestIds(): string[] {
  const ids: string[] = [];
  for (const frame of sentFrames) {
    if (frame.frame.case === "mecatlRelayCancel") ids.push(frame.frame.value.requestId);
  }
  return ids;
}

/** Awaits link activity rather than a duration: every relay frame the
 * coordinator emits releases the pending waiters. */
async function waitForLink(isSatisfied: () => boolean): Promise<void> {
  while (!isSatisfied()) {
    await new Promise<void>((resolve) => {
      linkWaiters.push(resolve);
    });
  }
}

async function waitForRelayRequests(count: number): Promise<DMecatlRelayRequest[]> {
  await waitForLink(() => relayRequests().length >= count);
  return relayRequests();
}

function chunk(
  fields: MessageInitShape<typeof WMecatlRelayChunkSchema>,
): WMecatlRelayChunk {
  return create(WMecatlRelayChunkSchema, fields);
}

test("a device JWT streams the head and body chunks to the caller in order", async () => {
  attachFakeWorker();
  const pending = relayFetch(deviceJwt, `/api/mecatl/${workerFp}/v1/sessions?limit=2`);
  const [request] = await waitForRelayRequests(1);
  const requestId = request!.requestId;

  expect(request!.method).toBe("GET");
  expect(request!.path).toBe("/v1/sessions?limit=2");
  // Only content-type and accept survive the hop: the Roost JWT must not.
  expect(JSON.parse(request!.headersJson)).toEqual({ accept: "text/event-stream" });

  deliverMecatlRelayChunk(workerFp, chunk({
    requestId,
    head: true,
    status: 200,
    headersJson: JSON.stringify({ "content-type": "text/event-stream", "x-secret": "no" }),
  }));
  const response = await pending;
  expect(response.status).toBe(200);
  expect(response.headers.get("content-type")).toBe("text/event-stream");
  expect(response.headers.get("x-secret")).toBeNull();

  const reader = response.body!.getReader();
  deliverMecatlRelayChunk(workerFp, chunk({
    requestId,
    body: new TextEncoder().encode("event: one\n"),
  }));
  deliverMecatlRelayChunk(workerFp, chunk({
    requestId,
    body: new TextEncoder().encode("event: two\n"),
  }));
  deliverMecatlRelayChunk(workerFp, chunk({ requestId, end: true }));

  const decoder = new TextDecoder();
  let streamed = "";
  for (;;) {
    const next = await reader.read();
    if (next.done) break;
    streamed += decoder.decode(next.value);
  }
  expect(streamed).toBe("event: one\nevent: two\n");
});

test("a worker-principal JWT carries no relay authority", async () => {
  attachFakeWorker();
  const response = await relayFetch(workerJwt, `/api/mecatl/${workerFp}/v1/sessions`);
  expect(response.status).toBe(401);
  expect(await response.json()).toEqual({ error: "unauthorized" });
  expect(relayRequests()).toEqual([]);
});

test("unknown and soft-deleted worker fingerprints are not relay targets", async () => {
  attachFakeWorker();
  const unknown = await relayFetch(deviceJwt, `/api/mecatl/${UNKNOWN_WORKER_FP}/v1/sessions`);
  expect(unknown.status).toBe(404);
  expect(await unknown.json()).toEqual({ error: "unknown_worker" });

  const deleted = await relayFetch(deviceJwt, `/api/mecatl/${deletedWorkerFp}/v1/sessions`);
  expect(deleted.status).toBe(404);
  expect(relayRequests()).toEqual([]);
});

test("a worker with no live link answers worker_offline", async () => {
  sentFrames = [];
  const response = await relayFetch(deviceJwt, `/api/mecatl/${workerFp}/v1/sessions`);
  expect(response.status).toBe(503);
  expect(await response.json()).toEqual({ error: "worker_offline" });
});

test("relays past the per-worker concurrency cap are refused as relay_busy", async () => {
  attachFakeWorker();
  const inFlight: Promise<Response>[] = [];
  for (let idx = 0; idx < RELAY_MAX_PER_WORKER; idx += 1) {
    inFlight.push(relayFetch(deviceJwt, `/api/mecatl/${workerFp}/v1/sessions/${idx}/events`));
  }
  const admitted = await waitForRelayRequests(RELAY_MAX_PER_WORKER);

  const refused = await relayFetch(deviceJwt, `/api/mecatl/${workerFp}/v1/sessions/cap/events`);
  expect(refused.status).toBe(429);
  expect(await refused.json()).toEqual({ error: "relay_busy" });
  expect(relayRequests()).toHaveLength(RELAY_MAX_PER_WORKER);

  for (const request of admitted) {
    deliverMecatlRelayChunk(workerFp, chunk({
      requestId: request.requestId,
      end: true,
      error: "daemon_exit",
    }));
  }
  const settled = await Promise.all(inFlight);
  expect(settled.map((response) => response.status))
    .toEqual(new Array(RELAY_MAX_PER_WORKER).fill(502));

  // Ending those relays released their slots, so the next caller is admitted.
  const reopened = relayFetch(deviceJwt, `/api/mecatl/${workerFp}/v1/sessions/next/events`);
  const requests = await waitForRelayRequests(RELAY_MAX_PER_WORKER + 1);
  deliverMecatlRelayChunk(workerFp, chunk({
    requestId: requests.at(-1)!.requestId,
    end: true,
    error: "daemon_exit",
  }));
  expect((await reopened).status).toBe(502);
});

test("the caller aborting sends a cancel frame to the worker", async () => {
  attachFakeWorker();
  const aborter = new AbortController();
  const pending = relayFetch(
    deviceJwt,
    `/api/mecatl/${workerFp}/v1/sessions/abc/events`,
    { signal: aborter.signal },
  );
  const [request] = await waitForRelayRequests(1);
  deliverMecatlRelayChunk(workerFp, chunk({
    requestId: request!.requestId,
    head: true,
    status: 200,
    headersJson: JSON.stringify({ "content-type": "text/event-stream" }),
  }));
  const response = await pending;
  expect(response.status).toBe(200);

  aborter.abort();
  await waitForLink(() => cancelledRequestIds().length > 0);
  expect(cancelledRequestIds()).toEqual([request!.requestId]);
  // An abandoned relay must fail the body, not look like a complete short one.
  await expect(response.body!.getReader().read()).rejects.toThrow(/mecatl relay/);
});

test("a refusal before the head becomes a 502 carrying the wire reason", async () => {
  attachFakeWorker();
  const pending = relayFetch(deviceJwt, `/api/mecatl/${workerFp}/v1/sessions`);
  const [request] = await waitForRelayRequests(1);
  deliverMecatlRelayChunk(workerFp, chunk({
    requestId: request!.requestId,
    end: true,
    error: "binary_missing",
  }));

  const response = await pending;
  expect(response.status).toBe(502);
  expect(await response.json()).toEqual({ error: "binary_missing" });
  // A refusal is terminal: the worker is told nothing further.
  expect(cancelledRequestIds()).toEqual([]);
});

test("a stalled reader past the unread ceiling loses the relay", async () => {
  attachFakeWorker();
  const pending = relayFetch(deviceJwt, `/api/mecatl/${workerFp}/v1/sessions/stalled/events`);
  const [request] = await waitForRelayRequests(1);
  deliverMecatlRelayChunk(workerFp, chunk({
    requestId: request!.requestId,
    head: true,
    status: 200,
    headersJson: JSON.stringify({ "content-type": "text/event-stream" }),
  }));
  const response = await pending;
  expect(response.status).toBe(200);

  // Nothing reads the body, so the coordinator holds every byte: 512 KiB of
  // unread bytes must end the relay instead of growing without bound.
  const block = new Uint8Array(32 * 1024);
  for (let queued = 0; queued < 512 * 1024; queued += block.length) {
    deliverMecatlRelayChunk(workerFp, chunk({ requestId: request!.requestId, body: block }));
  }
  expect(cancelledRequestIds()).toEqual([request!.requestId]);
  await expect(response.body!.getReader().read()).rejects.toThrow(/unread_ceiling/);
});
