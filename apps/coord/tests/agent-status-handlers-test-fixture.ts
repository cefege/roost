// Shared fixture for the observed-agent read-handler suite: one migrated
// organization with two dashboards, two workers and five sessions, the exact
// AgentStatusUpdate builders, and the per-test hub arming those cases read
// back. Used by agent-status-handlers.test.ts; bun caches this module, so hook
// registration stays in the suite and this file only exposes the bodies.

import { Code, ConnectError, createContextValues, type HandlerContext } from "@connectrpc/connect";
import { expect } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
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
import {
  makeAgentStatusHandlers,
  type AgentStatusHandlers,
} from "../src/connect/handlers-agent-status.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import { openDb, type KyselyDB } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";

const ORGANIZATION_ID = "agent-status-handlers-organization";
const DASHBOARD_A = "agent-status-handlers-dashboard-a";
const DASHBOARD_B = "agent-status-handlers-dashboard-b";
export const WORKER_A = asWorkerFp("a1".repeat(32));
const WORKER_B = asWorkerFp("b2".repeat(32));
export const SESSION_INTEGRATION = asSessionId("10000000-0000-4000-8000-000000000010");
export const SESSION_SCREEN = asSessionId("10000000-0000-4000-8000-000000000020");
export const SESSION_LEGACY = asSessionId("10000000-0000-4000-8000-000000000030");
export const SESSION_NO_STATUS = asSessionId("10000000-0000-4000-8000-000000000040");
export const SESSION_FOREIGN = asSessionId("10000000-0000-4000-8000-000000000050");
export const SESSION_MISSING = asSessionId("10000000-0000-4000-8000-000000000999");
export const STATUS_EPOCH = StatusEpoch.parse("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");

export const ACTOR_A: DashboardActor = {
  accountId: "agent-status-handlers-account-a",
  organizationId: ORGANIZATION_ID,
  dashboardId: DASHBOARD_A,
  organizationRole: "owner",
  dashboardRole: "admin",
  deviceFingerprint: "agent-status-handlers-device-a",
};
export const ACTOR_B: DashboardActor = {
  accountId: "agent-status-handlers-account-b",
  organizationId: ORGANIZATION_ID,
  dashboardId: DASHBOARD_B,
  organizationRole: "member",
  dashboardRole: "member",
  deviceFingerprint: "agent-status-handlers-device-b",
};

export function actorContext(actor: DashboardActor): HandlerContext {
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

export function anonymousContext(): HandlerContext {
  return { values: createContextValues() } as unknown as HandlerContext;
}

export function status(
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

export function identity(
  occupantId: string,
  source: AgentStatusSource,
): Pick<AgentStatusUpdateValue, "status_epoch" | "occupant_id" | "source"> {
  return {
    status_epoch: STATUS_EPOCH,
    occupant_id: AgentOccupantId.parse(occupantId),
    source,
  };
}

export function retain(workerFp: string, value: AgentStatusUpdateValue): void {
  const acceptance = handleWorkerAgentStatus(workerFp, value);
  if (acceptance !== "accepted") {
    throw new Error(`fixture status was ${acceptance}`);
  }
}

export async function notFoundFrom(operation: () => Promise<unknown> | unknown): Promise<{
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

export interface AgentStatusHandlersTestFixture {
  db: KyselyDB;
  handlers: AgentStatusHandlers;
  close: () => Promise<void>;
}

export async function startAgentStatusHandlersTestFixture(): Promise<AgentStatusHandlersTestFixture> {
  const workdir = mkdtempSync(join(tmpdir(), "roost-agent-status-handlers-"));
  const opened = openDb(join(workdir, "coord.db"));
  const db = opened.db;
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
  return {
    db,
    handlers: makeAgentStatusHandlers({ db } as unknown as ConnectDeps),
    close: async () => {
      await opened.close();
      rmSync(workdir, { recursive: true, force: true });
    },
  };
}

export function restartAgentStatusHubWithRetainedFixtures(): void {
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
}

export function stopAgentStatusHubAndEvictFixtures(): void {
  stopAgentStatusHub();
  for (const sessionId of [
    SESSION_INTEGRATION,
    SESSION_SCREEN,
    SESSION_LEGACY,
    SESSION_FOREIGN,
  ]) {
    evictSessionWorker(sessionId);
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
