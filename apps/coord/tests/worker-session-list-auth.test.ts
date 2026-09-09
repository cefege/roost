// Session-list authority through the real coordinator fetch stack.
// Proves a signed worker JWT resolves to its persisted worker principal and can
// read only its own open rows, and that a browser device never receives the
// private agent-conversation recovery metadata those rows carry.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fingerprintOf } from "@roost/shared/fingerprint";
import type { CoordConfig } from "@roost/shared/config";
import { AgentConversationReferenceV1Schema } from "@roost/shared/agent-conversation-reference";
import { sql } from "kysely";
import { openDb, type KyselyDB } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { CoordinatorWriteGate } from "../src/coordinator-write-gate.ts";
import { createCoord, type CoordHandle } from "../src/coord-factory.ts";
import { ensureSelfHostedTenant } from "../src/self-hosted-tenant.ts";
import { newJwtCache, signJwt } from "../src/jwt.ts";

const SESSION_ID = "00000000-0000-4000-8000-000000000103";
const NEVER_SET_SESSION_ID = "00000000-0000-4000-8000-000000000104";
const SESSIONS_LIST_PATH = "/roost.v1.CoordinatorService/SessionsList";
const PRIVATE_REFERENCE_VALUE = "/tmp/private worker/'$conversation.json";
const PRIVATE_REFERENCE = AgentConversationReferenceV1Schema.parse({
  schema_version: 1,
  agent_id: "omp",
  kind: "path",
  value: PRIVATE_REFERENCE_VALUE,
});

let workdir: string;
let db: KyselyDB;
let coord: CoordHandle;
let dashboardId: string;
let workerFingerprint: string;
let workerJwt: string;
let deviceJwt: string;
let closeDb: () => Promise<void>;

beforeAll(async () => {
  workdir = mkdtempSync(join(tmpdir(), "roost-worker-session-auth-"));
  const dbPath = join(workdir, "test.db");
  const authorizedKeysPath = join(workdir, "authorized_keys.roost");
  writeFileSync(authorizedKeysPath, "");

  const opened = openDb(dbPath);
  db = opened.db;
  closeDb = opened.close;
  await runMigrations(opened.sqlite);
  const tenant = ensureSelfHostedTenant(opened.sqlite, { backfillLegacyScopes: false });
  dashboardId = tenant.dashboardId;
  const jwtCache = newJwtCache();
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
    jwtCache,
    selfHostedTenant: tenant,
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
  const deviceKeys = await crypto.subtle.generateKey(
    { name: "Ed25519" },
    true,
    ["sign", "verify"],
  );
  const rawDevicePublicKey = new Uint8Array(
    await crypto.subtle.exportKey("raw", deviceKeys.publicKey),
  );
  const deviceFingerprint = await fingerprintOf(rawDevicePublicKey);
  await db.insertInto("authorized_keys").values([
    {
      fingerprint: workerFingerprint,
      public_key: rawPublicKey,
      label: "test worker",
      added_at: now,
    },
    {
      fingerprint: deviceFingerprint,
      public_key: rawDevicePublicKey,
      label: "test device",
      added_at: now,
    },
  ]).execute();
  await db.insertInto("account_devices").values({
    fingerprint: deviceFingerprint,
    account_id: tenant.accountId,
    added_at_ms: now,
    last_seen_at_ms: now,
  }).execute();
  await db.insertInto("workers").values({
    fp: workerFingerprint,
    dashboard_id: dashboardId,
    label: "test worker",
    os: "linux",
    git_sha: null,
    host_metrics_json: null,
    registered_at_ms: now,
    last_seen_ms: now,
    reachable_addr: null,
  }).execute();
  await db.insertInto("sessions").values([
    {
      id: SESSION_ID,
      dashboard_id: dashboardId,
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
      agent_reference_json: JSON.stringify(PRIVATE_REFERENCE),
      agent_reference_client_seq: 17,
    },
    {
      id: NEVER_SET_SESSION_ID,
      dashboard_id: dashboardId,
      worker_fp: workerFingerprint,
      channel: 8,
      kind: "shell",
      cwd: "/tmp/worker-auth-never-set",
      workspace_id: null,
      status: "open",
      agent_json: sql<undefined>`NULL`,
      created_at: now + 1,
      closed_at: null,
      custom_title: null,
      git_branch: null,
      git_remote: null,
      pr_number: null,
      pr_state: null,
      pr_checks: null,
      pr_url: null,
      ports_json: null,
      spawn_cwd: "/tmp/worker-auth-never-set",
      agent_reference_json: null,
      agent_reference_client_seq: null,
    },
  ]).execute();
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
  deviceJwt = await signJwt(
    {
      aud: "roost-coordinator",
      sub: deviceFingerprint,
      iat: nowSeconds,
      exp: nowSeconds + 60,
    },
    deviceKeys.privateKey,
    deviceFingerprint,
  );
});

