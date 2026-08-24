// ssh command builders and the remote journal controller for the macOS worker
// deploy: envelope parsing of the remote bun -e program's output (see
// macos-deploy-journal-program.ts), the per-action utility commands, target/
// prior lifecycle proof commands, and the recovery remote handed to
// _recoverMacosDeployJournal in deploy-macos-journal.ts. deploy.ts is the
// runtime caller; tests pin command strings — bodies are byte-stable.

import {
  DeployFailure,
  workerServiceIsRunning,
  workerServiceMatchesRelease,
} from "./deploy-exec.ts";
import { launchdBootstrapWithRetryCmd, verifyWorkerCmd } from "./service-ctl.ts";
import { posixShellQuote } from "./shell-quote.ts";
import { MACOS_DEPLOY_JOURNAL_PROGRAM } from "./macos-deploy-journal-program.ts";
import {
  MACOS_WORKER_LABEL,
  _parseMacosDeployJournal,
  type MacosDeployJournalV1,
  type MacosDeployRecoveryRemote,
} from "./deploy-macos-journal.ts";
import { isCanonicalAbsolutePosixPath } from "./posix-deploy-journal.ts";

const MACOS_WORKER_PLIST_RELATIVE = `Library/LaunchAgents/${MACOS_WORKER_LABEL}.plist`;
const MACOS_RELEASE_ROOT_RELATIVE = "RoostWorkerV2-releases";
const MACOS_DEPLOY_JOURNAL_OUTPUT = "RoostMacDeployJournal=";

interface MacosJournalEnvelopeCandidate {
  releaseRoot?: unknown;
  journal?: unknown;
}

function macosJournalUtilityCommand(
  journalPath: string,
  action: "load" | "prepare" | "checkpoint-activating" | "restore-prior"
    | "prove-prior-definition" | "remove-target" | "cleanup-prior" | "clear",
  target?: { gitSha: string; remoteDir: string },
): string {
  if (target && !target.remoteDir.startsWith("~/")) {
    throw new Error("macOS staged release must be relative to the remote home");
  }
  const targetDirectory = target
    ? `target_spec=${posixShellQuote(target.remoteDir.slice(2))}; ` +
      `target_path=$(cd "$HOME/$target_spec" && pwd -P); `
    : `target_path=''; `;
  return `set -e; umask 077; journal_spec=${posixShellQuote(journalPath)}; ` +
    `case "$journal_spec" in /*) journal="$journal_spec";; *) journal="$HOME/$journal_spec";; esac; ` +
    `release_root="$HOME/${MACOS_RELEASE_ROOT_RELATIVE}"; ` +
    `if test -d "$release_root"; then release_root=$(cd "$release_root" && pwd -P); fi; ` +
    `plist="$HOME/${MACOS_WORKER_PLIST_RELATIVE}"; ${targetDirectory}` +
    `ROOST_MAC_DEPLOY_ACTION=${posixShellQuote(action)} ` +
    `ROOST_MAC_DEPLOY_JOURNAL="$journal" ROOST_MAC_DEPLOY_RELEASE_ROOT="$release_root" ` +
    `ROOST_MAC_DEPLOY_PLIST="$plist" ROOST_MAC_DEPLOY_LABEL=${posixShellQuote(MACOS_WORKER_LABEL)} ` +
    `ROOST_MAC_DEPLOY_TARGET_SHA=${posixShellQuote(target?.gitSha ?? "")} ` +
    `ROOST_MAC_DEPLOY_TARGET_PATH="$target_path" bun -e ${posixShellQuote(MACOS_DEPLOY_JOURNAL_PROGRAM)}`;
}

function parseMacosJournalEnvelope(stdout: string): {
  releaseRoot: string;
  journal: MacosDeployJournalV1 | null;
} {
  const encoded = stdout.split(/\r?\n/)
    .find((line) => line.startsWith(MACOS_DEPLOY_JOURNAL_OUTPUT))
    ?.slice(MACOS_DEPLOY_JOURNAL_OUTPUT.length);
  if (!encoded) throw new Error("remote macOS deploy journal returned no state");
  let value: unknown;
  try {
    value = JSON.parse(Buffer.from(encoded, "base64").toString("utf8"));
  } catch {
    throw new Error("remote macOS deploy journal returned malformed state");
  }
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new Error("remote macOS deploy journal envelope is malformed");
  }
  const candidate = value as MacosJournalEnvelopeCandidate;
  if (typeof candidate.releaseRoot !== "string"
    || !isCanonicalAbsolutePosixPath(candidate.releaseRoot)) {
    throw new Error("remote macOS deploy release root is malformed");
  }
  return {
    releaseRoot: candidate.releaseRoot,
    journal: candidate.journal === null
      ? null
      : _parseMacosDeployJournal(candidate.journal, candidate.releaseRoot),
  };
}

