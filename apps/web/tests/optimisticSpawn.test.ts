// Client-only optimistic spawn registry and authoritative settlement coverage.
// Tests pin placeholder insertion/removal, bounded mount measurement, abort
// fencing, and delayed admission: only success schedules a UI state report.
// See src/store/optimisticSpawn.ts.

import { expect, test, describe, beforeEach, afterEach, mock, vi } from "bun:test";
import { asWorkerFp, asSessionId, asChannelId } from "@roost/shared/wire";
import type { Session } from "@roost/shared/wire";
import { rootStore, setRootStore } from "../src/store/root.ts";
import {
  beginOptimisticSpawn,
  endOptimisticSpawn,
  failOptimisticSpawn,
  abortOptimisticSpawn,
  isPendingSpawn,
  isClientOnlyOptimisticSpawn,
  projectOptimisticSpawnMembership,
  publishMountedSpawnMeasurement,
  waitForMountedSpawnMeasurement,
  settleOptimisticSpawnAdmission,
  wasAborted,
  clearAborted,
  resetOptimisticSpawnState,
} from "../src/store/optimisticSpawn.ts";

const FP = asWorkerFp("aa".repeat(32));
const CLIENT_ONLY_RETENTION_LIMIT = 256;
const REPORT_DEBOUNCE_MS = 300;

function anchor(over: Partial<Session> = {}): Session {
  return {
    id: asSessionId("00000000-0000-4000-8000-000000000001"),
    worker_fp: FP,
    channel: asChannelId(7),
    kind: "shell",
    cwd: "/Users/you/roost",
    spawn_cwd: "/Users/you/roost",
    workspace_id: null,
    status: "open",
    created_at: 1000,
    closed_at: null,
    custom_title: null,
    ...over,
  } as Session;
}

