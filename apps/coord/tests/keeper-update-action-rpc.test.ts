// Exercises the authenticated coordinator-to-worker keeper update RPC.
// The production handler owns the write gate, database session snapshot, and
// downstream frame; a routable worker fixture controls only the final reply.

import { afterEach, expect, test } from "bun:test";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { CoordConfig } from "@roost/shared/config";
import { fingerprintOf } from "@roost/shared/fingerprint";
import type { JournaledKeeperUpdateV1 } from "@roost/shared/keeper-update";
import type {
  CoordWorkerDown,
  DKeeperUpdatePrepare,
} from "@roost/shared/proto/worker_transport_pb";
import { X_ROOST_DASHBOARD_ID } from "@roost/shared/wire/headers";
import { createCoord } from "../src/coord-factory.ts";
import { CoordinatorWriteGate } from "../src/coordinator-write-gate.ts";
import { openDb } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { newJwtCache, signJwt } from "../src/jwt.ts";
import { __setConnectWorkerForTest } from "../src/connect/worker-registry.ts";
import {
  rejectPendingRpcsForWorker,
  resolvePendingRpc,
} from "../src/router/pending-rpcs.ts";

const WORKER_FP = "ab".repeat(32);
const ACCOUNT_ID = "keeper-action-account";
const ORGANIZATION_ID = "keeper-action-organization";
const DASHBOARD_ID = "keeper-action-dashboard";
const EARLIER_SESSION_ID = "10000000-0000-4000-8000-000000000001";
const LATER_SESSION_ID = "10000000-0000-4000-8000-000000000002";
const IMPLEMENTATION_DIGEST = "1".repeat(64);
const PRESERVE_UPDATE = {
  admission: {
    classification: "worker-only-safe",
    source_contract_digest: IMPLEMENTATION_DIGEST,
    target_contract_digest: IMPLEMENTATION_DIGEST,
    expected_keeper_pid: 1234,
    expected_keeper_epoch: "20000000-0000-4000-8000-000000000001",
    expected_binding_digest: "2".repeat(64),
    required_action: "preserve",
  },
  source_contract: {
    protocol_version: 1,
    supported_features: ["keeper-contract-v1"],
    required_features: ["keeper-contract-v1"],
    implementation_digest: IMPLEMENTATION_DIGEST,
    bun_abi: "test",
    platform: "linux",
    arch: "x64",
    build_sha: "a".repeat(40),
  },
  target_contract: {
    protocol_version: 1,
    supported_features: ["keeper-contract-v1"],
    required_features: ["keeper-contract-v1"],
    implementation_digest: IMPLEMENTATION_DIGEST,
    bun_abi: "test",
    platform: "linux",
    arch: "x64",
    build_sha: "b".repeat(40),
  },
} as const satisfies JournaledKeeperUpdateV1;
const REPLACE_UPDATE = {
  ...PRESERVE_UPDATE,
  admission: {
    ...PRESERVE_UPDATE.admission,
    classification: "keeper-restart-required",
    target_contract_digest: "3".repeat(64),
    expected_binding_digest:
      "74eb8cfffb89f155db2201d8c1b13202c29d91be6cc3d4fec6b465c9a9ede627",
    required_action: "replace-empty",
  },
  target_contract: {
    ...PRESERVE_UPDATE.target_contract,
    implementation_digest: "3".repeat(64),
  },
} as const satisfies JournaledKeeperUpdateV1;

interface RpcHarness {
  request: (update: JournaledKeeperUpdateV1) => Promise<Response>;
  dispatched: Promise<void>;
  frame: () => DKeeperUpdatePrepare | null;
  dispatchCount: () => number;
  close: () => Promise<void>;
}

const activeHarnesses: RpcHarness[] = [];
afterEach(async () => {
  await Promise.all(activeHarnesses.splice(0).map(harness => harness.close()));
});

