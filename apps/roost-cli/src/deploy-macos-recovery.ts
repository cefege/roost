// Crash-safe macOS worker journal recovery and fleet settlement.
// It owns durable source rollback and irreversible target-commit ordering.
// The journal controller supplies authenticated RPC and launchd operations.
// A rollback the host can never satisfy ends in the roll-forward outcome
// instead of a retained journal that would wedge every later deploy.

import { parsePosixServiceEnvironment } from "./deploy-plist-env.ts";
import {
  MACOS_GIT_SHA_RE,
  _decideMacosDeployRecovery,
  type MacosDeployRecoveryRemote,
  type MacosDeployRecoveryResult,
  type MacosDeployTargetProof,
} from "./deploy-macos-journal.ts";
import {
  assertWorkerRolloutDirective,
  assertWorkerRolloutMatches,
  type WorkerRolloutDirective,
} from "./worker-deploy-rollout.ts";
import {
  DurableStateRollForwardRequired,
  durableStateRollForwardNotice,
} from "./durable-worker-state.ts";

export async function _recoverMacosDeployJournal(
  remote: MacosDeployRecoveryRemote,
  directive?: Readonly<WorkerRolloutDirective>,
): Promise<MacosDeployRecoveryResult> {
  const journal = await remote.load();
  if (!journal) return { outcome: "none" };
  const requested = directive ? assertWorkerRolloutDirective(directive) : null;
  if (requested) {
    assertWorkerRolloutMatches({
      rolloutId: journal.rolloutId,
      targetSha: journal.targetGitSha,
      workerFingerprint: journal.workerFingerprint,
      keeperUpdate: journal.keeperUpdate,
    }, requested);
  }
  if (journal.rolloutId !== null && !requested) {
    throw new Error("a fleet rollout still owns the macOS worker deploy journal");
  }
  let priorSha: string | null = null;
  if (requested || journal.phase !== "prepared") {
    const priorEnvironment = journal.priorPlistBase64 === null
      ? {}
      : parsePosixServiceEnvironment(
          Buffer.from(journal.priorPlistBase64, "base64").toString("utf8"),
          "darwin",
        );
    priorSha = priorEnvironment.GIT_SHA ?? priorEnvironment.ROOST_GIT_SHA ?? null;
  }
  if (requested && priorSha?.toLowerCase() !== requested.priorSha) {
    throw new Error("macOS worker journal does not prove the fleet rollout prior identity");
  }
  if (journal.phase === "prepared") {
    if (requested?.action === "finalize") {
      throw new Error("cannot finalize a macOS worker before activation");
    }
    await remote.removeTarget(journal);
    await remote.clear(journal);
    return { outcome: "prepared-cleaned", journal };
  }
  if (requested && journal.priorLifecycle !== "running") {
    throw new Error("macOS worker journal does not prove the fleet rollout prior identity");
  }
  const applyRecordedKeeperUpdate = async (
    direction: "source" | "target",
  ): Promise<void> => {
    if (!journal.keeperUpdate) return;
    await remote.applyKeeperUpdate(
      journal.workerFingerprint!, journal.keeperUpdate, direction, journal.targetReleasePath,
    );
  };
  const startWorker = async (): Promise<void> => {
    await remote.setDisabled(journal, false);
    await remote.bootstrap(journal);
    await remote.kickstart(journal);
  };
  const replayTargetKeeperUpdate = async (): Promise<MacosDeployTargetProof> => {
    if (!journal.keeperUpdate) return await remote.proveTarget(journal);
    await remote.bootout(journal);
    await startWorker();
    await applyRecordedKeeperUpdate("target");
    await remote.bootout(journal);
    const heartbeatNotBeforeMs = (remote.now ?? Date.now)();
    await startWorker();
    const restarted = await remote.proveTarget(journal);
    if (!restarted.definitionMatches || !restarted.running) {
      throw new Error("macOS target did not recover after keeper update replay");
    }
    await remote.proveKeeperUpdate(
      journal.workerFingerprint!, journal.keeperUpdate, "target",
      journal.targetGitSha, heartbeatNotBeforeMs, journal.targetReleasePath,
    );
    return restarted;
  };
  const rollback = async (
    targetProof: MacosDeployTargetProof | null,
  ): Promise<MacosDeployRecoveryResult> => {
    if (journal.keeperUpdate && (!priorSha || !MACOS_GIT_SHA_RE.test(priorSha))) {
      throw new Error("macOS rollback journal lacks the prior worker identity");
    }
    const rollingBack = journal.phase === "rolling-back"
      ? journal
      : await remote.checkpointRollback(journal);
    await remote.bootout(journal);
    await remote.restorePriorDefinition(journal);
    let priorStarted = false;
    if (journal.keeperUpdate) {
      await startWorker();
      await applyRecordedKeeperUpdate("source");
      await remote.bootout(journal);
      const heartbeatNotBeforeMs = (remote.now ?? Date.now)();
      await startWorker();
      priorStarted = true;
      await remote.proveKeeperUpdate(
        journal.workerFingerprint!, journal.keeperUpdate, "source", priorSha!,
        heartbeatNotBeforeMs, journal.targetReleasePath,
      );
    } else if (journal.priorLifecycle !== "unloaded") {
      // Restored plist bytes are inert until launchd owns the job again, so a
      // prior release that must end up loaded or running is bootstrapped here.
      // Without it the lifecycle proof below can never pass.
      await startWorker();
      priorStarted = true;
    }
    if (journal.priorLifecycle === "loaded") {
      await remote.setDisabled(journal, true);
      await remote.stop(journal);
      await remote.setDisabled(journal, journal.priorDisabled);
    } else if (journal.priorLifecycle === "running") {
      await remote.setDisabled(journal, journal.priorDisabled);
    } else {
      await remote.bootout(journal);
      await remote.setDisabled(journal, journal.priorDisabled);
    }
    try {
      await remote.provePrior(rollingBack, priorStarted);
    } catch (error) {
      if (!(error instanceof DurableStateRollForwardRequired)) throw error;
      const reason = durableStateRollForwardNotice(error, rollingBack.targetReleasePath);
      console.error(reason);
      // The staged target is the only release that can still run, so it stays
      // while the journal goes: a retained journal would fail this same proof
      // on every later deploy.
      await remote.clear(rollingBack);
      return { outcome: "roll-forward-required", journal: rollingBack, targetProof, reason };
    }
    await remote.removeTarget(journal);
    await remote.clear(journal);
    return { outcome: "rolled-back", journal: rollingBack, targetProof };
  };
  if (journal.phase === "rolling-back") {
    if (requested?.action === "finalize") {
      throw new Error("cannot finalize a macOS worker after rollback was chosen");
    }
    return await rollback(null);
  }
  if (journal.phase === "committing" && requested?.action === "rollback") {
    throw new Error("cannot roll back a macOS worker after target commit was chosen");
  }
  if (requested?.action === "rollback") return await rollback(null);
  if (requested?.action === "finalize"
    && journal.phase !== "activated" && journal.phase !== "committing") {
    throw new Error("cannot finalize a macOS worker before activation");
  }
  let targetProof: MacosDeployTargetProof;
  try {
    targetProof = await replayTargetKeeperUpdate();
  } catch (error) {
    if (requested?.action === "finalize" || journal.phase === "committing") throw error;
    return await rollback(null);
  }
  if (journal.phase === "committing") {
    await remote.cleanupPriorRelease(journal);
    await remote.clear(journal);
    return { outcome: "committed", journal, targetProof };
  }
  const targetHealthy = targetProof.definitionMatches && targetProof.running;
  if (journal.rolloutId !== null) {
    if (requested?.action === "finalize") {
      await remote.checkpointCommit(journal);
      await remote.cleanupPriorRelease(journal);
      await remote.clear(journal);
      return { outcome: "committed", journal, targetProof };
    }
    if (!targetHealthy) return await rollback(targetProof);
    const heldJournal = journal.phase === "activating"
      ? await remote.checkpointActivated(journal)
      : journal;
    return { outcome: "held", journal: heldJournal, targetProof };
  }
  if (_decideMacosDeployRecovery(journal.phase, targetProof) === "commit") {
    await remote.checkpointCommit(journal);
    await remote.cleanupPriorRelease(journal);
    await remote.clear(journal);
    return { outcome: "committed", journal, targetProof };
  }
  return await rollback(targetProof);
}
