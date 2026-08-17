// `roost deploy <linux-host>` — update an in-place git checkout instead of
// rsyncing a slim tree. A Linux worker is enrolled by join.sh, which clones
// the repo (${ROOST_DIR:-$HOME/Roost}; /srv/roost is the other layout in the
// wild) and pins it to the coordinator's sha, so the box already has the full
// source + .git. Updating it is: fetch, checkout the local HEAD sha, bun
// install, re-run install.sh, verify the systemd unit.
//
// No `tailscale cert` step: the worker has had no inbound TLS surface since
// phase-25e, and no rsync: the checkout is the source of truth.

import { posix } from "node:path";
import {
  parsePosixServiceEnvironment,
  parseSystemdServiceDirective,
} from "./deploy-plist-env.ts";
import {
  acquireRemoteDeployLock,
  DeployFailure,
  failDeploy,
  finishWorkerDeploy,
  POSIX_WORKER_DEPLOY_JOURNAL_PATHS,
  releaseRemoteDeployLock,
  workerServiceIsRunning,
  workerServiceMatchesRelease,
  sshExec,
} from "./deploy-exec.ts";
import { COORD_UNIT, verifyWorkerCmd, WORKER_UNIT } from "./service-ctl.ts";

function quoteRemote(value: string): string {
  return `'${value.replaceAll("'", `'\"'\"'`)}'`;
}

const LINUX_WORKER_RELEASE_RELATIVE_ROOT = ".local/share/roost/releases/worker";
const LINUX_DEPLOY_JOURNAL_NAME = "worker-deploy-journal";
const LINUX_DEPLOY_JOURNAL_SCHEMA = "2";

export type LinuxDeployJournalPhase = "prepared" | "activating" | "activated";
export type LinuxDeployPriorLifecycle = "running" | "stopped";
export type LinuxDeployPriorEnablement = "enabled" | "disabled" | "masked" | "absent";

export interface LinuxDeployJournal {
  phase: LinuxDeployJournalPhase;
  targetSha: string;
  targetReleasePath: string;
  priorUnit: string | null;
  priorUnitMode: number | null;
  priorLifecycle: LinuxDeployPriorLifecycle;
  priorEnablement: LinuxDeployPriorEnablement;
  priorPid: number;
}

export type LinuxDeployRecoveryPlan =
  | { kind: "clean-prepared" }
  | { kind: "commit-target" }
  | {
      kind: "rollback";
      priorUnitState: "present" | "absent";
      priorLifecycle: LinuxDeployPriorLifecycle;
    };

type DeploySsh = (
  command: string,
) => Promise<{ exit: number; stdout: string; stderr: string }>;

function malformedLinuxJournal(detail: string): never {
  failDeploy(5, `Linux worker deployment journal is malformed: ${detail}`);
}

