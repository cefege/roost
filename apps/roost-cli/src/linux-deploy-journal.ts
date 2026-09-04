// Schema, confinement, parsing, and the recovery decision for the fixed-path
// Linux worker deploy journal (`worker-deploy-journal`, a directory of
// base64-encoded field files written by systemd-era shell helpers). Pure
// logic only — proof/restore command strings live in
// linux-deploy-journal-commands.ts and the ssh recovery driver stays in
// deploy-linux.ts. Built on the shared posix-deploy-journal core.

import { posix } from "node:path";
import {
  POSIX_FULL_GIT_SHA_RE,
  posixDeployJournalDecision,
} from "./posix-deploy-journal.ts";
import { failDeploy } from "./deploy-exec.ts";
import { workerRolloutIdOrNull } from "./worker-deploy-rollout.ts";

export const LINUX_WORKER_RELEASE_RELATIVE_ROOT = ".local/share/roost/releases/worker";
export const LINUX_DEPLOY_JOURNAL_NAME = "worker-deploy-journal";
export const LINUX_DEPLOY_JOURNAL_SCHEMA = "3";

export type LinuxDeployJournalPhase = "prepared" | "activating" | "activated";
export type LinuxDeployPriorLifecycle = "running" | "stopped";
export type LinuxDeployPriorEnablement = "enabled" | "disabled" | "masked" | "absent";

export interface LinuxDeployJournal {
  phase: LinuxDeployJournalPhase;
  targetSha: string;
  rolloutId: string | null;
  targetReleasePath: string;
  priorUnit: string | null;
  priorUnitMode: number | null;
  priorLifecycle: LinuxDeployPriorLifecycle;
  priorEnablement: LinuxDeployPriorEnablement;
  priorPid: number;
}

export type LinuxDeployRecoveryPlan =
  | { kind: "clean-prepared" }
  | { kind: "hold-target" }
  | { kind: "commit-target" }
  | {
      kind: "rollback";
      priorUnitState: "present" | "absent";
      priorLifecycle: LinuxDeployPriorLifecycle;
    };

export function malformedLinuxJournal(detail: string): never {
  failDeploy(5, `Linux worker deployment journal is malformed: ${detail}`);
}

export function linuxWorkerReleaseRoot(home: string): string {
  if (!posix.isAbsolute(home) || /[\r\n\0]/.test(home)) {
    failDeploy(2, `remote Linux home path is unsafe: ${JSON.stringify(home)}`);
  }
  return posix.join(home, LINUX_WORKER_RELEASE_RELATIVE_ROOT);
}

export function isManagedLinuxWorkerReleasePath(candidate: string, home: string): boolean {
  if (!posix.isAbsolute(candidate) || /[\r\n\0]/.test(candidate)) return false;
  let root: string;
  try {
    root = linuxWorkerReleaseRoot(home);
  } catch {
    return false;
  }
  return posix.normalize(candidate) === candidate
    && posix.dirname(candidate) === root
    && posix.basename(candidate).length > 0;
}

export function linuxDeployJournalPath(
  machineTransactionPath: string,
  home: string,
): string {
  linuxWorkerReleaseRoot(home);
  if (!machineTransactionPath || /[\r\n\0]/.test(machineTransactionPath)) {
    failDeploy(2, "remote Linux machine transaction path is unsafe");
  }
  const normalized = posix.normalize(machineTransactionPath);
  if (!posix.isAbsolute(normalized)
    && (normalized === ".." || normalized.startsWith("../"))) {
    failDeploy(2, "remote Linux machine transaction path escapes the remote home");
  }
  const absolute = posix.isAbsolute(normalized)
    ? normalized
    : posix.join(home, normalized);
  return posix.join(posix.dirname(absolute), LINUX_DEPLOY_JOURNAL_NAME);
}

export function assertFixedLinuxJournalPath(journalPath: string): void {
  if (!posix.isAbsolute(journalPath)
    || posix.basename(journalPath) !== LINUX_DEPLOY_JOURNAL_NAME
    || /[\r\n\0]/.test(journalPath)) {
    failDeploy(2, `Linux deployment journal path is unsafe: ${JSON.stringify(journalPath)}`);
  }
}

