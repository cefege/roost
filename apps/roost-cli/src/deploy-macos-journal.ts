// Schema, confinement, parsing, decision, and the recovery state machine for
// the macOS worker deploy journal (macos-worker-deploy-v1.json beside the
// machine-transaction database). Pure TypeScript only: the remote bun -e
// program lives in macos-deploy-journal-program.ts and the ssh command
// builders/controller live in deploy-macos-journal-controller.ts. Tests pin
// the `_`-prefixed surface; the remote program mirrors this module's
// validation, so change them together. Built on posix-deploy-journal.ts.

import { posix } from "node:path";
import {
  POSIX_RELEASE_ID_SUFFIX_RE,
  isCanonicalAbsolutePosixPath,
  posixDeployJournalDecision,
  posixJournalObjectValue,
} from "./posix-deploy-journal.ts";

export const MACOS_WORKER_LABEL = "com.roost.worker-v2";
const MACOS_DEPLOY_JOURNAL_FILE = "macos-worker-deploy-v1.json";
const MACOS_GIT_SHA_RE = /^[a-f0-9]{40,64}$/;

export type MacosWorkerLifecycle = "unloaded" | "loaded" | "running";
export type MacosDeployJournalPhase = "prepared" | "activating";

export interface MacosDeployJournalV1 {
  schemaVersion: 1;
  phase: MacosDeployJournalPhase;
  targetGitSha: string;
  targetReleasePath: string;
  priorPlistBase64: string | null;
  priorPlistMode: number | null;
  priorLifecycle: MacosWorkerLifecycle;
  priorPid: number | null;
  priorDisabled: boolean;
  createdAt: string;
  updatedAt: string;
}

interface MacosDeployJournalCandidate {
  schemaVersion?: unknown;
  phase?: unknown;
  targetGitSha?: unknown;
  targetReleasePath?: unknown;
  priorPlistBase64?: unknown;
  priorPlistMode?: unknown;
  priorLifecycle?: unknown;
  priorPid?: unknown;
  priorDisabled?: unknown;
  createdAt?: unknown;
  updatedAt?: unknown;
}

export interface MacosDeployTargetProof {
  definitionMatches: boolean;
  running: boolean;
  result: { exit: number; stdout: string; stderr: string };
}

export interface MacosDeployRecoveryRemote {
  load(): Promise<MacosDeployJournalV1 | null>;
  proveTarget(journal: Readonly<MacosDeployJournalV1>): Promise<MacosDeployTargetProof>;
  bootout(journal: Readonly<MacosDeployJournalV1>): Promise<void>;
  restorePriorDefinition(journal: Readonly<MacosDeployJournalV1>): Promise<void>;
  setDisabled(journal: Readonly<MacosDeployJournalV1>, disabled: boolean): Promise<void>;
  bootstrap(journal: Readonly<MacosDeployJournalV1>): Promise<void>;
  kickstart(journal: Readonly<MacosDeployJournalV1>): Promise<void>;
  stop(journal: Readonly<MacosDeployJournalV1>): Promise<void>;
  provePrior(journal: Readonly<MacosDeployJournalV1>): Promise<void>;
  removeTarget(journal: Readonly<MacosDeployJournalV1>): Promise<void>;
  cleanupPriorRelease(journal: Readonly<MacosDeployJournalV1>): Promise<void>;
  clear(journal: Readonly<MacosDeployJournalV1>): Promise<void>;
}

export type MacosDeployRecoveryResult =
  | { outcome: "none" }
  | { outcome: "prepared-cleaned"; journal: MacosDeployJournalV1 }
  | { outcome: "committed"; journal: MacosDeployJournalV1; targetProof: MacosDeployTargetProof }
  | { outcome: "rolled-back"; journal: MacosDeployJournalV1; targetProof: MacosDeployTargetProof };


/** The journal is fixed beside the renewable machine-transaction database. */
export function _macosDeployJournalPath(machineTransactionPath: string): string {
  return posix.join(posix.dirname(machineTransactionPath), MACOS_DEPLOY_JOURNAL_FILE);
}

/** Only a canonical, direct release child with the expected identity is trusted. */
export function _isConfinedMacosReleasePath(
  releaseRoot: string,
  releasePath: string,
  gitSha: string,
): boolean {
  if (!isCanonicalAbsolutePosixPath(releaseRoot)
    || !isCanonicalAbsolutePosixPath(releasePath)
    || !MACOS_GIT_SHA_RE.test(gitSha)) {
    return false;
  }
  const relative = posix.relative(releaseRoot, releasePath);
  if (!relative || relative.startsWith("../") || relative === ".." || relative.includes("/")) {
    return false;
  }
  if (!relative.startsWith(`${gitSha}-`)) return false;
  return POSIX_RELEASE_ID_SUFFIX_RE.test(relative.slice(gitSha.length + 1));
}