function linuxWorkerReleaseRoot(home: string): string {
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

function assertFixedLinuxJournalPath(journalPath: string): void {
  if (!posix.isAbsolute(journalPath)
    || posix.basename(journalPath) !== LINUX_DEPLOY_JOURNAL_NAME
    || /[\r\n\0]/.test(journalPath)) {
    failDeploy(2, `Linux deployment journal path is unsafe: ${JSON.stringify(journalPath)}`);
  }
}

function assertLinuxDeployJournal(
  journal: LinuxDeployJournal,
  home: string,
): void {
  if (journal.phase !== "prepared"
    && journal.phase !== "activating"
    && journal.phase !== "activated") {
    malformedLinuxJournal(`invalid phase ${JSON.stringify(journal.phase)}`);
  }
  if (!/^[0-9a-f]{40}(?:[0-9a-f]{24})?$/.test(journal.targetSha)) {
    malformedLinuxJournal("target SHA is not a full hexadecimal object id");
  }
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
  const expected = [
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
  if (text("schema") !== LINUX_DEPLOY_JOURNAL_SCHEMA) {
    malformedLinuxJournal("unsupported schema");
  }
  const phase = text("phase") as LinuxDeployJournalPhase;
  const targetSha = text("target-sha");
  const targetReleasePath = text("target-release");
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
  if (journal.phase === "prepared") return { kind: "clean-prepared" };
  if (targetHealthy) return { kind: "commit-target" };
  return {
    kind: "rollback",
    priorUnitState: journal.priorUnit === null ? "absent" : "present",
    priorLifecycle: journal.priorLifecycle,
  };
}

export function _linuxLoadDeployJournalCommand(journalPath: string): string {
  assertFixedLinuxJournalPath(journalPath);
  return `set -e; journal=${quoteRemote(journalPath)}; ` +
    `if test ! -e "$journal" && test ! -L "$journal"; then printf 'absent\\n'; exit 0; fi; ` +
    `test -d "$journal" && test ! -L "$journal"; ` +
    `for name in schema phase target-sha target-release prior-unit-state prior-unit-mode prior-lifecycle prior-enablement prior-pid; do ` +
    `test -f "$journal/$name" && test ! -L "$journal/$name"; done; ` +
    `prior_unit_state=$(cat "$journal/prior-unit-state"); ` +
    `case "$prior_unit_state" in ` +
    `present) test -f "$journal/prior-unit" && test ! -L "$journal/prior-unit";; ` +
    `absent) test ! -e "$journal/prior-unit" && test ! -L "$journal/prior-unit";; ` +
    `*) exit 65;; esac; ` +
    `emit() { name="$1"; printf '%s=' "$name"; base64 -w0 < "$journal/$name"; printf '\\n'; }; ` +
    `printf 'journal\\n'; ` +
    `for name in schema phase target-sha target-release prior-unit-state prior-unit-mode prior-lifecycle prior-enablement prior-pid; do emit "$name"; done; ` +
    `if test "$prior_unit_state" = present; then emit prior-unit; else printf 'prior-unit=\\n'; fi`;
}

interface LinuxPrepareJournalInput {
  journalPath: string;
  unitPath: string;
  targetSha: string;
  targetReleasePath: string;
  home: string;
}

export function _linuxPrepareDeployJournalCommand(
  input: LinuxPrepareJournalInput,
): string {
  const { journalPath, unitPath, targetSha, targetReleasePath, home } = input;
  assertFixedLinuxJournalPath(journalPath);
  const candidate: LinuxDeployJournal = {
    phase: "prepared",
    targetSha,
    targetReleasePath,
    priorUnit: "",
    priorUnitMode: 0o600,
    priorLifecycle: "stopped",
    priorEnablement: "enabled",
    priorPid: 0,
  };
  assertLinuxDeployJournal(candidate, home);
  const parent = posix.dirname(journalPath);
  return `set -e; umask 077; ` +
    `export XDG_RUNTIME_DIR="\${XDG_RUNTIME_DIR:-/run/user/\$(id -u)}"; ` +
    `journal=${quoteRemote(journalPath)}; parent=${quoteRemote(parent)}; unit=${quoteRemote(unitPath)}; ` +
    `target_sha=${quoteRemote(targetSha)}; target_release=${quoteRemote(targetReleasePath)}; ` +
    `test "$(basename -- "$journal")" = ${LINUX_DEPLOY_JOURNAL_NAME}; ` +
    `test ! -e "$journal" && test ! -L "$journal"; ` +
    `new="$journal.new"; rm -rf -- "$new"; mkdir "$new"; ` +
    `if test -L "$unit"; then exit 65; ` +
    `elif test -f "$unit"; then unit_state=present; unit_mode=$(stat -c '%a' "$unit"); ` +
    `case "$unit_mode" in [0-7][0-7][0-7]) ;; *) exit 65;; esac; ` +
    `cp -- "$unit" "$new/prior-unit"; chmod 600 "$new/prior-unit"; ` +
    `elif test -e "$unit"; then exit 65; else unit_state=absent; fi; ` +
    `active_state=$(systemctl --user show ${WORKER_UNIT} --property=ActiveState --value); ` +
    `prior_pid=$(systemctl --user show ${WORKER_UNIT} --property=MainPID --value); ` +
    `case "$active_state" in active) lifecycle=running; case "$prior_pid" in ''|0|*[!0-9]*) exit 65;; esac;; ` +
    `inactive|failed) lifecycle=stopped; prior_pid=0;; *) exit 65;; esac; ` +
    `if test "$unit_state" = present; then ` +
    `enablement=$(systemctl --user is-enabled ${WORKER_UNIT} 2>/dev/null || true); ` +
    `case "$enablement" in enabled|disabled|masked) ;; *) exit 65;; esac; ` +
    `else enablement=absent; fi; ` +
    `if test "$unit_state" = absent && test "$lifecycle" = running; then exit 65; fi; ` +
    `write_metadata() { name="$1"; value="$2"; printf '%s' "$value" > "$new/$name"; ` +
    `chmod 600 "$new/$name"; sync -f "$new/$name"; }; ` +
    `write_metadata schema ${LINUX_DEPLOY_JOURNAL_SCHEMA}; ` +
    `write_metadata phase prepared; write_metadata target-sha "$target_sha"; ` +
    `write_metadata target-release "$target_release"; write_metadata prior-unit-state "$unit_state"; ` +
    `write_metadata prior-unit-mode "$(if test "$unit_state" = present; then printf '%s' "$unit_mode"; fi)"; ` +
    `write_metadata prior-lifecycle "$lifecycle"; write_metadata prior-enablement "$enablement"; ` +
    `write_metadata prior-pid "$prior_pid"; ` +
    `if test "$unit_state" = present; then sync -f "$new/prior-unit"; fi; ` +
    `sync -f "$new"; mv -- "$new" "$journal"; sync -f "$parent"`;
}

export function _linuxCheckpointDeployJournalCommand(
  journalPath: string,
  from: LinuxDeployJournalPhase,
  to: LinuxDeployJournalPhase,
): string {
  assertFixedLinuxJournalPath(journalPath);
  if (!((from === "prepared" && to === "activating")
    || (from === "activating" && to === "activated"))) {
    throw new Error(`invalid Linux deployment journal transition: ${from} -> ${to}`);
  }
  return `set -e; umask 077; journal=${quoteRemote(journalPath)}; ` +
    `test -d "$journal" && test ! -L "$journal"; ` +
    `test "$(cat "$journal/phase")" = ${from}; next="$journal/phase.next"; ` +
    `rm -f -- "$next"; printf '%s' ${to} > "$next"; chmod 600 "$next"; ` +
    `sync -f "$next"; mv -- "$next" "$journal/phase"; sync -f "$journal"`;
}

export function _linuxClearDeployJournalCommand(journalPath: string): string {
  assertFixedLinuxJournalPath(journalPath);
  const parent = posix.dirname(journalPath);
  return `set -e; journal=${quoteRemote(journalPath)}; parent=${quoteRemote(parent)}; ` +
    `test "$(basename -- "$journal")" = ${LINUX_DEPLOY_JOURNAL_NAME}; ` +
    `if test ! -e "$journal" && test ! -L "$journal"; then exit 0; fi; ` +
    `test -d "$journal" && test ! -L "$journal"; cleared="$journal.cleared"; ` +
    `rm -rf -- "$cleared"; mv -- "$journal" "$cleared"; sync -f "$parent"; ` +
    `rm -rf -- "$cleared"; sync -f "$parent"`;
}

export function _linuxRemoveManagedWorkerReleaseCommand(
  targetReleasePath: string,
  home: string,
): string {
  if (!isManagedLinuxWorkerReleasePath(targetReleasePath, home)) {
    malformedLinuxJournal(
      `refusing to remove unmanaged worker release ${JSON.stringify(targetReleasePath)}`,
    );
  }
  const root = linuxWorkerReleaseRoot(home);
  return `set -e; root=${quoteRemote(root)}; target=${quoteRemote(targetReleasePath)}; ` +
    `test "$(dirname -- "$target")" = "$root"; ` +
    `if test ! -e "$target" && test ! -L "$target"; then exit 0; fi; ` +
    `if test -d "$target" && git -C "$target" rev-parse --git-dir >/dev/null 2>&1; then ` +
    `git -C "$target" worktree remove --force "$target" 2>/dev/null || true; fi; ` +
    `if test -e "$target" || test -L "$target"; then rm -rf -- "$target"; fi; ` +
    `test ! -e "$target" && test ! -L "$target"`;
}

export function _linuxTargetVerificationCommand(
  journal: LinuxDeployJournal,
  home: string,
): string {
  assertLinuxDeployJournal(journal, home);
  const expected = quoteRemote(journal.targetReleasePath);
  const targetSha = quoteRemote(journal.targetSha);
  return `${verifyWorkerCmd("linux")}; service_exit=$?; ` +
    `expected=${expected}; target_sha=${targetSha}; prior_pid=${journal.priorPid}; ` +
    `pid=$(systemctl --user show ${WORKER_UNIT} --property=MainPID --value); pid_exit=$?; ` +
    `case "$pid" in ''|0|*[!0-9]*) exit 1;; esac; ` +
    `actual=$(readlink -f -- "/proc/$pid/cwd"); actual_exit=$?; ` +
    `environment=$(tr '\\0' '\\n' < "/proc/$pid/environ"); environment_exit=$?; ` +
    `if test "$service_exit" -eq 0 && test "$pid_exit" -eq 0 ` +
    `&& test "$actual_exit" -eq 0 && test "$environment_exit" -eq 0 ` +
    `&& { test "$prior_pid" -eq 0 || test "$pid" -ne "$prior_pid"; } ` +
    `&& test -d "$expected" && test "$actual" = "$expected" ` +
    `&& printf '%s\\n' "$environment" | grep -Fqx -- "GIT_SHA=$target_sha"; ` +
    `then echo RoostReleaseMatch=yes; else exit 1; fi`;
}

export function _linuxRestorePriorServiceCommand(
  journal: LinuxDeployJournal,
  journalPath: string,
  unitPath: string,
  home: string,
): string {
  assertLinuxDeployJournal(journal, home);
  assertFixedLinuxJournalPath(journalPath);
  const restore = journal.priorUnit === null
    ? `rm -f -- "$unit"`
    : `test -f "$journal/prior-unit" && test ! -L "$journal/prior-unit"; ` +
      `mkdir -p "$(dirname -- "$unit")"; rm -f -- "$unit"; cp -- "$journal/prior-unit" "$unit"; ` +
      `chmod ${journal.priorUnitMode!.toString(8).padStart(3, "0")} "$unit"`;
  const enablement = journal.priorEnablement === "enabled"
    ? `systemctl --user enable ${WORKER_UNIT}`
    : journal.priorEnablement === "masked"
      ? `systemctl --user mask --runtime ${WORKER_UNIT}`
      : `systemctl --user disable ${WORKER_UNIT} 2>/dev/null || true`;
  const restart = journal.priorLifecycle === "running"
    ? `; systemctl --user start ${WORKER_UNIT}`
    : "";
  return `set -e; export XDG_RUNTIME_DIR="\${XDG_RUNTIME_DIR:-/run/user/\$(id -u)}"; ` +
    `journal=${quoteRemote(journalPath)}; unit=${quoteRemote(unitPath)}; ` +
    `test -d "$journal" && test ! -L "$journal"; ` +
    `systemctl --user stop ${WORKER_UNIT} 2>/dev/null || true; ` +
    `systemctl --user disable ${WORKER_UNIT} 2>/dev/null || true; ` +
    `systemctl --user unmask ${WORKER_UNIT} 2>/dev/null || true; ` +
    `systemctl --user reset-failed ${WORKER_UNIT} 2>/dev/null || true; ` +
    `${restore}; systemctl --user daemon-reload; ${enablement}${restart}`;
}

export function _linuxPriorServiceProofCommand(
  journal: LinuxDeployJournal,
  journalPath: string,
  unitPath: string,
  home: string,
): string {
  assertLinuxDeployJournal(journal, home);
  assertFixedLinuxJournalPath(journalPath);
  const expectedEnablement = journal.priorEnablement === "absent"
    ? "not-found"
    : journal.priorEnablement;
  const prefix = `export XDG_RUNTIME_DIR="\${XDG_RUNTIME_DIR:-/run/user/\$(id -u)}"; ` +
    `journal=${quoteRemote(journalPath)}; unit=${quoteRemote(unitPath)}; `;
  if (journal.priorUnit === null) {
    return prefix +
      `state=$(systemctl --user show ${WORKER_UNIT} --property=LoadState --property=ActiveState); show_exit=$?; ` +
      `enablement=$(systemctl --user is-enabled ${WORKER_UNIT} 2>/dev/null || true); ` +
      `printf '%s\\n' "$state"; ` +
      `if test "$show_exit" -eq 0 && test "$enablement" = ${expectedEnablement} ` +
      `&& test ! -e "$unit" && test ! -L "$unit" ` +
      `&& printf '%s\\n' "$state" | grep -q '^LoadState=not-found$' ` +
      `&& printf '%s\\n' "$state" | grep -q '^ActiveState=inactive$'; ` +
      `then echo RoostPriorStateMatch=yes; else exit 1; fi`;
  }
  const exactDefinition =
    `cmp -s "$journal/prior-unit" "$unit" ` +
    `&& test "$(stat -c '%a' "$unit")" = ${journal.priorUnitMode!.toString(8).padStart(3, "0")}`;
  if (journal.priorLifecycle === "running") {
    return prefix +
      `load_state=$(systemctl --user show ${WORKER_UNIT} --property=LoadState --value); load_exit=$?; ` +
      `enablement=$(systemctl --user is-enabled ${WORKER_UNIT} 2>/dev/null || true); ` +
      `${verifyWorkerCmd("linux")}; service_exit=$?; ` +
      `if test "$load_exit" -eq 0 && test "$load_state" = loaded && test "$service_exit" -eq 0 ` +
      `&& test "$enablement" = ${expectedEnablement} && ${exactDefinition}; ` +
      `then echo RoostPriorStateMatch=yes; else exit 1; fi`;
  }
  return prefix +
    `state=$(systemctl --user show ${WORKER_UNIT} --property=LoadState --property=ActiveState); show_exit=$?; ` +
    `enablement=$(systemctl --user is-enabled ${WORKER_UNIT} 2>/dev/null || true); ` +
    `printf '%s\\n' "$state"; ` +
    `if test "$show_exit" -eq 0 && test "$enablement" = ${expectedEnablement} ` +
    `&& ${exactDefinition} ` +
    `&& printf '%s\\n' "$state" | grep -q '^LoadState=loaded$' ` +
    `&& printf '%s\\n' "$state" | grep -q '^ActiveState=inactive$'; ` +
    `then echo RoostPriorStateMatch=yes; else exit 1; fi`;
}

export function linuxWorkerResourceEnvironment(definition: string): Record<string, string> {
  const installed = parsePosixServiceEnvironment(definition, "linux");
  const environment: Record<string, string> = {};
  for (const key of [
    "ROOST_WORKER_MEMORY_HIGH",
    "ROOST_WORKER_TASKS_MAX",
    "ROOST_WORKER_LOGROTATE_CONF",
  ] as const) {
    if (installed[key]) environment[key] = installed[key];
  }
  for (const [directive, key] of [
    ["MemoryHigh", "ROOST_WORKER_MEMORY_HIGH"],
    ["TasksMax", "ROOST_WORKER_TASKS_MAX"],
  ] as const) {
    const value = parseSystemdServiceDirective(definition, directive);
    if (value) environment[key] = value;
  }
  return environment;
}

export function shouldRemovePriorWorkerRelease(
  prior: string,
  current: string,
  coordinator: string | null,
  home: string,
): boolean {
  if ((coordinator !== null
    && (!posix.isAbsolute(coordinator) || /[\r\n\0]/.test(coordinator)))
    || !prior || prior === current || prior === coordinator
    || !isManagedLinuxWorkerReleasePath(prior, home)
    || !isManagedLinuxWorkerReleasePath(current, home)) {
    return false;
  }
  return true;
}

export function linuxCoordinatorWorkingDirectoryCommand(): string {
  return `set -e; export XDG_RUNTIME_DIR="\${XDG_RUNTIME_DIR:-/run/user/\$(id -u)}"; ` +
    `load_state=$(systemctl --user show ${COORD_UNIT} --property=LoadState --value); ` +
    `case "$load_state" in ` +
    `not-found) printf 'absent\\n';; ` +
    `loaded) systemctl --user show ${COORD_UNIT} --property=WorkingDirectory --value;; ` +
    `*) exit 65;; esac`;
}

export interface LinuxDeployTargetProof {
  healthy: boolean;
  proof: { exit: number; stdout: string; stderr: string };
}

export interface LinuxDeployRecoveryRemote {
  home: string;
  loadJournal: () => Promise<LinuxDeployJournal | null>;
  proveTarget: (journal: LinuxDeployJournal) => Promise<LinuxDeployTargetProof>;
  restorePrior: (journal: LinuxDeployJournal) => Promise<void>;
  provePrior: (journal: LinuxDeployJournal) => Promise<void>;
  cleanupPrior: (journal: LinuxDeployJournal) => Promise<void>;
  removeTarget: (journal: LinuxDeployJournal) => Promise<void>;
  clearJournal: () => Promise<void>;
}

export interface LinuxRecoveryOutcome {
  kind: "none" | "prepared-cleaned" | "target-committed" | "prior-restored";
  verification?: { exit: number; stdout: string; stderr: string };
}

async function loadLinuxDeployJournal(
  deploySsh: DeploySsh,
  journalPath: string,
  home: string,
): Promise<LinuxDeployJournal | null> {
  const loaded = await deploySsh(_linuxLoadDeployJournalCommand(journalPath));
  if (loaded.exit !== 0) {
    failDeploy(
      loaded.exit || 5,
      `cannot read the fixed Linux deployment journal; it was left intact\n${loaded.stdout}\n${loaded.stderr}`,
    );
  }
  return parseLinuxDeployJournalSnapshot(loaded.stdout, home);
}

async function removeManagedLinuxWorkerRelease(
  deploySsh: DeploySsh,
  targetReleasePath: string,
  home: string,
): Promise<void> {
  const removed = await deploySsh(
    _linuxRemoveManagedWorkerReleaseCommand(targetReleasePath, home),
  );
  if (removed.exit !== 0) {
    failDeploy(
      removed.exit || 5,
      `cannot remove managed worker stage ${targetReleasePath}; deployment journal retained\n` +
        `${removed.stdout}\n${removed.stderr}`,
    );
  }
}

async function clearLinuxDeployJournal(
  deploySsh: DeploySsh,
  journalPath: string,
): Promise<void> {
  const cleared = await deploySsh(_linuxClearDeployJournalCommand(journalPath));
  if (cleared.exit !== 0) {
    failDeploy(
      cleared.exit || 5,
      `cannot durably clear the Linux deployment journal\n${cleared.stdout}\n${cleared.stderr}`,
    );
  }
}

async function proveLinuxTargetRelease(
  deploySsh: DeploySsh,
  journal: LinuxDeployJournal,
  home: string,
  attempts = 20,
): Promise<{
  healthy: boolean;
  proof: { exit: number; stdout: string; stderr: string };
}> {
  let proof = { exit: 1, stdout: "", stderr: "target verification was not attempted" };
  for (let attempt = 0; attempt < attempts; attempt += 1) {
    proof = await deploySsh(_linuxTargetVerificationCommand(journal, home));
    if (proof.exit === 0
      && workerServiceIsRunning(proof.stdout, "linux")
      && workerServiceMatchesRelease(proof.stdout)) {
      return { healthy: true, proof };
    }
    if (proof.exit === 9 || proof.exit === 130 || proof.exit === 143) break;
    if (attempt + 1 < attempts) await Bun.sleep(250);
  }
  return { healthy: false, proof };
}

async function proveLinuxPriorService(
  deploySsh: DeploySsh,
  journal: LinuxDeployJournal,
  journalPath: string,
  unitPath: string,
  home: string,
): Promise<{ exit: number; stdout: string; stderr: string }> {
  let proof = { exit: 1, stdout: "", stderr: "rollback verification was not attempted" };
  for (let attempt = 0; attempt < 20; attempt += 1) {
    proof = await deploySsh(
      _linuxPriorServiceProofCommand(journal, journalPath, unitPath, home),
    );
    const lifecycleMatches = /^RoostPriorStateMatch=yes$/m.test(proof.stdout);
    const runningMatches = journal.priorLifecycle !== "running"
      || workerServiceIsRunning(proof.stdout, "linux");
    if (proof.exit === 0 && lifecycleMatches && runningMatches) return proof;
    if (proof.exit === 9 || proof.exit === 130 || proof.exit === 143) break;
    if (attempt < 19) await Bun.sleep(250);
  }
  failDeploy(
    proof.exit || 5,
    `rollback could not prove the exact prior unit and lifecycle; deployment journal retained\n` +
      `${proof.stdout}\n${proof.stderr}`,
  );
}

async function removePriorLinuxWorkerRelease(
  deploySsh: DeploySsh,
  journal: LinuxDeployJournal,
  home: string,
  signal: AbortSignal,
): Promise<void> {
  const prior = journal.priorUnit === null
    ? ""
    : parseSystemdServiceDirective(journal.priorUnit, "WorkingDirectory") ?? "";
  if (!prior || prior === journal.targetReleasePath) return;
  if (!shouldRemovePriorWorkerRelease(
    prior,
    journal.targetReleasePath,
    "/dev/null",
    home,
  )) return;
  const coordinator = await deploySsh(linuxCoordinatorWorkingDirectoryCommand());
  if (signal.aborted) {
    const reason = signal.reason;
    throw reason instanceof DeployFailure
      ? reason
      : new DeployFailure(coordinator.exit || 9, "deployment interrupted while retaining the prior release");
  }
  if (coordinator.exit !== 0) {
    failDeploy(
      coordinator.exit || 5,
      `cannot prove the coordinator release before prior worker cleanup; deployment journal retained\n` +
        `${coordinator.stdout}\n${coordinator.stderr}`,
    );
  }
  const reportedCoordinatorPath = coordinator.stdout.trim();
  const coordinatorPath = reportedCoordinatorPath === "absent"
    ? null
    : reportedCoordinatorPath;
  if (coordinatorPath !== null
    && (!posix.isAbsolute(coordinatorPath) || /[\r\n\0]/.test(coordinatorPath))) {
    failDeploy(5, "coordinator WorkingDirectory is malformed; deployment journal retained");
  }
  if (!shouldRemovePriorWorkerRelease(
    prior,
    journal.targetReleasePath,
    coordinatorPath,
    home,
  )) {
    return;
  }
  const removed = await deploySsh(
    _linuxRemoveManagedWorkerReleaseCommand(prior, home),
  );
  if (signal.aborted) {
    const reason = signal.reason;
    throw reason instanceof DeployFailure
      ? reason
      : new DeployFailure(removed.exit || 9, "deployment interrupted while removing the prior release");
  }
  if (removed.exit !== 0) {
    failDeploy(
      removed.exit || 5,
      `cannot retire prior worker release ${prior}; deployment journal retained\n` +
        `${removed.stdout}\n${removed.stderr}`,
    );
  }
}

export async function _recoverLinuxDeployJournal(
  remote: LinuxDeployRecoveryRemote,
): Promise<LinuxRecoveryOutcome> {
  const journal = await remote.loadJournal();
  if (journal === null) return { kind: "none" };

  let target: LinuxDeployTargetProof = {
    healthy: false,
    proof: { exit: 1, stdout: "", stderr: "prepared stages are never committed" },
  };
  if (journal.phase !== "prepared") target = await remote.proveTarget(journal);
  const plan = linuxDeployRecoveryPlan(journal, target.healthy, remote.home);
  if (plan.kind === "clean-prepared") {
    await remote.removeTarget(journal);
    await remote.clearJournal();
    return { kind: "prepared-cleaned" };
  }
  if (plan.kind === "commit-target") {
    await remote.cleanupPrior(journal);
    await remote.clearJournal();
    return { kind: "target-committed", verification: target.proof };
  }
  await remote.restorePrior(journal);
  await remote.provePrior(journal);
  await remote.removeTarget(journal);
  await remote.clearJournal();
  return { kind: "prior-restored" };
}

async function recoverLinuxDeployJournal(
  deploySsh: DeploySsh,
  journalPath: string,
  unitPath: string,
  home: string,
  signal: AbortSignal,
): Promise<LinuxRecoveryOutcome> {
  return await _recoverLinuxDeployJournal({
    home,
    loadJournal: () => loadLinuxDeployJournal(deploySsh, journalPath, home),
    proveTarget: (journal) => proveLinuxTargetRelease(deploySsh, journal, home),
    restorePrior: async (journal) => {
      const restored = await deploySsh(
        _linuxRestorePriorServiceCommand(journal, journalPath, unitPath, home),
      );
      if (restored.exit !== 0) {
        failDeploy(
          restored.exit || 5,
          `rollback could not restore the prior Linux unit; deployment journal retained\n` +
            `${restored.stdout}\n${restored.stderr}`,
        );
      }
    },
    provePrior: async (journal) => {
      await proveLinuxPriorService(deploySsh, journal, journalPath, unitPath, home);
    },
    cleanupPrior: (journal) => removePriorLinuxWorkerRelease(
      deploySsh,
      journal,
      home,
      signal,
    ),
    removeTarget: (journal) => removeManagedLinuxWorkerRelease(
      deploySsh,
      journal.targetReleasePath,
      home,
    ),
    clearJournal: () => clearLinuxDeployJournal(deploySsh, journalPath),
  });
}

export async function deployLinux(
  host: string,
  opts: { gitSha: string; passthroughEnv: string; machineTransactionPath: string },
): Promise<void> {
  const { gitSha, passthroughEnv, machineTransactionPath } = opts;

  // The caller has refreshed and proved the source upstream before acquiring
  // any target lease; retain only the exact clean identity at this boundary.
  if (!/^[0-9a-f]{40}(?:[0-9a-f]{24})?$/.test(gitSha) || gitSha.endsWith("-dirty")) {
    failDeploy(7, "a Linux deploy requires a clean pushed commit");
  }

  const releaseId = `${gitSha}-${crypto.randomUUID()}`;
  const deployLease = await acquireRemoteDeployLock(host, machineTransactionPath, releaseId);
  const deploySsh: DeploySsh = (command) => sshExec(host, command, deployLease.signal);
  try {
    const resolvedHome = await deploySsh("set -e; cd ~ && pwd");
    if (resolvedHome.exit !== 0) {
      failDeploy(
        resolvedHome.exit || 2,
        `cannot resolve the remote Linux home directory\n${resolvedHome.stdout}\n${resolvedHome.stderr}`,
      );
    }
    const home = resolvedHome.stdout.trim();
    const releaseRoot = linuxWorkerReleaseRoot(home);
    const journalPath = linuxDeployJournalPath(machineTransactionPath, home);
    const foreignJournalGuard = await deploySsh(
      `set -e; base=${quoteRemote(posix.dirname(journalPath))}; ` +
        `for relative in ${quoteRemote(POSIX_WORKER_DEPLOY_JOURNAL_PATHS.local)} ` +
        `${quoteRemote(POSIX_WORKER_DEPLOY_JOURNAL_PATHS.darwin)} ` +
        `${quoteRemote(POSIX_WORKER_DEPLOY_JOURNAL_PATHS.coordinator)}; do ` +
        `foreign="$base/$relative"; ` +
        `if test -e "$foreign" || test -L "$foreign"; then exit 66; fi; done`,
    );
    if (foreignJournalGuard.exit !== 0) {
      failDeploy(
        foreignJournalGuard.exit || 5,
        `cannot mutate past an unsettled foreign worker deploy journal on ${host}`,
      );
    }
    const unitPath = posix.join(home, ".config", "systemd", "user", WORKER_UNIT);

    // A fixed journal is always settled while holding the renewable machine
    // lease and before inspecting or staging the next release.
    const initialRecovery = await recoverLinuxDeployJournal(
      deploySsh,
      journalPath,
      unitPath,
      home,
      deployLease.signal,
    );
    if (initialRecovery.kind === "prepared-cleaned") {
      console.log(">> recovered interrupted Linux deploy (discarded prepared stage)");
    } else if (initialRecovery.kind === "target-committed") {
      console.log(">> recovered interrupted Linux deploy (verified activated target)");
    } else if (initialRecovery.kind === "prior-restored") {
      console.log(">> recovered interrupted Linux deploy (restored prior service)");
    }

    // The installed unit is authoritative. Accept both a primary checkout
    // (`.git/`) and a staged linked worktree (`.git` file). This discovery is
    // deliberately after journal recovery so a broken activation cannot
    // prevent the next lease owner from repairing the service.
    let remoteRepo = process.env.ROOST_LINUX_REPO_DIR?.trim() ?? "";
    if (!remoteRepo) {
      const probe = await deploySsh(
        `export XDG_RUNTIME_DIR="\${XDG_RUNTIME_DIR:-/run/user/\$(id -u)}"; ` +
          `unit_dir=$(systemctl --user show ${WORKER_UNIT} --property=WorkingDirectory --value 2>/dev/null || true); ` +
          `for d in "$unit_dir" "$HOME/Roost" /srv/roost; do ` +
          `[ -n "$d" ] && git -C "$d" rev-parse --git-dir >/dev/null 2>&1 && echo "$d" && break; done`,
      );
      remoteRepo = probe.stdout.trim();
    }
    if (!remoteRepo) {
      throw new Error(
        `no worker checkout found on ${host} (checked ${WORKER_UNIT} WorkingDirectory, ~/Roost and /srv/roost) — ` +
          "run join.sh first or set ROOST_LINUX_REPO_DIR",
      );
    }
    if (!posix.isAbsolute(remoteRepo) || /[\r\n\0]/.test(remoteRepo)) {
      failDeploy(2, `worker checkout path from ${host} is unsafe: ${JSON.stringify(remoteRepo)}`);
    }

    const releaseDir = posix.join(releaseRoot, releaseId);
    if (!isManagedLinuxWorkerReleasePath(releaseDir, home)) {
      failDeploy(2, `generated Linux worker release path is unsafe: ${releaseDir}`);
    }
    const cleanupStage = () =>
      removeManagedLinuxWorkerRelease(deploySsh, releaseDir, home);

    console.log(`>> stage ${gitSha.slice(0, 8)} in ${host}:${releaseDir}`);
    const stage = await deploySsh(
      `set -e; mkdir -p ${quoteRemote(releaseRoot)}; ` +
        `git -C ${quoteRemote(remoteRepo)} fetch --quiet origin; ` +
        `git -C ${quoteRemote(remoteRepo)} worktree add --quiet --force --detach ` +
        `${quoteRemote(releaseDir)} ${quoteRemote(gitSha)}`,
    );
    if (stage.exit !== 0) {
      if (!deployLease.signal.aborted) await cleanupStage();
      failDeploy(stage.exit || 2, `git worktree staging failed\n${stage.stdout}\n${stage.stderr}`);
    }

    const prepared = await deploySsh(_linuxPrepareDeployJournalCommand({
      journalPath,
      unitPath,
      targetSha: gitSha,
      targetReleasePath: releaseDir,
      home,
    }));
    if (prepared.exit !== 0) {
      try {
        const recovered = await recoverLinuxDeployJournal(
          deploySsh,
          journalPath,
          unitPath,
          home,
          deployLease.signal,
        );
        if (recovered.kind === "none") await cleanupStage();
      } catch (recoveryError) {
        const detail = recoveryError instanceof Error
          ? recoveryError.message
          : String(recoveryError);
        failDeploy(
          recoveryError instanceof DeployFailure
            ? recoveryError.exitCode
            : prepared.exit || 5,
          `cannot durably prepare the Linux deployment journal; recovery remains pending\n${detail}`,
        );
      }
      failDeploy(
        prepared.exit || 5,
        `cannot durably snapshot ${WORKER_UNIT} before activation\n${prepared.stdout}\n${prepared.stderr}`,
      );
    }

    const journal = await loadLinuxDeployJournal(deploySsh, journalPath, home);
    if (journal === null
      || journal.phase !== "prepared"
      || journal.targetSha !== gitSha
      || journal.targetReleasePath !== releaseDir) {
      malformedLinuxJournal("prepared checkpoint does not identify the staged target");
    }
    const priorDefinition = journal.priorUnit ?? "";
    const preservedResources = linuxWorkerResourceEnvironment(priorDefinition);
    const resourceAssignments = Object.entries(preservedResources)
      .map(([key, value]) => `${key}=${quoteRemote(value)}`)
      .join(" ");
    const activationEnvironment = [passthroughEnv, resourceAssignments]
      .filter(Boolean)
      .join(" ");

    const settleActivationFailure = async (
      summary: string,
      failed: { exit: number; stdout: string; stderr: string },
    ): Promise<{ exit: number; stdout: string; stderr: string }> => {
      let recovered: LinuxRecoveryOutcome;
      try {
        recovered = await recoverLinuxDeployJournal(
          deploySsh,
          journalPath,
          unitPath,
          home,
          deployLease.signal,
        );
      } catch (recoveryError) {
        const detail = recoveryError instanceof Error
          ? recoveryError.message
          : String(recoveryError);
        const interrupted = deployLease.signal.reason;
        failDeploy(
          interrupted instanceof DeployFailure
            ? interrupted.exitCode
            : recoveryError instanceof DeployFailure
              ? recoveryError.exitCode
              : failed.exit || 5,
          `${summary}\n${failed.stdout}\n${failed.stderr}\n` +
            `automatic recovery is incomplete; fixed journal retained\n${detail}`,
        );
      }
      if (recovered.kind === "target-committed" && recovered.verification) {
        console.warn(`   ${summary}; committed the independently verified target`);
        return recovered.verification;
      }
      const recoveryDetail = recovered.kind === "prior-restored"
        ? "prior worker unit and lifecycle restored"
        : recovered.kind === "prepared-cleaned"
          ? "prepared worker stage removed"
          : "no recoverable journal was found";
      failDeploy(
        failed.exit || 5,
        `${summary}\n${failed.stdout}\n${failed.stderr}\n${recoveryDetail}`,
      );
    };

    console.log(`>> frozen bun install on ${host}`);
    const install = await deploySsh(
      `set -eo pipefail; cd ${quoteRemote(releaseDir)} && ` +
        `bun install --frozen-lockfile 2>&1 | tail -25`,
    );
    if (install.exit !== 0) {
      await settleActivationFailure("bun install failed", install);
    }
    console.log("   bun install ok");

    const activating = await deploySsh(
      _linuxCheckpointDeployJournalCommand(journalPath, "prepared", "activating"),
    );
    if (activating.exit !== 0) {
      await settleActivationFailure("cannot checkpoint Linux activation", activating);
    }

    console.log(`>> activate staged systemd unit (${WORKER_UNIT}) on ${host}`);
    const installSh = await deploySsh(
      `${activationEnvironment} bash ` +
        `${quoteRemote(posix.join(releaseDir, "apps/worker/scripts/install.sh"))} install 2>&1`,
    );
    if (installSh.exit !== 0) {
      const committed = await settleActivationFailure("install.sh failed", installSh);
      finishWorkerDeploy(
        committed,
        `>> done — ${host} v2 worker deployed (linux)`,
        "linux",
      );
      return;
    }

    console.log(`>> verifying service is up on ${host}`);
    const target = await proveLinuxTargetRelease(deploySsh, journal, home);
    if (!target.healthy) {
      const committed = await settleActivationFailure(
        "worker service verification failed",
        target.proof,
      );
      finishWorkerDeploy(
        committed,
        `>> done — ${host} v2 worker deployed (linux)`,
        "linux",
      );
      return;
    }

    const activated = await deploySsh(
      _linuxCheckpointDeployJournalCommand(journalPath, "activating", "activated"),
    );
    if (activated.exit !== 0) {
      const committed = await settleActivationFailure(
        "cannot checkpoint verified Linux activation",
        activated,
      );
      finishWorkerDeploy(
        committed,
        `>> done — ${host} v2 worker deployed (linux)`,
        "linux",
      );
      return;
    }

    const finalTarget = await proveLinuxTargetRelease(deploySsh, {
      ...journal,
      phase: "activated",
    }, home);
    if (!finalTarget.healthy) {
      const committed = await settleActivationFailure(
        "activated worker lost exact release health before commit",
        finalTarget.proof,
      );
      finishWorkerDeploy(
        committed,
        `>> done — ${host} v2 worker deployed (linux)`,
        "linux",
      );
      return;
    }

    await removePriorLinuxWorkerRelease(
      deploySsh,
      journal,
      home,
      deployLease.signal,
    );
    const cleared = await deploySsh(_linuxClearDeployJournalCommand(journalPath));
    if (cleared.exit !== 0) {
      const recovered = await recoverLinuxDeployJournal(
        deploySsh,
        journalPath,
        unitPath,
        home,
        deployLease.signal,
      );
      if (recovered.kind === "prior-restored") {
        failDeploy(
          cleared.exit || 5,
          `commit journal cleanup failed and the prior worker service was restored\n` +
            `${cleared.stdout}\n${cleared.stderr}`,
        );
      }
      if (recovered.kind === "target-committed" && recovered.verification) {
        finishWorkerDeploy(
          recovered.verification,
          `>> done — ${host} v2 worker deployed (linux)`,
          "linux",
        );
        return;
      }
      if (recovered.kind !== "none") {
        failDeploy(cleared.exit || 5, "Linux deployment journal cleanup did not commit");
      }
    }
    finishWorkerDeploy(
      finalTarget.proof,
      `>> done — ${host} v2 worker deployed (linux)`,
      "linux",
    );
  } finally {
    await releaseRemoteDeployLock(host, machineTransactionPath, releaseId);
  }
}
