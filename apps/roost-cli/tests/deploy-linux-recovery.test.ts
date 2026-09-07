// Linux journal tests pin source routability ordering and crash-safe phases.
// SSH operations are injected, while exact persisted journals come from the
// same parser and command schema used by production recovery.

import { describe, expect, test } from "bun:test";
import {
  _recoverLinuxDeployJournal,
} from "../src/deploy-linux-recovery.ts";
import {
  parseLinuxDeployJournalSnapshot,
  type LinuxDeployJournal,
} from "../src/linux-deploy-journal.ts";
import {
  HOME,
  KEEPER_UPDATE,
  PRIOR_SHA,
  ROLLOUT_ID,
  SHA,
  TARGET,
  WORKER_FINGERPRINT,
  fakeRemote,
  journalSnapshot,
} from "./deploy-linux-recovery-fixture.ts";

function parsed(phase: LinuxDeployJournal["phase"]) {
  return parseLinuxDeployJournalSnapshot(journalSnapshot({ phase }), HOME)!;
}

describe("Linux deploy recovery", () => {
  test("prepared recovery removes only the partial stage and clears the journal", async () => {
    const fixture = fakeRemote(parsed("prepared"), false);
    await expect(_recoverLinuxDeployJournal(fixture.remote))
      .resolves.toMatchObject({ kind: "prepared-cleaned" });
    expect(fixture.calls).toEqual(["load", `remove-${TARGET}`, "clear"]);
  });

  test("keeps a prior unit whose staged deploy has no journaled keeper update", () => {
    const bootstrap = parseLinuxDeployJournalSnapshot(journalSnapshot({
      phase: "prepared",
      keeperUpdate: null,
      workerFingerprint: null,
    }), HOME)!;

    expect(bootstrap.keeperUpdate).toBeNull();
    expect(bootstrap.priorUnit).not.toBeNull();
    expect(() => parseLinuxDeployJournalSnapshot(journalSnapshot({
      phase: "prepared",
      priorUnit: null,
      lifecycle: "stopped",
      keeperUpdate: KEEPER_UPDATE,
      workerFingerprint: WORKER_FINGERPRINT,
    }), HOME)).toThrow("keeper update requires a prior worker unit");
  });

  test("prepared fleet rollback cleans an originally stopped worker without service or keeper calls", async () => {
    const prepared = parseLinuxDeployJournalSnapshot(journalSnapshot({
      phase: "prepared",
      rolloutId: ROLLOUT_ID,
      lifecycle: "stopped",
    }), HOME)!;
    const fixture = fakeRemote(prepared, false);
    const directive = {
      action: "rollback" as const,
      rolloutId: ROLLOUT_ID,
      priorSha: PRIOR_SHA,
      workerFingerprint: WORKER_FINGERPRINT,
      targetSha: SHA,
      keeperUpdate: KEEPER_UPDATE,
    };
    await expect(_recoverLinuxDeployJournal(fixture.remote, directive))
      .resolves.toMatchObject({ kind: "prepared-cleaned" });
    expect(fixture.calls).toEqual(["load", `remove-${TARGET}`, "clear"]);
  });

  test("prepared fleet finalization and invalid ownership fail before cleanup", async () => {
    const prepared = parseLinuxDeployJournalSnapshot(journalSnapshot({
      phase: "prepared",
      rolloutId: ROLLOUT_ID,
    }), HOME)!;
    const directive = {
      action: "finalize" as const,
      rolloutId: ROLLOUT_ID,
      priorSha: PRIOR_SHA,
      workerFingerprint: WORKER_FINGERPRINT,
      targetSha: SHA,
      keeperUpdate: KEEPER_UPDATE,
    };
    const finalizeFixture = fakeRemote(prepared, false);
    await expect(_recoverLinuxDeployJournal(finalizeFixture.remote, directive))
      .rejects.toThrow("before activation");
    expect(finalizeFixture.calls).toEqual(["load"]);

    const unownedFixture = fakeRemote(prepared, false);
    await expect(_recoverLinuxDeployJournal(unownedFixture.remote))
      .rejects.toThrow("fleet rollout still owns");
    expect(unownedFixture.calls).toEqual(["load"]);

    const foreignFixture = fakeRemote(prepared, false);
    await expect(_recoverLinuxDeployJournal(foreignFixture.remote, {
      ...directive,
      action: "rollback",
      rolloutId: "22222222-2222-4222-8222-222222222222",
    })).rejects.toThrow("does not match the requested fleet rollout");
    expect(foreignFixture.calls).toEqual(["load"]);

    const wrongPriorFixture = fakeRemote(prepared, false);
    await expect(_recoverLinuxDeployJournal(wrongPriorFixture.remote, {
      ...directive,
      action: "rollback",
      priorSha: "d".repeat(40),
    })).rejects.toThrow("does not prove the fleet rollout prior identity");
    expect(wrongPriorFixture.calls).toEqual(["load"]);
  });

  test("rollback starts source before RPC, restarts it, and proves before cleanup", async () => {
    const fixture = fakeRemote(parsed("activated"), false);
    await expect(_recoverLinuxDeployJournal(fixture.remote))
      .resolves.toMatchObject({ kind: "prior-restored" });
    const sourceAction = fixture.calls.indexOf("keeper-replace-empty-source");
    expect(fixture.calls.slice(sourceAction - 2, sourceAction + 7)).toEqual([
      "stop-worker",
      "restore-present-stopped",
      "keeper-replace-empty-source",
      "stop-worker",
      "start-worker",
      `prove-prior-worker-${PRIOR_SHA}`,
      `prove-keeper-source-${PRIOR_SHA}`,
      "settle-prior",
      "prove-prior:started",
    ]);
    expect(fixture.calls.at(-1)).toBe("clear");
  });

  test("rolling-back reentry repeats the recorded source action", async () => {
    const journal = parsed("rolling-back");
    const fixture = fakeRemote(journal, false);
    await expect(_recoverLinuxDeployJournal(fixture.remote))
      .resolves.toMatchObject({ kind: "prior-restored" });
    expect(fixture.calls).not.toContain("checkpoint-rollback");
    expect(fixture.calls.filter(call => call === "keeper-replace-empty-source"))
      .toHaveLength(1);
  });

  test("committing is irreversible and repairs target before prior cleanup", async () => {
    const fixture = fakeRemote(parsed("committing"), true);
    await expect(_recoverLinuxDeployJournal(fixture.remote))
      .resolves.toMatchObject({ kind: "target-committed" });
    expect(fixture.calls).toEqual([
      "load", "stop-worker", "start-worker", "keeper-replace-empty-target",
      "stop-worker", "start-worker", "prove-target",
      `prove-keeper-target-${SHA}`, "cleanup-prior", "clear",
    ]);
  });

  test("journal carries the immutable keeper action", () => {
    expect(parsed("prepared").keeperUpdate).toEqual(KEEPER_UPDATE);
    expect(parsed("prepared").targetReleasePath).toBe(TARGET);
  });
});
