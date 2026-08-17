import { describe, expect, test } from "bun:test";
import {
  _recoverLocalWorkerDeployJournal,
  localWorkerDeployStageIsConfined,
  localWorkerDeployJournalPath,
  parseLocalWorkerDeployJournal,
  type LocalWorkerDeployConfinement,
  type LocalWorkerDeployJournalV1,
  type LocalWorkerDeployRecoveryDeps,
  type LocalWorkerLifecycle,
  type LocalWorkerServiceSnapshot,
} from "../src/deploy-local.ts";

const TARGET_SHA = "a".repeat(40);
const PRIOR_SHA = "b".repeat(40);
const SOURCE_ROOT = "/srv/roost/source";
const RELEASE_ROOT = "/srv/roost/service/releases/worker";
const STAGED_RELEASE = `${RELEASE_ROOT}/${TARGET_SHA}-11111111-1111-4111-8111-111111111111`;
const PRIOR_RELEASE = `${RELEASE_ROOT}/${PRIOR_SHA}-prior`;
const CONFINEMENT: LocalWorkerDeployConfinement = {
  os: "linux",
  sourceRoot: SOURCE_ROOT,
  releaseRoot: RELEASE_ROOT,
};

function snapshot(definition: string, mode = 0o600): LocalWorkerServiceSnapshot {
  return { definitionBase64: Buffer.from(definition).toString("base64"), mode };
}

const PRIOR_SERVICE = snapshot([
  "[Service]",
  `WorkingDirectory=\"${PRIOR_RELEASE}\"`,
  `Environment=\"GIT_SHA=${PRIOR_SHA}\"`,
].join("\n"), 0o640);
const TARGET_SERVICE = snapshot([
  "[Service]",
  `WorkingDirectory=\"${STAGED_RELEASE}\"`,
  `Environment=\"GIT_SHA=${TARGET_SHA}\"`,
].join("\n"), 0o640);

function journal(
  overrides: Partial<LocalWorkerDeployJournalV1> = {},
): LocalWorkerDeployJournalV1 {
  return {
    schemaVersion: 1,
    phase: "prepared",
    os: "linux",
    sourceRoot: SOURCE_ROOT,
    releaseRoot: RELEASE_ROOT,
    stagedReleasePath: STAGED_RELEASE,
    targetSha: TARGET_SHA,
    priorService: PRIOR_SERVICE,
    priorWasRunning: true,
    priorWorkingDirectory: PRIOR_RELEASE,
    priorGitSha: PRIOR_SHA,
    targetService: null,
    ...overrides,
  };
}

function unusedRecoveryDeps(onMutation: () => void): LocalWorkerDeployRecoveryDeps {
  return {
    readService: () => {
      onMutation();
      throw new Error("unexpected service read");
    },
    probeLifecycle: () => {
      onMutation();
      throw new Error("unexpected lifecycle probe");
    },
    restorePrior: async () => {
      onMutation();
      throw new Error("unexpected rollback");
    },
    cleanupStage: async () => {
      onMutation();
      throw new Error("unexpected cleanup");
    },
    commitTarget: async () => {
      onMutation();
      throw new Error("unexpected commit");
    },
    clearJournal: async () => {
      onMutation();
      throw new Error("unexpected journal clear");
    },
    proofAttempts: 1,
  };
}

