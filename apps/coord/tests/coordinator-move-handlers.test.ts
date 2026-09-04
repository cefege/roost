import { expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import { Code, type HandlerContext } from "@connectrpc/connect";
import { CoordinatorMoveStatusRequestSchema } from "@roost/shared/proto/coordinator_pb";
import { makeCoordinatorMoveHandlers } from "../src/connect/handlers-coordinator-move.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import type { HandoffState } from "../src/coord-move/state.ts";

const DASHBOARD_ID = "coordinator-move-handlers-dashboard";
const handoff: HandoffState = {
  version: 1,
  handoff_id: "00000000-0000-4000-8000-000000000001",
  role: "SOURCE",
  dashboard_id: DASHBOARD_ID,
  phase: "COMMITTED",
  source_url: "https://source.ts.net:4102",
  target_url: "https://target.ts.net:4102",
  target_worker_fp: "target",
  expected_worker_fps: ["target"],
  commit_acked_worker_fps: ["target"],
  expected_coord_kid: "kid",
  expected_git_sha: "sha",
  secret_sha256: "a".repeat(64),
  secret: "secret",
  started_at_ms: 1,
  updated_at_ms: 1,
};

function authedContext(): HandlerContext {
  // One object satisfies the typed account principal and selected actor keys.
  return {
    values: {
      get: () => ({
        kind: "account-device",
        fingerprint: "fp",
        label: "l",
        accountId: "account",
        organizationId: "organization",
        dashboardId: DASHBOARD_ID,
        organizationRole: "owner",
        dashboardRole: "admin",
        deviceFingerprint: "fp",
      }),
    },
  } as unknown as HandlerContext;
}

function anonymousContext(): HandlerContext {
  return { values: { get: () => null } } as unknown as HandlerContext;
}

test("coordinator move status requires auth and matches only its exact handoff", async () => {
  const handlers = makeCoordinatorMoveHandlers({
    move: {
      status: (dashboardId: string, handoffId: string) =>
        dashboardId === DASHBOARD_ID && handoffId === handoff.handoff_id ? handoff : null,
      statusForWorker: async (handoffId: string, workerFp: string) =>
        workerFp === "fp" && handoffId === handoff.handoff_id ? handoff : null,
    },
    cfg: { saasMode: false },

  } as unknown as ConnectDeps);
  await expect(handlers.coordinatorMoveStatus(
    create(CoordinatorMoveStatusRequestSchema, { handoffId: handoff.handoff_id }),
    authedContext(),
  )).resolves.toMatchObject({
    sourceUrl: handoff.source_url,
    targetUrl: handoff.target_url,
  });

  await expect(handlers.coordinatorMoveStatus(
    create(CoordinatorMoveStatusRequestSchema, { handoffId: "00000000-0000-4000-8000-000000000002" }),
    authedContext(),
  )).rejects.toMatchObject({ code: Code.NotFound });

  await expect(handlers.coordinatorMoveStatus(
    create(CoordinatorMoveStatusRequestSchema, { handoffId: handoff.handoff_id }),
    anonymousContext(),
  )).rejects.toMatchObject({ code: Code.Unauthenticated });
});
