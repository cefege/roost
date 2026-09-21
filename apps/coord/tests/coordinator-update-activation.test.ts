// Coordinator activation gate tests pin the V4 target boot fence without real
// timers or services. Only an exact finalizing transaction releases the shared
// mutation gate; malformed evidence keeps it held.

import { afterEach, beforeEach, expect, test } from "bun:test";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { CoordinatorWriteGate } from "../src/coordinator-write-gate.ts";
import { createCoordinatorActivationGate } from "../src/coordinator-update-activation.ts";

const TARGET_SHA = "a".repeat(40);
const PRIOR_SHA = "b".repeat(40);
const ROLLOUT_ID = "11111111-1111-4111-8111-111111111111";
let directory: string;
let journalPath: string;
let sourceRoot: string;

function journal(phase: "activating" | "finalizing") {
  return {
    schemaVersion: 4,
    phase,
    preparedAtMs: 1,
    rolloutId: ROLLOUT_ID,
    priorDefinitionBase64: "YQ==",
    priorDefinitionMode: 0o600,
    priorSha: PRIOR_SHA,
    targetSha: TARGET_SHA,
    servicePath: join(directory, "coord.service"),
    sourceReleasePath: join(directory, "prior"),
    stagingRepoPath: join(directory, "repo"),
    stagedReleasePath: sourceRoot,
    databasePath: join(directory, "coord.db"),
    databaseSnapshotPath: join(directory, "snapshot.db.gz"),
    databaseSnapshotSha256: "c".repeat(64),
  };
}

beforeEach(() => {
  directory = mkdtempSync(join(tmpdir(), "roost-activation-gate-"));
  journalPath = join(directory, "coordinator-deploy.json");
  sourceRoot = join(directory, "target");
  writeFileSync(journalPath, JSON.stringify(journal("activating")));
});

afterEach(() => rmSync(directory, { recursive: true, force: true }));

test("matching target stays fenced until exact finalizing checkpoint", async () => {
  const writeGate = new CoordinatorWriteGate();
  const scheduled: { current?: () => Promise<void> } = {};
  const activation = await createCoordinatorActivationGate({
    journalPath,
    sourceRoot,
    targetSha: TARGET_SHA,
    writeGate,
    scheduleRecheck: callback => {
      scheduled.current = callback;
      return () => { delete scheduled.current; };
    },
  });
  expect(activation.updateReady()).toBe(false);
  expect(activation.updateTransactionId()).toBe(ROLLOUT_ID);
  expect(() => writeGate.acquire()).toThrow();
  writeFileSync(journalPath, JSON.stringify(journal("finalizing")));
  if (!scheduled.current) throw new Error("activation recheck was not scheduled");
  await scheduled.current();
  expect(activation.updateReady()).toBe(true);
  writeGate.acquire().release();
  activation.dispose();
});

test("malformed evidence never releases the target gate", async () => {
  const writeGate = new CoordinatorWriteGate();
  const scheduled: { current?: () => Promise<void> } = {};
  const activation = await createCoordinatorActivationGate({
    journalPath,
    sourceRoot,
    targetSha: TARGET_SHA,
    writeGate,
    scheduleRecheck: callback => {
      scheduled.current = callback;
      return () => { delete scheduled.current; };
    },
  });
  writeFileSync(journalPath, "{}");
  if (!scheduled.current) throw new Error("activation recheck was not scheduled");
  await scheduled.current();
  expect(activation.updateReady()).toBe(false);
  expect(() => writeGate.acquire()).toThrow();
  activation.dispose();
});
