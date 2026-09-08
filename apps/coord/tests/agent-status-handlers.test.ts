// Observed-agent read handler contract: durable dashboard authorization precedes
// volatile hub lookup, retained status remains readable while a worker is offline,
// and public projections expose promptability only for identified integrations
// whose occupant is still present.
// The focused router check also guards the single-service composition boundary.
// The migrated dashboard/session rows, status builders and per-test hub arming
// live in ./agent-status-handlers-test-fixture.ts.

import { create } from "@bufbuild/protobuf";
import { Code } from "@connectrpc/connect";
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, test } from "bun:test";
import {
  AgentStatusGetRequestSchema,
  AgentStatusListRequestSchema,
} from "@roost/shared/proto/coordinator_pb";
import { evictSessionWorker } from "../src/byte-hub.ts";
import { makeConnectBunHandler } from "../src/connect/bun-handler.ts";
import type { AgentStatusHandlers } from "../src/connect/handlers-agent-status.ts";
import { buildConnectRouter, type ConnectDeps } from "../src/connect/router.ts";
import type { KyselyDB } from "../src/db/connection.ts";
import {
  ACTOR_A,
  ACTOR_B,
  SESSION_FOREIGN,
  SESSION_INTEGRATION,
  SESSION_LEGACY,
  SESSION_MISSING,
  SESSION_NO_STATUS,
  SESSION_SCREEN,
  STATUS_EPOCH,
  WORKER_A,
  actorContext,
  anonymousContext,
  identity,
  notFoundFrom,
  restartAgentStatusHubWithRetainedFixtures,
  retain,
  startAgentStatusHandlersTestFixture,
  status,
  stopAgentStatusHubAndEvictFixtures,
  type AgentStatusHandlersTestFixture,
} from "./agent-status-handlers-test-fixture.ts";

let fixture: AgentStatusHandlersTestFixture;
let db: KyselyDB;
let handlers: AgentStatusHandlers;

beforeAll(async () => {
  fixture = await startAgentStatusHandlersTestFixture();
  db = fixture.db;
  handlers = fixture.handlers;
});

beforeEach(() => {
  restartAgentStatusHubWithRetainedFixtures();
});

afterEach(() => {
  stopAgentStatusHubAndEvictFixtures();
});

afterAll(async () => {
  await fixture?.close();
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

  test("a released integration occupant is readable but never promptable", async () => {
    retain(WORKER_A, status(SESSION_INTEGRATION, {
      ...identity("11111111-1111-4111-8111-111111111111", "integration"),
      state: "idle",
      revision: 8,
      completed_revision: 4,
      occupant_exited: true,
    }));
    const response = await handlers.agentStatusGet(
      create(AgentStatusGetRequestSchema, { sessionId: SESSION_INTEGRATION }),
      actorContext(ACTOR_A),
    );

    expect(response.status).toMatchObject({
      revision: 8n,
      completedRevision: 4n,
      source: "integration",
      active: true,
      promptable: false,
    });
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
      cfg: {},
    } as unknown as ConnectDeps);
    const connectHandler = makeConnectBunHandler(router);
    for (const rpcMethod of [
      "AgentStatusGet",
      "AgentStatusList",
      "AgentStatusWait",
      "SessionsPrompt",
    ]) {
      const path = `/roost.v1.CoordinatorService/${rpcMethod}`;
      expect(connectHandler.matches(path)).toBe(true);
      expect([...router.handlers].filter((handler) => handler.requestPath === path))
        .toHaveLength(1);
    }
  });
});