async function openHarness(
  sessionIds: readonly string[],
): Promise<RpcHarness> {
  const directory = mkdtempSync(join(tmpdir(), "roost-keeper-action-rpc-"));
  const opened = openDb(join(directory, "coord.db"));
  await runMigrations(opened.sqlite);
  const browserKeys = await crypto.subtle.generateKey(
    { name: "Ed25519" }, true, ["sign", "verify"],
  );
  const publicKey = new Uint8Array(
    await crypto.subtle.exportKey("raw", browserKeys.publicKey),
  );
  const browserFp = await fingerprintOf(publicKey);
  const now = Date.now();
  await opened.db.insertInto("authorized_keys").values({
    fingerprint: browserFp,
    public_key: publicKey,
    label: "keeper-action-browser",
    added_at: now,
  }).execute();
  await opened.db.insertInto("accounts").values({
    id: ACCOUNT_ID,
    email_normalized: "keeper-action@example.test",
    status: "active",
    created_at_ms: now,
  }).execute();
  await opened.db.insertInto("account_devices").values({
    fingerprint: browserFp,
    account_id: ACCOUNT_ID,
    added_at_ms: now,
    last_seen_at_ms: now,
  }).execute();
  await opened.db.insertInto("organizations").values({
    id: ORGANIZATION_ID,
    slug: "keeper-action",
    name: "Keeper action",
    status: "active",
    created_at_ms: now,
  }).execute();
  await opened.db.insertInto("organization_memberships").values({
    organization_id: ORGANIZATION_ID,
    account_id: ACCOUNT_ID,
    role: "owner",
    created_at_ms: now,
  }).execute();
  await opened.db.insertInto("dashboards").values({
    id: DASHBOARD_ID,
    organization_id: ORGANIZATION_ID,
    slug: "keeper-action",
    name: "Keeper action",
    status: "active",
    created_at_ms: now,
  }).execute();
  await opened.db.insertInto("dashboard_memberships").values({
    dashboard_id: DASHBOARD_ID,
    account_id: ACCOUNT_ID,
    role: "admin",
    created_at_ms: now,
  }).execute();
  await opened.db.insertInto("workers").values({
    fp: WORKER_FP,
    dashboard_id: DASHBOARD_ID,
    label: "keeper-action-worker",
    os: "linux",
    reachable_addr: "127.0.0.1",
    git_sha: null,
    host_metrics_json: null,
    registered_at_ms: now,
    last_seen_ms: now,
  }).execute();
  for (const [index, sessionId] of sessionIds.entries()) {
    await opened.db.insertInto("sessions").values({
      id: sessionId,
      dashboard_id: DASHBOARD_ID,
      worker_fp: WORKER_FP,
      channel: index + 1,
      kind: "shell",
      cwd: "/tmp/keeper-action",
      status: "open",
      created_at: now,
    }).execute();
  }
  const cfg: CoordConfig = {
    trustProxy: false,
    bind: "127.0.0.1:0",
    pushAllowedOrigins: [],
    dbPath: join(directory, "coord.db"),
    authorizedKeysPath: join(directory, "keys"),
    webDistPath: "",
    jwtMaxAgeSecs: 300,
    auditRetentionDays: 90,
    relaxedCsp: false,
    corsAllowedOrigins: [],
    logDir: directory,
    publicUrl: undefined,
  };
  const coord = createCoord({
    db: opened.db,
    sqlite: opened.sqlite,
    cfg,
    jwtCache: newJwtCache(),
    writeGate: new CoordinatorWriteGate(),
  });
  const issuedAt = Math.floor(now / 1_000);
  const jwt = await signJwt({
    aud: "roost-coordinator",
    sub: browserFp,
    iat: issuedAt,
    exp: issuedAt + 60,
  }, browserKeys.privateKey, browserFp);
  const dispatched = Promise.withResolvers<void>();
  let keeperFrame: DKeeperUpdatePrepare | null = null;
  let closed = false;
  let dispatchCount = 0;
  __setConnectWorkerForTest(WORKER_FP, {
    workerFp: WORKER_FP,
    dashboardId: DASHBOARD_ID,
    send(frame: CoordWorkerDown): number {
      if (frame.frame.case === "keeperUpdatePrepare") {
        keeperFrame = frame.frame.value;
        dispatchCount += 1;
        dispatched.resolve();
      }
      return 1;
    },
  });
  const harness: RpcHarness = {
    request: update => coord.fetch(new Request(
      "http://test/roost.v1.CoordinatorService/WorkersPrepareKeeperUpdate",
      {
        method: "POST",
        headers: {
          "content-type": "application/json",
          authorization: `Bearer ${jwt}`,
          [X_ROOST_DASHBOARD_ID]: DASHBOARD_ID,
        },
        body: JSON.stringify({
          workerFp: WORKER_FP,
          journaledUpdateJson: JSON.stringify(update),
          direction: "target",
          maintenance: false,
        }),
      },
    )),
    dispatched: dispatched.promise,
    frame: () => keeperFrame,
    dispatchCount: () => dispatchCount,
    close: async () => {
      if (closed) return;
      closed = true;
      __setConnectWorkerForTest(WORKER_FP, null);
      rejectPendingRpcsForWorker(WORKER_FP, "keeper action RPC fixture closed");
      coord.dispose();
      await opened.close();
      if (existsSync(directory)) rmSync(directory, { recursive: true, force: true });
    },
  };
  activeHarnesses.push(harness);
  return harness;
}

