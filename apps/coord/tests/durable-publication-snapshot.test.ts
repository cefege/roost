// Verifies that a worker snapshot replaces only that worker's live routes without rewriting history.
// Bun discovers this suite directly and resets isolated SQLite and byte-hub state before each case.
// The contract depends on event projection, exact channel reconciliation, and input route lookup.

import { afterAll, beforeEach, describe, expect, test } from "bun:test";
import {
  evictSessionWorker,
  getCachedSessionWorker,
  isWorkerChannelIndexReconciled,
  lookupSessionId,
} from "../src/byte-hub.ts";
import { processInputControl, terminalViewerIdentity } from "../src/connect/session-control.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import { SessionEvent, asChannelId } from "@roost/shared/wire";
import { createDurablePublicationFixture } from "./durable-publication-fixture.ts";

const fixture = createDurablePublicationFixture({
  slug: "snapshot",
  primaryFingerprintByte: "d5",
  secondaryFingerprintByte: "d6",
  sessionGroup: "3",
});
const {
  FP,
  OTHER_FP,
  SID_A,
  SID_B,
  SID_C,
  DASHBOARD_ID,
  append,
  liveSession,
  openedEvent,
  snapshotEvent,
} = fixture;

let writer: typeof fixture.writer;

beforeEach(async () => {
  await fixture.reset();
  writer = fixture.writer;
});
afterAll(() => fixture.close());

describe("exact worker snapshot reconciliation", () => {
  test("removes stale/rebound live routes, keeps DB breadcrumbs", async () => {
    await append(openedEvent(SID_A, 11));
    await append(openedEvent(SID_B, 12));
    await append(openedEvent(SID_C, 13, OTHER_FP), OTHER_FP);

    // The returning worker announces only A, on a NEW channel.
    await append(snapshotEvent([liveSession(SID_A, 21)]));

    expect(lookupSessionId(FP, asChannelId(21))).toBe(SID_A);
    expect(getCachedSessionWorker(SID_A)).toEqual({ worker_fp: FP, channel: 21 });
    // Rebound: A's pre-restart channel no longer routes.
    expect(lookupSessionId(FP, asChannelId(11))).toBeUndefined();
    // Absent: B loses its live route and its input/claim route cache.
    expect(lookupSessionId(FP, asChannelId(12))).toBeUndefined();
    expect(getCachedSessionWorker(SID_B)).toBeUndefined();
    // Another worker's routes are untouched by this worker's snapshot.
    expect(lookupSessionId(OTHER_FP, asChannelId(13))).toBe(SID_C);
    expect(getCachedSessionWorker(SID_C)).toEqual({ worker_fp: OTHER_FP, channel: 13 });
    expect(isWorkerChannelIndexReconciled(FP)).toBe(true);
    expect(isWorkerChannelIndexReconciled(OTHER_FP)).toBe(false);

    // Breadcrumbs survive: the sidebar keeps showing where you were working.
    const rows = await writer.db.selectFrom("sessions").select(["id", "channel", "status"])
      .orderBy("id").execute();
    expect(rows).toEqual([
      { id: SID_A, channel: 21, status: "open" },
      { id: SID_B, channel: 12, status: "open" },
      { id: SID_C, channel: 13, status: "open" },
    ]);
  });

  test("snapshot upsert preserves immutable and coordinator-owned SQL fields", async () => {
    const workspaceId = "00000000-0000-4000-8000-000000000099";
    await writer.db.insertInto("workspaces").values({
      id: workspaceId,
      dashboard_id: DASHBOARD_ID,
      worker_fp: FP,
      name: "kept",
      folder_path: "/tmp",
      color: null,
      position: 0,
      version: 0,
      created_at_ms: 1,
      updated_at_ms: 1,
    }).onConflict((conflict) => conflict.column("id").doNothing()).execute();
    await append(openedEvent(SID_A, 11));
    await append(SessionEvent.parse({
      kind: "workspace_assigned",
      session_id: SID_A,
      workspace_id: workspaceId,
      ts: 2,
    }), null);
    await append(SessionEvent.parse({
      kind: "renamed",
      session_id: SID_A,
      custom_title: "kept title",
      ts: 3,
    }), null);

    await append(snapshotEvent([{
      ...liveSession(SID_A, 21),
      created_at: 9999,
      spawn_cwd: "/rewritten",
      workspace_id: null,
      custom_title: null,
    }]));

    const row = await writer.db.selectFrom("sessions")
      .select([
        "channel",
        "created_at",
        "spawn_cwd",
        "workspace_id",
        "custom_title",
        "dashboard_id",
      ])
      .where("id", "=", SID_A)
      .executeTakeFirstOrThrow();
    expect(row).toEqual({
      channel: 21,
      created_at: 1,
      spawn_cwd: "/tmp",
      workspace_id: workspaceId,
      custom_title: "kept title",
      dashboard_id: DASHBOARD_ID,
    });
  });

  test("input cannot repopulate a stale route from the open breadcrumb", async () => {
    await append(openedEvent(SID_B, 12));
    // processInputControl reads only db before route resolution; the remaining
    // router dependencies belong to unrelated RPC handlers.
    const deps = { db: writer.db } as unknown as ConnectDeps;
    const command = {
      identity: terminalViewerIdentity("f".repeat(64), "tab-1", undefined, DASHBOARD_ID),
      sessionId: SID_B,
      inputSeq: 1n,
      data: new TextEncoder().encode("ls\r"),
    };

    // Pre-reconcile (coord restarted under a live worker): the DB breadcrumb is
    // the only route source, so the batch reaches route resolution and fails on
    // the missing transport — NOT on an unknown session.
    evictSessionWorker(SID_B);
    const beforeReconcile = await processInputControl(deps, command);
    expect(beforeReconcile).toMatchObject({ status: "rejected", reason: "worker unavailable" });

    // After the worker declared its exact live set without B, B is offline and
    // the breadcrumb must not resurrect its pre-restart channel.
    await append(snapshotEvent([liveSession(SID_A, 21)]));
    const afterReconcile = await processInputControl(deps, { ...command, inputSeq: 2n });
    expect(afterReconcile).toMatchObject({ status: "rejected", reason: "unknown session" });
    expect(getCachedSessionWorker(SID_B)).toBeUndefined();
  });

  test("a foreign session inside a snapshot is not bound", async () => {
    const result = await append(
      snapshotEvent([liveSession(SID_A, 21), liveSession(SID_B, 22, OTHER_FP)]),
    );
    expect(result).toMatchObject({ admitted: false, inserted: false, published: false });
    expect(lookupSessionId(FP, asChannelId(21))).toBeUndefined();
    expect(lookupSessionId(FP, asChannelId(22))).toBeUndefined();
    expect(lookupSessionId(OTHER_FP, asChannelId(22))).toBeUndefined();
  });
});
