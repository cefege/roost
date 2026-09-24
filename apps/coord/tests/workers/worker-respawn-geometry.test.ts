// A respawn dispatched at a hardcoded 80x24 makes every attached TUI redraw
// twice and briefly contradicts the SCD. These tests pin that the respawn
// command carries the session's live effective geometry when views exist, and
// only falls back to 80x24 when nothing is watching.

import { afterEach, expect, test } from "bun:test";
import { CoordinatorWriteGate } from "../../src/coordinator-write-gate.ts";
import { respawnMissingForWorker } from "../../src/workers/worker-respawn.ts";
import {
  __setConnectWorkerForTest,
  type WorkerHandle,
} from "../../src/workers/worker-registry.ts";
import { installTerminalViewHub } from "../../src/terminal/view/terminal-view-hub.ts";
import {
  SESSION,
  VIEW_A,
  VIEW_B,
  disposeHubs,
  makeHarness,
  register,
  settle,
  viewCommand,
} from "../terminal/view/terminal-view-hub-harness.ts";
import { databaseWithOpenSession } from "./worker-respawn-harness.ts";

const WORKER_FP = "b".repeat(64);

afterEach(() => {
  __setConnectWorkerForTest(WORKER_FP, null);
  installTerminalViewHub(null);
  disposeHubs();
});

async function dispatchedRespawnFrames(): Promise<Array<Record<string, unknown>>> {
  const frames: Array<Record<string, unknown>> = [];
  const worker: WorkerHandle = {
    workerFp: WORKER_FP,
    processEpoch: null,
    connectionGeneration: "respawn-geometry-test-connection",
    capabilities: new Set(),
    revoked: false,
    ready: true,
    send: (frame) => {
      if (frame.frame.case !== "browserCommand") {
        throw new Error(`expected a browser command, got ${String(frame.frame.case)}`);
      }
      frames.push(JSON.parse(frame.frame.value.frameJson) as Record<string, unknown>);
      return 1;
    },
  };
  __setConnectWorkerForTest(WORKER_FP, worker);
  await respawnMissingForWorker(
    databaseWithOpenSession(SESSION) as never,
    WORKER_FP,
    worker,
    new CoordinatorWriteGate(),
  );
  return frames;
}

test("respawns at the session's live effective geometry", async () => {
  const { hub } = makeHarness();
  installTerminalViewHub(hub);
  register(hub, "socket-a", "viewer-a", "fingerprint-a");
  register(hub, "socket-b", "viewer-b", "fingerprint-b");
  hub.handleViewCommand("socket-a", viewCommand(VIEW_A, 1n, { cols: 100, rows: 30 }));
  hub.handleViewCommand("socket-b", viewCommand(VIEW_B, 1n, { cols: 90, rows: 40 }));
  await settle();
  expect(hub.snapshot(SESSION)?.effective).toEqual({ cols: 90, rows: 30 });

  expect(await dispatchedRespawnFrames()).toEqual([{
    kind: "respawn-if-missing",
    request_id: expect.any(String),
    session_id: SESSION,
    cwd: "/tmp",
    cols: 90,
    rows: 30,
  }]);
});

test("respawns an unwatched session at 80x24", async () => {
  const { hub } = makeHarness();
  installTerminalViewHub(hub);

  expect(hub.snapshot(SESSION)).toBeNull();
  expect(await dispatchedRespawnFrames()).toMatchObject([{ cols: 80, rows: 24 }]);
});