test("preserve forwards the full envelope and exact sorted nonempty session IDs", async () => {
  const harness = await openHarness([LATER_SESSION_ID, EARLIER_SESSION_ID]);
  const responsePromise = harness.request(PRESERVE_UPDATE);
  await harness.dispatched;
  const frame = harness.frame();
  expect(frame?.coordinatorOpenSessionIds).toEqual([
    EARLIER_SESSION_ID,
    LATER_SESSION_ID,
  ]);
  expect(JSON.parse(frame?.journaledUpdateJson ?? "null")).toEqual(PRESERVE_UPDATE);
  expect(frame?.direction).toBe("target");
  expect(frame?.maintenance).toBe(false);
  expect(resolvePendingRpc(frame!.requestId, {
    outcome: "preserved",
    keeper_pid: PRESERVE_UPDATE.admission.expected_keeper_pid,
    keeper_epoch: PRESERVE_UPDATE.admission.expected_keeper_epoch,
    binding_digest: PRESERVE_UPDATE.admission.expected_binding_digest,
  }, WORKER_FP)).toBe(true);
  const response = await responsePromise;
  expect(response.status).toBe(200);
  expect(await response.json()).toMatchObject({ outcome: "preserved" });
});

test("replace-empty rejects live sessions and forwards an exact empty proof", async () => {
  const blocked = await openHarness([EARLIER_SESSION_ID]);
  const blockedResponse = await blocked.request(REPLACE_UPDATE);
  expect(blockedResponse.status).toBe(400);
  expect(await blockedResponse.json()).toEqual({
    code: "failed_precondition",
    message: "keeper replacement blocked by live sessions",
  });
  expect(blocked.dispatchCount()).toBe(0);

  await blocked.close();
  const empty = await openHarness([]);
  const responsePromise = empty.request(REPLACE_UPDATE);
  await empty.dispatched;
  const frame = empty.frame();
  expect(frame?.coordinatorOpenSessionIds).toEqual([]);
  expect(resolvePendingRpc(frame!.requestId, { outcome: "shutdown" }, WORKER_FP)).toBe(true);
  const response = await responsePromise;
  expect(response.status).toBe(200);
  expect(await response.json()).toMatchObject({ outcome: "shutdown" });
});

test("rejects a worker outcome that belongs to a different recorded action", async () => {
  const harness = await openHarness([EARLIER_SESSION_ID]);
  const responsePromise = harness.request(PRESERVE_UPDATE);
  await harness.dispatched;
  const frame = harness.frame();
  expect(resolvePendingRpc(frame!.requestId, { outcome: "shutdown" }, WORKER_FP)).toBe(true);
  const response = await responsePromise;
  expect(response.status).toBe(500);
});