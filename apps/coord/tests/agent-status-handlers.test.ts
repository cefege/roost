// Observed-agent read handler contract: durable dashboard authorization precedes
// volatile hub lookup, retained status remains readable while a worker is offline,
// and public projections expose promptability only for identified integrations.
// The focused router check also guards the single-service composition boundary.

import { create } from "@bufbuild/protobuf";
import {
  Code,
  ConnectError,
  createContextValues,
  type HandlerContext,
} from "@connectrpc/connect";
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  AgentStatusGetRequestSchema,
  AgentStatusListRequestSchema,
} from "@roost/shared/proto/coordinator_pb";
import {
  AgentOccupantId,
  AgentStatusUpdate,
  StatusEpoch,
  asSessionId,
  asWorkerFp,
  type AgentStatusSource,
  type AgentStatusUpdate as AgentStatusUpdateValue,
} from "@roost/shared/wire";
import {
  handleWorkerAgentStatus,
  startAgentStatusHub,
  stopAgentStatusHub,
} from "../src/agent-status-hub.ts";
import { cacheSessionWorker, evictSessionWorker } from "../src/byte-hub.ts";
import {
  callerKey,
  dashboardActorKey,
  type DashboardActor,
} from "../src/connect/auth-interceptor.ts";
import { makeConnectBunHandler } from "../src/connect/bun-handler.ts";
import {
  makeAgentStatusHandlers,
  type AgentStatusHandlers,
} from "../src/connect/handlers-agent-status.ts";
import { buildConnectRouter, type ConnectDeps } from "../src/connect/router.ts";
import { openDb, type KyselyDB } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";

const ORGANIZATION_ID = "agent-status-handlers-organization";
const DASHBOARD_A = "agent-status-handlers-dashboard-a";
const DASHBOARD_B = "agent-status-handlers-dashboard-b";
const WORKER_A = asWorkerFp("a1".repeat(32));
const WORKER_B = asWorkerFp("b2".repeat(32));
const SESSION_INTEGRATION = asSessionId("10000000-0000-4000-8000-000000000010");
const SESSION_SCREEN = asSessionId("10000000-0000-4000-8000-000000000020");
const SESSION_LEGACY = asSessionId("10000000-0000-4000-8000-000000000030");
const SESSION_NO_STATUS = asSessionId("10000000-0000-4000-8000-000000000040");
const SESSION_FOREIGN = asSessionId("10000000-0000-4000-8000-000000000050");
const SESSION_MISSING = asSessionId("10000000-0000-4000-8000-000000000999");
const STATUS_EPOCH = StatusEpoch.parse("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");

const ACTOR_A: DashboardActor = {
  accountId: "agent-status-handlers-account-a",
  organizationId: ORGANIZATION_ID,
  dashboardId: DASHBOARD_A,
  organizationRole: "owner",
  dashboardRole: "admin",
  deviceFingerprint: "agent-status-handlers-device-a",
};
const ACTOR_B: DashboardActor = {
  accountId: "agent-status-handlers-account-b",
  organizationId: ORGANIZATION_ID,
  dashboardId: DASHBOARD_B,
  organizationRole: "member",
  dashboardRole: "member",
  deviceFingerprint: "agent-status-handlers-device-b",
};

let workdir = "";
let db: KyselyDB;
let closeDb: () => Promise<void>;
let handlers: AgentStatusHandlers;

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

function anonymousContext(): HandlerContext {
  return { values: createContextValues() } as unknown as HandlerContext;
}

function status(
  sessionId: AgentStatusUpdateValue["session_id"],
  overrides: Partial<AgentStatusUpdateValue> = {},
): AgentStatusUpdateValue {
  return AgentStatusUpdate.parse({
    session_id: sessionId,
    agent_id: "omp",
    state: "working",
    revision: 1,
    completed_revision: 0,
    updated_at: 1_800_000_000_000,
    active: true,
    ...overrides,
  });
}

function identity(
  occupantId: string,
  source: AgentStatusSource,
): Pick<AgentStatusUpdateValue, "status_epoch" | "occupant_id" | "source"> {
  return {
    status_epoch: STATUS_EPOCH,
    occupant_id: AgentOccupantId.parse(occupantId),
    source,
  };
}

function retain(workerFp: string, value: AgentStatusUpdateValue): void {
  const acceptance = handleWorkerAgentStatus(workerFp, value);
  if (acceptance !== "accepted") {
    throw new Error(`fixture status was ${acceptance}`);
  }
}

