// Agent-status wait handler tests pin dashboard authorization before volatile
// admission, Connect error mapping, and exact response outcomes. A real migrated
// database supplies the same open-session boundary used by status reads.

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
import { AgentStatusWaitRequestSchema } from "@roost/shared/proto/coordinator_pb";
import {
  AgentOccupantId,
  AgentStatusUpdate,
  StatusEpoch,
  asSessionId,
  asWorkerFp,
} from "@roost/shared/wire";
import {
  handleWorkerAgentStatus,
  startAgentStatusHub,
  stopAgentStatusHub,
  waitForAgentStatus,
} from "../src/agent-status-hub.ts";
import {
  AGENT_STATUS_WAIT_MAX_PER_SESSION,
  _agentStatusWaiterStats,
} from "../src/agent-status-wait.ts";
import { cacheSessionWorker, evictSessionWorker } from "../src/byte-hub.ts";
import {
  callerKey,
  dashboardActorKey,
  type DashboardActor,
} from "../src/connect/auth-interceptor.ts";
import { makeAgentStatusHandlers, type AgentStatusHandlers } from "../src/connect/handlers-agent-status.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import { openDb, type KyselyDB } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";

const ORGANIZATION_ID = "agent-wait-organization";
const DASHBOARD_A = "agent-wait-dashboard-a";
const DASHBOARD_B = "agent-wait-dashboard-b";
const WORKER_A = asWorkerFp("a1".repeat(32));
const WORKER_B = asWorkerFp("b2".repeat(32));
const SESSION_A = asSessionId("50000000-0000-4000-8000-000000000001");
const SESSION_FOREIGN = asSessionId("50000000-0000-4000-8000-000000000002");
const SESSION_MISSING = asSessionId("50000000-0000-4000-8000-000000000099");
const STATUS_EPOCH = StatusEpoch.parse("60000000-0000-4000-8000-000000000001");
const OCCUPANT_ID = AgentOccupantId.parse("70000000-0000-4000-8000-000000000001");
const ACTOR_A: DashboardActor = {
  accountId: "agent-wait-account-a",
  organizationId: ORGANIZATION_ID,
  dashboardId: DASHBOARD_A,
  organizationRole: "owner",
  dashboardRole: "admin",
  deviceFingerprint: "agent-wait-device-a",
};

let workdir = "";
let db: KyselyDB;
let closeDb: () => Promise<void>;
let handlers: AgentStatusHandlers;

function actorContext(
  actor: DashboardActor,
  signal = new AbortController().signal,
): HandlerContext {
  const values = createContextValues();
  values.set(callerKey, {
    kind: "account-device",
    fingerprint: actor.deviceFingerprint,
    label: "wait test device",
    accountId: actor.accountId,
  });
  values.set(dashboardActorKey, actor);
  return { values, signal } as unknown as HandlerContext;
}

function anonymousContext(): HandlerContext {
  return {
    values: createContextValues(),
    signal: new AbortController().signal,
  } as unknown as HandlerContext;
}

function request(overrides: {
  sessionId?: string;
  statusEpoch?: string;
  occupantId?: string;
  desiredStates?: string[];
  afterRevision?: bigint;
  timeoutMs?: number;
} = {}) {
  return create(AgentStatusWaitRequestSchema, {
    sessionId: SESSION_A,
    statusEpoch: STATUS_EPOCH,
    occupantId: OCCUPANT_ID,
    desiredStates: ["blocked"],
    timeoutMs: 30_000,
    ...overrides,
  });
}

function openSession(id: string, dashboardId: string, workerFp: string, channel: number) {
  return {
    id,
    dashboard_id: dashboardId,
    worker_fp: workerFp,
    channel,
    kind: "shell" as const,
    cwd: "/tmp",
    workspace_id: null,
    status: "open" as const,
    created_at: Date.now(),
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
  };
}

function currentStatus() {
  return AgentStatusUpdate.parse({
    session_id: SESSION_A,
    agent_id: "omp",
    state: "working",
    revision: 4,
    completed_revision: 1,
    updated_at: 1_800_000_000_000,
    active: true,
    status_epoch: STATUS_EPOCH,
    occupant_id: OCCUPANT_ID,
    source: "integration",
  });
}

async function connectFailure(operation: () => Promise<unknown> | unknown): Promise<{
  code: Code;
  message: string;
}> {
  try {
    await operation();
  } catch (error) {
    expect(error).toBeInstanceOf(ConnectError);
    return {
      code: (error as ConnectError).code,
      message: (error as ConnectError).rawMessage,
    };
  }
  throw new Error("expected Connect failure");
}