type MacosRemoteExecutor = (
  command: string,
) => Promise<{ exit: number; stdout: string; stderr: string }>;

export interface MacosDeployJournalController {
  recovery: MacosDeployRecoveryRemote;
  prepare(gitSha: string, remoteDir: string): Promise<MacosDeployJournalV1>;
  checkpointActivating(gitSha: string, remoteDir: string): Promise<MacosDeployJournalV1>;
}

function macosTargetVerificationCommand(journal: Readonly<MacosDeployJournalV1>): string {
  return `${verifyWorkerCmd("darwin")}; verify_status=$?; ` +
    `actual=$(/usr/libexec/PlistBuddy -c 'Print :WorkingDirectory' "$HOME/${MACOS_WORKER_PLIST_RELATIVE}" 2>/dev/null || true); ` +
    `sha=$(/usr/libexec/PlistBuddy -c 'Print :EnvironmentVariables:GIT_SHA' "$HOME/${MACOS_WORKER_PLIST_RELATIVE}" 2>/dev/null || true); ` +
    `if test "$actual" = ${posixShellQuote(journal.targetReleasePath)} ` +
    `&& test "$sha" = ${posixShellQuote(journal.targetGitSha)}; then echo RoostReleaseMatch=yes; fi; ` +
    `exit "$verify_status"`;
}

const MACOS_PRIOR_LIFECYCLE_PROOF_COMMAND =
  `uid=$(id -u); launch_output=$(launchctl print gui/$uid/${MACOS_WORKER_LABEL} 2>&1); launch_status=$?; ` +
  `printf '%s\\n' "$launch_output"; ` +
  `if test "$launch_status" -eq 0; then echo RoostLaunchdLoaded=yes; else echo RoostLaunchdLoaded=no; fi; ` +
  `disabled_output=$(launchctl print-disabled gui/$uid 2>&1); disabled_status=$?; ` +
  `printf '%s\\n' "$disabled_output"; test "$disabled_status" -eq 0 || exit "$disabled_status"; ` +
  `if printf '%s\\n' "$disabled_output" | ` +
  // Both launchctl shapes mean disabled: the legacy boolean and the current
  // word form (macOS 15 prints "com.roost.worker-v2" => enabled|disabled).
  `grep -Eq '"com[.]roost[.]worker-v2"[[:space:]]*=>[[:space:]]*(true|disabled)'; ` +
  `then echo RoostLaunchdDisabled=yes; else echo RoostLaunchdDisabled=no; fi`;

export function createMacosDeployJournalController(
  execute: MacosRemoteExecutor,
  journalPath: string,
  signal?: AbortSignal,
): MacosDeployJournalController {
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
  ): { releaseRoot: string; journal: MacosDeployJournalV1 | null } => {
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
    target?: { gitSha: string; remoteDir: string },
  ) => macosJournalUtilityCommand(journalPath, action, target);

  const recovery: MacosDeployRecoveryRemote = {
    async load() {
      const result = await checked("load macOS deploy journal", utility("load"));
      return parseEnvelope(result, "load macOS deploy journal").journal;
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
    async restorePriorDefinition() {
      await checked("restore prior macOS worker plist", utility("restore-prior"));
    },
    async setDisabled(_journal, disabled) {
      await checked(
        `restore macOS worker ${disabled ? "disabled" : "enabled"} override`,
        `launchctl ${disabled ? "disable" : "enable"} gui/$(id -u)/${MACOS_WORKER_LABEL}`,
      );
    },
    async bootstrap() {
      await checked(
        "bootstrap prior macOS worker",
        launchdBootstrapWithRetryCmd(MACOS_WORKER_LABEL, MACOS_WORKER_PLIST_RELATIVE, {
          role: "prior launchd", reload: false, homeRelative: true,
        }),
      );
    },
    async kickstart() {
      await checked(
        "kickstart prior macOS worker",
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
    async prepare(gitSha, remoteDir) {
      const result = await checked(
        "prepare macOS deploy journal",
        utility("prepare", { gitSha, remoteDir }),
      );
      const journal = parseEnvelope(result, "prepare macOS deploy journal").journal;
      if (!journal || journal.phase !== "prepared") {
        throw new DeployFailure(5, "remote Mac did not durably prepare its deploy journal");
      }
      return journal;
    },
    async checkpointActivating(gitSha, remoteDir) {
      const result = await checked(
        "checkpoint activating macOS deploy",
        utility("checkpoint-activating", { gitSha, remoteDir }),
      );
      const journal = parseEnvelope(result, "checkpoint activating macOS deploy").journal;
      if (!journal || journal.phase !== "activating") {
        throw new DeployFailure(5, "remote Mac did not durably checkpoint activation");
      }
      return journal;
    },
  };
}
