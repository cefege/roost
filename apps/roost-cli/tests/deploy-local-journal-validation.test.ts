// Strict local journal validation tests pin schema cutover, confinement, and
// coordinator ownership before any recovery callback can mutate the machine.
// Canonical snapshots and keeper contracts come from the sibling fixture.

import { describe, expect, test } from "bun:test";
import {
  _recoverLocalWorkerDeployJournal,
  localWorkerDeployJournalPath,
  localWorkerDeployStageIsConfined,
  parseLocalWorkerDeployJournal,
  type LocalWorkerDeployRecoveryDeps,
} from "../src/local-worker-deploy-journal.ts";
import { coordinatorJournalAllowsLocalWorkerRollout } from "../src/local-worker-rollout-coordinator.ts";
import type { CoordinatorDeployJournalV2 } from "../src/coordinator-deploy-journal.ts";
import {
  LOCAL_CONFINEMENT,
  LOCAL_KEEPER_UPDATE,
  PRIOR_SHA,
  RELEASE_ROOT,
  ROLLOUT_ID,
  STAGED_RELEASE,
  WORKER_FINGERPRINT,
  TARGET_SHA,
  localWorkerJournal,
} from "./deploy-local-journal-fixture.ts";

function mutationCountingDeps(onMutation: () => void): LocalWorkerDeployRecoveryDeps {
  return {
    readService: () => { onMutation(); return null; },
    probeLifecycle: () => { onMutation(); return "stopped"; },
    probeStartupPolicy: () => { onMutation(); return "disabled"; },
    checkpointRollback: async () => { onMutation(); },
    checkpointCommit: async () => { onMutation(); },
    restorePriorDefinition: async () => { onMutation(); },
    stopWorker: async () => { onMutation(); },
    startPrior: async () => { onMutation(); },
    restorePriorLifecycle: async () => { onMutation(); },
    applyKeeperUpdate: async () => { onMutation(); },
    proveKeeperUpdate: async () => { onMutation(); },
    activateTarget: async () => { onMutation(); },
    cleanupStage: async () => { onMutation(); },
    commitTarget: async () => { onMutation(); },
    clearJournal: async () => { onMutation(); },
    proofAttempts: 1,
  };
}

function coordinatorJournal(): CoordinatorDeployJournalV2 {
  return {
    phase: "fleet-converging",
    rolloutId: ROLLOUT_ID,
    priorSha: PRIOR_SHA,
    targetSha: TARGET_SHA,
    workerKeeperPlans: [{
      fingerprint: WORKER_FINGERPRINT,
      keeperUpdate: LOCAL_KEEPER_UPDATE,
    }],
  } as unknown as CoordinatorDeployJournalV2;
}