beforeAll(async () => {
  workdir = mkdtempSync(join(tmpdir(), "roost-agent-wait-handlers-"));
  const opened = openDb(join(workdir, "coord.db"));
  db = opened.db;
  closeDb = opened.close;
  await runMigrations(opened.sqlite);
  const now = Date.now();
  await db.insertInto("organizations").values({
    id: ORGANIZATION_ID,
    slug: "agent-wait",
    name: "Agent wait",
    status: "active",
    created_at_ms: now,
  }).execute();
  await db.insertInto("dashboards").values([
    { id: DASHBOARD_A, organization_id: ORGANIZATION_ID, slug: "agent-wait-a", name: "A", status: "active", created_at_ms: now },
    { id: DASHBOARD_B, organization_id: ORGANIZATION_ID, slug: "agent-wait-b", name: "B", status: "active", created_at_ms: now },
  ]).execute();
  await db.insertInto("workers").values([
    { fp: WORKER_A, dashboard_id: DASHBOARD_A, label: "A", os: "linux", git_sha: null, host_metrics_json: null, reachable_addr: null, registered_at_ms: now, last_seen_ms: now },
    { fp: WORKER_B, dashboard_id: DASHBOARD_B, label: "B", os: "linux", git_sha: null, host_metrics_json: null, reachable_addr: null, registered_at_ms: now, last_seen_ms: now },
  ]).execute();
  await db.insertInto("sessions").values([
    openSession(SESSION_A, DASHBOARD_A, WORKER_A, 1),
    openSession(SESSION_FOREIGN, DASHBOARD_B, WORKER_B, 2),
  ]).execute();
  handlers = makeAgentStatusHandlers({ db } as unknown as ConnectDeps);
});

beforeEach(() => {
  stopAgentStatusHub();
  startAgentStatusHub();
  cacheSessionWorker(SESSION_A, WORKER_A, 1);
  expect(handleWorkerAgentStatus(WORKER_A, currentStatus())).toBe("accepted");
});

afterEach(() => {
  stopAgentStatusHub();
  evictSessionWorker(SESSION_A);
});

afterAll(async () => {
  await closeDb?.();
  rmSync(workdir, { recursive: true, force: true });
});

describe("agent status wait handler", () => {
  test("returns an immediate exact-occupant match", async () => {
    const response = await handlers.agentStatusWait(
      request({ desiredStates: ["working"] }),
      actorContext(ACTOR_A),
    );
    expect(response.outcome).toBe("matched");
    expect(_agentStatusWaiterStats().total).toBe(0);
  });

  test("authorizes before validation and preserves the missing/foreign non-oracle", async () => {
    await expect(handlers.agentStatusWait(request(), anonymousContext()))
      .rejects.toMatchObject({ code: Code.Unauthenticated });
    const failures = await Promise.all([
      request({ sessionId: SESSION_MISSING, timeoutMs: 0 }),
      request({ sessionId: SESSION_FOREIGN, timeoutMs: 0 }),
    ].map((value) => connectFailure(() => handlers.agentStatusWait(
      value,
      actorContext(ACTOR_A),
    ))));
    expect(failures).toEqual([
      { code: Code.NotFound, message: "agent status not found" },
      { code: Code.NotFound, message: "agent status not found" },
    ]);
    expect(_agentStatusWaiterStats().total).toBe(0);
  });

  test("maps every authorized malformed wait to InvalidArgument", async () => {
    for (const value of [
      request({ statusEpoch: "invalid" }),
      request({ occupantId: "invalid" }),
      request({ desiredStates: [] }),
      request({ desiredStates: ["idle", "idle"] }),
      request({ desiredStates: ["done"] }),
      request({ timeoutMs: 0 }),
      request({ timeoutMs: 300_001 }),
      request({ afterRevision: BigInt(Number.MAX_SAFE_INTEGER) + 1n }),
    ]) {
      await expect(handlers.agentStatusWait(value, actorContext(ACTOR_A)))
        .rejects.toMatchObject({ code: Code.InvalidArgument });
    }
    expect(_agentStatusWaiterStats().total).toBe(0);
  });

  test("maps abort to Canceled and per-session admission to ResourceExhausted", async () => {
    const controller = new AbortController();
    const canceled = handlers.agentStatusWait(
      request({ desiredStates: ["idle"] }),
      actorContext(ACTOR_A, controller.signal),
    );
    controller.abort();
    await expect(canceled).rejects.toMatchObject({ code: Code.Canceled });

    const pending = Array.from({ length: AGENT_STATUS_WAIT_MAX_PER_SESSION }, () =>
      waitForAgentStatus({
        sessionId: SESSION_A,
        statusEpoch: STATUS_EPOCH,
        occupantId: OCCUPANT_ID,
        desiredStates: ["idle"],
        timeoutMs: 300_000,
      }, new AbortController().signal));
    await expect(handlers.agentStatusWait(
      request({ desiredStates: ["idle"] }),
      actorContext(ACTOR_A),
    )).rejects.toMatchObject({ code: Code.ResourceExhausted });
    const settled = Promise.allSettled(pending);
    stopAgentStatusHub();
    await settled;
    expect(_agentStatusWaiterStats().total).toBe(0);
  });
});
