// Agent-status wait handler tests pin account-device authorization before
// volatile admission, Connect error mapping, and exact response outcomes. A real
// migrated database supplies the same open-session boundary used by status reads.

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
  type AccountDeviceCaller,
} from "../src/connect/auth-interceptor.ts";
import { makeAgentStatusHandlers, type AgentStatusHandlers } from "../src/connect/handlers-agent-status.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import { openDb, type KyselyDB } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { ensureSelfHostedTenant } from "../src/self-hosted-tenant.ts";

const WORKER_A = asWorkerFp("a1".repeat(32));
const SESSION_A = asSessionId("50000000-0000-4000-8000-000000000001");
const SESSION_MISSING = asSessionId("50000000-0000-4000-8000-000000000099");
const STATUS_EPOCH = StatusEpoch.parse("60000000-0000-4000-8000-000000000001");
const OCCUPANT_ID = AgentOccupantId.parse("70000000-0000-4000-8000-000000000001");
const CALLER: AccountDeviceCaller = {
  kind: "account-device",
  fingerprint: "agent-wait-device-a",
  label: "wait test device",
  accountId: "agent-wait-account-a",
};

let workdir = "";
let db: KyselyDB;
let closeDb: () => Promise<void>;
let handlers: AgentStatusHandlers;

function callerContext(signal = new AbortController().signal): HandlerContext {
  const values = createContextValues();
  values.set(callerKey, CALLER);
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
  const tenant = ensureSelfHostedTenant(opened.sqlite, { backfillLegacyScopes: false });
  const now = Date.now();
  await db.insertInto("workers").values({
    fp: WORKER_A, dashboard_id: tenant.dashboardId, label: "A", os: "linux",
    git_sha: null, host_metrics_json: null, reachable_addr: null,
    registered_at_ms: now, last_seen_ms: now,
  }).execute();
  await db.insertInto("sessions")
    .values(openSession(SESSION_A, tenant.dashboardId, WORKER_A, 1))
    .execute();
  handlers = makeAgentStatusHandlers({
    db,
    selfHostedTenant: tenant,
  } as unknown as ConnectDeps);
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
      callerContext(),
    );
    expect(response.outcome).toBe("matched");
    expect(_agentStatusWaiterStats().total).toBe(0);
  });

  test("authorizes before validation and keeps the missing-session non-oracle", async () => {
    await expect(handlers.agentStatusWait(request(), anonymousContext()))
      .rejects.toMatchObject({ code: Code.Unauthenticated });
    // A malformed timeout on a missing session still answers not-found: the
    // session boundary runs before request validation, so a caller cannot use
    // error shape to probe which session ids exist.
    expect(await connectFailure(() => handlers.agentStatusWait(
      request({ sessionId: SESSION_MISSING, timeoutMs: 0 }),
      callerContext(),
    ))).toEqual({ code: Code.NotFound, message: "agent status not found" });
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
      await expect(handlers.agentStatusWait(value, callerContext()))
        .rejects.toMatchObject({ code: Code.InvalidArgument });
    }
    expect(_agentStatusWaiterStats().total).toBe(0);
  });

  test("maps abort to Canceled and per-session admission to ResourceExhausted", async () => {
    const controller = new AbortController();
    const canceled = handlers.agentStatusWait(
      request({ desiredStates: ["idle"] }),
      callerContext(controller.signal),
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
      callerContext(),
    )).rejects.toMatchObject({ code: Code.ResourceExhausted });
    const settled = Promise.allSettled(pending);
    stopAgentStatusHub();
    await settled;
    expect(_agentStatusWaiterStats().total).toBe(0);
  });
});
