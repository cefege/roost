// Remote shell commands that restore, settle, and prove the PRIOR Linux
// worker unit during a rollback. Split from linux-deploy-journal-commands.ts,
// which owns the journal itself; deploy-linux-recovery.ts and its runtime
// adapters transmit these strings verbatim over ssh, so treat the bodies as
// byte-stability-sensitive. Validation comes from linux-deploy-journal.ts.

import { posixShellQuote } from "@roost/shared/shell-quote";
import {
  assertFixedLinuxJournalPath,
  assertLinuxDeployJournal,
  type LinuxDeployJournal,
} from "./linux-deploy-journal.ts";
import { verifyWorkerCmd, WORKER_UNIT } from "./service-ctl.ts";

export function _linuxRestorePriorServiceCommand(
  journal: LinuxDeployJournal,
  journalPath: string,
  unitPath: string,
  home: string,
): string {
  assertLinuxDeployJournal(journal, home);
  assertFixedLinuxJournalPath(journalPath);
  const restore = journal.priorUnit === null
    ? `rm -f -- "$unit"; systemctl --user daemon-reload`
    : `test -f "$journal/prior-unit" && test ! -L "$journal/prior-unit"; ` +
      `systemctl --user unmask ${WORKER_UNIT} 2>/dev/null || true; ` +
      `mkdir -p "$(dirname -- "$unit")"; rm -f -- "$unit"; cp -- "$journal/prior-unit" "$unit"; ` +
      `chmod ${journal.priorUnitMode!.toString(8).padStart(3, "0")} "$unit"; ` +
      `systemctl --user daemon-reload; ` +
      `systemctl --user reset-failed ${WORKER_UNIT} 2>/dev/null || true; ` +
      `systemctl --user start ${WORKER_UNIT}`;
  return `set -e; export XDG_RUNTIME_DIR="\${XDG_RUNTIME_DIR:-/run/user/\$(id -u)}"; ` +
    `journal=${posixShellQuote(journalPath)}; unit=${posixShellQuote(unitPath)}; ` +
    `test -d "$journal" && test ! -L "$journal"; ${restore}`;
}

export function _linuxSettlePriorServiceCommand(
  journal: LinuxDeployJournal,
  journalPath: string,
  home: string,
): string {
  assertLinuxDeployJournal(journal, home);
  assertFixedLinuxJournalPath(journalPath);
  const lifecycle = journal.priorLifecycle === "stopped"
    ? `systemctl --user stop ${WORKER_UNIT}; `
    : "";
  const enablement = journal.priorEnablement === "enabled"
    ? `systemctl --user enable ${WORKER_UNIT}`
    : journal.priorEnablement === "masked"
      ? `systemctl --user mask --runtime ${WORKER_UNIT}`
      : journal.priorEnablement === "disabled"
        ? `systemctl --user disable ${WORKER_UNIT}`
        : ":";
  return `set -e; export XDG_RUNTIME_DIR="\${XDG_RUNTIME_DIR:-/run/user/\$(id -u)}"; ` +
    `journal=${posixShellQuote(journalPath)}; test -d "$journal" && test ! -L "$journal"; ` +
    `${lifecycle}${enablement}`;
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