export function assertLinuxDeployJournal(
  journal: LinuxDeployJournal,
  home: string,
): void {
  if (journal.phase !== "prepared"
    && journal.phase !== "activating"
    && journal.phase !== "activated") {
    malformedLinuxJournal(`invalid phase ${JSON.stringify(journal.phase)}`);
  }
  if (!POSIX_FULL_GIT_SHA_RE.test(journal.targetSha)) {
    malformedLinuxJournal("target SHA is not a full hexadecimal object id");
  }
  workerRolloutIdOrNull(journal.rolloutId, "Linux worker rollout ID");
  if (!isManagedLinuxWorkerReleasePath(journal.targetReleasePath, home)) {
    malformedLinuxJournal(
      `target release path is outside the managed worker release root: ${JSON.stringify(journal.targetReleasePath)}`,
    );
  }
  if (journal.priorLifecycle !== "running" && journal.priorLifecycle !== "stopped") {
    malformedLinuxJournal(`invalid prior lifecycle ${JSON.stringify(journal.priorLifecycle)}`);
  }
  if (!Number.isSafeInteger(journal.priorPid) || journal.priorPid < 0) {
    malformedLinuxJournal("prior process epoch is malformed");
  }
  if ((journal.priorLifecycle === "running") !== (journal.priorPid > 0)) {
    malformedLinuxJournal("prior process epoch and lifecycle disagree");
  }
  if (journal.priorUnit === null && journal.priorLifecycle === "running") {
    malformedLinuxJournal("an absent prior unit cannot have a running lifecycle");
  }
  if (journal.priorUnit === null) {
    if (journal.priorUnitMode !== null) {
      malformedLinuxJournal("an absent prior unit cannot have a saved mode");
    }
  } else if (!Number.isInteger(journal.priorUnitMode)
    || journal.priorUnitMode! < 0
    || journal.priorUnitMode! > 0o777) {
    malformedLinuxJournal("prior unit mode is malformed");
  }
  if (!["enabled", "disabled", "masked", "absent"].includes(journal.priorEnablement)) {
    malformedLinuxJournal("prior unit enablement is malformed");
  }
  if ((journal.priorUnit === null) !== (journal.priorEnablement === "absent")) {
    malformedLinuxJournal("prior unit presence and enablement disagree");
  }
  if (journal.priorLifecycle === "running" && journal.priorEnablement === "masked") {
    malformedLinuxJournal("a masked prior unit cannot have a running lifecycle");
  }
}

function decodeJournalField(name: string, value: string): Buffer {
  if (value.length % 4 !== 0
    || !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(value)) {
    malformedLinuxJournal(`${name} is not canonical base64`);
  }
  const decoded = Buffer.from(value, "base64");
  if (decoded.toString("base64") !== value) {
    malformedLinuxJournal(`${name} is not canonical base64`);
  }
  return decoded;
}

