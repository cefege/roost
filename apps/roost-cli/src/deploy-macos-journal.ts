// Schema, strict parsing, and pure decisions for the macOS worker journal.
// The remote Bun program and SSH controller mirror this byte contract.
// Recovery choreography lives in deploy-macos-recovery.ts.

import { posix } from "node:path";
import {
  JournaledKeeperUpdateV1Schema,
  type JournaledKeeperUpdateV1,
} from "@roost/shared/keeper-update";
import {
  POSIX_RELEASE_ID_SUFFIX_RE,
  isCanonicalAbsolutePosixPath,
  posixDeployJournalDecision,
  posixJournalObjectValue,
} from "./posix-deploy-journal.ts";
import { parsePosixServiceEnvironment } from "./deploy-plist-env.ts";
import {
  workerRolloutIdOrNull,
  workerRolloutFingerprintOrNull,
} from "./worker-deploy-rollout.ts";
import { isDurableStateVersion } from "./durable-worker-state.ts";

export const MACOS_WORKER_LABEL = "com.roost.worker-v2";
const MACOS_DEPLOY_JOURNAL_FILE = "macos-worker-deploy-v1.json";
export const MACOS_GIT_SHA_RE = /^[a-f0-9]{40,64}$/;

export type MacosWorkerLifecycle = "unloaded" | "loaded" | "running";
export type MacosDeployJournalPhase =
  | "prepared"
  | "activating"
  | "activated"
  | "committing"
  | "rolling-back";

export interface MacosDeployJournalV3 {
  schemaVersion: 3;
  phase: MacosDeployJournalPhase;
  targetGitSha: string;
  targetReleasePath: string;
  rolloutId: string | null;
  workerFingerprint: string | null;
  keeperUpdate: JournaledKeeperUpdateV1 | null;
  priorPlistBase64: string | null;
  priorPlistMode: number | null;
  priorLifecycle: MacosWorkerLifecycle;
  priorPid: number | null;
  priorDisabled: boolean;
  // Durable session-event store schema versions: the one the prior release
  // ran with, and the one the target release left behind. A higher target
  // version means the prior release can no longer open the store, so no
  // amount of launchd choreography can restore it.
  priorDurableStateVersion: number | null;
  targetDurableStateVersion: number | null;
  createdAt: string;
  updatedAt: string;
}

interface MacosDeployJournalCandidate {
  schemaVersion?: unknown;
  phase?: unknown;
  targetGitSha?: unknown;
  targetReleasePath?: unknown;
  rolloutId?: unknown;
  workerFingerprint?: unknown;
  keeperUpdate?: unknown;
  priorPlistBase64?: unknown;
  priorPlistMode?: unknown;
  priorLifecycle?: unknown;
  priorPid?: unknown;
  priorDisabled?: unknown;
  priorDurableStateVersion?: unknown;
  targetDurableStateVersion?: unknown;
  createdAt?: unknown;
  updatedAt?: unknown;
}

export interface MacosDeployTargetProof {
  definitionMatches: boolean;
  running: boolean;
  result: { exit: number; stdout: string; stderr: string };
}

export interface MacosDeployRecoveryRemote {
  load(): Promise<MacosDeployJournalV3 | null>;
  checkpointActivated(journal: Readonly<MacosDeployJournalV3>): Promise<MacosDeployJournalV3>;
  /** Records the durable-state version the target left behind, so a rollback
   * that cannot restore service can prove why on every later attempt. */
  checkpointRollback(journal: Readonly<MacosDeployJournalV3>): Promise<MacosDeployJournalV3>;
  checkpointCommit(journal: Readonly<MacosDeployJournalV3>): Promise<void>;
  proveTarget(journal: Readonly<MacosDeployJournalV3>): Promise<MacosDeployTargetProof>;
  bootout(journal: Readonly<MacosDeployJournalV3>): Promise<void>;
  restorePriorDefinition(journal: Readonly<MacosDeployJournalV3>): Promise<void>;
  setDisabled(journal: Readonly<MacosDeployJournalV3>, disabled: boolean): Promise<void>;
  applyKeeperUpdate(
    workerFingerprint: string,
    update: JournaledKeeperUpdateV1,
    direction: "source" | "target",
    actionReleasePath: string,
  ): Promise<void>;
  proveKeeperUpdate(
    workerFingerprint: string,
    update: JournaledKeeperUpdateV1,
    direction: "source" | "target",
    expectedWorkerSha: string,
    heartbeatNotBeforeMs: number,
    actionReleasePath: string,
  ): Promise<void>;
  bootstrap(journal: Readonly<MacosDeployJournalV3>): Promise<void>;
  kickstart(journal: Readonly<MacosDeployJournalV3>): Promise<void>;
  stop(journal: Readonly<MacosDeployJournalV3>): Promise<void>;
  /** Proves the prior plist is restored byte-exactly and its recorded
   * lifecycle round-tripped. `priorStarted` states whether this rollback
   * actually started that release: it is what separates a prior release that
   * cannot run from one that was never started, and only the former may
   * resolve to a roll-forward. */
  provePrior(
    journal: Readonly<MacosDeployJournalV3>,
    priorStarted: boolean,
  ): Promise<void>;
  removeTarget(journal: Readonly<MacosDeployJournalV3>): Promise<void>;
  cleanupPriorRelease(journal: Readonly<MacosDeployJournalV3>): Promise<void>;
  clear(journal: Readonly<MacosDeployJournalV3>): Promise<void>;
  now?: () => number;
}

