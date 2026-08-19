// `roost deploy <host>` — refresh the v2 worker on a tailnet host.
//
// macOS target: rsync the worker tree, `bun install`, (re)install the
// LaunchAgent via apps/worker/scripts/install.sh, kickstart it. Idempotent —
// first deploy installs, subsequent deploys just refresh + kickstart.
//
// Layout on remote:
//   ~/RoostWorkerV2/
//     apps/worker/...        (rsynced)
//     apps/shared/...        (rsynced)
//     package.json + bun.lock
//     node_modules/          (bun install)
//
// Linux target: forks to deploy-linux.ts, which updates the git checkout
// join.sh left at /srv/roost and drives the systemd --user unit instead.
//
// LaunchAgent label: com.roost.worker-v2 (avoids collision with legacy
// com.roost.worker until R6.5).

import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join, posix, resolve } from "node:path";
import { acquireRemoteDeployLock, DeployFailure, failDeploy, finishWorkerDeploy, POSIX_WORKER_DEPLOY_JOURNAL_PATHS, releaseRemoteDeployLock, remoteMachineTransactionPath, workerServiceIsRunning, workerServiceMatchesRelease, run, runOrDie, SSH_OPTS, RSYNC_RSH, sshExec, resolveLocalGitShaOrDie, resolvePublishedGitShaOrDie } from "./deploy-exec.ts";
import { _isSelfHost } from "./deploy-self-host.ts";
import { _backfillEnvFromPlist, _resolveDeployEnvValue } from "./deploy-plist-env.ts";
import { _deployLocal } from "./deploy-local.ts";
import { manifestOnlyWorkspaces } from "./deploy-workspaces.ts";
import { deployLinux } from "./deploy-linux.ts";
import { launchdBootstrapWithRetryCmd, loadWindowsServiceDefinitions, verifyWorkerCmd } from "./service-ctl.ts";
import { buildAuthorizedApiClient, buildSelfAuthorizedApiClient } from "./api.ts";
import { loadWorkerConfig } from "../../worker/src/config.ts";
import type { CoordClient } from "../../worker/src/coord-client.ts";

// Re-export the public surface external modules import from ./deploy.ts —
// keeper-refresh.ts pulls { _isSelfHost, sshExec }; main/push/quickstart use deploy.
export { sshExec, _isSelfHost };

const REMOTE_DIR = "~/RoostWorkerV2";
const REPO_ROOT = resolve(import.meta.dir, "..", "..", "..");


/** Quote one value for a POSIX remote-shell assignment without expansion. */
export function _quoteRemoteShell(value: string): string {
  return `'${value.replaceAll("'", `'\"'\"'`)}'`;
}


function remoteEnvAssignment(key: string, value: string): string {
  return `${key}=${_quoteRemoteShell(value)}`;
}

function workerInstallEnvironment(
  installed: Record<string, string>,
  overrides: Record<string, string | undefined>,
  gitSha: string,
): string {
  const values: Record<string, string> = { ...installed };
  // These identify the active release itself and must point at the new stage,
  // never at the prior service's working directory or compiled binary.
  for (const key of ["GIT_SHA", "ROOST_GIT_SHA", "ROOST_WORKDIR", "ROOST_EXEC_BIN", "ROOST_BOOTSTRAP_TOKEN"]) {
    delete values[key];
  }
  for (const [key, value] of Object.entries(overrides)) {
    if (value === undefined) delete values[key];
    else values[key] = value;
  }
  values.GIT_SHA = gitSha;
  return Object.entries(values)
    .filter(([key]) => key === "GIT_SHA" || /^ROOST_[A-Z_]+$/.test(key))
    .map(([key, value]) => remoteEnvAssignment(key, value))
    .join(" ");
}

const MACOS_WORKER_LABEL = "com.roost.worker-v2";
const MACOS_WORKER_PLIST_RELATIVE = `Library/LaunchAgents/${MACOS_WORKER_LABEL}.plist`;
const MACOS_RELEASE_ROOT_RELATIVE = "RoostWorkerV2-releases";
const MACOS_DEPLOY_JOURNAL_FILE = "macos-worker-deploy-v1.json";
const MACOS_DEPLOY_JOURNAL_OUTPUT = "RoostMacDeployJournal=";
const MACOS_GIT_SHA_RE = /^[a-f0-9]{40,64}$/;
const MACOS_RELEASE_SUFFIX_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

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


function isCanonicalAbsolutePosixPath(value: string): boolean {
  return posix.isAbsolute(value)
    && posix.normalize(value) === value
    && !/[\r\n\0]/.test(value);
}

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
  return MACOS_RELEASE_SUFFIX_RE.test(relative.slice(gitSha.length + 1));
}


/** Parse untrusted remote state before any path from it reaches a command. */
export function _parseMacosDeployJournal(
  value: unknown,
  releaseRoot: string,
): MacosDeployJournalV1 {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new Error("macOS deploy journal is not an object");
  }
  const candidate = value as MacosDeployJournalCandidate;
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
  if (phase === "prepared") return "clean-prepared";
  return target?.definitionMatches === true && target.running === true ? "commit" : "rollback";
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

