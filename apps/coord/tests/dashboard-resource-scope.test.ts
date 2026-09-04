import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import { createContextValues, Code, ConnectError, type HandlerContext } from "@connectrpc/connect";
import type { ClientControlFrame } from "@roost/shared/wire";
import { asSessionId } from "@roost/shared/wire";
import {
  McpCreateRequestSchema,
  McpDeleteRequestSchema,
  McpListRequestSchema,
  McpPublishRequestSchema,
  SessionsKillRequestSchema,
  SessionsListRequestSchema,
} from "@roost/shared/proto/coordinator_pb";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { sql } from "kysely";
import { openDb, type KyselyDB } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import {
  callerKey,
  dashboardActorKey,
  type DashboardActor,
} from "../src/connect/auth-interceptor.ts";
import { makeSessionHandlers } from "../src/connect/handlers-sessions.ts";
import { makeMcpHandlers } from "../src/connect/handlers-mcp.ts";
import { mcpBus } from "../src/buses.ts";
import {
  forwardToSessionWorker,
  requireSessionWorkerSocket,
} from "../src/connect/router-helpers.ts";
import { __setConnectWorkerForTest } from "../src/connect/worker-service.ts";
import type { ConnectDeps } from "../src/connect/router.ts";

let workdir: string;
let closeDb: () => Promise<void>;
let db: KyselyDB;
let workerSendCount = 0;

const dashboardA = "dashboard-a";
const dashboardB = "dashboard-b";
const workerA = "worker-a";
const workerB = "worker-b";
const sessionA = "00000000-0000-4000-8000-000000000001";
const sessionB = "00000000-0000-4000-8000-000000000002";

const actorA: DashboardActor = {
  accountId: "account-a",
  organizationId: "organization-a",
  dashboardId: dashboardA,
  organizationRole: "owner",
  dashboardRole: "admin",
  deviceFingerprint: "device-a",
};

function actorContext(actor: DashboardActor): HandlerContext {
  const values = createContextValues();
  values.set(callerKey, {
    kind: "account-device",
    fingerprint: actor.deviceFingerprint,
    label: "test device",
    accountId: actor.accountId,
  });
  values.set(dashboardActorKey, actor);
  return { values } as unknown as HandlerContext;
}

function workerContext(fingerprint: string, dashboardId: string): HandlerContext {
  const values = createContextValues();
  values.set(callerKey, {
    kind: "worker",
    fingerprint,
    label: "test worker",
    dashboardId,
  });
  return { values } as unknown as HandlerContext;
}

async function notFoundFrom(operation: () => Promise<unknown>): Promise<{ code: Code; message: string }> {
  try {
    await operation();
    throw new Error("expected not found");
  } catch (error) {
    expect(error).toBeInstanceOf(ConnectError);
    const connectError = error as ConnectError;
    return { code: connectError.code, message: connectError.rawMessage };
  }
}

beforeAll(async () => {
  workdir = mkdtempSync(join(tmpdir(), "roost-dashboard-resource-"));
  const opened = openDb(join(workdir, "test.db"));
  db = opened.db;
  closeDb = opened.close;
  await runMigrations(opened.sqlite);
  const now = Date.now();
  await db.insertInto("organizations").values({
    id: "organization-a",
    slug: "organization-a",
    name: "Organization A",
    status: "active",
    created_at_ms: now,
  }).execute();
  await db.insertInto("dashboards").values([
    {
      id: dashboardA,
      organization_id: "organization-a",
      slug: "dashboard-a",
      name: "Dashboard A",
      status: "active",
      created_at_ms: now,
    },
    {
      id: dashboardB,
      organization_id: "organization-a",
      slug: "dashboard-b",
      name: "Dashboard B",
      status: "active",
      created_at_ms: now,
    },
  ]).execute();
  await db.insertInto("workers").values([
    {
      fp: workerA,
      dashboard_id: dashboardA,
      label: "A",
      os: "linux",
      git_sha: null,
      host_metrics_json: null,
      registered_at_ms: now,
      last_seen_ms: now,
      reachable_addr: null,
      keeper_stale: null,
    },
    {
      fp: workerB,
      dashboard_id: dashboardB,
      label: "B",
      os: "linux",
      git_sha: null,
      host_metrics_json: null,
      registered_at_ms: now,
      last_seen_ms: now,
      reachable_addr: null,
      keeper_stale: null,
    },
  ]).execute();
  await db.insertInto("sessions").values([
    {
      id: sessionA,
      dashboard_id: dashboardA,
      worker_fp: workerA,
      channel: 1,
      kind: "shell",
      cwd: "/tmp/a",
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
      spawn_cwd: "/tmp/a",
    },
    {
      id: sessionB,
      dashboard_id: dashboardB,
      worker_fp: workerB,
      channel: 2,
      kind: "shell",
      cwd: "/tmp/b",
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
      spawn_cwd: "/tmp/b",
    },
  ]).execute();
  __setConnectWorkerForTest(workerB, {
    workerFp: workerB,
    dashboardId: dashboardB,
    send: () => {
      workerSendCount += 1;
      return 1;
    },
  });
});