export type MacosDeployRecoveryResult =
  | { outcome: "none" }
  | { outcome: "prepared-cleaned"; journal: MacosDeployJournalV3 }
  | { outcome: "held"; journal: MacosDeployJournalV3; targetProof: MacosDeployTargetProof }
  | { outcome: "committed"; journal: MacosDeployJournalV3; targetProof: MacosDeployTargetProof }
  | { outcome: "rolled-back"; journal: MacosDeployJournalV3; targetProof: MacosDeployTargetProof | null }
  | {
      outcome: "roll-forward-required";
      journal: MacosDeployJournalV3;
      targetProof: MacosDeployTargetProof | null;
      reason: string;
    };


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
): MacosDeployJournalV3 {
  const candidate = posixJournalObjectValue(
    value,
    "macOS deploy journal is not an object",
  ) as MacosDeployJournalCandidate;
  const {
    schemaVersion,
    phase,
    targetGitSha,
    targetReleasePath,
    rolloutId,
    workerFingerprint,
    keeperUpdate,
    priorPlistBase64,
    priorPlistMode,
    priorLifecycle,
    priorPid,
    priorDisabled,
    priorDurableStateVersion,
    targetDurableStateVersion,
    createdAt,
    updatedAt,
  } = candidate;
  // A v2 journal predates the durable-state observation and upgrades with
  // both versions unknown, which can never authorize a roll-forward.
  const durableFields = ["priorDurableStateVersion", "targetDurableStateVersion"];
  const journalFields = [
    "schemaVersion", "phase", "targetGitSha", "targetReleasePath", "rolloutId",
    "workerFingerprint", "keeperUpdate", "priorPlistBase64", "priorPlistMode",
    "priorLifecycle", "priorPid", "priorDisabled", "createdAt", "updatedAt",
    ...(schemaVersion === 2 ? [] : durableFields),
  ];
  const presentFields = Object.keys(candidate);
  if (presentFields.length !== journalFields.length
    || presentFields.some(field => !journalFields.includes(field))) {
    throw new Error("macOS deploy journal fields are malformed");
  }
  if (schemaVersion !== 2 && schemaVersion !== 3) {
    throw new Error("macOS deploy journal schema is unsupported");
  }
  const priorDurableState = schemaVersion === 2 ? null : priorDurableStateVersion;
  const targetDurableState = schemaVersion === 2 ? null : targetDurableStateVersion;
  if ((priorDurableState !== null && !isDurableStateVersion(priorDurableState))
    || (targetDurableState !== null && !isDurableStateVersion(targetDurableState))) {
    throw new Error("macOS deploy journal durable state version is malformed");
  }
  if (phase !== "prepared" && phase !== "activating"
    && phase !== "activated" && phase !== "committing"
    && phase !== "rolling-back") {
    throw new Error("macOS deploy journal phase is malformed");
  }
  if (typeof targetGitSha !== "string"
    || typeof targetReleasePath !== "string"
    || !_isConfinedMacosReleasePath(releaseRoot, targetReleasePath, targetGitSha)) {
    throw new Error("macOS deploy journal target path or identity is malformed");
  }
  const parsedRolloutId = workerRolloutIdOrNull(rolloutId, "macOS worker rollout ID");
  const parsedWorkerFingerprint = workerRolloutFingerprintOrNull(
    workerFingerprint,
    "macOS worker fingerprint",
  );
  const parsedKeeperUpdate = keeperUpdate === null
    ? null
    : JournaledKeeperUpdateV1Schema.safeParse(keeperUpdate);
  if (parsedKeeperUpdate !== null && !parsedKeeperUpdate.success) {
    throw new Error("macOS deploy journal keeper update is malformed");
  }
  if ((parsedKeeperUpdate === null) !== (parsedWorkerFingerprint === null)) {
    throw new Error("macOS deploy journal keeper update and worker fingerprint disagree");
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
  if (priorLifecycle === "loaded" && !priorDisabled) {
    throw new Error("enabled KeepAlive worker cannot have a durable loaded lifecycle");
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
  const checkedKeeperUpdate = parsedKeeperUpdate === null
    ? null
    : parsedKeeperUpdate.data;
  // One-directional on purpose: a journaled keeper action can only be replayed
  // against prior plist bytes to restore, but a prior service does not require
  // one. A worker that reports no keeper runtime is staged without an update,
  // so its journal restores the service and mutates no keeper.
  if (checkedKeeperUpdate !== null && priorPlistBase64 === null) {
    throw new Error(
      "macOS deploy journal keeper update requires prior plist bytes to restore",
    );
  }
  if (checkedKeeperUpdate && priorPlistBase64) {
    const priorEnvironment = parsePosixServiceEnvironment(
      Buffer.from(priorPlistBase64, "base64").toString("utf8"),
      "darwin",
    );
    const priorSha = priorEnvironment.GIT_SHA ?? priorEnvironment.ROOST_GIT_SHA;
    if (!priorSha || !MACOS_GIT_SHA_RE.test(priorSha)) {
      throw new Error("macOS deploy journal cannot prove the prior worker identity");
    }
  }
  if (typeof createdAt !== "string"
    || typeof updatedAt !== "string"
    || !Number.isFinite(Date.parse(createdAt))
    || !Number.isFinite(Date.parse(updatedAt))) {
    throw new Error("macOS deploy journal timestamps are malformed");
  }
  return {
    schemaVersion: 3,
    phase,
    targetGitSha,
    targetReleasePath,
    rolloutId: parsedRolloutId,
    workerFingerprint: parsedWorkerFingerprint,
    keeperUpdate: checkedKeeperUpdate,
    priorPlistBase64,
    priorPlistMode,
    priorLifecycle,
    priorPid,
    priorDisabled,
    priorDurableStateVersion: priorDurableState,
    targetDurableStateVersion: targetDurableState,
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