// Runs on the remote Mac. It owns only durable journal/definition bytes and
// confined release deletion; launchd lifecycle transitions remain explicit in
// the TypeScript recovery state machine above.
const MACOS_DEPLOY_JOURNAL_PROGRAM = String.raw`
// This program is transmitted to bun -e and has no module file from which
// static imports could resolve; literal dynamic imports are the remote boundary.
const fs = await import("node:fs");
const path = await import("node:path");
const { randomUUID } = await import("node:crypto");
const decoder = new TextDecoder();
const action = process.env.ROOST_MAC_DEPLOY_ACTION ?? "";
const journalPath = process.env.ROOST_MAC_DEPLOY_JOURNAL ?? "";
const releaseRoot = process.env.ROOST_MAC_DEPLOY_RELEASE_ROOT ?? "";
const plistPath = process.env.ROOST_MAC_DEPLOY_PLIST ?? "";
const label = process.env.ROOST_MAC_DEPLOY_LABEL ?? "";
const requestedSha = process.env.ROOST_MAC_DEPLOY_TARGET_SHA ?? "";
const requestedTarget = process.env.ROOST_MAC_DEPLOY_TARGET_PATH ?? "";
const outputPrefix = "RoostMacDeployJournal=";
const shaPattern = /^[a-f0-9]{40,64}$/;
const suffixPattern = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

function reject(message) {
  throw new Error(message);
}

function canonicalAbsolute(value, name) {
  if (!value || !path.isAbsolute(value) || path.normalize(value) !== value || /[\r\n\0]/.test(value)) {
    reject(name + " is not a canonical absolute path");
  }
  return value;
}

canonicalAbsolute(journalPath, "journal path");
canonicalAbsolute(releaseRoot, "release root");
canonicalAbsolute(plistPath, "plist path");
if (!label || /[\r\n\0/]/.test(label)) reject("launchd label is malformed");

function directReleasePath(value, sha) {
  canonicalAbsolute(value, "target release path");
  const relative = path.relative(releaseRoot, value);
  if (!relative || relative === ".." || relative.startsWith("../") || relative.includes("/")) {
    reject("target release path escapes the managed root");
  }
  if (sha !== null) {
    if (!shaPattern.test(sha)
      || !relative.startsWith(sha + "-")
      || !suffixPattern.test(relative.slice(sha.length + 1))) {
      reject("target release identity is malformed");
    }
  }
  if (fs.existsSync(releaseRoot)) {
    const rootStat = fs.lstatSync(releaseRoot);
    if (!rootStat.isDirectory() || rootStat.isSymbolicLink() || fs.realpathSync(releaseRoot) !== releaseRoot) {
      reject("managed release root is not a canonical directory");
    }
  }
  if (fs.existsSync(value)) {
    const targetStat = fs.lstatSync(value);
    if (!targetStat.isDirectory() || targetStat.isSymbolicLink() || fs.realpathSync(value) !== value) {
      reject("managed release target is not a canonical directory");
    }
  }
  return value;
}

function run(argv) {
  const result = Bun.spawnSync(argv, {
    stdin: "ignore",
    stdout: "pipe",
    stderr: "pipe",
  });
  return {
    exit: result.exitCode ?? 1,
    stdout: decoder.decode(result.stdout),
    stderr: decoder.decode(result.stderr),
  };
}

function syncDirectory(directory) {
  const fd = fs.openSync(directory, "r");
  try {
    fs.fsyncSync(fd);
  } finally {
    fs.closeSync(fd);
  }
}

function durableWrite(destination, bytes, mode) {
  const directory = path.dirname(destination);
  fs.mkdirSync(directory, { recursive: true, mode: 0o700 });
  const temporary = destination + ".tmp-" + process.pid + "-" + randomUUID();
  let fd = null;
  try {
    fd = fs.openSync(temporary, "wx", mode);
    fs.writeFileSync(fd, bytes);
    fs.fsyncSync(fd);
    fs.closeSync(fd);
    fd = null;
    fs.chmodSync(temporary, mode);
    fs.renameSync(temporary, destination);
    syncDirectory(directory);
  } catch (error) {
    if (fd !== null) {
      try { fs.closeSync(fd); } catch {}
    }
    try { fs.rmSync(temporary, { force: true }); } catch {}
    throw error;
  }
}

function durableRemove(destination) {
  if (!fs.existsSync(destination)) return;
  fs.unlinkSync(destination);
  syncDirectory(path.dirname(destination));
}

function validateJournal(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) reject("journal is not an object");
  const keys = [
    "schemaVersion", "phase", "targetGitSha", "targetReleasePath",
    "priorPlistBase64", "priorPlistMode", "priorLifecycle", "priorPid",
    "priorDisabled", "createdAt", "updatedAt",
  ];
  if (Object.keys(value).some((key) => !keys.includes(key))) reject("journal has unknown fields");
  if (value.schemaVersion !== 1) reject("journal schema is unsupported");
  if (value.phase !== "prepared" && value.phase !== "activating") reject("journal phase is malformed");
  if (typeof value.targetGitSha !== "string" || typeof value.targetReleasePath !== "string") {
    reject("journal target identity is malformed");
  }
  directReleasePath(value.targetReleasePath, value.targetGitSha);
  if (!["unloaded", "loaded", "running"].includes(value.priorLifecycle)) {
    reject("journal prior lifecycle is malformed");
  }
  if (value.priorLifecycle === "running") {
    if (!Number.isSafeInteger(value.priorPid) || value.priorPid < 1) {
      reject("journal prior process epoch is malformed");
    }
  } else if (value.priorPid !== null) {
    reject("journal has a process epoch for a non-running service");
  }
  if (typeof value.priorDisabled !== "boolean") reject("journal disabled override is malformed");
  if (value.priorPlistBase64 === null) {
    if (value.priorPlistMode !== null || value.priorLifecycle !== "unloaded") {
      reject("journal cannot restore a loaded service without plist bytes");
    }
  } else {
    if (typeof value.priorPlistBase64 !== "string"
      || value.priorPlistBase64.length > 2 * 1024 * 1024
      || Buffer.from(value.priorPlistBase64, "base64").toString("base64") !== value.priorPlistBase64) {
      reject("journal prior plist bytes are malformed");
    }
    if (!Number.isSafeInteger(value.priorPlistMode)
      || value.priorPlistMode < 0
      || value.priorPlistMode > 0o777) {
      reject("journal prior plist mode is malformed");
    }
  }
  if (typeof value.createdAt !== "string"
    || typeof value.updatedAt !== "string"
    || !Number.isFinite(Date.parse(value.createdAt))
    || !Number.isFinite(Date.parse(value.updatedAt))) {
    reject("journal timestamps are malformed");
  }
  return value;
}

function readJournal() {
  if (!fs.existsSync(journalPath)) return null;
  const stat = fs.lstatSync(journalPath);
  if (!stat.isFile() || stat.isSymbolicLink() || stat.size > 3 * 1024 * 1024) {
    reject("journal file is malformed");
  }
  return validateJournal(JSON.parse(fs.readFileSync(journalPath, "utf8")));
}

function writeJournal(journal) {
  validateJournal(journal);
  durableWrite(journalPath, Buffer.from(JSON.stringify(journal) + "\n"), 0o600);
}

function emit(journal) {
  const payload = Buffer.from(JSON.stringify({ releaseRoot, journal })).toString("base64");
  console.log(outputPrefix + payload);
}

function launchdPrint() {
  const uid = String(process.getuid());
  return run(["launchctl", "print", "gui/" + uid + "/" + label]);
}


function disabledOverride() {
  const uid = String(process.getuid());
  const result = run(["launchctl", "print-disabled", "gui/" + uid]);
  if (result.exit !== 0) reject("cannot read launchd disabled overrides: " + result.stderr);
  const overrideLine = result.stdout.split(/\r?\n/)
    .find((line) => line.includes('"' + label + '"'));
  if (!overrideLine) return false;
  // launchctl prints this override in two shapes depending on the OS version:
  // the older boolean (=> true / => false) and the current word form
  // (=> disabled / => enabled, measured on macOS 15 / mihai-m5-air). Accepting
  // only the boolean made every current-macOS deploy abort as "malformed"
  // before it touched the host. Both mean the same thing: the first of each
  // pair is "this service is disabled".
  const match = overrideLine.match(/=>\s*(true|false|disabled|enabled)\s*$/);
  if (!match) reject("launchd disabled override is malformed: " + overrideLine.trim());
  return match[1] === "true" || match[1] === "disabled";
}

function exactPriorDefinition(journal) {
  if (journal.priorPlistBase64 === null) return !fs.existsSync(plistPath);
  if (!fs.existsSync(plistPath)) return false;
  const stat = fs.lstatSync(plistPath);
  return stat.isFile()
    && !stat.isSymbolicLink()
    && (stat.mode & 0o777) === journal.priorPlistMode
    && fs.readFileSync(plistPath).equals(Buffer.from(journal.priorPlistBase64, "base64"));
}

function removeRelease(releasePath, sha) {
  directReleasePath(releasePath, sha);
  if (!fs.existsSync(releasePath)) return;
  fs.rmSync(releasePath, { recursive: true, force: false });
  syncDirectory(releaseRoot);
}

function plistWorkingDirectory(bytes) {
  const temporary = journalPath + ".plist-probe-" + randomUUID();
  fs.writeFileSync(temporary, bytes, { mode: 0o600, flag: "wx" });
  try {
    const result = run([
      "/usr/libexec/PlistBuddy",
      "-c",
      "Print :WorkingDirectory",
      temporary,
    ]);
    return result.exit === 0 ? result.stdout.trim() : null;
  } finally {
    fs.rmSync(temporary, { force: true });
  }
}

try {
  if (action === "load") {
    emit(readJournal());
  } else if (action === "prepare") {
    if (fs.existsSync(journalPath)) reject("an uncleared macOS deploy journal already exists");
    directReleasePath(requestedTarget, requestedSha);
    if (!fs.existsSync(requestedTarget)) reject("target release is not staged");
    const launchd = launchdPrint();
    const lifecycle = launchd.exit !== 0
      ? "unloaded"
      : /^\s*state = running\s*$/m.test(launchd.stdout) ? "running" : "loaded";
    const priorPidMatch = launchd.stdout.match(/^\s*pid = ([1-9]\d*)\s*$/m);
    const priorPid = lifecycle === "running" && priorPidMatch
      ? Number(priorPidMatch[1])
      : null;
    if (lifecycle === "running" && !Number.isSafeInteger(priorPid)) {
      reject("cannot capture the prior worker process epoch");
    }
    let priorPlistBase64 = null;
    let priorPlistMode = null;
    if (fs.existsSync(plistPath)) {
      const stat = fs.lstatSync(plistPath);
      if (!stat.isFile() || stat.isSymbolicLink()) reject("worker plist is not a regular file");
      priorPlistBase64 = fs.readFileSync(plistPath).toString("base64");
      priorPlistMode = stat.mode & 0o777;
    } else if (lifecycle !== "unloaded") {
      reject("cannot recover a loaded worker whose plist is absent");
    }
    const now = new Date().toISOString();
    const journal = {
      schemaVersion: 1,
      phase: "prepared",
      targetGitSha: requestedSha,
      targetReleasePath: requestedTarget,
      priorPlistBase64,
      priorPlistMode,
      priorLifecycle: lifecycle,
      priorPid,
      priorDisabled: disabledOverride(),
      createdAt: now,
      updatedAt: now,
    };
    writeJournal(journal);
    emit(journal);
  } else if (action === "checkpoint-activating") {
    const journal = readJournal();
    if (!journal || journal.phase !== "prepared") reject("prepared journal is missing");
    directReleasePath(requestedTarget, requestedSha);
    if (journal.targetGitSha !== requestedSha || journal.targetReleasePath !== requestedTarget) {
      reject("activation checkpoint target does not match the prepared journal");
    }
    const activating = { ...journal, phase: "activating", updatedAt: new Date().toISOString() };
    writeJournal(activating);
    emit(activating);
  } else if (action === "restore-prior") {
    const journal = readJournal();
    if (!journal || journal.phase !== "activating") reject("activating journal is missing");
    if (journal.priorPlistBase64 === null) {
      durableRemove(plistPath);
    } else {
      durableWrite(
        plistPath,
        Buffer.from(journal.priorPlistBase64, "base64"),
        journal.priorPlistMode,
      );
    }
    if (!exactPriorDefinition(journal)) reject("prior plist did not round-trip exactly");
  } else if (action === "prove-prior-definition") {
    const journal = readJournal();
    if (!journal || !exactPriorDefinition(journal)) reject("prior plist definition is not restored");
    console.log("RoostPriorDefinitionMatch=yes");
  } else if (action === "remove-target") {
    const journal = readJournal();
    if (!journal) reject("journal is missing");
    removeRelease(journal.targetReleasePath, journal.targetGitSha);
  } else if (action === "cleanup-prior") {
    const journal = readJournal();
    if (!journal || journal.phase !== "activating") reject("activating journal is missing");
    if (journal.priorPlistBase64 !== null) {
      const priorPath = plistWorkingDirectory(Buffer.from(journal.priorPlistBase64, "base64"));
      if (priorPath && priorPath !== journal.targetReleasePath) {
        const protectedPaths = new Set();
        for (const name of fs.readdirSync(path.dirname(plistPath))) {
          if (!name.endsWith(".plist")) continue;
          const candidate = path.join(path.dirname(plistPath), name);
          const candidateStat = fs.lstatSync(candidate);
          if (!candidateStat.isFile() || candidateStat.isSymbolicLink()) continue;
          const result = run([
            "/usr/libexec/PlistBuddy",
            "-c",
            "Print :WorkingDirectory",
            candidate,
          ]);
          if (result.exit === 0 && result.stdout.trim()
            && path.isAbsolute(result.stdout.trim())) {
            protectedPaths.add(path.normalize(result.stdout.trim()));
          }
        }
        if (!protectedPaths.has(priorPath)) {
          let managed = true;
          try {
            directReleasePath(priorPath, null);
          } catch {
            managed = false;
          }
          if (managed) removeRelease(priorPath, null);
        }
      }
    }
  } else if (action === "clear") {
    if (!readJournal()) reject("journal is missing");
    durableRemove(journalPath);
  } else {
    reject("unknown macOS deploy journal action");
  }
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  process.exitCode = 65;
}
`;

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
    ? `target_spec=${_quoteRemoteShell(target.remoteDir.slice(2))}; ` +
      `target_path=$(cd "$HOME/$target_spec" && pwd -P); `
    : `target_path=''; `;
  return `set -e; umask 077; journal_spec=${_quoteRemoteShell(journalPath)}; ` +
    `case "$journal_spec" in /*) journal="$journal_spec";; *) journal="$HOME/$journal_spec";; esac; ` +
    `release_root="$HOME/${MACOS_RELEASE_ROOT_RELATIVE}"; ` +
    `if test -d "$release_root"; then release_root=$(cd "$release_root" && pwd -P); fi; ` +
    `plist="$HOME/${MACOS_WORKER_PLIST_RELATIVE}"; ${targetDirectory}` +
    `ROOST_MAC_DEPLOY_ACTION=${_quoteRemoteShell(action)} ` +
    `ROOST_MAC_DEPLOY_JOURNAL="$journal" ROOST_MAC_DEPLOY_RELEASE_ROOT="$release_root" ` +
    `ROOST_MAC_DEPLOY_PLIST="$plist" ROOST_MAC_DEPLOY_LABEL=${_quoteRemoteShell(MACOS_WORKER_LABEL)} ` +
    `ROOST_MAC_DEPLOY_TARGET_SHA=${_quoteRemoteShell(target?.gitSha ?? "")} ` +
    `ROOST_MAC_DEPLOY_TARGET_PATH="$target_path" bun -e ${_quoteRemoteShell(MACOS_DEPLOY_JOURNAL_PROGRAM)}`;
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

