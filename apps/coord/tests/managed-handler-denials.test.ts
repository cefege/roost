import { expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import { Code, type HandlerContext } from "@connectrpc/connect";
import {
  CoordinatorMovePreflightRequestSchema,
  CoordinatorMoveStartRequestSchema,
  CoordinatorMoveStatusRequestSchema,
  MiscDbExportUrlRequestSchema,
  WorkersDeployOutputRequestSchema,
  WorkersDeployStartRequestSchema,
} from "@roost/shared/proto/coordinator_pb";
import {
  callerKey,
  dashboardActorKey,
  listenerTrustKey,
  onHostKey,
  remoteAddressKey,
} from "../src/connect/auth-interceptor.ts";
import { makeCoordinatorMoveHandlers } from "../src/connect/handlers-coordinator-move.ts";
import { makeSystemHandlers } from "../src/connect/handlers-system.ts";
import { makeWorkerHandlers } from "../src/connect/handlers-workers.ts";
import type { ConnectDeps } from "../src/connect/router.ts";

const EMPTY_CONTEXT = {
  values: { get: () => undefined },
} as unknown as HandlerContext;

const MANAGED_DEPS = {
  cfg: { saasMode: true, bind: "127.0.0.1:4103" },
} as ConnectDeps;

test("managed handler layer denies coordinator move and database export before dependencies or auth", async () => {
  const move = makeCoordinatorMoveHandlers(MANAGED_DEPS);
  const system = makeSystemHandlers(MANAGED_DEPS);
  const calls = [
    move.coordinatorMovePreflight(
      create(CoordinatorMovePreflightRequestSchema, { targetWorkerFp: "worker" }),
      EMPTY_CONTEXT,
    ),
    move.coordinatorMoveStart(
      create(CoordinatorMoveStartRequestSchema, { targetWorkerFp: "worker" }),
      EMPTY_CONTEXT,
    ),
    move.coordinatorMoveStatus(
      create(CoordinatorMoveStatusRequestSchema, { handoffId: "handoff" }),
      EMPTY_CONTEXT,
    ),
    system.miscDbExportUrl(create(MiscDbExportUrlRequestSchema), EMPTY_CONTEXT),
  ];

  for (const call of calls) {
    await expect(call).rejects.toMatchObject({ code: Code.PermissionDenied });
  }
});

test("managed handler layer denies both coordinator-originated worker deploy methods", async () => {
  const workers = makeWorkerHandlers(MANAGED_DEPS);
  await expect(workers.workersDeployStart(
    create(WorkersDeployStartRequestSchema, { host: "worker" }),
    EMPTY_CONTEXT,
  )).rejects.toMatchObject({ code: Code.PermissionDenied });

  const output = workers.workersDeployOutput(
    create(WorkersDeployOutputRequestSchema, { jobId: "job" }),
    EMPTY_CONTEXT,
  );
  await expect(output[Symbol.asyncIterator]().next())
    .rejects.toMatchObject({ code: Code.PermissionDenied });
});

test("self-hosted worker deployment still reaches the existing resolver", async () => {
  const caller = {
    kind: "account-device" as const,
    fingerprint: "admin-device",
    label: "Admin",
    accountId: "account",
  };
  const actor = {
    accountId: caller.accountId,
    organizationId: "organization",
    dashboardId: "dashboard",
    organizationRole: "owner" as const,
    dashboardRole: "admin" as const,
    deviceFingerprint: caller.fingerprint,
  };
  const context = {
    values: {
      get: (key: unknown) => {
        if (key === callerKey) return caller;
        if (key === dashboardActorKey) return actor;
        return undefined;
      },
    },
  } as unknown as HandlerContext;
  const query = {
    select: () => query,
    where: () => query,
    execute: async () => [],
  };
  const handlers = makeWorkerHandlers({
    cfg: { saasMode: false },
    db: { selectFrom: () => query },
  } as unknown as ConnectDeps);

  await expect(handlers.workersDeployStart(
    create(WorkersDeployStartRequestSchema, { host: "missing-worker" }),
    context,
  )).resolves.toMatchObject({ ok: false, error: "worker not found" });
});

test("self-hosted on-host database export URL remains available", async () => {
  const caller = {
    kind: "account-device" as const,
    fingerprint: "browser-fp",
    label: "Browser",
    accountId: "account",
  };
  const context = {
    values: {
      get: (key: unknown) => {
        if (key === callerKey) return caller;
        if (key === onHostKey) return true;
        if (key === remoteAddressKey) return "127.0.0.1";
        if (key === listenerTrustKey) return "direct";
        return undefined;
      },
    },
  } as unknown as HandlerContext;
  const handlers = makeSystemHandlers({
    cfg: { saasMode: false, bind: "127.0.0.1:4199" },
  } as ConnectDeps);

  await expect(handlers.miscDbExportUrl(
    create(MiscDbExportUrlRequestSchema),
    context,
  )).resolves.toMatchObject({ url: "http://127.0.0.1:4199/api/db-export" });
});
