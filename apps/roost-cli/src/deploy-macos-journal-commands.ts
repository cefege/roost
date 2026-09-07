// Remote macOS deploy journal command construction and envelope parsing.
// deploy-macos-journal-controller.ts executes these byte-stable commands.
// The transmitted Bun program and shared shell quoting define the boundary.

import { JournaledKeeperUpdateV1Schema } from "@roost/shared/keeper-update";
import type { JournaledKeeperUpdateV1 } from "@roost/shared/keeper-update";
import { posixShellQuote } from "@roost/shared/shell-quote";
import {
  MACOS_WORKER_LABEL,
  _parseMacosDeployJournal,
} from "./deploy-macos-journal.ts";
import type { MacosDeployJournalV3 } from "./deploy-macos-journal.ts";
import { DURABLE_WORKER_STATE_FILE } from "./durable-worker-state.ts";
import { MACOS_DEPLOY_JOURNAL_PROGRAM } from "./macos-deploy-journal-program.ts";
import { isCanonicalAbsolutePosixPath } from "./posix-deploy-journal.ts";
import { verifyWorkerCmd } from "./service-ctl.ts";

export const MACOS_WORKER_PLIST_RELATIVE =
  `Library/LaunchAgents/${MACOS_WORKER_LABEL}.plist`;
const MACOS_RELEASE_ROOT_RELATIVE = "RoostWorkerV2-releases";
const MACOS_DEPLOY_JOURNAL_OUTPUT = "RoostMacDeployJournal=";

interface MacosJournalEnvelopeCandidate {
  releaseRoot?: unknown;
  journal?: unknown;
}

export type MacosJournalTarget = {
  gitSha: string;
  rolloutId: string | null;
  workerFingerprint: string | null;
  keeperUpdate: JournaledKeeperUpdateV1 | null;
} & ({ remoteDir: string } | { targetPath: string });

export function macosJournalUtilityCommand(
  journalPath: string,
  action: "load" | "prepare" | "checkpoint-activating" | "checkpoint-activated"
    | "checkpoint-rollback" | "checkpoint-commit" | "restore-prior"
    | "prove-prior-definition" | "remove-target" | "cleanup-prior" | "clear",
  target?: MacosJournalTarget,
): string {
  if (target && "remoteDir" in target && !target.remoteDir.startsWith("~/")) {
    throw new Error("macOS staged release must be relative to the remote home");
  }
  if (target && "targetPath" in target && !isCanonicalAbsolutePosixPath(target.targetPath)) {
    throw new Error("macOS staged release must be a canonical absolute path");
  }
  const keeperUpdate = target?.keeperUpdate ?? null;
  const encodedKeeperUpdate = keeperUpdate === null
    ? ""
    : Buffer.from(JSON.stringify(
        JournaledKeeperUpdateV1Schema.parse(keeperUpdate),
      )).toString("base64");
  const targetDirectory = !target
    ? `target_path=''; `
    : "remoteDir" in target
      ? `target_spec=${posixShellQuote(target.remoteDir.slice(2))}; ` +
        `target_path=$(cd "$HOME/$target_spec" && pwd -P); `
      : `target_path=${posixShellQuote(target.targetPath)}; `;
  return `set -e; umask 077; journal_spec=${posixShellQuote(journalPath)}; ` +
    `case "$journal_spec" in /*) journal="$journal_spec";; *) journal="$HOME/$journal_spec";; esac; ` +
    `release_root="$HOME/${MACOS_RELEASE_ROOT_RELATIVE}"; ` +
    `if test -d "$release_root"; then release_root=$(cd "$release_root" && pwd -P); fi; ` +
    `plist="$HOME/${MACOS_WORKER_PLIST_RELATIVE}"; ${targetDirectory}` +
    `durable_state="$(dirname -- "$(dirname -- "$journal")")/${DURABLE_WORKER_STATE_FILE}"; ` +
    `ROOST_MAC_DEPLOY_ACTION=${posixShellQuote(action)} ` +
    `ROOST_MAC_DEPLOY_JOURNAL="$journal" ROOST_MAC_DEPLOY_RELEASE_ROOT="$release_root" ` +
    `ROOST_MAC_DEPLOY_PLIST="$plist" ROOST_MAC_DEPLOY_LABEL=${posixShellQuote(MACOS_WORKER_LABEL)} ` +
    `ROOST_MAC_DEPLOY_DURABLE_STATE="$durable_state" ` +
    `ROOST_MAC_DEPLOY_TARGET_SHA=${posixShellQuote(target?.gitSha ?? "")} ` +
    `ROOST_MAC_DEPLOY_ROLLOUT_ID=${posixShellQuote(target?.rolloutId ?? "")} ` +
    `ROOST_MAC_DEPLOY_WORKER_FINGERPRINT=${posixShellQuote(target?.workerFingerprint ?? "")} ` +
    `ROOST_MAC_DEPLOY_KEEPER_UPDATE=${posixShellQuote(encodedKeeperUpdate)} ` +
    `ROOST_MAC_DEPLOY_TARGET_PATH="$target_path" bun -e ${posixShellQuote(MACOS_DEPLOY_JOURNAL_PROGRAM)}`;
}

export function parseMacosJournalEnvelope(stdout: string): {
  releaseRoot: string;
  journal: MacosDeployJournalV3 | null;
} {
  const encoded = stdout.split(/\r?\n/)
    .find((line) => line.startsWith(MACOS_DEPLOY_JOURNAL_OUTPUT))
    ?.slice(MACOS_DEPLOY_JOURNAL_OUTPUT.length);
  if (!encoded || Buffer.from(encoded, "base64").toString("base64") !== encoded) {
    throw new Error("remote macOS deploy journal returned malformed state");
  }
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
  const envelopeFields = Object.keys(candidate);
  if (envelopeFields.length !== 2
    || envelopeFields.some(field => field !== "releaseRoot" && field !== "journal")) {
    throw new Error("remote macOS deploy journal envelope fields are malformed");
  }
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

export function macosTargetVerificationCommand(
  journal: Readonly<MacosDeployJournalV3>,
): string {
  return `${verifyWorkerCmd("darwin")}; verify_status=$?; ` +
    `actual=$(/usr/libexec/PlistBuddy -c 'Print :WorkingDirectory' "$HOME/${MACOS_WORKER_PLIST_RELATIVE}" 2>/dev/null || true); ` +
    `sha=$(/usr/libexec/PlistBuddy -c 'Print :EnvironmentVariables:GIT_SHA' "$HOME/${MACOS_WORKER_PLIST_RELATIVE}" 2>/dev/null || true); ` +
    `if test "$actual" = ${posixShellQuote(journal.targetReleasePath)} ` +
    `&& test "$sha" = ${posixShellQuote(journal.targetGitSha)}; then echo RoostReleaseMatch=yes; fi; ` +
    `exit "$verify_status"`;
}

export const MACOS_PRIOR_LIFECYCLE_PROOF_COMMAND =
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