function persistedWorker(
  fingerprint: string, dashboardId: string, label: string, timestamp: number,
) {
  return {
    fp: fingerprint, dashboard_id: dashboardId, label, os: "linux",
    git_sha: null, host_metrics_json: null, reachable_addr: null,
    registered_at_ms: timestamp, last_seen_ms: timestamp,
  };
}

async function notFoundFrom(operation: () => Promise<unknown> | unknown): Promise<{
  code: Code;
  message: string;
}> {
  try {
    await operation();
  } catch (error) {
    expect(error).toBeInstanceOf(ConnectError);
    const connectError = error as ConnectError;
    return { code: connectError.code, message: connectError.rawMessage };
  }
  throw new Error("expected agent status not found");
}

beforeAll(async () => {
  workdir = mkdtempSync(join(tmpdir(), "roost-agent-status-handlers-"));
  const opened = openDb(join(workdir, "coord.db"));
  db = opened.db;
  closeDb = opened.close;
  await runMigrations(opened.sqlite);
  const now = Date.now();
  await db.insertInto("organizations").values({
    id: ORGANIZATION_ID,
    slug: "agent-status-handlers",
    name: "Agent status handlers",
    status: "active",
    created_at_ms: now,
  }).execute();
  await db.insertInto("dashboards").values([
    {
      id: DASHBOARD_A,
      organization_id: ORGANIZATION_ID,
      slug: "agent-status-handlers-a",
      name: "Agent status handlers A",
      status: "active",
      created_at_ms: now,
    },
    {
      id: DASHBOARD_B,
      organization_id: ORGANIZATION_ID,
      slug: "agent-status-handlers-b",
      name: "Agent status handlers B",
      status: "active",
      created_at_ms: now,
    },
  ]).execute();
  await db.insertInto("workers").values([
    persistedWorker(WORKER_A, DASHBOARD_A, "Agent status A", now),
    persistedWorker(WORKER_B, DASHBOARD_B, "Agent status B", now),
  ]).execute();
  await db.insertInto("sessions").values([
    [SESSION_INTEGRATION, DASHBOARD_A, WORKER_A, 10],
    [SESSION_SCREEN, DASHBOARD_A, WORKER_A, 20],
    [SESSION_LEGACY, DASHBOARD_A, WORKER_A, 30],
    [SESSION_NO_STATUS, DASHBOARD_A, WORKER_A, 40],
    [SESSION_FOREIGN, DASHBOARD_B, WORKER_B, 50],
  ].map(([id, dashboardId, workerFp, channel]) => ({
    id: String(id),
    dashboard_id: String(dashboardId),
    worker_fp: String(workerFp),
    channel: Number(channel),
    kind: "shell" as const,
    cwd: "/tmp",
    workspace_id: null,
    status: "open" as const,
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
    spawn_cwd: null,
  }))).execute();
  handlers = makeAgentStatusHandlers({ db } as unknown as ConnectDeps);
});

beforeEach(() => {
  stopAgentStatusHub();
  startAgentStatusHub();
  cacheSessionWorker(SESSION_INTEGRATION, WORKER_A, 10);
  cacheSessionWorker(SESSION_SCREEN, WORKER_A, 20);
  cacheSessionWorker(SESSION_LEGACY, WORKER_A, 30);
  cacheSessionWorker(SESSION_FOREIGN, WORKER_B, 50);

  // Insert out of session order so list ordering cannot inherit Map insertion.
  retain(WORKER_A, status(SESSION_LEGACY, {
    message: "legacy worker",
    revision: 5,
    completed_revision: 1,
  }));
  retain(WORKER_A, status(SESSION_INTEGRATION, {
    ...identity("11111111-1111-4111-8111-111111111111", "integration"),
    message: "review ready",
    state: "idle",
    revision: 7,
    completed_revision: 4,
  }));
  retain(WORKER_A, status(SESSION_SCREEN, {
    ...identity("22222222-2222-4222-8222-222222222222", "screen"),
    state: "blocked",
    revision: 2,
  }));
  retain(WORKER_B, status(SESSION_FOREIGN, {
    ...identity("33333333-3333-4333-8333-333333333333", "integration"),
    revision: 9,
  }));
});

afterEach(() => {
  stopAgentStatusHub();
  for (const sessionId of [
    SESSION_INTEGRATION,
    SESSION_SCREEN,
    SESSION_LEGACY,
    SESSION_FOREIGN,
  ]) {
    evictSessionWorker(sessionId);
  }
});