interface MacosDeployJournalController {
  recovery: MacosDeployRecoveryRemote;
  prepare(gitSha: string, remoteDir: string): Promise<MacosDeployJournalV1>;
  checkpointActivating(gitSha: string, remoteDir: string): Promise<MacosDeployJournalV1>;
}

function macosTargetVerificationCommand(journal: Readonly<MacosDeployJournalV1>): string {
  return `${verifyWorkerCmd("darwin")}; verify_status=$?; ` +
    `actual=$(/usr/libexec/PlistBuddy -c 'Print :WorkingDirectory' "$HOME/${MACOS_WORKER_PLIST_RELATIVE}" 2>/dev/null || true); ` +
    `sha=$(/usr/libexec/PlistBuddy -c 'Print :EnvironmentVariables:GIT_SHA' "$HOME/${MACOS_WORKER_PLIST_RELATIVE}" 2>/dev/null || true); ` +
    `if test "$actual" = ${_quoteRemoteShell(journal.targetReleasePath)} ` +
    `&& test "$sha" = ${_quoteRemoteShell(journal.targetGitSha)}; then echo RoostReleaseMatch=yes; fi; ` +
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

function createMacosDeployJournalController(
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

type WindowsDeployClient = Pick<CoordClient, "workersList" | "workersDeployStart" | "workersDeployOutput">;
type WindowsDeployRetryOptions = {
  now?: () => number;
  sleep?: (ms: number) => Promise<void>;
  deadlineMs?: number;
  expectedGitSha?: string;
  expectedManifestSha256?: string;
};

const WINDOWS_DEPLOY_RECONNECT_DEADLINE_MS = 16 * 60 * 1000;

function normalizedHost(value: string): string {
  return value.trim().replace(/\.$/, "").toLowerCase();
}

/** Drive the coordinator-owned signed update channel when `host` is Windows. */
export async function deployWindowsWorkerViaCoordinator(
  client: WindowsDeployClient,
  host: string,
  log: (line: string) => void = console.log,
  retryOptions: WindowsDeployRetryOptions = {},
): Promise<boolean> {
  const now = retryOptions.now ?? Date.now;
  const sleep = retryOptions.sleep ?? ((ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms)));
  const deadline = now() + (retryOptions.deadlineMs ?? WINDOWS_DEPLOY_RECONNECT_DEADLINE_MS);
  const requested = normalizedHost(host);
  if (!requested) failDeploy(2, "deploy target must not be empty");
  const inventory = await client.workersList({}).catch(() => null);
  // Client creation is lazy. An unreachable coordinator must not block the
  // existing direct SSH path before we have positively identified Windows.
  if (!inventory) return false;

  const fingerprintMatches = inventory.workers.filter(
    (candidate) => normalizedHost(candidate.fp) === requested,
  );
  if (fingerprintMatches.length > 1) {
    failDeploy(2, `ambiguous deploy target "${host}" matches multiple worker fingerprints`);
  }
  const aliasMatches = fingerprintMatches.length === 0
    ? inventory.workers.filter((candidate) =>
        [candidate.label, candidate.reachableAddr ?? ""].some((identity) => {
          const normalized = normalizedHost(identity);
          return normalized.length > 0 && normalized === requested;
        }))
    : [];
  if (aliasMatches.length > 1) {
    failDeploy(
      2,
      `ambiguous deploy target "${host}" matches multiple registered workers; use the worker fingerprint`,
    );
  }
  const worker = fingerprintMatches[0] ?? aliasMatches[0];
  if (worker?.os !== "win32") return false;

  const started = await client.workersDeployStart({
    host: worker.fp,
    expectedGitSha: retryOptions.expectedGitSha,
    expectedManifestSha256: retryOptions.expectedManifestSha256,
  });
  if (!started.ok || !started.jobId) {
    failDeploy(2, started.error || `failed to start signed Windows update for ${worker.label}`);
  }
  log(`>> signed Windows update ${worker.label} (${started.jobId})`);

  let completed = false;
  let reconnectAttempt = 0;
  const deliveredLines: string[] = [];
  while (!completed) {
    let lineIndex = 0;
    try {
      for await (const frame of client.workersDeployOutput({ jobId: started.jobId })) {
        if (frame.kind === "line") {
          if (frame.text) {
            if (deliveredLines[lineIndex] !== frame.text) {
              deliveredLines.splice(lineIndex);
              deliveredLines.push(frame.text);
              log(frame.text);
            }
            lineIndex++;
          }
          continue;
        }
        if (frame.kind !== "done") continue;
        if (frame.exit !== 0) {
          if (frame.error === "unknown jobId" && now() < deadline) break;
          failDeploy(2, frame.error || `signed Windows update failed with exit ${frame.exit}`);
        }
        completed = true;
        log(`✓ signed Windows update complete for ${worker.label}`);
        break;
      }
    } catch (error) {
      if (error instanceof DeployFailure) throw error;
      if (now() >= deadline) {
        failDeploy(2, `signed Windows update stream did not recover: ${error instanceof Error ? error.message : String(error)}`);
      }
    }
    if (completed) break;
    if (now() >= deadline) {
      failDeploy(2, "signed Windows update stream ended without a terminal result");
    }
    reconnectAttempt++;
    const delayMs = Math.min(250 * (2 ** Math.min(reconnectAttempt - 1, 3)), 2_000);
    log(`>> coordinator stream unavailable; reconnecting to Windows update ${started.jobId}`);
    await sleep(delayMs);
  }
  return true;
}

async function tryCoordinatorWindowsDeploy(
  host: string,
  expectedGitSha?: string,
  expectedManifestSha256?: string,
): Promise<boolean> {
  let client: CoordClient;
  try {
    if (process.platform === "win32") {
      const definitions = await loadWindowsServiceDefinitions();
      const config = loadWorkerConfig(definitions.worker.environment);
      client = await buildAuthorizedApiClient({
        coordinatorUrl: config.coordinatorUrl,
        keyPath: join(homedir(), ".roost", "cli-key"),
        label: "roost-cli",
      });
    } else {
      client = await buildSelfAuthorizedApiClient();
    }
  } catch {
    return false;
  }
  return deployWindowsWorkerViaCoordinator(client, host, console.log, {
    expectedGitSha,
    expectedManifestSha256,
  });
}


export async function deploy(args: string[]): Promise<void> {
  const host = args[0];
  if (!host) failDeploy(1, "usage: roost deploy <tailnet-host>");
  const expectedGitSha = args.find((arg) => arg.startsWith("--expected-sha="))
    ?.slice("--expected-sha=".length);
  const expectedManifestSha256 = args.find((arg) => arg.startsWith("--expected-manifest-sha256="))
    ?.slice("--expected-manifest-sha256=".length);
  const sourceRootValue = args.find((arg) => arg.startsWith("--source-root="))
    ?.slice("--source-root=".length) ?? REPO_ROOT;
  if (!sourceRootValue || /[\r\n\0]/.test(sourceRootValue)) {
    failDeploy(1, "--source-root must be a local source checkout path");
  }
  const sourceCheckout = resolve(sourceRootValue);
  if (expectedGitSha !== undefined && !/^[a-f0-9]{40,64}$/i.test(expectedGitSha)) {
    failDeploy(1, "--expected-sha must be a 40-64 hex build identity");
  }
  if (expectedManifestSha256 !== undefined && !/^[a-f0-9]{64}$/i.test(expectedManifestSha256)) {
    failDeploy(1, "--expected-manifest-sha256 must be a 64-hex digest");
  }
  if (await tryCoordinatorWindowsDeploy(
    host,
    expectedGitSha,
    expectedManifestSha256,
  )) return;
  if (/^[a-f0-9]{64}$/i.test(host)) {
    failDeploy(2, "registered Windows worker requires a reachable coordinator update channel");
  }
  if (process.platform === "win32") {
    failDeploy(2, "the target is not a registered Windows worker; POSIX source deploy is unavailable on Windows");
  }

  const selfHost = await _isSelfHost(host);
  const allowUnpublishedLocal = args.includes("--allow-unpublished-local");
  if (allowUnpublishedLocal && !selfHost) {
    failDeploy(1, "--allow-unpublished-local is restricted to the localhost quickstart path");
  }
  const sourceGitSha = allowUnpublishedLocal
    ? resolveLocalGitShaOrDie(sourceCheckout)
    : resolvePublishedGitShaOrDie(sourceCheckout, expectedGitSha);
  if (selfHost) {
    await _deployLocal(host, { sourceRoot: sourceCheckout, gitSha: sourceGitSha });
    return;
  }

  console.log(`>> reachability check ssh ${host}`);
  const ssh = await run(
    ["ssh", ...SSH_OPTS, "-o", "BatchMode=yes", "--", host, "true"],
  );
  if (ssh.exit !== 0) {
    failDeploy(2, "ssh failed; ensure key-based / tailscale-ssh auth");
  }

  console.log(`>> verify bun on ${host}`);
  const bunCheck = await sshExec(host, "command -v bun && bun --version");
  if (bunCheck.exit !== 0) {
    failDeploy(
      3,
      `bun not found in remote login shell. Install: curl -fsSL https://bun.sh/install | bash\n${bunCheck.stderr}`,
    );
  }
  console.log(`   bun: ${bunCheck.stdout.trim().split("\n").slice(-2).join(" @ ")}`);

  // Linux targets hold a full git checkout (join.sh clones the repo), so
  // they update in place — no rsync of a slim tree, no tailscale cert.
  const unameOut = await sshExec(host, "uname -s");
  if (unameOut.stdout.trim() === "Linux") {
    const gitSha = sourceGitSha;
    const { env: hostEnv, filled } = await _backfillEnvFromPlist(host);
    if (filled.length > 0) {
      console.log(`>> reused from the installed unit on ${host}: ${filled.join(", ")}`);
    }
    // Installed target identity wins; ambient values only seed fresh installs.
    const resolved = (key: string): string | undefined => _resolveDeployEnvValue(key, hostEnv);
    if (!resolved("ROOST_COORDINATOR_URL")) {
      failDeploy(6, "ROOST_COORDINATOR_URL env var required (no prior install on target to reuse)");
    }
    const passthroughEnv = workerInstallEnvironment(hostEnv, {
      ROOST_COORDINATOR_URL: resolved("ROOST_COORDINATOR_URL"),
      ROOST_WORKER_LABEL: resolved("ROOST_WORKER_LABEL"),
      ROOST_REACHABLE_ADDR: resolved("ROOST_REACHABLE_ADDR"),
      ROOST_BOOTSTRAP_TOKEN: process.env.ROOST_BOOTSTRAP_TOKEN,
    }, gitSha);
    await deployLinux(host, {
      gitSha,
      passthroughEnv,
      machineTransactionPath: remoteMachineTransactionPath("linux", hostEnv),
    });
    return;
  }
  if (unameOut.exit !== 0 || unameOut.stdout.trim() !== "Darwin") {
    failDeploy(
      2,
      `unsupported deploy target platform from ${host}: ${unameOut.stdout.trim() || unameOut.stderr.trim() || "unknown"}`,
    );
  }

  // Resolve every local and installed prerequisite before the first remote
  // write. A missing URL or unprovable source identity leaves the live tree.
  const localGitSha = sourceGitSha;
  const releaseId = `${localGitSha}-${crypto.randomUUID()}`;
  const remoteDir = `${REMOTE_DIR}-releases/${releaseId}`;
  const { env: hostEnv, filled } = await _backfillEnvFromPlist(host);
  if (filled.length > 0) {
    console.log(`>> reused from existing plist on ${host}: ${filled.join(", ")}`);
  }
  const resolved = (key: string): string | undefined => _resolveDeployEnvValue(key, hostEnv);
  const resolvedCoordinatorUrl = resolved("ROOST_COORDINATOR_URL");
  if (!resolvedCoordinatorUrl) {
    failDeploy(
      6,
      `ROOST_COORDINATOR_URL env var required (no prior plist on target to reuse). ` +
        `Set it before running: roost deploy ${host}`,
    );
  }
  const resolvedReachableAddr = resolved("ROOST_REACHABLE_ADDR");
  if (localGitSha.endsWith("-dirty")) {
    failDeploy(7, "a macOS deploy requires a clean committed source snapshot");
  }
  const snapshotParent = mkdtempSync(join(tmpdir(), "roost-deploy-source-"));
  const sourceRoot = join(snapshotParent, "source");
  try {
    await runOrDie(
      ["git", "-C", sourceCheckout, "worktree", "add", "--quiet", "--force", "--detach", sourceRoot, localGitSha],
      "local source snapshot",
    );
  } catch (error) {
    rmSync(snapshotParent, { recursive: true, force: true });
    throw error;
  }
  const cleanupSource = async (): Promise<void> => {
    await run(["git", "-C", sourceCheckout, "worktree", "remove", "--force", sourceRoot], { quiet: true });
    rmSync(snapshotParent, { recursive: true, force: true });
  };
  try {

  const deployLock = remoteMachineTransactionPath("darwin", hostEnv);
  const deployLease = await acquireRemoteDeployLock(host, deployLock, releaseId);
  const deploySsh = (command: string) => sshExec(host, command, deployLease.signal);
  try {
  const deployLockSpec = posix.normalize(deployLock);
  const deployLockBase = posix.dirname(deployLockSpec);
  const foreignJournalGuard = await deploySsh(
    `set -e; base_spec=${_quoteRemoteShell(deployLockBase)}; ` +
      `case "$base_spec" in /*) base="$base_spec";; *) base="$HOME/$base_spec";; esac; ` +
      `for relative in ${_quoteRemoteShell(POSIX_WORKER_DEPLOY_JOURNAL_PATHS.local)} ` +
      `${_quoteRemoteShell(POSIX_WORKER_DEPLOY_JOURNAL_PATHS.linux)} ` +
      `${_quoteRemoteShell(POSIX_WORKER_DEPLOY_JOURNAL_PATHS.coordinator)}; do ` +
      `foreign="$base/$relative"; ` +
      `if test -e "$foreign" || test -L "$foreign"; then exit 66; fi; done`,
  );
  if (foreignJournalGuard.exit !== 0) {
    failDeploy(
      foreignJournalGuard.exit || 5,
      `cannot mutate past an unsettled foreign worker deploy journal on ${host}`,
    );
  }
  const deployJournal = _macosDeployJournalPath(deployLock);
  const journalController = createMacosDeployJournalController(
    deploySsh,
    deployJournal,
    deployLease.signal,
  );
  const interrupted = await _recoverMacosDeployJournal(journalController.recovery);
  if (interrupted.outcome === "prepared-cleaned") {
    console.log(">> cleaned an interrupted prepared macOS release");
  } else if (interrupted.outcome === "rolled-back") {
    console.log(">> restored the prior macOS worker from an interrupted activation");
  } else if (interrupted.outcome === "committed") {
    console.log(">> committed a previously activated healthy macOS worker");
  }
  console.log(`>> ensure staged release ${remoteDir}/ on ${host}`);
  // Manifest-only workspaces are derived, never enumerated (deploy-workspaces.ts).
  const manifestOnly = manifestOnlyWorkspaces(sourceRoot);
  const stage = await deploySsh(`mkdir -p ${manifestOnly.map((w) => `${remoteDir}/${w}`).join(" ")}`);
  if (stage.exit !== 0) {
    failDeploy(stage.exit || 4, `cannot create macOS release stage\n${stage.stdout}\n${stage.stderr}`);
  }
  const cleanupStage = async (): Promise<void> => {
    await deploySsh(`rm -rf ${remoteDir}`);
  };

  // Every worker ships the coordinator too: CoordTarget resolves the installer
  // and the SPA build against process.cwd(), so a Mac that only has
  // apps/worker can never accept a coordinator move. The Linux path already
  // deploys a full checkout — this keeps the two consistent.
  console.log(`>> rsync canonical workspace + apps/{worker,shared,coord,web}/ + vendor/ → ${host}:${remoteDir}/`);
  const rsyncs = await Promise.allSettled([
    runOrDie([
      "rsync", "-az", "-e", RSYNC_RSH, "--delete",
      "--exclude", "node_modules", "--exclude", "tests", "--exclude", "test-results",
      `${sourceRoot}/apps/worker/`, `${host}:${remoteDir}/apps/worker/`,
    ], "rsync apps/worker", deployLease.signal),
    runOrDie([
      "rsync", "-az", "-e", RSYNC_RSH, "--delete",
      "--exclude", "node_modules",
      `${sourceRoot}/apps/shared/`, `${host}:${remoteDir}/apps/shared/`,
    ], "rsync apps/shared", deployLease.signal),
    runOrDie([
      "rsync", "-az", "-e", RSYNC_RSH, "--delete",
      "--exclude", "node_modules", "--exclude", "tests", "--exclude", "test-results",
      `${sourceRoot}/apps/coord/`, `${host}:${remoteDir}/apps/coord/`,
    ], "rsync apps/coord", deployLease.signal),
    runOrDie([
      // dist is excluded on purpose: CoordTarget builds it on the target at
      // PREPARE, and a stale copy would be served in preference to that.
      "rsync", "-az", "-e", RSYNC_RSH, "--delete",
      "--exclude", "node_modules", "--exclude", "dist", "--exclude", "tests",
      `${sourceRoot}/apps/web/`, `${host}:${remoteDir}/apps/web/`,
    ], "rsync apps/web", deployLease.signal),
    ...(existsSync(join(sourceRoot, "vendor"))
      ? [runOrDie([
          "rsync", "-az", "-e", RSYNC_RSH, "--delete",
          "--exclude", "node_modules", "--exclude", "dist",
          `${sourceRoot}/vendor/`, `${host}:${remoteDir}/vendor/`,
        ], "rsync vendor", deployLease.signal)]
      : []),
    runOrDie([
      // Canonical workspace manifests + lock preserve artifact identity for a
      // given Git SHA. Empty source dirs are enough for omitted workspaces.
      "rsync", "-az", "-e", RSYNC_RSH,
      `${sourceRoot}/package.json`, `${sourceRoot}/bun.lock`,
      `${sourceRoot}/tsconfig.base.json`, `${sourceRoot}/bunfig.toml`,
      `${host}:${remoteDir}/`,
    ], "rsync root workspace", deployLease.signal),
    ...manifestOnly.map((relative) => runOrDie([
      "rsync", "-az", "-e", RSYNC_RSH,
      `${sourceRoot}/${relative}/package.json`, `${host}:${remoteDir}/${relative}/`,
    ], `rsync ${relative} manifest`, deployLease.signal)),
  ]);
  const rejectedRsync = rsyncs.find(
    (result): result is PromiseRejectedResult => result.status === "rejected",
  );
  if (rejectedRsync) {
    await cleanupStage();
    throw rejectedRsync.reason;
  }


  // NOT --production: apps/web's build needs vite + vite-plugin-solid, which
  // are devDependencies. Without them a coordinator move fails at PREPARE.
  console.log(`>> bun install on ${host}`);
  const installRes = await deploySsh(
    // `set -o pipefail` so the tail filter doesn't mask a non-zero exit.
    `set -eo pipefail; cd ${remoteDir} && bun install --frozen-lockfile 2>&1 | tail -25`,
  );
  if (installRes.exit !== 0) {
    await cleanupStage();
    failDeploy(
      4,
      `bun install failed\n${installRes.stdout}\n${installRes.stderr}`,
    );
  }
  console.log("   bun install ok");


  const passthroughEnv = workerInstallEnvironment(hostEnv, {
    ROOST_COORDINATOR_URL: resolvedCoordinatorUrl,
    ROOST_WORKER_LABEL: resolved("ROOST_WORKER_LABEL"),
    ROOST_BOOTSTRAP_TOKEN: process.env.ROOST_BOOTSTRAP_TOKEN,
    ROOST_REACHABLE_ADDR: resolvedReachableAddr,
  }, localGitSha);
  await journalController.prepare(localGitSha, remoteDir);
  // Atomic checkpoint is the final operation before install.sh can rewrite or
  // boot out the live LaunchAgent.
  await journalController.checkpointActivating(localGitSha, remoteDir);

  const throwIfActivationTransportLost = (
    result: { exit: number; stdout: string; stderr: string },
    operation: string,
  ): void => {
    if (!deployLease.signal.aborted && result.exit !== 255 && result.exit < 128) return;
    const reason = deployLease.signal.reason;
    if (reason instanceof DeployFailure) throw reason;
    throw new DeployFailure(
      result.exit || 9,
      `${operation} lost its remote shell; durable macOS deploy journal retained\n` +
        `${result.stdout}\n${result.stderr}`,
    );
  };
  const recoverFailedActivation = async (
    exitCode: number,
    failure: string,
  ): Promise<void> => {
    let recovery: MacosDeployRecoveryResult;
    try {
      recovery = await _recoverMacosDeployJournal(journalController.recovery);
    } catch (error) {
      if (error instanceof DeployFailure
        && (deployLease.signal.aborted || error.exitCode === 255 || error.exitCode >= 128)) {
        throw error;
      }
      failDeploy(
        exitCode,
        `${failure}\nrollback failed; durable macOS deploy journal retained\n` +
          (error instanceof Error ? error.message : String(error)),
      );
    }
    if (recovery.outcome === "committed") {
      finishWorkerDeploy(
        recovery.targetProof.result,
        `>> done — ${host} v2 worker deployed`,
        "darwin",
      );
      return;
    }
    if (recovery.outcome === "rolled-back") {
      failDeploy(exitCode, `${failure}\nprior worker service restored`);
    }
    failDeploy(exitCode, `${failure}\nmacOS deploy journal disappeared before recovery`);
  };

  const installSh = await deploySsh(
    `${passthroughEnv} bash ${remoteDir}/apps/worker/scripts/install.sh install 2>&1`,
  );
  if (installSh.exit !== 0) {
    throwIfActivationTransportLost(installSh, "install macOS worker");
    await recoverFailedActivation(
      5,
      `install.sh failed\n${installSh.stdout}\n${installSh.stderr}`,
    );
    return;
  }
  console.log(installSh.stdout.trim().split("\n").map((l) => `   ${l}`).join("\n"));

  console.log(`>> kickstart ${MACOS_WORKER_LABEL} on ${host}`);
  // install.sh already kickstarts, but bounce again after rsync to be sure.
  const kick = await deploySsh(
    `launchctl kickstart -k gui/$(id -u)/${MACOS_WORKER_LABEL} 2>&1`,
  );
  if (kick.exit !== 0) {
    throwIfActivationTransportLost(kick, "kickstart macOS worker");
    await recoverFailedActivation(
      4,
      `kickstart failed (exit ${kick.exit})\n${kick.stdout}\n${kick.stderr}`,
    );
    return;
  }

  console.log(`>> verifying service is up on ${host}`);
  const { promise: settled, resolve: markSettled } = Promise.withResolvers<void>();
  setTimeout(markSettled, 1500);
  await settled;
  const activation = await _recoverMacosDeployJournal(journalController.recovery);
  if (activation.outcome === "committed") {
    finishWorkerDeploy(
      activation.targetProof.result,
      `>> done — ${host} v2 worker deployed`,
      "darwin",
    );
  } else if (activation.outcome === "rolled-back") {
    const proof = activation.targetProof.result;
    failDeploy(
      proof.exit || 8,
      `worker service verification failed\n${proof.stdout}\n${proof.stderr}\n` +
        "prior worker service restored",
    );
  } else {
    failDeploy(8, "macOS activation journal was not available for final verification");
  }
  } finally {
    await releaseRemoteDeployLock(host, deployLock, releaseId);
  }
  } finally {
    await cleanupSource();
  }
}
