// Agent-prompt tests share one migrated install fixture — two workers, the
// prompted session and a second worker's session — plus the exact request
// builder and an explicit fake worker transport. The fixture owns status-hub
// reset and route cleanup so each case can observe waiter and pending-RPC
// behavior without network I/O.

import { create } from "@bufbuild/protobuf";
import { createContextValues, type HandlerContext } from "@connectrpc/connect";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  AgentOccupantId,
  AgentStatusUpdate,
  StatusEpoch,
  asSessionId,
  asWorkerFp,
  type AgentRuntimeState,
} from "@roost/shared/wire";
import {
  SessionsPromptRequestSchema,
  type SessionsPromptRequest,
} from "@roost/shared/proto/coordinator_pb";
import type { CoordWorkerDown } from "@roost/shared/proto/worker_transport_pb";
import {
  handleWorkerAgentStatus,
  startAgentStatusHub,
  stopAgentStatusHub,
} from "../src/agent-status-hub.ts";
import { cacheSessionWorker, evictSessionWorker } from "../src/byte-hub.ts";
import {
  callerKey,
  tabIdKey,
  type AccountDeviceCaller,
} from "../src/connect/auth-interceptor.ts";
import { makeAgentPromptHandlers } from "../src/connect/handlers-agent-prompt.ts";
import { __setConnectWorkerForTest } from "../src/connect/worker-registry.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import { openDb } from "../src/db/connection.ts";
import { CoordinatorWriteGate } from "../src/coordinator-write-gate.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { ensureSelfHostedTenant } from "../src/self-hosted-tenant.ts";

export const PROMPT_WORKER = asWorkerFp("a5".repeat(32));
export const FOREIGN_WORKER = asWorkerFp("b6".repeat(32));
export const PROMPT_SESSION = asSessionId("81000000-0000-4000-8000-000000000001");
export const FOREIGN_SESSION = asSessionId("81000000-0000-4000-8000-000000000002");
export const MISSING_SESSION = asSessionId("81000000-0000-4000-8000-000000000099");
export const PROMPT_STATUS_EPOCH = StatusEpoch.parse("82000000-0000-4000-8000-000000000001");
export const PROMPT_OCCUPANT = AgentOccupantId.parse("83000000-0000-4000-8000-000000000001");

export function agentPromptRequest(overrides: Partial<{
  sessionId: string;
  expectedStatusEpoch: string;
  expectedOccupantId: string;
  expectedRevision: bigint;
  text: string;
  waitStates: string[];
  waitTimeoutMs: number;
}> = {}): SessionsPromptRequest {
  return create(SessionsPromptRequestSchema, {
    sessionId: PROMPT_SESSION,
    expectedStatusEpoch: PROMPT_STATUS_EPOCH,
    expectedOccupantId: PROMPT_OCCUPANT,
    expectedRevision: 1n,
    text: "continue",
    ...overrides,
  });
}

const ACTOR: AccountDeviceCaller = {
  kind: "account-device",
  fingerprint: "agent-prompt-device",
  label: "prompt test device",
  accountId: "agent-prompt-account",
};

export async function startAgentPromptTestFixture() {
  const workdir = mkdtempSync(join(tmpdir(), "roost-agent-prompt-"));
  const opened = openDb(join(workdir, "coord.db"));
  await runMigrations(opened.sqlite);
  const tenant = ensureSelfHostedTenant(opened.sqlite, { backfillLegacyScopes: false });
  const now = Date.now();
  await opened.db.insertInto("workers").values([
    {
      fp: PROMPT_WORKER,
      dashboard_id: tenant.dashboardId,
      label: "Prompt worker",
      os: "linux",
      registered_at_ms: now,
      last_seen_ms: now,
    },
    {
      fp: FOREIGN_WORKER,
      dashboard_id: tenant.dashboardId,
      label: "Second worker",
      os: "linux",
      registered_at_ms: now,
      last_seen_ms: now,
    },
  ]).execute();
  await opened.db.insertInto("sessions").values([
    {
      id: PROMPT_SESSION,
      dashboard_id: tenant.dashboardId,
      worker_fp: PROMPT_WORKER,
      channel: 41,
      kind: "shell",
      cwd: "/tmp",
      status: "open",
      created_at: now,
    },
    {
      id: FOREIGN_SESSION,
      dashboard_id: tenant.dashboardId,
      worker_fp: FOREIGN_WORKER,
      channel: 42,
      kind: "shell",
      cwd: "/tmp",
      status: "open",
      created_at: now,
    },
  ]).execute();
  const deps = {
    db: opened.db,
    writeGate: new CoordinatorWriteGate(),
    selfHostedTenant: tenant,
  } as unknown as ConnectDeps;
  const handlers = makeAgentPromptHandlers(deps);

  function context(signal = new AbortController().signal): HandlerContext {
    const values = createContextValues();
    values.set(callerKey, ACTOR);
    values.set(tabIdKey, "prompt-test-tab");
    return { values, signal } as unknown as HandlerContext;
  }

  function retainStatus(
    state: AgentRuntimeState = "working",
    revision = 1,
    message?: string,
    completedRevision = 0,
  ): void {
    const accepted = handleWorkerAgentStatus(PROMPT_WORKER, AgentStatusUpdate.parse({
      session_id: PROMPT_SESSION,
      agent_id: "omp",
      state,
      message,
      revision,
      completed_revision: completedRevision,
      updated_at: now + revision,
      active: true,
      status_epoch: PROMPT_STATUS_EPOCH,
      occupant_id: PROMPT_OCCUPANT,
      source: "integration",
    }));
    if (accepted !== "accepted") throw new Error(`status fixture was ${accepted}`);
  }

  return {
    db: opened.db,
    deps,
    handlers,
    context,
    anonymousContext: () => ({
      values: createContextValues(),
      signal: new AbortController().signal,
    }) as unknown as HandlerContext,
    attachWorker(send: (frame: CoordWorkerDown) => number): void {
      __setConnectWorkerForTest(PROMPT_WORKER, {
        workerFp: PROMPT_WORKER,
        send,
      });
    },
    resetStatus(): void {
      stopAgentStatusHub();
      startAgentStatusHub();
      cacheSessionWorker(PROMPT_SESSION, PROMPT_WORKER, 41);
      retainStatus();
    },
    retainStatus,
    clearCase(): void {
      __setConnectWorkerForTest(PROMPT_WORKER, null);
      stopAgentStatusHub();
      evictSessionWorker(PROMPT_SESSION);
    },
    async cleanup(): Promise<void> {
      __setConnectWorkerForTest(PROMPT_WORKER, null);
      stopAgentStatusHub();
      evictSessionWorker(PROMPT_SESSION);
      await opened.close();
      rmSync(workdir, { recursive: true, force: true });
    },
  };
}

export type AgentPromptTestFixture = Awaited<
  ReturnType<typeof startAgentPromptTestFixture>
>;
