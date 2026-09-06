// Remote journal controller for macOS worker deployment and recovery.
// Command construction and envelope parsing live in the byte-stable command module.
// deploy.ts supplies the SSH executor; deploy-macos-journal.ts owns decisions.

import type { JournaledKeeperUpdateV1 } from "@roost/shared/keeper-update";
import {
  MACOS_PRIOR_LIFECYCLE_PROOF_COMMAND,
  MACOS_WORKER_PLIST_RELATIVE,
  macosJournalUtilityCommand,
  macosTargetVerificationCommand,
  parseMacosJournalEnvelope,
} from "./deploy-macos-journal-commands.ts";
import type { MacosJournalTarget } from "./deploy-macos-journal-commands.ts";
import { MACOS_WORKER_LABEL } from "./deploy-macos-journal.ts";
import type {
  MacosDeployJournalV2,
  MacosDeployRecoveryRemote,
} from "./deploy-macos-journal.ts";
import {
  DeployFailure,
  workerServiceIsRunning,
  workerServiceMatchesRelease,
} from "./deploy-exec.ts";
import { launchdBootstrapWithRetryCmd } from "./service-ctl.ts";

type MacosRemoteExecutor = (
  command: string,
) => Promise<{ exit: number; stdout: string; stderr: string }>;

export type MacosApplyKeeperUpdate = (
  workerFingerprint: string,
  update: JournaledKeeperUpdateV1,
  direction: "source" | "target",
  actionReleasePath: string,
) => Promise<void>;
export type MacosProveKeeperUpdate = (
  workerFingerprint: string,
  update: JournaledKeeperUpdateV1,
  direction: "source" | "target",
  expectedWorkerSha: string,
  heartbeatNotBeforeMs: number,
  actionReleasePath: string,
) => Promise<void>;

export interface MacosDeployJournalControllerOptions {
  signal?: AbortSignal;
  applyKeeperUpdate?: MacosApplyKeeperUpdate;
  proveKeeperUpdate?: MacosProveKeeperUpdate;
}

export interface MacosDeployJournalController {
  recovery: MacosDeployRecoveryRemote;
  prepare(
    gitSha: string,
    remoteDir: string,
    rolloutId: string | null,
    workerFingerprint: string | null,
    keeperUpdate: JournaledKeeperUpdateV1 | null,
  ): Promise<MacosDeployJournalV2>;
  checkpointActivating(
    gitSha: string,
    remoteDir: string,
    rolloutId: string | null,
    workerFingerprint: string | null,
    keeperUpdate: JournaledKeeperUpdateV1 | null,
  ): Promise<MacosDeployJournalV2>;
  activateTarget(journal: Readonly<MacosDeployJournalV2>): Promise<void>;
}

