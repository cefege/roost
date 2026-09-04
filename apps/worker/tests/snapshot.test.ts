import { expect, test } from "bun:test";
import type { WorkerFp } from "@roost/shared/wire";
import { buildSnapshot } from "../src/snapshot.ts";
import type { SessionRecord } from "../src/session-record.ts";

const workerFp = "SHA256:snapshot-worker" as WorkerFp;

test("buildSnapshot copies once and preserves immutable spawn fields beside current metadata", () => {
  let copies = 0;
  const record = {
    sessionId: "00000000-0000-4000-8000-000000000001",
    channelId: 41,
    kind: "shell",
    cwd: "/current/folder",
    shellSpec: { cwd: "/original/folder" },
    spawnedAtMs: 1_700_000_000_123,
    git_branch: "main",
    git_remote: "roostorg/roost",
    pr: { number: 17, state: "open", checks: "passing", url: "https://example.test/pr/17" },
    ports: [4102, 5173],
  } as unknown as SessionRecord;
  const manager = {
    allSessions(): SessionRecord[] {
      copies += 1;
      return [record];
    },
  };

  const snapshot = buildSnapshot(manager, workerFp, 1_700_000_000_999);

  expect(copies).toBe(1);
  expect(snapshot.ts).toBe(1_700_000_000_999);
  expect(snapshot.sessions).toEqual([{
    id: record.sessionId,
    worker_fp: workerFp,
    channel: record.channelId,
    kind: "shell",
    cwd: "/current/folder",
    spawn_cwd: "/original/folder",
    workspace_id: null,
    status: "open",
    created_at: 1_700_000_000_123,
    closed_at: null,
    custom_title: null,
    git_branch: "main",
    git_remote: "roostorg/roost",
    pr_number: 17,
    pr_state: "open",
    pr_checks: "passing",
    pr_url: "https://example.test/pr/17",
    ports: [4102, 5173],
  }]);
});
