// Canonical pane-layout membership tests.
// The selector is shared by deck rendering, UI state/commands, and portable
// imports so worker/folder, lifecycle, close, and ordering rules cannot drift.

import { beforeEach, describe, expect, test } from "bun:test";
import {
  asChannelId,
  asSessionId,
  asWorkerFp,
} from "@roost/shared/wire";
import type { Session, WorkerFp } from "@roost/shared/wire";
import {
  deleteStoreRecord,
  rootStore,
  setRootStore,
} from "../src/store/root.ts";
import { liveSessionIdsForFolder } from "../src/store/selectors.ts";
import { folderKeyOf } from "../src/lib/folderKey.ts";
import {
  resetPendingCloses,
  scheduleClose,
  undoOne,
} from "../src/lib/pendingClose.ts";

const SESSION_A = "00000000-0000-4000-8000-000000000001";
const SESSION_B = "00000000-0000-4000-8000-000000000002";
const SESSION_C = "00000000-0000-4000-8000-000000000003";
const SESSION_D = "00000000-0000-4000-8000-000000000004";
const SESSION_E = "00000000-0000-4000-8000-000000000005";
const SESSION_F = "00000000-0000-4000-8000-000000000006";
const WORKER_A = asWorkerFp("aa".repeat(32));
const WORKER_B = asWorkerFp("bb".repeat(32));

function session(
  id: string,
  workerFp: WorkerFp,
  cwd: string,
  createdAt: number,
  status: "open" | "closed" = "open",
): Session {
  return {
    id: asSessionId(id),
    worker_fp: workerFp,
    channel: asChannelId(1),
    kind: "shell",
    cwd,
    spawn_cwd: cwd,
    workspace_id: null,
    status,
    created_at: createdAt,
    closed_at: status === "closed" ? createdAt + 1 : null,
    custom_title: null,
  };
}

beforeEach(() => {
  resetPendingCloses();
  for (const sessionId of Object.keys(rootStore.sessions)) {
    deleteStoreRecord("sessions", sessionId);
  }
});

describe("liveSessionIdsForFolder", () => {
  test("selects open same-worker/folder sessions in canonical order", () => {
    const sessions = [
      session(SESSION_C, WORKER_A, "/work", 20),
      session(SESSION_B, WORKER_A, "/work", 10),
      session(SESSION_A, WORKER_A, "/work", 10),
      session(SESSION_D, WORKER_A, "/elsewhere", 5),
      session(SESSION_E, WORKER_B, "/work", 1),
      session(SESSION_F, WORKER_A, "/work", 2, "closed"),
    ];
    for (const current of sessions) setRootStore("sessions", current.id, current);
    const folderKey = folderKeyOf(sessions[0]!);

    expect(liveSessionIdsForFolder(folderKey)).toEqual([
      SESSION_A,
      SESSION_B,
      SESSION_C,
    ]);
    scheduleClose(
      SESSION_B,
      { terminalName: "B", folder: "work", server: "worker-a" },
      () => {},
    );
    expect(liveSessionIdsForFolder(folderKey)).toEqual([SESSION_A, SESSION_C]);
    undoOne(SESSION_B);
  });
});