describe("localhost worker deploy journal validation", () => {
  test("uses one fixed transaction journal outside unique release paths", () => {
    expect(localWorkerDeployJournalPath("/srv/roost/service")).toBe(
      "/srv/roost/service/transactions/worker-deploy.json",
    );
  });

  test("round trips the complete keeper update proof", () => {
    expect(parseLocalWorkerDeployJournal(
      JSON.stringify(localWorkerJournal()),
      LOCAL_CONFINEMENT,
    ).keeperUpdate).toEqual(LOCAL_KEEPER_UPDATE);
  });

  test("rejects legacy, incomplete, inconsistent, and malformed installed-service journals", async () => {
    let mutations = 0;
    const deps = mutationCountingDeps(() => { mutations += 1; });
    const missing = { ...localWorkerJournal() } as Record<string, unknown>;
    delete missing.keeperUpdate;
    const malformedUpdate = {
      ...LOCAL_KEEPER_UPDATE,
      admission: { ...LOCAL_KEEPER_UPDATE.admission, expected_keeper_pid: 0 },
    };
    for (const raw of [
      "{not-json",
      JSON.stringify({ ...localWorkerJournal(), schemaVersion: 2 }),
      JSON.stringify(missing),
      JSON.stringify({ ...localWorkerJournal(), unexpected: true }),
      JSON.stringify({ ...localWorkerJournal(), workerFingerprint: "not-a-fingerprint" }),
      JSON.stringify(localWorkerJournal({
        rolloutId: ROLLOUT_ID,
        workerFingerprint: null,
      })),
      JSON.stringify({ ...localWorkerJournal(), keeperUpdate: null }),
      JSON.stringify({ ...localWorkerJournal(), keeperUpdate: malformedUpdate }),
    ]) {
      await expect(_recoverLocalWorkerDeployJournal(
        raw,
        LOCAL_CONFINEMENT,
        deps,
      )).rejects.toThrow();
    }
    expect(mutations).toBe(0);
  });

  test("allows null keeper state only for a true bootstrap", () => {
    const bootstrap = localWorkerJournal({
      keeperUpdate: null,
      workerFingerprint: null,
      priorService: null,
      priorLifecycle: "unloaded",
      priorStartupPolicy: "absent",
      priorWorkingDirectory: null,
      priorGitSha: null,
    });
    expect(parseLocalWorkerDeployJournal(
      JSON.stringify(bootstrap),
      LOCAL_CONFINEMENT,
    ).keeperUpdate).toBeNull();
  });

  test("rejects traversal, nesting, unrelated stages, and foreign roots", () => {
    expect(localWorkerDeployStageIsConfined(RELEASE_ROOT, STAGED_RELEASE)).toBe(true);
    for (const unsafe of [
      RELEASE_ROOT,
      `${RELEASE_ROOT}/nested/release`,
      `${RELEASE_ROOT}/../escape`,
      "/srv/roost/service/releases/escape",
      "relative-release",
    ]) {
      expect(localWorkerDeployStageIsConfined(RELEASE_ROOT, unsafe)).toBe(false);
      expect(() => parseLocalWorkerDeployJournal(
        JSON.stringify(localWorkerJournal({ stagedReleasePath: unsafe })),
        LOCAL_CONFINEMENT,
      )).toThrow();
    }
    expect(() => parseLocalWorkerDeployJournal(
      JSON.stringify(localWorkerJournal({ stagedReleasePath: `${RELEASE_ROOT}/unrelated` })),
      LOCAL_CONFINEMENT,
    )).toThrow("staged release identifier is invalid");
    expect(() => parseLocalWorkerDeployJournal(
      JSON.stringify(localWorkerJournal({ sourceRoot: "/foreign/source" })),
      LOCAL_CONFINEMENT,
    )).toThrow("source root does not match");
    expect(() => parseLocalWorkerDeployJournal(
      JSON.stringify(localWorkerJournal({ releaseRoot: "/foreign/releases" })),
      LOCAL_CONFINEMENT,
    )).toThrow("release root does not match");
  });

  test("allows only the matching coordinator phase and keeper proof to coexist", () => {
    const directive = {
      action: "hold" as const,
      rolloutId: ROLLOUT_ID,
      priorSha: PRIOR_SHA,
      workerFingerprint: WORKER_FINGERPRINT,
      targetSha: TARGET_SHA,
      keeperUpdate: LOCAL_KEEPER_UPDATE,
    };
    const load = () => coordinatorJournal();
    expect(coordinatorJournalAllowsLocalWorkerRollout(
      "/srv/roost/service", "linux", directive, load,
    )).toBeTrue();
    expect(coordinatorJournalAllowsLocalWorkerRollout(
      "/srv/roost/service", "linux", { ...directive, action: "finalize" }, load,
    )).toBeFalse();
    expect(coordinatorJournalAllowsLocalWorkerRollout(
      "/srv/roost/service",
      "linux",
      { ...directive, rolloutId: "22222222-2222-4222-8222-222222222222" },
      load,
    )).toBeFalse();
    expect(coordinatorJournalAllowsLocalWorkerRollout(
      "/srv/roost/service",
      "linux",
      {
        ...directive,
        keeperUpdate: {
          ...LOCAL_KEEPER_UPDATE,
          admission: { ...LOCAL_KEEPER_UPDATE.admission, expected_keeper_pid: 5252 },
        },
      },
      load,
    )).toBeFalse();
  });
});