afterAll(async () => {
  __setConnectWorkerForTest(workerB, null);
  await closeDb?.();
  rmSync(workdir, { recursive: true, force: true });
});

describe("dashboard resource scope", () => {
  test("worker lists only its own dashboard-scoped open sessions", async () => {
    const handlers = makeSessionHandlers({ db } as unknown as ConnectDeps);
    const response = await handlers.sessionsList(
      create(SessionsListRequestSchema, { workerFp: workerA, status: "open" }),
      workerContext(workerA, dashboardA),
    );
    expect(response.sessions?.map((session) => session.id)).toEqual([sessionA]);
    expect(response.syncSnapshotToken).toBeUndefined();
  });

  test("worker listing rejects every scope broader than its own open sessions", async () => {
    const handlers = makeSessionHandlers({ db } as unknown as ConnectDeps);
    for (const request of [
      create(SessionsListRequestSchema, { workerFp: workerB, status: "open" }),
      create(SessionsListRequestSchema, { status: "open" }),
      create(SessionsListRequestSchema, { workerFp: workerA, status: "all" }),
      create(SessionsListRequestSchema, { workerFp: workerA, status: "" }),
      create(SessionsListRequestSchema, {
        workerFp: workerA,
        status: "open",
        syncSocketId: "browser-socket",
      }),
      create(SessionsListRequestSchema, {
        workerFp: workerA,
        status: "open",
        syncSocketId: "",
      }),
    ]) {
      await expect(
        handlers.sessionsList(request, workerContext(workerA, dashboardA)),
      ).rejects.toMatchObject({ code: Code.PermissionDenied });
    }
  });

  test("foreign and absent session IDs have identical not-found admission before a worker socket", async () => {
    const [foreign, absent] = await Promise.all([
      notFoundFrom(() => requireSessionWorkerSocket(db, actorA, sessionB)),
      notFoundFrom(() => requireSessionWorkerSocket(db, actorA, "missing-session")),
    ]);
    expect(foreign).toEqual(absent);
    expect(foreign.code).toBe(Code.NotFound);
    expect(workerSendCount).toBe(0);
  });

  test("foreign session forwarding is rejected before a worker command", async () => {
    const frame: ClientControlFrame = {
      kind: "cursor-pos",
      session_id: asSessionId(sessionB),
      col: 3,
      row: 2,
    };
    await expect(forwardToSessionWorker(
      db,
      actorA,
      sessionB,
      { kind: "account-device", fingerprint: actorA.deviceFingerprint, label: "test device", accountId: actorA.accountId },
      frame,
    )).resolves.toBe(false);
    expect(workerSendCount).toBe(0);
  });

  test("foreign session kill is an indistinguishable no-op with no dispatch or mutation", async () => {
    const handlers = makeSessionHandlers({ db } as unknown as ConnectDeps);
    const response = await handlers.sessionsKill(
      create(SessionsKillRequestSchema, { sessionId: sessionB, force: false }),
      actorContext(actorA),
    );
    expect(response.accepted).toBe(false);
    expect(workerSendCount).toBe(0);
    const foreign = await db.selectFrom("sessions").select(["id", "status"])
      .where("id", "=", sessionB)
      .executeTakeFirst();
    expect(foreign).toEqual({ id: sessionB, status: "open" });
  });

  test("MCP create, list, publish, and delete retain dashboard-scoped behavior", async () => {
    const handlers = makeMcpHandlers({ db } as unknown as ConnectDeps);
    const events: unknown[] = [];
    const unsubscribe = mcpBus.subscribe((event) => events.push(event), dashboardA);
    try {
      const created = await handlers.mcpCreate(create(McpCreateRequestSchema, {
        label: "Local tools",
        kind: "stdio",
        configJson: JSON.stringify({ command: "bun", args: ["run", "mcp"] }),
      }), actorContext(actorA));
      expect(created.relay).toMatchObject({
        label: "Local tools",
        kind: "stdio",
      });
      const id = created.relay!.id;

      const listed = await handlers.mcpList(
        create(McpListRequestSchema),
        actorContext(actorA),
      );
      expect(listed.relays).toBeDefined();
      expect(listed.relays?.map((relay) => relay.id)).toEqual([id]);

      await handlers.mcpPublish(create(McpPublishRequestSchema, {
        id,
        payloadJson: JSON.stringify({ method: "tools/list" }),
      }), actorContext(actorA));
      const deleted = await handlers.mcpDelete(create(McpDeleteRequestSchema, {
        id,
      }), actorContext(actorA));
      expect(deleted.ok).toBe(true);
      expect((await handlers.mcpList(
        create(McpListRequestSchema),
        actorContext(actorA),
      )).relays).toEqual([]);

      expect(events).toHaveLength(3);
      expect(events[0]).toMatchObject({
        kind: "created",
        _dashboard_id: dashboardA,
        relay: { id, label: "Local tools", kind: "stdio" },
      });
      expect(events[1]).toMatchObject({
        relay_id: id,
        payload: { method: "tools/list" },
        _dashboard_id: dashboardA,
      });
      expect(events[2]).toMatchObject({
        kind: "deleted",
        id,
        _dashboard_id: dashboardA,
      });
    } finally {
      unsubscribe();
    }
  });
});