afterAll(async () => {
  coord?.dispose();
  await closeDb?.();
  if (existsSync(workdir)) rmSync(workdir, { recursive: true, force: true });
});

function sessionsListFetch(jwt: string, body: object): Promise<Response> {
  return coord.fetch(new Request(`http://coord${SESSIONS_LIST_PATH}`, {
    method: "POST",
    headers: {
      authorization: `Bearer ${jwt}`,
      "content-type": "application/json",
    },
    body: JSON.stringify(body),
  }));
}

function workerFetch(body: object): Promise<Response> {
  return sessionsListFetch(workerJwt, body);
}

// A browser device on the sole account: the authority that must never observe
// the worker's private agent-conversation reference.
function deviceFetch(body: object): Promise<Response> {
  return sessionsListFetch(deviceJwt, body);
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
    recoveryMetadata?: Array<{
      sessionId?: string;
      agentReference?: {
        schemaVersion?: number;
        agentId?: string;
        kind?: string;
        value?: string;
      };
      agentReferenceClientSeq?: string;
    }>;
  };
  expect(body.sessions).toHaveLength(2);
  expect(body.sessions).toEqual(expect.arrayContaining([
    expect.objectContaining({
      id: SESSION_ID,
      workerFp: workerFingerprint,
    }),
    expect.objectContaining({
      id: NEVER_SET_SESSION_ID,
      workerFp: workerFingerprint,
    }),
  ]));
  expect(body.syncSnapshotToken).toBeUndefined();
  expect(body.recoveryMetadata).toHaveLength(2);
  expect(body.recoveryMetadata?.find((row) => row.sessionId === SESSION_ID))
    .toEqual({
      sessionId: SESSION_ID,
      agentReference: {
        schemaVersion: 1,
        agentId: "omp",
        kind: "path",
        value: PRIVATE_REFERENCE_VALUE,
      },
      agentReferenceClientSeq: "17",
    });
  expect(body.recoveryMetadata?.find((row) =>
    row.sessionId === NEVER_SET_SESSION_ID
  )).toEqual({ sessionId: NEVER_SET_SESSION_ID });
  expect(JSON.stringify(body.sessions)).not.toContain(PRIVATE_REFERENCE_VALUE);
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

test("a browser device lists the sessions but never their recovery metadata", async () => {
  const response = await deviceFetch({ workerFp: workerFingerprint, status: "open" });
  expect(response.status).toBe(200);
  const raw = await response.text();
  const body = JSON.parse(raw) as {
    sessions?: Array<{ id?: string }>;
    recoveryMetadata?: unknown[];
  };
  expect(body.sessions?.map((session) => session.id)?.sort()).toEqual(
    [SESSION_ID, NEVER_SET_SESSION_ID].sort(),
  );
  expect(body.recoveryMetadata ?? []).toEqual([]);
  // The private conversation reference is worker-recovery state, so it must be
  // absent from the ENTIRE browser response, not merely from `sessions`.
  expect(raw).not.toContain(PRIVATE_REFERENCE_VALUE);
});
