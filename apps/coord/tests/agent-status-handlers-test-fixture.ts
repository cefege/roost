// Shared fixture for the observed-agent read-handler suite: one install with two
// workers and five sessions, the exact AgentStatusUpdate builders, and the
// per-test hub arming those cases read back. Used by
// agent-status-handlers.test.ts; bun caches this module, so hook registration
// stays in the suite and this file only exposes the bodies.

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
  type AccountDeviceCaller,
} from "../src/connect/auth-interceptor.ts";
import {
  makeAgentStatusHandlers,
  type AgentStatusHandlers,
} from "../src/connect/handlers-agent-status.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import { openDb, type KyselyDB } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { ensureSelfHostedTenant } from "../src/self-hosted-tenant.ts";

export const WORKER_A = asWorkerFp("a1".repeat(32));
const WORKER_B = asWorkerFp("b2".repeat(32));
export const SESSION_INTEGRATION = asSessionId("10000000-0000-4000-8000-000000000010");
export const SESSION_SCREEN = asSessionId("10000000-0000-4000-8000-000000000020");
export const SESSION_LEGACY = asSessionId("10000000-0000-4000-8000-000000000030");
export const SESSION_NO_STATUS = asSessionId("10000000-0000-4000-8000-000000000040");
export const SESSION_FOREIGN = asSessionId("10000000-0000-4000-8000-000000000050");
export const SESSION_MISSING = asSessionId("10000000-0000-4000-8000-000000000999");
export const STATUS_EPOCH = StatusEpoch.parse("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");

const ACCOUNT_ID = "agent-status-handlers-account";

// Two browser devices of the one account: identity differs, authority does not.
export const ACTOR_A: AccountDeviceCaller = {
  kind: "account-device",
  fingerprint: "agent-status-handlers-device-a",
  label: "test device a",
  accountId: ACCOUNT_ID,
};
export const ACTOR_B: AccountDeviceCaller = {
  kind: "account-device",
  fingerprint: "agent-status-handlers-device-b",
  label: "test device b",
  accountId: ACCOUNT_ID,
};

export function actorContext(caller: AccountDeviceCaller): HandlerContext {
  const values = createContextValues();
  values.set(callerKey, caller);
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
  const tenant = ensureSelfHostedTenant(opened.sqlite, { backfillLegacyScopes: false });
  const now = Date.now();
  await db.insertInto("workers").values([
    persistedWorker(WORKER_A, tenant.dashboardId, "Agent status A", now),
    persistedWorker(WORKER_B, tenant.dashboardId, "Agent status B", now),
  ]).execute();
  await db.insertInto("sessions").values([
    [SESSION_INTEGRATION, WORKER_A, 10],
    [SESSION_SCREEN, WORKER_A, 20],
    [SESSION_LEGACY, WORKER_A, 30],
    [SESSION_NO_STATUS, WORKER_A, 40],
    [SESSION_FOREIGN, WORKER_B, 50],
  ].map(([id, workerFp, channel]) => ({
    id: String(id),
    dashboard_id: tenant.dashboardId,
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
    handlers: makeAgentStatusHandlers({
      db,
      selfHostedTenant: tenant,
    } as unknown as ConnectDeps),
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