/** Parse untrusted remote state before any path from it reaches a command. */
export function _parseMacosDeployJournal(
  value: unknown,
  releaseRoot: string,
): MacosDeployJournalV1 {
  const candidate = posixJournalObjectValue(
    value,
    "macOS deploy journal is not an object",
  ) as MacosDeployJournalCandidate;
  const {
    schemaVersion,
    phase,
    targetGitSha,
    targetReleasePath,
    priorPlistBase64,
    priorPlistMode,
    priorLifecycle,
    priorPid,
    priorDisabled,
    createdAt,
    updatedAt,
  } = candidate;
  if (schemaVersion !== 1) throw new Error("macOS deploy journal schema is unsupported");
  if (phase !== "prepared" && phase !== "activating") {
    throw new Error("macOS deploy journal phase is malformed");
  }
  if (typeof targetGitSha !== "string"
    || typeof targetReleasePath !== "string"
    || !_isConfinedMacosReleasePath(releaseRoot, targetReleasePath, targetGitSha)) {
    throw new Error("macOS deploy journal target path or identity is malformed");
  }
  if (priorLifecycle !== "unloaded" && priorLifecycle !== "loaded" && priorLifecycle !== "running") {
    throw new Error("macOS deploy journal prior lifecycle is malformed");
  }
  if (priorLifecycle === "running") {
    if (typeof priorPid !== "number" || !Number.isSafeInteger(priorPid) || priorPid < 1) {
      throw new Error("macOS deploy journal prior process epoch is malformed");
    }
  } else if (priorPid !== null) {
    throw new Error("macOS deploy journal has a process epoch for a non-running service");
  }
  if (typeof priorDisabled !== "boolean") {
    throw new Error("macOS deploy journal disabled override is malformed");
  }
  if (priorPlistBase64 !== null) {
    if (typeof priorPlistBase64 !== "string" || priorPlistBase64.length > 2 * 1024 * 1024) {
      throw new Error("macOS deploy journal prior plist is malformed");
    }
    try {
      if (Buffer.from(priorPlistBase64, "base64").toString("base64") !== priorPlistBase64) {
        throw new Error("non-canonical base64");
      }
    } catch {
      throw new Error("macOS deploy journal prior plist is malformed");
    }
  }
  if (priorPlistBase64 === null) {
    if (priorPlistMode !== null || priorLifecycle !== "unloaded") {
      throw new Error("macOS deploy journal cannot restore a loaded service without plist bytes");
    }
  } else if (typeof priorPlistMode !== "number"
    || !Number.isSafeInteger(priorPlistMode)
    || priorPlistMode < 0
    || priorPlistMode > 0o777) {
    throw new Error("macOS deploy journal prior plist mode is malformed");
  }
  if (typeof createdAt !== "string"
    || typeof updatedAt !== "string"
    || !Number.isFinite(Date.parse(createdAt))
    || !Number.isFinite(Date.parse(updatedAt))) {
    throw new Error("macOS deploy journal timestamps are malformed");
  }
  return {
    schemaVersion: 1,
    phase,
    targetGitSha,
    targetReleasePath,
    priorPlistBase64,
    priorPlistMode,
    priorLifecycle,
    priorDisabled,
    priorPid,
    createdAt,
    updatedAt,
  };
}

export function _decideMacosDeployRecovery(
  phase: MacosDeployJournalPhase,
  target: Pick<MacosDeployTargetProof, "definitionMatches" | "running"> | null,
): "clean-prepared" | "commit" | "rollback" {
  // Shared prepared⇒clean / health⇒commit|rollback decision; macOS health is
  // "definition matches AND service running (with an advanced pid)".
  return posixDeployJournalDecision(
    phase,
    target?.definitionMatches === true && target.running === true,
  );
}

/**
 * Recover one journal while its caller holds the remote deployment lease.
 * Clear is deliberately last in every branch: a dead CLI or failed proof
 * leaves an idempotent transaction for the next deploy to resume.
 */
export async function _recoverMacosDeployJournal(
  remote: MacosDeployRecoveryRemote,
): Promise<MacosDeployRecoveryResult> {
  const journal = await remote.load();
  if (!journal) return { outcome: "none" };
  if (_decideMacosDeployRecovery(journal.phase, null) === "clean-prepared") {
    await remote.removeTarget(journal);
    await remote.clear(journal);
    return { outcome: "prepared-cleaned", journal };
  }

  const targetProof = await remote.proveTarget(journal);
  if (_decideMacosDeployRecovery(journal.phase, targetProof) === "commit") {
    await remote.cleanupPriorRelease(journal);
    await remote.clear(journal);
    return { outcome: "committed", journal, targetProof };
  }

  await remote.bootout(journal);
  await remote.restorePriorDefinition(journal);
  if (journal.priorLifecycle === "running") {
    await remote.setDisabled(journal, false);
    await remote.bootstrap(journal);
    await remote.kickstart(journal);
    await remote.setDisabled(journal, journal.priorDisabled);
  } else if (journal.priorLifecycle === "loaded") {
    // Bootstrap while enabled, then suppress KeepAlive long enough to restore
    // the exact loaded-but-not-running lifecycle before restoring the override.
    await remote.setDisabled(journal, false);
    await remote.bootstrap(journal);
    await remote.setDisabled(journal, true);
    await remote.stop(journal);
    await remote.setDisabled(journal, journal.priorDisabled);
  } else {
    await remote.setDisabled(journal, journal.priorDisabled);
  }
  await remote.provePrior(journal);
  await remote.removeTarget(journal);
  await remote.clear(journal);
  return { outcome: "rolled-back", journal, targetProof };
}
