// Local worker journal tests pin routable source replay and crash reentry.
// They exercise the production coordinator callback adapter against a fake
// worker generation rather than replacing the RPC boundary with event stubs.

import { describe, expect, test } from "bun:test";
import { KEEPER_EMPTY_BINDING_DIGEST } from "@roost/shared/keeper-update";
import { createJournaledKeeperUpdateCallbacks } from "../src/direct-keeper-update.ts";
import {
  _recoverLocalWorkerDeployJournal,
  type LocalWorkerDeployJournal,
  type LocalWorkerDeployRecoveryDeps,
} from "../src/local-worker-deploy-journal.ts";
import {
  LOCAL_CONFINEMENT,
  LOCAL_KEEPER_UPDATE,
  PRIOR_SERVICE,
  PRIOR_SHA,
  ROLLOUT_ID,
  TARGET_SERVICE,
  TARGET_SHA,
  WORKER_FINGERPRINT,
  localWorkerJournal,
} from "./deploy-local-journal-fixture.ts";

function rollbackFixture(failFirstProof = false): {
  deps: LocalWorkerDeployRecoveryDeps;
  events: string[];
  rollingJournal: () => LocalWorkerDeployJournal | null;
} {
  const events: string[] = [];
  let service = { ...TARGET_SERVICE, mode: 0o600 };
  let lifecycle: "running" | "stopped" = "running";
  let reconciliation = 10;
  let coordinatorLastSeen = 200;
  let proofFailures = failFirstProof ? 1 : 0;
  let checkpoint: LocalWorkerDeployJournal | null = null;
  const callbacks = createJournaledKeeperUpdateCallbacks({
    attempts: 1,
    sleep: async () => {},
    routable: async () => lifecycle === "running",
    prepare: async request => {
      expect(lifecycle).toBe("running");
      events.push(`rpc:${request.direction}`);
      return { outcome: "already-converged" };
    },
    inventory: () => [{
      fingerprint: WORKER_FINGERPRINT,
      label: "local",
      os: "linux",
      reachableAddr: null,
      gitSha: PRIOR_SHA,
      coordinatorOpenSessionIds: [],
      lastSeenMs: coordinatorLastSeen,
      ageMs: 0,
      stale: false,
      keeperRuntime: {
        schema_version: 1,
        running_contract: {
          ...LOCAL_KEEPER_UPDATE.source_contract,
          build_sha: PRIOR_SHA,
        },
        keeper_pid: 800,
        keeper_epoch: "33333333-3333-4333-8333-333333333333",
        channel_count: 0,
        binding_digest: KEEPER_EMPTY_BINDING_DIGEST,
        reconciled_at_ms: reconciliation,
      },
    }],
  });
  const deps: LocalWorkerDeployRecoveryDeps = {
    readService: () => service,
    probeLifecycle: () => lifecycle,
    probeStartupPolicy: () => "enabled",
    checkpointRollback: async journal => {
      checkpoint = { ...journal };
      events.push("checkpoint-rollback");
    },
    checkpointCommit: async () => { events.push("checkpoint-commit"); },
    stopWorker: async () => { lifecycle = "stopped"; events.push("stop"); },
    restorePriorDefinition: async () => { service = PRIOR_SERVICE; events.push("restore"); },
    startPrior: async () => {
      lifecycle = "running";
      reconciliation += 1;
      coordinatorLastSeen += 1;
      events.push("start-prior");
    },
    restorePriorLifecycle: async () => { events.push("settle-prior"); },
    activateTarget: async () => { lifecycle = "running"; events.push("start-target"); },
    applyKeeperUpdate: callbacks.apply,
    proveKeeperUpdate: async (...args) => {
      if (proofFailures > 0) {
        proofFailures -= 1;
        throw new Error("injected proof crash");
      }
      await callbacks.prove(...args);
      events.push("prove-keeper");
    },
    cleanupStage: async () => { events.push("cleanup"); },
    commitTarget: async () => { events.push("commit"); },
    clearJournal: async () => { events.push("clear"); },
    proofAttempts: 1,
    now: () => 5,
  };
  return { deps, events, rollingJournal: () => checkpoint };
}

function preparedFixture(): {
  deps: LocalWorkerDeployRecoveryDeps;
  events: string[];
} {
  const fixture = rollbackFixture();
  return {
    events: fixture.events,
    deps: {
      ...fixture.deps,
      readService: () => {
        fixture.events.push("read-service");
        return null;
      },
      probeLifecycle: () => {
        fixture.events.push("probe-lifecycle");
        return "stopped";
      },
      probeStartupPolicy: () => {
        fixture.events.push("probe-startup-policy");
        return "disabled";
      },
      applyKeeperUpdate: async () => {
        fixture.events.push("keeper-action");
      },
      proveKeeperUpdate: async () => {
        fixture.events.push("keeper-proof");
      },
    },
  };
}