describe("optimisticSpawn", () => {
  // rootStore is a module-level singleton shared across every test file in the
  // `bun test` process. A whole-record `setRootStore("sessions", {})` is a Solid
  // MERGE no-op (won't drop keys), so clear PER KEY (the projector's delete path)
  // both before and after — otherwise a placeholder we intentionally leave after
  // endOptimisticSpawn leaks into sibling suites (e.g. resolveSessionByFolder).
  const clearSessions = (): void => {
    for (const id of Object.keys(rootStore.sessions)) {
      setRootStore("sessions", id, undefined as unknown as Session);
    }
    resetOptimisticSpawnState();
  };
  beforeEach(clearSessions);
  afterEach(clearSessions);

  test("beginOptimisticSpawn inserts an open shell placeholder + marks it pending", () => {
    const a = anchor();
    const id = beginOptimisticSpawn(a);
    expect(isPendingSpawn(id)).toBe(true);
    expect(isClientOnlyOptimisticSpawn(id)).toBe(true);
    expect(projectOptimisticSpawnMembership([id, "authoritative-session"])).toEqual({
      authoritativeSessionIds: ["authoritative-session"],
      hasClientOnlySession: true,
    });
    const s = rootStore.sessions[id];
    expect(s).toBeTruthy();
    expect(s?.status).toBe("open");
    expect(s?.kind).toBe("shell");
    expect(s?.worker_fp).toBe(a.worker_fp);
    expect(s?.cwd).toBe(a.cwd);
    expect(s?.spawn_cwd).toBe(a.cwd); // folder bucket === anchor's (folderKeyOf = worker_fp::cwd)
    endOptimisticSpawn(id); // clear the module-level pending set for the next test
  });

  test("endOptimisticSpawn clears pending without removing the session", () => {
    const id = beginOptimisticSpawn(anchor());
    endOptimisticSpawn(id);
    expect(isPendingSpawn(id)).toBe(false);
    expect(isClientOnlyOptimisticSpawn(id)).toBe(false);
    expect(projectOptimisticSpawnMembership([id])).toEqual({
      authoritativeSessionIds: [id],
      hasClientOnlySession: false,
    });
    // The real `opened` event replaces the value at this key; end must NOT delete it.
    expect(rootStore.sessions[id]).toBeTruthy();
  });

  test("failOptimisticSpawn removes the session and clears pending", () => {
    const id = beginOptimisticSpawn(anchor());
    failOptimisticSpawn(id, new Error("boom"));
    expect(isPendingSpawn(id)).toBe(false);
    expect(isClientOnlyOptimisticSpawn(id)).toBe(true);
    expect(rootStore.sessions[id]).toBeUndefined();
  });

  test("abortOptimisticSpawn marks wasAborted and removes the session", () => {
    const id = beginOptimisticSpawn(anchor());
    abortOptimisticSpawn(id);
    expect(wasAborted(id)).toBe(true);
    expect(isPendingSpawn(id)).toBe(false);
    expect(rootStore.sessions[id]).toBeUndefined();
    expect(isClientOnlyOptimisticSpawn(id)).toBe(true);
    clearAborted(id);
    expect(wasAborted(id)).toBe(false);
  });

  test("each call mints a fresh id (two placeholders coexist)", () => {
    const a = beginOptimisticSpawn(anchor());
    const b = beginOptimisticSpawn(anchor());
    expect(a).not.toBe(b);
    expect(isPendingSpawn(a)).toBe(true);
    expect(isPendingSpawn(b)).toBe(true);
    endOptimisticSpawn(a);
    expect(isPendingSpawn(a)).toBe(false);
    expect(isPendingSpawn(b)).toBe(true); // clearing one leaves the other pending
    endOptimisticSpawn(b);
  });

  test("mounted measurement is a one-shot bounded initial PTY hint", async () => {
    const id = beginOptimisticSpawn(anchor());
    const waiting = waitForMountedSpawnMeasurement(id, 100);
    expect(publishMountedSpawnMeasurement(id, {
      cols: 101,
      rows: 37,
    })).toBe(true);
    expect(publishMountedSpawnMeasurement(id, {
      cols: 80,
      rows: 24,
    })).toBe(false);
    expect(await waiting).toEqual({ cols: 101, rows: 37 });
    endOptimisticSpawn(id);
    expect(isPendingSpawn(id)).toBe(false);
  });

  test("only success schedules after admission exceeds the 300 ms debounce", async () => {
    vi.useFakeTimers();
    const scheduleReport = mock(() => {});
    const settleAdmission = (
      id: string,
      admission: Promise<unknown>,
    ): Promise<void> => admission.then(
      () => settleOptimisticSpawnAdmission(
        id,
        { status: "admitted" },
        scheduleReport,
      ),
      (error) => settleOptimisticSpawnAdmission(
        id,
        { status: "rejected", error },
        scheduleReport,
      ),
    );
    try {
      const admittedId = beginOptimisticSpawn(anchor());
      const admitted = Promise.withResolvers<void>();
      const admittedSettlement = settleAdmission(admittedId, admitted.promise);
      vi.advanceTimersByTime(REPORT_DEBOUNCE_MS + 1);
      expect(isPendingSpawn(admittedId)).toBe(true);
      expect(scheduleReport).not.toHaveBeenCalled();
      admitted.resolve();
      await admittedSettlement;
      expect(isPendingSpawn(admittedId)).toBe(false);
      expect(isClientOnlyOptimisticSpawn(admittedId)).toBe(false);
      expect(scheduleReport).toHaveBeenCalledTimes(1);

      const rejectedId = beginOptimisticSpawn(anchor());
      const rejected = Promise.withResolvers<void>();
      const rejectedSettlement = settleAdmission(rejectedId, rejected.promise);
      vi.advanceTimersByTime(REPORT_DEBOUNCE_MS + 1);
      rejected.reject(new Error("admission rejected"));
      await rejectedSettlement;
      expect(isPendingSpawn(rejectedId)).toBe(false);
      expect(rootStore.sessions[rejectedId]).toBeUndefined();
      expect(isClientOnlyOptimisticSpawn(rejectedId)).toBe(true);
      expect(scheduleReport).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });

  test("bounds settled client-only spawn identities", () => {
    const retainedIds: string[] = [];
    for (let count = 0; count <= CLIENT_ONLY_RETENTION_LIMIT; count++) {
      const id = beginOptimisticSpawn(anchor());
      retainedIds.push(id);
      abortOptimisticSpawn(id);
      clearAborted(id);
    }
    expect(isClientOnlyOptimisticSpawn(retainedIds[0]!)).toBe(false);
    expect(isClientOnlyOptimisticSpawn(retainedIds.at(-1)!)).toBe(true);
  });

  test("aborting resolves an in-flight mount measurement without spawning", async () => {
    const id = beginOptimisticSpawn(anchor());
    const waiting = waitForMountedSpawnMeasurement(id, 100);
    abortOptimisticSpawn(id);
    expect(await waiting).toBeNull();
    expect(publishMountedSpawnMeasurement(id, {
      cols: 80,
      rows: 24,
    })).toBe(false);
    clearAborted(id);
  });
});
