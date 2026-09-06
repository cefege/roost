// macOS journal tests pin confinement and routable keeper-action recovery.
// Launchd operations are injected while the production parser and recovery
// state machine own phase transitions and exact lifecycle settlement.

import { describe, expect, test } from "bun:test";
import {
  _isConfinedMacosReleasePath,
  _macosDeployJournalPath,
  _parseMacosDeployJournal,
} from "../src/deploy-macos-journal.ts";
import { _recoverMacosDeployJournal } from "../src/deploy-macos-recovery.ts";
import {
  MACOS_KEEPER_UPDATE,
  MACOS_WORKER_FINGERPRINT,
} from "./deploy-macos-keeper-update-fixture.ts";
import {
  PRIOR_SHA,
  RELEASE_ID,
  RELEASE_PATH,
  RELEASE_ROOT,
  ROLLOUT_ID,
  SHA,
  fakeRemote,
  journal,
} from "./deploy-macos-recovery-fixture.ts";

describe("remote macOS deploy journal recovery", () => {
  test("prepared recovery removes only the partial stage and clears the journal", async () => {
    const fixture = fakeRemote(journal({ phase: "prepared", priorLifecycle: "unloaded" }));
    await expect(_recoverMacosDeployJournal(fixture.remote))
      .resolves.toMatchObject({ outcome: "prepared-cleaned" });
    expect(fixture.calls).toEqual(["load", "remove-target", "clear"]);
  });

  test("prepared fleet rollback cleans an originally unloaded worker without service or keeper calls", async () => {
    const durable = journal({
      phase: "prepared",
      rolloutId: ROLLOUT_ID,
      priorLifecycle: "unloaded",
    });
    const fixture = fakeRemote(durable);
    const directive = {
      action: "rollback" as const,
      rolloutId: ROLLOUT_ID,
      priorSha: PRIOR_SHA,
      workerFingerprint: MACOS_WORKER_FINGERPRINT,
      targetSha: SHA,
      keeperUpdate: MACOS_KEEPER_UPDATE,
    };
    await expect(_recoverMacosDeployJournal(fixture.remote, directive))
      .resolves.toMatchObject({ outcome: "prepared-cleaned" });
    expect(fixture.calls).toEqual(["load", "remove-target", "clear"]);
  });

  test("prepared fleet finalization and invalid ownership fail before cleanup", async () => {
    const durable = journal({ phase: "prepared", rolloutId: ROLLOUT_ID });
    const directive = {
      action: "finalize" as const,
      rolloutId: ROLLOUT_ID,
      priorSha: PRIOR_SHA,
      workerFingerprint: MACOS_WORKER_FINGERPRINT,
      targetSha: SHA,
      keeperUpdate: MACOS_KEEPER_UPDATE,
    };
    const finalizeFixture = fakeRemote(durable);
    await expect(_recoverMacosDeployJournal(finalizeFixture.remote, directive))
      .rejects.toThrow("before activation");
    expect(finalizeFixture.calls).toEqual(["load"]);

    const unownedFixture = fakeRemote(durable);
    await expect(_recoverMacosDeployJournal(unownedFixture.remote))
      .rejects.toThrow("fleet rollout still owns");
    expect(unownedFixture.calls).toEqual(["load"]);

    const foreignFixture = fakeRemote(durable);
    await expect(_recoverMacosDeployJournal(foreignFixture.remote, {
      ...directive,
      action: "rollback",
      rolloutId: "22222222-2222-4222-8222-222222222222",
    })).rejects.toThrow("does not match the requested fleet rollout");
    expect(foreignFixture.calls).toEqual(["load"]);

    const wrongPriorFixture = fakeRemote(durable);
    await expect(_recoverMacosDeployJournal(wrongPriorFixture.remote, {
      ...directive,
      action: "rollback",
      priorSha: "d".repeat(40),
    })).rejects.toThrow("does not prove the fleet rollout prior identity");
    expect(wrongPriorFixture.calls).toEqual(["load"]);
  });

  test("uses one journal beside the machine transaction database", () => {
    expect(_macosDeployJournalPath(
      "Library/Application Support/RoostWorkerV2/service/machine-transaction.sqlite",
    )).toBe("Library/Application Support/RoostWorkerV2/service/macos-worker-deploy-v1.json");
  });

  test("accepts only the exact release path and rejects unrestorable launchd state", () => {
    expect(_isConfinedMacosReleasePath(RELEASE_ROOT, RELEASE_PATH, SHA)).toBe(true);
    for (const path of [
      `${RELEASE_ROOT}-attacker/${RELEASE_ID}`,
      `${RELEASE_ROOT}/${RELEASE_ID}/../../victim`,
      `${RELEASE_PATH}/nested`,
    ]) {
      expect(_isConfinedMacosReleasePath(RELEASE_ROOT, path, SHA)).toBe(false);
    }
    expect(() => _parseMacosDeployJournal(journal({
      priorLifecycle: "loaded",
      priorDisabled: false,
    }), RELEASE_ROOT)).toThrow("enabled KeepAlive worker cannot have a durable loaded lifecycle");
  });

  test("rollback makes source routable, invokes RPC, restarts, proves, then settles", async () => {
    const fixture = fakeRemote(journal({ phase: "activated" }));
    await expect(_recoverMacosDeployJournal(fixture.remote))
      .resolves.toMatchObject({ outcome: "rolled-back" });
    const action = fixture.calls.indexOf("keeper:source:preserve");
    expect(action).toBeGreaterThan(0);
    expect(fixture.calls.slice(action - 3, action + 7)).toEqual([
      "disabled:false", "bootstrap", "kickstart",
      "keeper:source:preserve",
      "bootout", "disabled:false", "bootstrap", "kickstart",
      `prove-keeper:source:${PRIOR_SHA}`, "bootout",
    ]);
    expect(fixture.calls.slice(-3)).toEqual(["prove-prior", "remove-target", "clear"]);
  });

  test("rolling-back crash reentry repeats the source action", async () => {
    const fixture = fakeRemote(journal({ phase: "rolling-back" }));
    await expect(_recoverMacosDeployJournal(fixture.remote))
      .resolves.toMatchObject({ outcome: "rolled-back" });
    expect(fixture.calls).not.toContain("checkpoint-rollback");
    expect(fixture.calls.filter(call => call === "keeper:source:preserve")).toHaveLength(1);
  });

  test("committing repairs and proves target before irreversible cleanup", async () => {
    const fixture = fakeRemote(journal({ phase: "committing" }), {
      definitionMatches: true,
      running: true,
      result: { exit: 0, stdout: "state = running\n", stderr: "" },
    });
    await expect(_recoverMacosDeployJournal(fixture.remote))
      .resolves.toMatchObject({ outcome: "committed" });
    expect(fixture.calls).toEqual([
      "load", "bootout", "disabled:false", "bootstrap", "kickstart",
      "keeper:target:preserve", "bootout", "disabled:false", "bootstrap", "kickstart",
      "prove-target", `prove-keeper:target:${SHA}`, "cleanup-prior", "clear",
    ]);
  });
});