describe("localhost worker deploy journal", () => {
  test("prepared recovery removes only the partial stage and clears the journal", async () => {
    const fixture = preparedFixture();
    const journal = localWorkerJournal({
      priorLifecycle: "stopped",
      priorStartupPolicy: "disabled",
    });
    await expect(_recoverLocalWorkerDeployJournal(
      JSON.stringify(journal), LOCAL_CONFINEMENT, fixture.deps,
    )).resolves.toBe("prepared-cleaned");
    expect(fixture.events).toEqual(["cleanup", "clear"]);
  });

  test("prepared fleet rollback cleans an originally stopped worker without service or keeper calls", async () => {
    const fixture = preparedFixture();
    const journal = localWorkerJournal({
      rolloutId: ROLLOUT_ID,
      priorLifecycle: "stopped",
      priorStartupPolicy: "disabled",
    });
    const directive = {
      action: "rollback" as const,
      rolloutId: ROLLOUT_ID,
      priorSha: PRIOR_SHA,
      workerFingerprint: WORKER_FINGERPRINT,
      targetSha: TARGET_SHA,
      keeperUpdate: LOCAL_KEEPER_UPDATE,
    };
    await expect(_recoverLocalWorkerDeployJournal(
      JSON.stringify(journal), LOCAL_CONFINEMENT, fixture.deps, directive,
    )).resolves.toBe("prepared-cleaned");
    expect(fixture.events).toEqual(["cleanup", "clear"]);
  });

  test("prepared fleet finalization and invalid ownership fail before cleanup", async () => {
    const journal = localWorkerJournal({ rolloutId: ROLLOUT_ID });
    const directive = {
      action: "finalize" as const,
      rolloutId: ROLLOUT_ID,
      priorSha: PRIOR_SHA,
      workerFingerprint: WORKER_FINGERPRINT,
      targetSha: TARGET_SHA,
      keeperUpdate: LOCAL_KEEPER_UPDATE,
    };
    const finalizeFixture = preparedFixture();
    await expect(_recoverLocalWorkerDeployJournal(
      JSON.stringify(journal), LOCAL_CONFINEMENT, finalizeFixture.deps, directive,
    )).rejects.toThrow("before activation");
    expect(finalizeFixture.events).toEqual([]);

    const unownedFixture = preparedFixture();
    await expect(_recoverLocalWorkerDeployJournal(
      JSON.stringify(journal), LOCAL_CONFINEMENT, unownedFixture.deps,
    )).rejects.toThrow("fleet rollout still owns");
    expect(unownedFixture.events).toEqual([]);

    const foreignFixture = preparedFixture();
    await expect(_recoverLocalWorkerDeployJournal(
      JSON.stringify(journal),
      LOCAL_CONFINEMENT,
      foreignFixture.deps,
      {
        ...directive,
        action: "rollback",
        rolloutId: "22222222-2222-4222-8222-222222222222",
      },
    )).rejects.toThrow("does not match the requested fleet rollout");
    expect(foreignFixture.events).toEqual([]);

    const wrongPriorFixture = preparedFixture();
    await expect(_recoverLocalWorkerDeployJournal(
      JSON.stringify(journal),
      LOCAL_CONFINEMENT,
      wrongPriorFixture.deps,
      { ...directive, action: "rollback", priorSha: "d".repeat(40) },
    )).rejects.toThrow("does not prove the fleet rollout prior identity");
    expect(wrongPriorFixture.events).toEqual([]);
  });

  test("rollback restores a routable source, replays action, restarts, and proves before clear", async () => {
    const fixture = rollbackFixture();
    const journal = localWorkerJournal({ phase: "activated", targetService: TARGET_SERVICE });
    await expect(_recoverLocalWorkerDeployJournal(
      JSON.stringify(journal), LOCAL_CONFINEMENT, fixture.deps,
    )).resolves.toBe("prior-restored");
    expect(fixture.events).toEqual([
      "checkpoint-rollback", "stop", "restore", "start-prior", "rpc:source",
      "stop", "start-prior", "prove-keeper", "settle-prior", "cleanup", "clear",
    ]);
  });

  test("crash reentry retains rolling-back and repeats the recorded source action", async () => {
    const fixture = rollbackFixture(true);
    const journal = localWorkerJournal({ phase: "activated", targetService: TARGET_SERVICE });
    await expect(_recoverLocalWorkerDeployJournal(
      JSON.stringify(journal), LOCAL_CONFINEMENT, fixture.deps,
    )).rejects.toThrow("injected proof crash");
    expect(fixture.rollingJournal()?.phase).toBe("rolling-back");
    const firstActions = fixture.events.filter(event => event === "rpc:source").length;
    await expect(_recoverLocalWorkerDeployJournal(
      JSON.stringify(fixture.rollingJournal()), LOCAL_CONFINEMENT, fixture.deps,
    )).resolves.toBe("prior-restored");
    expect(fixture.events.filter(event => event === "rpc:source")).toHaveLength(firstActions + 1);
    expect(fixture.events.at(-1)).toBe("clear");
  });

  test("commit cleanup is durably irreversible and retries target proof", async () => {
    const events: string[] = [];
    const journal = localWorkerJournal({ phase: "committing", targetService: TARGET_SERVICE });
    const fixture = rollbackFixture();
    const deps = {
      ...fixture.deps,
      readService: () => TARGET_SERVICE,
      probeLifecycle: () => "running" as const,
      applyKeeperUpdate: async () => { events.push("target-action"); },
      stopWorker: async () => { events.push("stop"); },
      activateTarget: async () => { events.push("start"); },
      proveKeeperUpdate: async () => { events.push("prove"); },
      commitTarget: async () => { events.push("cleanup-prior"); },
      clearJournal: async () => { events.push("clear"); },
    };
    await expect(_recoverLocalWorkerDeployJournal(
      JSON.stringify(journal), LOCAL_CONFINEMENT, deps,
    )).resolves.toBe("target-committed");
    expect(events).toEqual([
      "stop", "start", "target-action", "stop", "start", "prove", "cleanup-prior", "clear",
    ]);
  });
});