export function parseLinuxDeployJournalSnapshot(
  output: string,
  home: string,
): LinuxDeployJournal | null {
  if (output === "absent" || output === "absent\n") return null;
  const lines = output.endsWith("\n")
    ? output.slice(0, -1).split("\n")
    : output.split("\n");
  if (lines.shift() !== "journal") {
    malformedLinuxJournal("snapshot header is missing");
  }
  const encoded = new Map<string, string>();
  for (const line of lines) {
    const separator = line.indexOf("=");
    if (separator < 1) malformedLinuxJournal("snapshot field framing is invalid");
    const name = line.slice(0, separator);
    if (encoded.has(name)) malformedLinuxJournal(`duplicate ${name} field`);
    encoded.set(name, line.slice(separator + 1));
  }
  const baseFields = [
    "schema",
    "phase",
    "target-sha",
    "target-release",
    "prior-unit-state",
    "prior-unit-mode",
    "prior-lifecycle",
    "prior-enablement",
    "prior-pid",
    "prior-unit",
  ];
  const schema = encoded.has("schema")
    ? decodeJournalField("schema", encoded.get("schema")!).toString("utf8")
    : "";
  const expected = schema === "2" ? baseFields : [...baseFields, "rollout-id"];
  if (encoded.size !== expected.length || expected.some((name) => !encoded.has(name))) {
    malformedLinuxJournal("snapshot fields are incomplete or unexpected");
  }
  const text = (name: string): string => {
    const value = decodeJournalField(name, encoded.get(name)!);
    const decoded = value.toString("utf8");
    if (Buffer.from(decoded, "utf8").compare(value) !== 0 || /[\r\n\0]/.test(decoded)) {
      malformedLinuxJournal(`${name} is not a single UTF-8 value`);
    }
    return decoded;
  };
  if (schema !== "2" && schema !== LINUX_DEPLOY_JOURNAL_SCHEMA) {
    malformedLinuxJournal("unsupported schema");
  }
  const phase = text("phase") as LinuxDeployJournalPhase;
  const targetSha = text("target-sha");
  const targetReleasePath = text("target-release");
  const rolloutId = workerRolloutIdOrNull(
    schema === "2" ? null : text("rollout-id"),
    "Linux worker rollout ID",
  );
  const priorUnitState = text("prior-unit-state");
  const priorUnitModeText = text("prior-unit-mode");
  const priorLifecycle = text("prior-lifecycle") as LinuxDeployPriorLifecycle;
  const priorEnablement = text("prior-enablement") as LinuxDeployPriorEnablement;
  const priorPidText = text("prior-pid");
  const priorUnitBytes = decodeJournalField("prior-unit", encoded.get("prior-unit")!);
  if (priorUnitState !== "present" && priorUnitState !== "absent") {
    malformedLinuxJournal(`invalid prior unit state ${JSON.stringify(priorUnitState)}`);
  }
  if (priorUnitState === "absent" && priorUnitBytes.length !== 0) {
    malformedLinuxJournal("absent prior unit has saved bytes");
  }
  if (priorUnitState === "absent" && priorUnitModeText !== "") {
    malformedLinuxJournal("absent prior unit has a saved mode");
  }
  if (priorUnitState === "present" && !/^[0-7]{3}$/.test(priorUnitModeText)) {
    malformedLinuxJournal("prior unit mode is malformed");
  }
  if (!/^(?:0|[1-9][0-9]*)$/.test(priorPidText)
    || !Number.isSafeInteger(Number(priorPidText))) {
    malformedLinuxJournal("prior process epoch is malformed");
  }
  const journal: LinuxDeployJournal = {
    phase,
    targetSha,
    targetReleasePath,
    rolloutId,
    priorUnit: priorUnitState === "present"
      ? priorUnitBytes.toString("utf8")
      : null,
    priorUnitMode: priorUnitState === "present"
      ? Number.parseInt(priorUnitModeText, 8)
      : null,
    priorLifecycle,
    priorEnablement,
    priorPid: Number(priorPidText),
  };
  if (priorUnitState === "present"
    && Buffer.from(journal.priorUnit!, "utf8").compare(priorUnitBytes) !== 0) {
    malformedLinuxJournal("prior unit is not UTF-8");
  }
  assertLinuxDeployJournal(journal, home);
  return journal;
}

export function linuxDeployRecoveryPlan(
  journal: LinuxDeployJournal,
  targetHealthy: boolean,
  home: string,
): LinuxDeployRecoveryPlan {
  assertLinuxDeployJournal(journal, home);
  const decision = posixDeployJournalDecision(journal.phase, targetHealthy);
  if (decision === "clean-prepared") return { kind: "clean-prepared" };
  if (decision === "commit") {
    return journal.rolloutId === null ? { kind: "commit-target" } : { kind: "hold-target" };
  }
  return {
    kind: "rollback",
    priorUnitState: journal.priorUnit === null ? "absent" : "present",
    priorLifecycle: journal.priorLifecycle,
  };
}
