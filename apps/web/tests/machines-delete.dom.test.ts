import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import type { Session, Worker, Workspace } from "@roost/shared/wire";
import {
  applyWorkerDeleteResponse,
  sessionWorkerIsOffline,
} from "../src/store/worker-removal.ts";
import { deleteStoreRecord, rootStore, setRootStore } from "../src/store/root.ts";
import { _handlePresenceEvent } from "../src/store/sync-handlers.ts";

const REMOVED_FP = "a".repeat(64);
const ACTIVE_FP = "b".repeat(64);
const SESSION_ID = "00000000-0000-4000-8000-000000000001";
const WORKSPACE_ID = "00000000-0000-4000-8000-000000000002";

function worker(fp: string, label: string): Worker {
  return {
    fp,
    label,
    os: "linux",
    git_sha: null,
    host_metrics: null,
    registered_at_ms: 1,
    last_seen_ms: Date.now(),
    reachable_addr: null,
    keeper_stale: null,
  } as Worker;
}

const removedWorker = worker(REMOVED_FP, "removed");
const activeWorker = worker(ACTIVE_FP, "active");
const retainedSession = {
  id: SESSION_ID,
  worker_fp: REMOVED_FP,
  status: "open",
} as Session;
const retainedWorkspace = {
  id: WORKSPACE_ID,
  name: "saved",
  session_ids: [SESSION_ID],
} as Workspace;

beforeEach(() => {
  setRootStore("workers", {
    [REMOVED_FP]: removedWorker,
    [ACTIVE_FP]: activeWorker,
  });
  setRootStore("sessions", { [SESSION_ID]: retainedSession });
  setRootStore("workspaces", { [WORKSPACE_ID]: retainedWorkspace });
});

afterEach(() => {
  deleteStoreRecord("workers", REMOVED_FP);
  deleteStoreRecord("workers", ACTIVE_FP);
  deleteStoreRecord("sessions", SESSION_ID);
  deleteStoreRecord("workspaces", WORKSPACE_ID);
});

describe("machine credential removal", () => {
  test("does not change the local replica when the coordinator returns ok=false", () => {
    expect(applyWorkerDeleteResponse(REMOVED_FP, { ok: false })).toBe(false);
    expect(rootStore.workers[REMOVED_FP]).toBe(removedWorker);
    expect(rootStore.sessions[SESSION_ID]).toBe(retainedSession);
    expect(rootStore.workspaces[WORKSPACE_ID]).toBe(retainedWorkspace);
  });

  test("removes only the confirmed worker and retains terminal/workspace history", () => {
    expect(applyWorkerDeleteResponse(REMOVED_FP, { ok: true })).toBe(true);
    expect(rootStore.workers[REMOVED_FP]).toBeUndefined();
    expect(rootStore.workers[ACTIVE_FP]).toBe(activeWorker);
    expect(rootStore.sessions[SESSION_ID]).toBe(retainedSession);
    expect(rootStore.workspaces[WORKSPACE_ID]).toBe(retainedWorkspace);
  });

  test("an active-only refresh and late heartbeat cannot restore the removed worker", () => {
    expect(applyWorkerDeleteResponse(REMOVED_FP, { ok: true })).toBe(true);

    // WorkersList refreshes contain active workers only. Publishing that roster
    // cannot reintroduce a tombstone, and a late heartbeat never creates a
    // worker record that is no longer present.
    setRootStore("workers", { [ACTIVE_FP]: activeWorker });
    _handlePresenceEvent({
      kind: "heartbeat",
      fp: REMOVED_FP,
      last_seen_ms: Date.now(),
      host_metrics: null,
    });

    expect(rootStore.workers[REMOVED_FP]).toBeUndefined();
    expect(rootStore.sessions[SESSION_ID]).toBe(retainedSession);
    expect(rootStore.workspaces[WORKSPACE_ID]).toBe(retainedWorkspace);
  });

  test("a retained session with no worker is offline so force-remove remains available", () => {
    expect(applyWorkerDeleteResponse(REMOVED_FP, { ok: true })).toBe(true);
    expect(sessionWorkerIsOffline(rootStore.workers[REMOVED_FP])).toBe(true);
  });
});
