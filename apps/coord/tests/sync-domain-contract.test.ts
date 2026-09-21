// Sync domain contract tests pin stable IDs, subscription defaults, and
// registered-worker projection fields that must survive the live firehose.
// They exercise protocol frames without opening a socket.

import { expect, test } from "bun:test";
import { SyncDomain } from "@roost/shared/proto/sync_pb";
import { WorkerUpdateStatus } from "@roost/shared/proto/wire_pb";
import type { Worker } from "@roost/shared/wire";
import { presenceFrame } from "../src/connect/sync-feed-frames.ts";
import {
  V2_DOMAINS,
  createSyncV2SocketState,
  isLazyDomain,
} from "../src/connect/sync-ws-v2-state.ts";

const SURVIVING_DOMAINS = [
  SyncDomain.TERMINAL,
  SyncDomain.WORKERS,
  SyncDomain.WORKSPACES,
  SyncDomain.TASKS,
  SyncDomain.MCP,
  SyncDomain.PAIR,
  SyncDomain.AUDIT,
] as const;

test("Sync advertises exactly the seven surviving stable domain IDs", () => {
  expect([...V2_DOMAINS]).toEqual([...SURVIVING_DOMAINS]);
  expect([...V2_DOMAINS]).toEqual([1, 2, 3, 4, 6, 7, 9]);
});

test("audit is the only lazy Sync domain in a fresh generation snapshot", () => {
  const state = createSyncV2SocketState();
  expect([...state.domains.keys()]).toEqual([...SURVIVING_DOMAINS]);
  for (const domain of SURVIVING_DOMAINS) {
    expect(state.domains.get(domain)).toMatchObject({
      ready: false,
      subscribed: domain !== SyncDomain.AUDIT,
    });
  }
  expect(V2_DOMAINS.filter(isLazyDomain)).toEqual([SyncDomain.AUDIT]);
});

test("registered worker frames retain live update progress", () => {
  const workerFp = "a".repeat(64) as Worker["fp"];
  const frame = presenceFrame({
    kind: "registered",
    worker: {
      fp: workerFp,
      label: "worker-a",
      os: "linux",
      host_identity: null,
      git_sha: "b".repeat(40),
      host_metrics: null,
      registered_at_ms: 1,
      last_seen_ms: 2,
      reachable_addr: null,
      keeper_runtime: null,
      terminal_core_capacity: null,
      update_operation: {
        jobId: "11111111-1111-4111-8111-111111111111",
        workerFp,
        host: "worker.example.test",
        revision: 3,
        targetGitSha: "c".repeat(40),
        source: "manual",
        status: "running",
        phase: "staging",
        reasonCode: null,
        message: "Installing",
        createdAtMs: 1,
        updatedAtMs: 2,
        startedAtMs: 2,
        completedAtMs: null,
        nextAttemptAtMs: null,
        exitCode: null,
      },
    },
  });
  expect(frame?.frame.case).toBe("workerPresence");
  if (frame?.frame.case !== "workerPresence") throw new Error("expected worker presence");
  expect(frame.frame.value.kind.case).toBe("registered");
  if (frame.frame.value.kind.case !== "registered") throw new Error("expected registered worker");
  expect(frame.frame.value.kind.value.updateOperation).toMatchObject({
    jobId: "11111111-1111-4111-8111-111111111111",
    status: WorkerUpdateStatus.RUNNING,
    revision: 3n,
  });
});
