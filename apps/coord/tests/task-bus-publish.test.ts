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
import { openDb } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { makeTaskHandlers } from "../src/connect/handlers-tasks.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import { taskBus, type TaskBusMsg } from "../src/buses.ts";

let workdir: string;
let closeDb: () => Promise<void>;
let handlers: ReturnType<typeof makeTaskHandlers>;
// requireAuth only reads values.get(callerKey); a fake caller is enough.
const authCtx = { values: { get: () => ({ fingerprint: "fp-test" }) } } as any;

beforeAll(async () => {
  workdir = mkdtempSync(join(tmpdir(), "roost-taskbus-"));
  const opened = openDb(join(workdir, "test.db"));
  await runMigrations(opened.sqlite);
  closeDb = async () => { await opened.close(); };
  // Only `db` is read by the task handlers; a full ConnectDeps would drag in the transport.
  handlers = makeTaskHandlers({ db: opened.db } as unknown as ConnectDeps);
});

afterAll(async () => { await closeDb?.(); rmSync(workdir, { recursive: true, force: true }); });

function capture(): { msgs: TaskBusMsg[]; stop: () => void } {
  const msgs: TaskBusMsg[] = [];
  const stop = taskBus.subscribe((m) => msgs.push(m));
  return { msgs, stop };
}

async function enqueue(): Promise<string> {
  const resp = await handlers.tasksEnqueue({ payloadJson: "{}" } as any, authCtx);
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
    await handlers.tasksSetState({ id, state: "claimed" } as any, authCtx);
    cap.stop();
    expect(cap.msgs.some((m) => m.kind === "state" && m.task.id === id)).toBe(true);
  });

  test("cancel publishes a state delta", async () => {
    const id = await enqueue();
    const cap = capture();
    await handlers.tasksCancel({ id } as any, authCtx);
    cap.stop();
    expect(cap.msgs.some((m) => m.kind === "state" && m.task.id === id)).toBe(true);
  });

  test("nextPending claim publishes a state delta", async () => {
    await enqueue();
    const cap = capture();
    await handlers.tasksNextPending({} as any, authCtx);
    cap.stop();
    expect(cap.msgs.some((m) => m.kind === "state")).toBe(true);
  });
});