afterAll(async () => {
  await closeDb?.();
  rmSync(workdir, { recursive: true, force: true });
});

describe("agent status read handlers", () => {
  test("requires a dashboard actor for both methods", async () => {
    await expect(handlers.agentStatusGet(
      create(AgentStatusGetRequestSchema, { sessionId: SESSION_INTEGRATION }),
      anonymousContext(),
    )).rejects.toMatchObject({ code: Code.Unauthenticated });
    await expect(handlers.agentStatusList(
      create(AgentStatusListRequestSchema),
      anonymousContext(),
    )).rejects.toMatchObject({ code: Code.Unauthenticated });
  });

  test("reads identified integration status after its worker route goes offline", async () => {
    evictSessionWorker(SESSION_INTEGRATION);
    const response = await handlers.agentStatusGet(
      create(AgentStatusGetRequestSchema, { sessionId: SESSION_INTEGRATION }),
      actorContext(ACTOR_A),
    );

    expect(response.status).toMatchObject({
      sessionId: SESSION_INTEGRATION,
      agentId: "omp",
      state: "idle",
      message: "review ready",
      revision: 7n,
      completedRevision: 4n,
      updatedAt: 1_800_000_000_000,
      active: true,
      statusEpoch: STATUS_EPOCH,
      occupantId: "11111111-1111-4111-8111-111111111111",
      source: "integration",
      promptable: true,
    });
    expect(response.status && "pid" in response.status).toBe(false);
  });

  test("makes missing, foreign, and authorized statusless sessions indistinguishable", async () => {
    const failures = await Promise.all([
      SESSION_MISSING,
      SESSION_FOREIGN,
      SESSION_NO_STATUS,
    ].map((sessionId) => notFoundFrom(() => handlers.agentStatusGet(
      create(AgentStatusGetRequestSchema, { sessionId }),
      actorContext(ACTOR_A),
    ))));

    expect(failures).toEqual([
      { code: Code.NotFound, message: "agent status not found" },
      { code: Code.NotFound, message: "agent status not found" },
      { code: Code.NotFound, message: "agent status not found" },
    ]);
    const foreign = await handlers.agentStatusGet(
      create(AgentStatusGetRequestSchema, { sessionId: SESSION_FOREIGN }),
      actorContext(ACTOR_B),
    );
    expect(foreign.status?.sessionId).toBe(SESSION_FOREIGN);
  });

  test("lists only authorized open statuses in session order with derived promptability", async () => {
    const response = await handlers.agentStatusList(
      create(AgentStatusListRequestSchema),
      actorContext(ACTOR_A),
    );
    const statuses = response.statuses ?? [];
    expect(statuses.map((statusView) => statusView.sessionId)).toEqual([
      SESSION_INTEGRATION,
      SESSION_SCREEN,
      SESSION_LEGACY,
    ]);
    expect(statuses.map((statusView) => statusView.promptable)).toEqual([
      true,
      false,
      false,
    ]);
    expect(statuses[1]).toMatchObject({
      source: "screen",
      statusEpoch: STATUS_EPOCH,
      occupantId: "22222222-2222-4222-8222-222222222222",
    });
    expect(statuses[2]?.statusEpoch).toBeUndefined();
    expect(statuses[2]?.occupantId).toBeUndefined();
    expect(statuses[2]?.source).toBeUndefined();
    expect(statuses.some((statusView) => statusView.sessionId === SESSION_NO_STATUS))
      .toBe(false);

    const foreignDashboard = await handlers.agentStatusList(
      create(AgentStatusListRequestSchema),
      actorContext(ACTOR_B),
    );
    expect((foreignDashboard.statuses ?? []).map((statusView) => statusView.sessionId))
      .toEqual([SESSION_FOREIGN]);
  });

  test("the single coordinator router admits both read methods exactly once", () => {
    const router = buildConnectRouter({
      db,
      cfg: { saasMode: false },
    } as unknown as ConnectDeps);
    const connectHandler = makeConnectBunHandler(router);
    for (const rpcMethod of ["AgentStatusGet", "AgentStatusList", "AgentStatusWait"]) {
      const path = `/roost.v1.CoordinatorService/${rpcMethod}`;
      expect(connectHandler.matches(path)).toBe(true);
      expect([...router.handlers].filter((handler) => handler.requestPath === path))
        .toHaveLength(1);
    }
  });
});
