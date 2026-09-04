// POSIX service-control command strings for launchd (darwin) and systemd
// --user (linux), retained for the POSIX deploy/update/reset callers.
//
// BYTE-STABILITY: every string here is handed to bash locally and over ssh;
// deploy recovery machines match on exact output. Do not reformat, re-quote,
// or "modernize" any literal — see the XDG and launchd notes below.
//
// Callers: deploy.ts, deploy-linux.ts, deploy-exec.ts, push.ts, update.ts,
// reset.ts, status.ts via the service-ctl.ts barrel.

import {
  COORD_LABEL_DARWIN,
  COORD_LABEL_LINUX,
  WORKER_LABEL_DARWIN,
  WORKER_LABEL_LINUX,
} from "@roost/shared/paths";
import { posixShellQuote } from "@roost/shared/shell-quote";

/** The command-string helpers below are retained for POSIX deploy callers. */
export type PosixServiceOs = "darwin" | "linux";
export type ServiceOs = PosixServiceOs | "win32";

export const WORKER_UNIT = `${WORKER_LABEL_LINUX}.service`;
export const WORKER_AGENT = WORKER_LABEL_DARWIN;
export const COORD_UNIT = `${COORD_LABEL_LINUX}.service`;
export const COORD_AGENT = COORD_LABEL_DARWIN;

// systemd --user over ssh has no login session, so XDG_RUNTIME_DIR is
// unset and systemctl can't find the user bus. Keep this byte-for-byte stable:
// these strings are handed to bash locally and over ssh by POSIX-only callers.
const XDG = `export XDG_RUNTIME_DIR="\${XDG_RUNTIME_DIR:-/run/user/$(id -u)}";`;

function posixServiceCommand(
  os: ServiceOs,
  linux: () => string,
  darwin: () => string,
): string {
  switch (os) {
    case "linux":
      return linux();
    case "darwin":
      return darwin();
    case "win32":
      throw new Error("Windows services require createWindowsServiceManager(); POSIX command strings are disabled");
    default:
      return assertNever(os);
  }
}

function assertNever(value: never): never {
  throw new Error(`unhandled service platform: ${String(value)}`);
}

export function restartWorkerCmd(os: ServiceOs): string {
  return posixServiceCommand(
    os,
    () => `${XDG} systemctl --user restart ${WORKER_UNIT}`,
    () => `launchctl kickstart -k gui/$(id -u)/${WORKER_AGENT}`,
  );
}

export function verifyWorkerCmd(os: ServiceOs): string {
  return posixServiceCommand(
    os,
    () => `${XDG} systemctl --user show ${WORKER_UNIT} -p ActiveState -p SubState -p MainPID`,
    () => `set -o pipefail; launchctl print gui/$(id -u)/${WORKER_AGENT} 2>&1 | grep -E '^\\s*(state|pid|active count)' | head -5`,
  );
}

export function restartCoordCmd(os: ServiceOs): string {
  return posixServiceCommand(
    os,
    () => `${XDG} systemctl --user daemon-reload && systemctl --user restart ${COORD_UNIT}`,
    () => `launchctl kickstart -k gui/$(id -u)/${COORD_AGENT}`,
  );
}

export function stopServicesCmd(os: ServiceOs): string {
  return posixServiceCommand(
    os,
    () => `${XDG} systemctl --user stop ${COORD_UNIT} ${WORKER_UNIT}`,
    () => `launchctl bootout gui/$(id -u)/${COORD_AGENT} 2>/dev/null; launchctl bootout gui/$(id -u)/${WORKER_AGENT} 2>/dev/null; true`,
  );
}

export interface LaunchdBootstrapOptions {
  /** Names the caller in the failure line, e.g. `worker rollback`. */
  role: string;
  /** Bracket the retry with `bootout` before and `enable` + `kickstart` after,
   *  for `label`. Default true. The macOS deploy recovery machine passes false:
   *  its bootout runs its own settle poll and its enable/disable state is
   *  journal-driven, so it drives those steps as separate remote commands. */
  reload?: boolean;
  /** `plistPath` is relative to the *target's* `$HOME`, expanded by the remote
   *  shell instead of being quoted as a literal local path. */
  homeRelative?: boolean;
}

/** launchd refuses `bootstrap` while the prior job is still unloading, so every
 *  reload path retries for ten seconds with a settle sleep before each attempt.
 *  Keep this byte-for-byte stable for the same reason as XDG above: these
 *  strings are handed to bash locally and over ssh. */
export function launchdBootstrapWithRetryCmd(
  label: string,
  plistPath: string,
  options: LaunchdBootstrapOptions,
): string {
  const plist = options.homeRelative ? `"$HOME/${plistPath}"` : posixShellQuote(plistPath);
  const retry = `for i in 1 2 3 4 5 6 7 8 9 10; do sleep 1; `
    + `launchctl bootstrap gui/$uid ${plist} 2>/dev/null && break; `
    + `if test "$i" = 10; then echo "${options.role} bootstrap failed after 10 retries" >&2; exit 1; fi; done`;
  if (options.reload === false) return `uid=$(id -u); ${retry}`;
  const job = `gui/$uid/${posixShellQuote(label)}`;
  return `set -e; uid=$(id -u); launchctl bootout ${job} 2>/dev/null || true; `
    + `${retry}; `
    + `launchctl enable ${job}; `
    + `launchctl kickstart -k ${job} 2>/dev/null || true`;
}

export function currentServiceOs(): ServiceOs {
  switch (process.platform) {
    case "darwin":
    case "linux":
    case "win32":
      return process.platform;
    default:
      throw new Error(`unsupported service platform: ${process.platform}`);
  }
}
