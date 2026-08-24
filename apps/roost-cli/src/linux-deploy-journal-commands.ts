// Remote shell command builders for the fixed-path Linux worker deploy
// journal and its release/proof choreography (the `_linux*Command` exports).
// Every string is transmitted verbatim over ssh by the recovery driver in
// deploy-linux.ts and pinned by deploy-linux-recovery/deploy-verification
// tests — treat bodies as byte-stability-sensitive. Validation and schema
// come from linux-deploy-journal.ts.

import { posix } from "node:path";
import { verifyWorkerCmd, WORKER_UNIT } from "./service-ctl.ts";
import { posixShellQuote } from "./shell-quote.ts";
import {
  assertFixedLinuxJournalPath,
  assertLinuxDeployJournal,
  isManagedLinuxWorkerReleasePath,
  LINUX_DEPLOY_JOURNAL_NAME,
  LINUX_DEPLOY_JOURNAL_SCHEMA,
  linuxWorkerReleaseRoot,
  malformedLinuxJournal,
  type LinuxDeployJournal,
  type LinuxDeployJournalPhase,
} from "./linux-deploy-journal.ts";

export function _linuxLoadDeployJournalCommand(journalPath: string): string {
  assertFixedLinuxJournalPath(journalPath);
  return `set -e; journal=${posixShellQuote(journalPath)}; ` +
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

export interface LinuxPrepareJournalInput {
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
    `journal=${posixShellQuote(journalPath)}; parent=${posixShellQuote(parent)}; unit=${posixShellQuote(unitPath)}; ` +
    `target_sha=${posixShellQuote(targetSha)}; target_release=${posixShellQuote(targetReleasePath)}; ` +
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
  return `set -e; umask 077; journal=${posixShellQuote(journalPath)}; ` +
    `test -d "$journal" && test ! -L "$journal"; ` +
    `test "$(cat "$journal/phase")" = ${from}; next="$journal/phase.next"; ` +
    `rm -f -- "$next"; printf '%s' ${to} > "$next"; chmod 600 "$next"; ` +
    `sync -f "$next"; mv -- "$next" "$journal/phase"; sync -f "$journal"`;
}

export function _linuxClearDeployJournalCommand(journalPath: string): string {
  assertFixedLinuxJournalPath(journalPath);
  const parent = posix.dirname(journalPath);
  return `set -e; journal=${posixShellQuote(journalPath)}; parent=${posixShellQuote(parent)}; ` +
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
  return `set -e; root=${posixShellQuote(root)}; target=${posixShellQuote(targetReleasePath)}; ` +
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
  const expected = posixShellQuote(journal.targetReleasePath);
  const targetSha = posixShellQuote(journal.targetSha);
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
    `journal=${posixShellQuote(journalPath)}; unit=${posixShellQuote(unitPath)}; ` +
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
    `journal=${posixShellQuote(journalPath)}; unit=${posixShellQuote(unitPath)}; `;
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
