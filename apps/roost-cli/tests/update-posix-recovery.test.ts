// POSIX self-update recovery tests pin executable rollback and source RPC reachability.
// Real temporary files exercise durable journal/backup bytes; injected lifecycle
// and authenticated callback seams model worker routing and keeper convergence.

import { createHash } from "node:crypto";
import { existsSync } from "node:fs";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test } from "bun:test";
import type { JournaledKeeperUpdateCallbacks } from "../src/direct-keeper-update.ts";
import {
  loadPosixSelfUpdateJournal,
  writePosixSelfUpdateJournal,
  type PosixSelfUpdateJournalV3,
} from "../src/posix-self-update-journal.ts";
import {
  _continueAfterPosixSelfUpdateRecovery,
  recoverPosixSelfUpdateJournal,
  type PosixSelfUpdateRecoveryRuntime,
} from "../src/update-posix-rollout.ts";
import { LOCAL_KEEPER_UPDATE, WORKER_FINGERPRINT } from "./deploy-local-journal-fixture.ts";

function digest(bytes: string): string {
  return createHash("sha256").update(bytes).digest("hex");
}

test("self-update crash reentry repeats source action before exact proof and clear", async () => {
  const directory = await mkdtemp(join(tmpdir(), "roost-self-update-"));
  const executablePath = join(directory, "roost");
  const rollbackPath = `${executablePath}.rollback`;
  const journalPath = join(directory, "self-update.json");
  const sourceBytes = "source executable\n";
  const targetBytes = "target executable\n";
  const events: string[] = [];
  let failProof = true;
  let failCoordinatorProof = false;
  await writeFile(executablePath, sourceBytes, { mode: 0o755 });
  await writeFile(rollbackPath, sourceBytes, { mode: 0o755 });
  const journal: PosixSelfUpdateJournalV3 = {
    schema_version: 3,
    phase: "installed",
    created_at_ms: 1,
    target_version: "1.2.3",
    executable_path: executablePath,
    rollback_path: rollbackPath,
    source_binary_sha256: digest(sourceBytes),
    target_binary_sha256: digest(targetBytes),
    source_binary_mode: 0o755,
    source_worker_sha: "a".repeat(40),
    target_worker_sha: "b".repeat(40),
    prior_lifecycle: "running",
    prior_startup_policy: "enabled",
    prior_coord_lifecycle: "running",
    prior_coord_startup_policy: "enabled",
    worker_fingerprint: WORKER_FINGERPRINT,
    keeper_update: LOCAL_KEEPER_UPDATE,
  };
  await writePosixSelfUpdateJournal(journal, journalPath);
  const callbacks: JournaledKeeperUpdateCallbacks = {
    async apply(_fingerprint, _update, direction) {
      events.push(`rpc:${direction}`);
    },
    async prove(_fingerprint, _update, direction) {
      events.push(`prove:${direction}`);
      if (failProof) {
        failProof = false;
        throw new Error("injected proof crash");
      }
    },
  };
  const runtime: PosixSelfUpdateRecoveryRuntime = {
    callbacks,
    async stopWorker() { events.push("stop"); },
    async startWorker() { events.push("start"); },
    async stopCoordinator() { events.push("stop-coord"); },
    async startCoordinator() { events.push("start-coord"); },
    async proveCoordinator(_journal, expectedSha) {
      events.push(`prove-coord:${expectedSha}`);
      if (failCoordinatorProof) throw new Error("injected coordinator proof failure");
    },
    now: () => 100,
  };
  try {
    await expect(recoverPosixSelfUpdateJournal(runtime, journalPath))
      .rejects.toThrow("injected proof crash");
    // Simulate a crash after source proof removed the backup but before journal clear.
    await rm(rollbackPath, { force: true });
    expect(loadPosixSelfUpdateJournal(journalPath)?.phase).toBe("rolling_back");
    expect(await readFile(executablePath, "utf8")).toBe(sourceBytes);
    failCoordinatorProof = true;
    await expect(recoverPosixSelfUpdateJournal(runtime, journalPath))
      .rejects.toThrow("injected coordinator proof failure");
    expect(loadPosixSelfUpdateJournal(journalPath)?.phase).toBe("rolling_back");
    failCoordinatorProof = false;
    await expect(recoverPosixSelfUpdateJournal(runtime, journalPath))
      .resolves.toBe("source-restored");
    expect(events.filter(event => event === "rpc:source")).toHaveLength(2);
    expect(events.slice(-7)).toEqual([
      "start-coord", `prove-coord:${"a".repeat(40)}`, "start",
      "rpc:source", "stop", "start", "prove:source",
    ]);
    expect(loadPosixSelfUpdateJournal(journalPath)).toBeNull();
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("prepared self-update cleanup does not touch services or issue keeper RPC", async () => {
  const directory = await mkdtemp(join(tmpdir(), "roost-self-update-prepared-"));
  const executablePath = join(directory, "roost");
  const rollbackPath = `${executablePath}.rollback`;
  const journalPath = join(directory, "self-update.json");
  const sourceBytes = "source executable\n";
  const events: string[] = [];
  await writeFile(executablePath, sourceBytes, { mode: 0o755 });
  await writeFile(rollbackPath, sourceBytes, { mode: 0o755 });
  await writePosixSelfUpdateJournal({
    schema_version: 3,
    phase: "prepared",
    created_at_ms: 1,
    target_version: "1.2.3",
    executable_path: executablePath,
    rollback_path: rollbackPath,
    source_binary_sha256: digest(sourceBytes),
    target_binary_sha256: digest("target executable\n"),
    source_binary_mode: 0o755,
    source_worker_sha: "a".repeat(40),
    target_worker_sha: "b".repeat(40),
    prior_lifecycle: "running",
    prior_startup_policy: "enabled",
    prior_coord_lifecycle: "running",
    prior_coord_startup_policy: "enabled",
    worker_fingerprint: WORKER_FINGERPRINT,
    keeper_update: LOCAL_KEEPER_UPDATE,
  }, journalPath);
  const runtime: PosixSelfUpdateRecoveryRuntime = {
    callbacks: {
      async apply() { events.push("rpc"); },
      async prove() { events.push("prove"); },
    },
    async stopWorker() { events.push("stop-worker"); },
    async startWorker() { events.push("start-worker"); },
    async stopCoordinator() { events.push("stop-coord"); },
    async startCoordinator() { events.push("start-coord"); },
    async proveCoordinator() { events.push("prove-coord"); },
    now: () => 100,
  };
  try {
    await expect(recoverPosixSelfUpdateJournal(runtime, journalPath))
      .resolves.toBe("prepared-cleaned");
    expect(events).toEqual([]);
    expect(await readFile(executablePath, "utf8")).toBe(sourceBytes);
    expect(existsSync(rollbackPath)).toBe(false);
    expect(loadPosixSelfUpdateJournal(journalPath)).toBeNull();
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("source-restored recovery blocks a fresh self-update attempt", async () => {
  let continued = false;
  await expect(_continueAfterPosixSelfUpdateRecovery(
    async () => "source-restored",
    async () => {
      continued = true;
    },
  )).rejects.toThrow("refusing an automatic retry");
  expect(continued).toBe(false);
});
