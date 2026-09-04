// Worker session-list authentication through the real coordinator fetch stack.
// Proves a signed worker JWT resolves to its persisted dashboard principal and
// can read only that worker's open rows during boot reconciliation.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fingerprintOf } from "@roost/shared/fingerprint";
import type { CoordConfig } from "@roost/shared/config";
import { sql } from "kysely";
import { openDb, type KyselyDB } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { loadOrCreateCoordKey } from "../src/coord-key.ts";
import { createCoord, type CoordHandle } from "../src/coord-factory.ts";
import { newJwtCache, signJwt } from "../src/jwt.ts";
import { PasswordWorkGate } from "../src/connect/password-work-gate.ts";

const ORGANIZATION_ID = "00000000-0000-4000-8000-000000000101";
const DASHBOARD_ID = "00000000-0000-4000-8000-000000000102";
const SESSION_ID = "00000000-0000-4000-8000-000000000103";
const SESSIONS_LIST_PATH = "/roost.v1.CoordinatorService/SessionsList";

let workdir: string;
let db: KyselyDB;
let coord: CoordHandle;
let workerFingerprint: string;
let workerJwt: string;
let closeDb: () => Promise<void>;

beforeAll(async () => {
  workdir = mkdtempSync(join(tmpdir(), "roost-worker-session-auth-"));
  const dbPath = join(workdir, "test.db");
  const keyPath = join(workdir, "coord.key");
  const authorizedKeysPath = join(workdir, "authorized_keys.roost");
  writeFileSync(authorizedKeysPath, "");

  const opened = openDb(dbPath);
  db = opened.db;
  closeDb = opened.close;
  await runMigrations(opened.sqlite);
  const coordKey = await loadOrCreateCoordKey(keyPath);
  const jwtCache = newJwtCache();
  const cfg: CoordConfig = {
    trustProxy: false,
    bind: "127.0.0.1:0",
    saasMode: true,
    managedContainer: true,
    instanceId: DASHBOARD_ID,
    pushAllowedOrigins: [],
    dbPath,
    coordKeyPath: keyPath,
    authorizedKeysPath,
    webDistPath: "",
    tlsCertPath: undefined,
    tlsKeyPath: undefined,
    jwtMaxAgeSecs: 300,
    auditRetentionDays: 90,
    relaxedCsp: false,
    corsAllowedOrigins: [],
    logDir: workdir,
    publicUrl: undefined,
    handoffPath: join(workdir, "coord-handoff.json"),
  };
  coord = createCoord({
    db,
    sqlite: opened.sqlite,
    coordKey,
    cfg,
    jwtCache,
    passwordWorkGate: new PasswordWorkGate(),
  });

  const workerKeys = await crypto.subtle.generateKey(
    { name: "Ed25519" },
    true,
    ["sign", "verify"],
  );
  const rawPublicKey = new Uint8Array(
    await crypto.subtle.exportKey("raw", workerKeys.publicKey),
  );
  workerFingerprint = await fingerprintOf(rawPublicKey);
  const now = Date.now();
  await db.insertInto("organizations").values({
    id: ORGANIZATION_ID,
    slug: "worker-auth",
    name: "Worker auth",
    status: "active",
    created_at_ms: now,
  }).execute();
  await db.insertInto("dashboards").values({
    id: DASHBOARD_ID,
    organization_id: ORGANIZATION_ID,
    slug: "worker-auth",
    name: "Worker auth",
    status: "active",
    created_at_ms: now,
  }).execute();
  await db.insertInto("authorized_keys").values({
    fingerprint: workerFingerprint,
    public_key: rawPublicKey,
    label: "test worker",
    added_at: now,
  }).execute();
  await db.insertInto("workers").values({
    fp: workerFingerprint,
    dashboard_id: DASHBOARD_ID,
    label: "test worker",
    os: "linux",
    git_sha: null,
    host_metrics_json: null,
    registered_at_ms: now,
    last_seen_ms: now,
    reachable_addr: null,
    keeper_stale: null,
  }).execute();
  await db.insertInto("sessions").values({
    id: SESSION_ID,
    dashboard_id: DASHBOARD_ID,
    worker_fp: workerFingerprint,
    channel: 7,
    kind: "shell",
    cwd: "/tmp/worker-auth",
    workspace_id: null,
    status: "open",
    agent_json: sql<undefined>`NULL`,
    created_at: now,
    closed_at: null,
    custom_title: null,
    git_branch: null,
    git_remote: null,
    pr_number: null,
    pr_state: null,
    pr_checks: null,
    pr_url: null,
    ports_json: null,
    spawn_cwd: "/tmp/worker-auth",
  }).execute();
  const nowSeconds = Math.floor(now / 1_000);
  workerJwt = await signJwt(
    {
      aud: "roost-coordinator",
      sub: workerFingerprint,
      iat: nowSeconds,
      exp: nowSeconds + 60,
    },
    workerKeys.privateKey,
    workerFingerprint,
  );
});

afterAll(async () => {
  coord?.dispose();
  await closeDb?.();
  if (existsSync(workdir)) rmSync(workdir, { recursive: true, force: true });
});

function workerFetch(body: object): Promise<Response> {
  return coord.fetch(new Request(`http://coord${SESSIONS_LIST_PATH}`, {
    method: "POST",
    headers: {
      authorization: `Bearer ${workerJwt}`,
      "content-type": "application/json",
    },
    body: JSON.stringify(body),
  }));
}

test("worker JWT lists only its own open sessions through coord.fetch", async () => {
  const response = await workerFetch({
    workerFp: workerFingerprint,
    status: "open",
  });
  expect(response.status).toBe(200);
  const body = await response.json() as {
    sessions?: Array<{ id?: string; workerFp?: string }>;
    syncSnapshotToken?: string;
  };
  expect(body.sessions).toEqual([expect.objectContaining({
    id: SESSION_ID,
    workerFp: workerFingerprint,
  })]);
  expect(body.syncSnapshotToken).toBeUndefined();
});

test("worker JWT cannot broaden its session-list scope", async () => {
  for (const body of [
    { status: "open" },
    { workerFp: workerFingerprint, status: "all" },
    { workerFp: workerFingerprint, status: "" },
    { workerFp: workerFingerprint, status: "open", syncSocketId: "browser" },
    { workerFp: workerFingerprint, status: "open", syncSocketId: "" },
  ]) {
    const response = await workerFetch(body);
    expect(response.status).toBe(403);
  }
});
