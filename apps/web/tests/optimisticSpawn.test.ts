// optimisticSpawn.test.ts — the client-only placeholder registry behind the
// "instant +". Asserts: beginOptimisticSpawn inserts an OPEN shell placeholder
// (worker_fp/cwd copied from the anchor so it lands in the same folder bucket)
// and marks it pending; endOptimisticSpawn clears pending but leaves the session
// (the `opened` event owns removal); failOptimisticSpawn removes the session and
// clears pending; abortOptimisticSpawn marks wasAborted and removes the session.
// See src/store/optimisticSpawn.ts.

import { expect, test, describe, beforeEach, afterEach } from "bun:test";
import { asWorkerFp, asSessionId, asChannelId } from "@roost/shared/wire";
import type { Session } from "@roost/shared/wire";
import { rootStore, setRootStore } from "../src/store/root.ts";
import {
  beginOptimisticSpawn,
  endOptimisticSpawn,
  failOptimisticSpawn,
  abortOptimisticSpawn,
  isPendingSpawn,
  wasAborted,
  clearAborted,
} from "../src/store/optimisticSpawn.ts";

const FP = asWorkerFp("aa".repeat(32));

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
    agent: null,
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
  };
  beforeEach(clearSessions);
  afterEach(clearSessions);

  test("beginOptimisticSpawn inserts an open shell placeholder + marks it pending", () => {
    const a = anchor();
    const id = beginOptimisticSpawn(a);
    expect(isPendingSpawn(id)).toBe(true);
    const s = rootStore.sessions[id];
    expect(s).toBeTruthy();
    expect(s?.status).toBe("open");
    expect(s?.kind).toBe("shell");
    expect(s?.worker_fp).toBe(a.worker_fp);
    expect(s?.cwd).toBe(a.cwd);
    expect(s?.spawn_cwd).toBe(a.cwd); // folder bucket === anchor's (folderKeyOf = worker_fp::cwd)
    expect(s?.agent).toBeNull(); // D-3 invariant: shell ⇒ agent null
    endOptimisticSpawn(id); // clear the module-level pending set for the next test
  });

  test("endOptimisticSpawn clears pending without removing the session", () => {
    const id = beginOptimisticSpawn(anchor());
    endOptimisticSpawn(id);
    expect(isPendingSpawn(id)).toBe(false);
    // The real `opened` event replaces the value at this key; end must NOT delete it.
    expect(rootStore.sessions[id]).toBeTruthy();
  });

  test("failOptimisticSpawn removes the session and clears pending", () => {
    const id = beginOptimisticSpawn(anchor());
    failOptimisticSpawn(id, new Error("boom"));
    expect(isPendingSpawn(id)).toBe(false);
    expect(rootStore.sessions[id]).toBeUndefined();
  });

  test("abortOptimisticSpawn marks wasAborted and removes the session", () => {
    const id = beginOptimisticSpawn(anchor());
    abortOptimisticSpawn(id);
    expect(wasAborted(id)).toBe(true);
    expect(isPendingSpawn(id)).toBe(false);
    expect(rootStore.sessions[id]).toBeUndefined();
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
});
