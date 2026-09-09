// Regression guard for L11 "task state invisible cross-browser": every task
// UPDATE path (next-pending / set-state / cancel) MUST publish a taskBus
// `state` delta. The bug (docs/FAILURE-INDEX.md) was that
// enqueue published `created` but the UPDATE paths did their DB write and
// returned without publishing, so other browsers' QueueViews stayed stale
// until a manual refresh — and the Sync backfill can't recover in-memory bus
// deltas. Drives the REAL handlers + REAL taskBus over an in-memory DB.

import { describe, test, expect, beforeAll, afterAll } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { create } from "@bufbuild/protobuf";
import {
  createContextValues,
  type HandlerContext,
  type ServiceImpl,
} from "@connectrpc/connect";
import {
  CoordinatorService,
  TasksCancelRequestSchema,
  TasksEnqueueRequestSchema,
  TasksNextPendingRequestSchema,
  TasksSetStateRequestSchema,
} from "@roost/shared/proto/coordinator_pb";
import { openDb } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { makeTaskHandlers } from "../src/connect/handlers-tasks.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import { taskBus, type TaskBusMsg } from "../src/buses.ts";
import { callerKey } from "../src/connect/auth-interceptor.ts";
import { ensureSelfHostedTenant } from "../src/self-hosted-tenant.ts";
type TaskHandlers = Pick<
  ServiceImpl<typeof CoordinatorService>,
  "tasksEnqueue" | "tasksNextPending" | "tasksSetState" | "tasksCancel"
>;

const DEVICE_FP = "fp-test";

function deviceContext(accountId: string): HandlerContext {
  const values = createContextValues();
  values.set(callerKey, {
    kind: "account-device",
    fingerprint: DEVICE_FP,
    label: "task bus test device",
    accountId,
  });
  return { values } as unknown as HandlerContext;
}

let workdir: string;
let closeDb: () => Promise<void>;
let handlers: TaskHandlers;
let authCtx: HandlerContext;

beforeAll(async () => {
  workdir = mkdtempSync(join(tmpdir(), "roost-taskbus-"));
  const opened = openDb(join(workdir, "test.db"));
  const db = opened.db;
  await runMigrations(opened.sqlite);
  const tenant = ensureSelfHostedTenant(opened.sqlite, { backfillLegacyScopes: false });
  authCtx = deviceContext(tenant.accountId);
  const now = Date.now();
  await db.insertInto("authorized_keys").values({
    fingerprint: DEVICE_FP,
    public_key: new Uint8Array(32),
    label: "task bus test device",
    added_at: now,
  }).execute();
  await db.insertInto("account_devices").values({
    fingerprint: DEVICE_FP,
    account_id: tenant.accountId,
    added_at_ms: now,
    last_seen_at_ms: now,
  }).execute();
  closeDb = async () => { await opened.close(); };
  handlers = makeTaskHandlers({ db, selfHostedTenant: tenant } as unknown as ConnectDeps);
});

afterAll(async () => { await closeDb?.(); rmSync(workdir, { recursive: true, force: true }); });

function capture(): { msgs: TaskBusMsg[]; stop: () => void } {
  const msgs: TaskBusMsg[] = [];
  const stop = taskBus.subscribe((m) => msgs.push(m));
  return { msgs, stop };
}

async function enqueue(): Promise<string> {
  const resp = await handlers.tasksEnqueue(
    create(TasksEnqueueRequestSchema, { payloadJson: "{}" }),
    authCtx,
  );
  return resp.task?.id ?? "";
}

describe("task mutations publish taskBus deltas", () => {
  test("enqueue publishes a created delta", async () => {
    const cap = capture();
    const id = await enqueue();
    cap.stop();
    expect(cap.msgs.some((m) => m.kind === "created" && m.task.id === id)).toBe(true);
  });

  test("setState publishes a state delta", async () => {
    const id = await enqueue();
    const cap = capture();
    await handlers.tasksSetState(
      create(TasksSetStateRequestSchema, { id, state: "claimed" }),
      authCtx,
    );
    cap.stop();
    expect(cap.msgs.some((m) => m.kind === "state" && m.task.id === id)).toBe(true);
  });

  test("cancel publishes a state delta", async () => {
    const id = await enqueue();
    const cap = capture();
    await handlers.tasksCancel(create(TasksCancelRequestSchema, { id }), authCtx);
    cap.stop();
    expect(cap.msgs.some((m) => m.kind === "state" && m.task.id === id)).toBe(true);
  });

  test("nextPending claim publishes a state delta", async () => {
    await enqueue();
    const cap = capture();
    await handlers.tasksNextPending(create(TasksNextPendingRequestSchema), authCtx);
    cap.stop();
    expect(cap.msgs.some((m) => m.kind === "state")).toBe(true);
  });
});
