// Workspace rows are keyed by (worker_fp, folder) and must dedupe on PATH
// IDENTITY, not string equality. On darwin `/tmp` IS `/private/tmp`: a session
// reports the realpath (worker canonicalSessionCwd) while a row written earlier,
// or created from a path the user typed, holds the unresolved form. Raw equality
// created a second workspace for one directory and the SPA then matched neither
// row (folderKey.ts resolves by identity), losing the folder's name and colour.
// Drives the REAL makeWorkspaceHandlers over an in-memory DB, same direct-handler
// pattern as ui-handlers.test.ts.

import { describe, test, expect, beforeEach, afterEach } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { create } from "@bufbuild/protobuf";
import { createContextValues, type HandlerContext } from "@connectrpc/connect";
import { WorkspacesCreateRequestSchema } from "@roost/shared/proto/coordinator_pb";
import { openDb, type DbHandle } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import { makeWorkspaceHandlers, type WorkspaceHandlers } from "../src/connect/handlers-workspaces.ts";
import {
  callerKey,
  dashboardActorKey,
  type DashboardActor,
} from "../src/connect/auth-interceptor.ts";

const ACCOUNT_ID = "workspace-identity-account";
const ORGANIZATION_ID = "workspace-identity-organization";
const DASHBOARD_ID = "workspace-identity-dashboard";
const DEVICE_FP = "workspace-identity-device";
const DARWIN_FP = "fp-darwin";
const LINUX_FP = "fp-linux";
const UNKNOWN_OS_FP = "fp-unknown-os";

const actor: DashboardActor = {
  accountId: ACCOUNT_ID,
  organizationId: ORGANIZATION_ID,
  dashboardId: DASHBOARD_ID,
  organizationRole: "owner",
  dashboardRole: "admin",
  deviceFingerprint: DEVICE_FP,
};

function actorContext(): HandlerContext {
  const values = createContextValues();
  values.set(callerKey, {
    kind: "account-device",
    fingerprint: DEVICE_FP,
    label: "workspace identity test device",
    accountId: ACCOUNT_ID,
  });
  values.set(dashboardActorKey, actor);
  return { values } as unknown as HandlerContext;
}

const authCtx = actorContext();

let workdir: string;
let opened: DbHandle;
let handlers: WorkspaceHandlers;

async function registerWorker(fp: string, os: string): Promise<void> {
  await opened.db.insertInto("workers").values({
    dashboard_id: DASHBOARD_ID,
    fp, label: fp, os, git_sha: null, host_metrics_json: null,
    registered_at_ms: Date.now(), last_seen_ms: Date.now(), reachable_addr: null,
    keeper_stale: null,
  }).execute();
}

function createReq(workerFp: string, name: string, folderPath: string) {
  return create(WorkspacesCreateRequestSchema, { workerFp, name, folderPath, attachSessionIds: [] });
}

beforeEach(async () => {
  workdir = mkdtempSync(join(tmpdir(), "roost-ws-identity-"));
  opened = openDb(join(workdir, "test.db"));
  await runMigrations(opened.sqlite);
  const now = Date.now();
  await opened.db.insertInto("authorized_keys").values({
    fingerprint: DEVICE_FP,
    public_key: new Uint8Array(32),
    label: "workspace identity test device",
    added_at: now,
  }).execute();
  await opened.db.insertInto("accounts").values({
    id: ACCOUNT_ID,
    email_normalized: "workspace-identity@example.test",
    password_hash: null,
    status: "active",
    created_at_ms: now,
    password_changed_at_ms: null,
  }).execute();
  await opened.db.insertInto("account_devices").values({
    fingerprint: DEVICE_FP,
    account_id: ACCOUNT_ID,
    added_at_ms: now,
    last_seen_at_ms: now,
  }).execute();
  await opened.db.insertInto("organizations").values({
    id: ORGANIZATION_ID,
    slug: "workspace-identity",
    name: "Workspace Identity",
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
    slug: "workspace-identity",
    name: "Workspace Identity",
    status: "active",
    created_at_ms: now,
  }).execute();
  await opened.db.insertInto("dashboard_memberships").values({
    dashboard_id: DASHBOARD_ID,
    account_id: ACCOUNT_ID,
    role: "admin",
    created_at_ms: now,
  }).execute();
  await registerWorker(DARWIN_FP, "darwin");
  await registerWorker(LINUX_FP, "linux");
  await registerWorker(UNKNOWN_OS_FP, "unknown");
  handlers = makeWorkspaceHandlers({ db: opened.db } as unknown as ConnectDeps);
});

afterEach(async () => {
  try {
    await opened.close();
  } finally {
    rmSync(workdir, { recursive: true, force: true });
  }
});

describe("workspace folder identity", () => {
  test("darwin /tmp and /private/tmp are one workspace", async () => {
    const first = await handlers.workspacesCreate(createReq(DARWIN_FP, "Scratch", "/tmp/proj"), authCtx);
    const second = await handlers.workspacesCreate(createReq(DARWIN_FP, "Scratch again", "/private/tmp/proj"), authCtx);
    // Same row returned, so the original name survives and no duplicate exists.
    expect(second.workspace?.id).toBe(first.workspace?.id ?? "");
    expect(second.workspace?.name).toBe("Scratch");
    const rows = await opened.db.selectFrom("workspaces").selectAll()
      .where("worker_fp", "=", DARWIN_FP).execute();
    expect(rows).toHaveLength(1);
  });

  test("the fold is exact: a real /private sibling stays its own workspace", async () => {
    const priv = await handlers.workspacesCreate(createReq(DARWIN_FP, "Private", "/private/other"), authCtx);
    const root = await handlers.workspacesCreate(createReq(DARWIN_FP, "Other", "/other"), authCtx);
    expect(root.workspace?.id).not.toBe(priv.workspace?.id ?? "");
    expect(await opened.db.selectFrom("workspaces").selectAll()
      .where("worker_fp", "=", DARWIN_FP).execute()).toHaveLength(2);
  });

  test("linux keeps /tmp and /private/tmp distinct — no symlink there", async () => {
    const plain = await handlers.workspacesCreate(createReq(LINUX_FP, "Plain", "/tmp/proj"), authCtx);
    const priv = await handlers.workspacesCreate(createReq(LINUX_FP, "Private", "/private/tmp/proj"), authCtx);
    expect(priv.workspace?.id).not.toBe(plain.workspace?.id ?? "");
    expect(await opened.db.selectFrom("workspaces").selectAll()
      .where("worker_fp", "=", LINUX_FP).execute()).toHaveLength(2);
  });

  test("an unknown worker OS falls back to exact equality, never a false merge", async () => {
    const first = await handlers.workspacesCreate(createReq(UNKNOWN_OS_FP, "A", "/tmp/proj"), authCtx);
    const second = await handlers.workspacesCreate(createReq(UNKNOWN_OS_FP, "B", "/private/tmp/proj"), authCtx);
    expect(second.workspace?.id).not.toBe(first.workspace?.id ?? "");
    const again = await handlers.workspacesCreate(createReq(UNKNOWN_OS_FP, "C", "/tmp/proj"), authCtx);
    expect(again.workspace?.id).toBe(first.workspace?.id ?? "");
  });
});