export function createMacosDeployJournalController(
  execute: MacosRemoteExecutor,
  journalPath: string,
  options: Readonly<MacosDeployJournalControllerOptions> = {},
): MacosDeployJournalController {
  const { signal, applyKeeperUpdate, proveKeeperUpdate } = options;
  const transportFailure = (
    result: { exit: number; stdout: string; stderr: string },
    operation: string,
  ): void => {
    if (!signal?.aborted && result.exit !== 255 && result.exit < 128) return;
    const reason = signal?.reason;
    if (reason instanceof DeployFailure) throw reason;
    throw new DeployFailure(
      result.exit || 9,
      `${operation} lost its remote shell; durable macOS deploy journal retained\n` +
        `${result.stdout}\n${result.stderr}`,
    );
  };
  const checked = async (
    operation: string,
    command: string,
  ): Promise<{ exit: number; stdout: string; stderr: string }> => {
    const result = await execute(command);
    transportFailure(result, operation);
    if (result.exit !== 0) {
      throw new DeployFailure(
        result.exit,
        `${operation} failed; durable macOS deploy journal retained\n${result.stdout}\n${result.stderr}`,
      );
    }
    return result;
  };
  const parseEnvelope = (
    result: { stdout: string },
    operation: string,
  ): { releaseRoot: string; journal: MacosDeployJournalV2 | null } => {
    try {
      return parseMacosJournalEnvelope(result.stdout);
    } catch (error) {
      throw new DeployFailure(
        5,
        `${operation} returned invalid durable state; journal retained\n` +
          (error instanceof Error ? error.message : String(error)),
      );
    }
  };
  const utility = (
    action: Parameters<typeof macosJournalUtilityCommand>[1],
    target?: MacosJournalTarget,
  ) => macosJournalUtilityCommand(journalPath, action, target);
  const assertKeeperActionAvailable = (
    direction: "source" | "target",
  ): void => {
    if (applyKeeperUpdate) return;
    throw new DeployFailure(
      5,
      `macOS keeper update ${direction} action is unavailable; durable deploy journal retained`,
    );
  };

  const recovery: MacosDeployRecoveryRemote = {
    async load() {
      const result = await checked("load macOS deploy journal", utility("load"));
      return parseEnvelope(result, "load macOS deploy journal").journal;
    },
    async checkpointActivated(journal) {
      const result = await checked(
        "checkpoint activated macOS deploy",
        utility("checkpoint-activated", {
          gitSha: journal.targetGitSha,
          targetPath: journal.targetReleasePath,
          rolloutId: journal.rolloutId,
          keeperUpdate: journal.keeperUpdate,
          workerFingerprint: journal.workerFingerprint,
        }),
      );
      const checkpointedJournal = parseEnvelope(result, "checkpoint activated macOS deploy").journal;
      if (!checkpointedJournal || checkpointedJournal.phase !== "activated") {
        throw new DeployFailure(5, "remote Mac did not durably checkpoint activated state");
      }
      return checkpointedJournal;
    },
    async checkpointRollback() {
      await checked("checkpoint macOS worker rollback", utility("checkpoint-rollback"));
    },
    async checkpointCommit() {
      await checked("checkpoint macOS target commit", utility("checkpoint-commit"));
    },
    async proveTarget(journal) {
      const result = await execute(macosTargetVerificationCommand(journal));
      transportFailure(result, "prove activated macOS worker");
      const pidMatch = result.stdout.match(/^\s*pid = ([1-9]\d*)\s*$/m);
      const processAdvanced = journal.priorPid === null
        || (pidMatch !== null && Number(pidMatch[1]) !== journal.priorPid);
      return {
        definitionMatches: workerServiceMatchesRelease(result.stdout),
        running: result.exit === 0
          && workerServiceIsRunning(result.stdout, "darwin")
          && processAdvanced,
        result,
      };
    },
    async bootout() {
      await checked(
        "boot out current macOS worker",
        `uid=$(id -u); launchctl bootout gui/$uid/${MACOS_WORKER_LABEL} 2>/dev/null || true; ` +
          `for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do ` +
          `if ! launchctl print gui/$uid/${MACOS_WORKER_LABEL} >/dev/null 2>&1; then exit 0; fi; ` +
          `sleep 0.25; done; echo 'launchd bootout did not settle' >&2; exit 1`,
      );
    },
    async applyKeeperUpdate(workerFingerprint, update, direction, actionReleasePath) {
      assertKeeperActionAvailable(direction);
      await applyKeeperUpdate!(workerFingerprint, update, direction, actionReleasePath);
    },
    async proveKeeperUpdate(
      workerFingerprint,
      update,
      direction,
      expectedWorkerSha,
      heartbeatNotBeforeMs,
      actionReleasePath,
    ) {
      if (!proveKeeperUpdate) {
        throw new DeployFailure(5, "macOS keeper convergence proof is unavailable");
      }
      await proveKeeperUpdate(
        workerFingerprint,
        update,
        direction,
        expectedWorkerSha,
        heartbeatNotBeforeMs,
        actionReleasePath,
      );
    },
    async restorePriorDefinition() {
      await checked("restore prior macOS worker plist", utility("restore-prior"));
    },
    async setDisabled(_journal, disabled) {
      await checked(
        `set macOS worker ${disabled ? "disabled" : "enabled"} override`,
        `launchctl ${disabled ? "disable" : "enable"} gui/$(id -u)/${MACOS_WORKER_LABEL}`,
      );
    },
    async bootstrap() {
      await checked(
        "bootstrap macOS worker",
        launchdBootstrapWithRetryCmd(MACOS_WORKER_LABEL, MACOS_WORKER_PLIST_RELATIVE, {
          role: "launchd worker", reload: false, homeRelative: true,
        }),
      );
    },
    async kickstart() {
      await checked(
        "kickstart macOS worker",
        `launchctl kickstart -k gui/$(id -u)/${MACOS_WORKER_LABEL}`,
      );
    },
    async stop() {
      await checked(
        "stop prior loaded macOS worker",
        `launchctl stop gui/$(id -u)/${MACOS_WORKER_LABEL}`,
      );
    },
    async provePrior(journal) {
      await checked(
        "prove prior macOS worker plist",
        utility("prove-prior-definition"),
      );
      let consecutive = 0;
      let last = { exit: 1, stdout: "", stderr: "" };
      for (let attempt = 0; attempt < 20; attempt += 1) {
        last = await execute(MACOS_PRIOR_LIFECYCLE_PROOF_COMMAND);
        transportFailure(last, "prove prior macOS worker lifecycle");
        const loaded = /^RoostLaunchdLoaded=yes$/m.test(last.stdout);
        const disabled = /^RoostLaunchdDisabled=yes$/m.test(last.stdout);
        const disabledMatches = disabled === journal.priorDisabled;
        const running = last.exit === 0 && workerServiceIsRunning(last.stdout, "darwin");
        const pidMatch = last.stdout.match(/^\s*pid = ([1-9]\d*)\s*$/m);
        const processAdvanced = journal.priorPid === null
          || (pidMatch !== null && Number(pidMatch[1]) !== journal.priorPid);
        const lifecycleMatches = journal.priorLifecycle === "running"
          ? loaded && running && processAdvanced
          : journal.priorLifecycle === "loaded"
            ? loaded && !running
            : !loaded;
        if (last.exit === 0 && disabledMatches && lifecycleMatches) {
          consecutive += 1;
          const required = journal.priorLifecycle === "running" ? 1 : 3;
          if (consecutive >= required) return;
        } else {
          consecutive = 0;
        }
        await Bun.sleep(250);
      }
      throw new DeployFailure(
        5,
        `prior macOS worker lifecycle did not round-trip; journal retained\n` +
          `${last.stdout}\n${last.stderr}`,
      );
    },
    async removeTarget() {
      await checked("remove macOS target stage", utility("remove-target"));
    },
    async cleanupPriorRelease() {
      await checked("clean prior macOS release", utility("cleanup-prior"));
    },
    async clear() {
      await checked("clear macOS deploy journal", utility("clear"));
    },
  };

  return {
    recovery,
    async prepare(gitSha, remoteDir, rolloutId, workerFingerprint, keeperUpdate) {
      const result = await checked(
        "prepare macOS deploy journal",
        utility("prepare", { gitSha, remoteDir, rolloutId, workerFingerprint, keeperUpdate }),
      );
      const journal = parseEnvelope(result, "prepare macOS deploy journal").journal;
      if (!journal || journal.phase !== "prepared") {
        throw new DeployFailure(5, "remote Mac did not durably prepare its deploy journal");
      }
      return journal;
    },
    async checkpointActivating(gitSha, remoteDir, rolloutId, workerFingerprint, keeperUpdate) {
      const result = await checked(
        "checkpoint activating macOS deploy",
        utility("checkpoint-activating", {
          gitSha,
          remoteDir,
          rolloutId,
          workerFingerprint,
          keeperUpdate,
        }),
      );
      const journal = parseEnvelope(result, "checkpoint activating macOS deploy").journal;
      if (!journal || journal.phase !== "activating") {
        throw new DeployFailure(5, "remote Mac did not durably checkpoint activation");
      }
      return journal;
    },
    async activateTarget(journal) {
      if (journal.keeperUpdate) {
        assertKeeperActionAvailable("target");
        await recovery.applyKeeperUpdate(
          journal.workerFingerprint!,
          journal.keeperUpdate,
          "target",
          journal.targetReleasePath,
        );
      }
      await recovery.bootout(journal);
      await recovery.setDisabled(journal, false);
      await recovery.bootstrap(journal);
      await recovery.kickstart(journal);
    },
  };
}