describe("localhost worker deploy journal", () => {
  test("uses one fixed transaction journal outside unique release paths", () => {
    expect(localWorkerDeployJournalPath("/srv/roost/service")).toBe(
      "/srv/roost/service/transactions/worker-deploy.json",
    );
  });

  test("prepared recovery only removes the confined stage before clearing", async () => {
    const events: string[] = [];
    const deps: LocalWorkerDeployRecoveryDeps = {
      ...unusedRecoveryDeps(() => {
        throw new Error("unexpected recovery operation");
      }),
      cleanupStage: async () => { events.push("cleanup"); },
      clearJournal: async () => { events.push("clear"); },
    };

    await expect(_recoverLocalWorkerDeployJournal(
      JSON.stringify(journal()),
      CONFINEMENT,
      deps,
    )).resolves.toBe("prepared-cleaned");
    expect(events).toEqual(["cleanup", "clear"]);
  });

  test("activating recovery commits only the exact target definition and healthy lifecycle", async () => {
    const events: string[] = [];
    const activating = journal({ phase: "activating", targetService: TARGET_SERVICE });
    const deps: LocalWorkerDeployRecoveryDeps = {
      ...unusedRecoveryDeps(() => {
        throw new Error("unexpected rollback operation");
      }),
      readService: () => { events.push("read"); return TARGET_SERVICE; },
      probeLifecycle: () => { events.push("probe"); return "running"; },
      commitTarget: async () => { events.push("commit"); },
      clearJournal: async () => { events.push("clear"); },
    };

    await expect(_recoverLocalWorkerDeployJournal(
      JSON.stringify(activating),
      CONFINEMENT,
      deps,
    )).resolves.toBe("target-committed");
    expect(events).toEqual(["read", "probe", "commit", "clear"]);
  });

  test("activated recovery restores and proves a prior running service before stage removal", async () => {
    const events: string[] = [];
    let activeService: LocalWorkerServiceSnapshot | null = {
      ...TARGET_SERVICE,
      mode: 0o600,
    };
    let lifecycle: LocalWorkerLifecycle = "running";
    const activated = journal({ phase: "activated", targetService: TARGET_SERVICE });
    const deps: LocalWorkerDeployRecoveryDeps = {
      readService: () => { events.push("read"); return activeService; },
      probeLifecycle: () => { events.push("probe"); return lifecycle; },
      restorePrior: async () => {
        events.push("restore");
        activeService = PRIOR_SERVICE;
        lifecycle = "running";
      },
      cleanupStage: async () => { events.push("cleanup"); },
      commitTarget: async () => { events.push("commit"); },
      clearJournal: async () => { events.push("clear"); },
      proofAttempts: 1,
    };

    await expect(_recoverLocalWorkerDeployJournal(
      JSON.stringify(activated),
      CONFINEMENT,
      deps,
    )).resolves.toBe("prior-restored");
    expect(events).toEqual(["read", "restore", "read", "probe", "cleanup", "clear"]);
  });

  test("rollback with no prior service proves absent definition and stopped lifecycle", async () => {
    const events: string[] = [];
    let activeService: LocalWorkerServiceSnapshot | null = TARGET_SERVICE;
    let lifecycle: LocalWorkerLifecycle = "running";
    const absentPrior = journal({
      phase: "activating",
      priorService: null,
      priorWasRunning: false,
      priorWorkingDirectory: null,
      priorGitSha: null,
      targetService: null,
    });
    const deps: LocalWorkerDeployRecoveryDeps = {
      readService: () => { events.push("read"); return activeService; },
      probeLifecycle: () => { events.push("probe"); return lifecycle; },
      restorePrior: async () => {
        events.push("restore-absent");
        activeService = null;
        lifecycle = "stopped";
      },
      cleanupStage: async () => { events.push("cleanup"); },
      commitTarget: async () => { events.push("commit"); },
      clearJournal: async () => { events.push("clear"); },
      proofAttempts: 1,
    };

    await expect(_recoverLocalWorkerDeployJournal(
      JSON.stringify(absentPrior),
      CONFINEMENT,
      deps,
    )).resolves.toBe("prior-restored");
    expect(events).toEqual(["restore-absent", "read", "probe", "cleanup", "clear"]);
  });

  test("unproven rollback retains both target stage and journal", async () => {
    const events: string[] = [];
    const deps: LocalWorkerDeployRecoveryDeps = {
      readService: () => null,
      probeLifecycle: () => "unknown",
      restorePrior: async () => { events.push("restore"); },
      cleanupStage: async () => { events.push("cleanup"); },
      commitTarget: async () => { events.push("commit"); },
      clearJournal: async () => { events.push("clear"); },
      proofAttempts: 1,
    };

    await expect(_recoverLocalWorkerDeployJournal(
      JSON.stringify(journal({ phase: "activating" })),
      CONFINEMENT,
      deps,
    )).rejects.toThrow("could not be proven");
    expect(events).toEqual(["restore"]);
  });

  test("journal path confinement rejects traversal, nesting, and foreign roots", () => {
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
        JSON.stringify(journal({ stagedReleasePath: unsafe })),
        CONFINEMENT,
      )).toThrow();
    }
    expect(localWorkerDeployStageIsConfined(RELEASE_ROOT, `${RELEASE_ROOT}/unrelated`)).toBe(true);
    expect(() => parseLocalWorkerDeployJournal(
      JSON.stringify(journal({ stagedReleasePath: `${RELEASE_ROOT}/unrelated` })),
      CONFINEMENT,
    )).toThrow("staged release identifier is invalid");
    expect(() => parseLocalWorkerDeployJournal(
      JSON.stringify(journal({ sourceRoot: "/foreign/source" })),
      CONFINEMENT,
    )).toThrow("source root does not match");
    expect(() => parseLocalWorkerDeployJournal(
      JSON.stringify(journal({ releaseRoot: "/foreign/releases" })),
      CONFINEMENT,
    )).toThrow("release root does not match");
  });

  test("malformed journal fails before any recovery mutation", async () => {
    let mutations = 0;
    await expect(_recoverLocalWorkerDeployJournal(
      "{not-json",
      CONFINEMENT,
      unusedRecoveryDeps(() => { mutations += 1; }),
    )).rejects.toThrow("malformed JSON");
    await expect(_recoverLocalWorkerDeployJournal(
      JSON.stringify(journal({
        priorService: null,
        priorWasRunning: true,
        priorWorkingDirectory: null,
        priorGitSha: null,
      })),
      CONFINEMENT,
      unusedRecoveryDeps(() => { mutations += 1; }),
    )).rejects.toThrow("absent prior service as running");
    expect(mutations).toBe(0);
  });
});
