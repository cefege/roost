import { COORD_LABEL_DARWIN, COORD_LABEL_LINUX, WORKER_LABEL_DARWIN, WORKER_LABEL_LINUX } from "@roost/shared/paths";
// Single source of truth for the worker- and coordinator-service dialects:
// launchd on macOS, systemd --user on Linux. Callers (deploy-local,
// deploy-linux, status) hand the resulting string to `bash -c` locally or over
// ssh, so the $(id -u) / XDG_RUNTIME_DIR expansion happens on the target box.

export type ServiceOs = "darwin" | "linux";

export const WORKER_UNIT = `${WORKER_LABEL_LINUX}.service`; // linux
export const WORKER_AGENT = WORKER_LABEL_DARWIN;            // darwin
export const COORD_UNIT = `${COORD_LABEL_LINUX}.service`; // linux
export const COORD_AGENT = COORD_LABEL_DARWIN;            // darwin

// systemd --user over ssh has no login session, so XDG_RUNTIME_DIR is
// unset and systemctl can't find the user bus. Set it in every command.
const XDG = `export XDG_RUNTIME_DIR="\${XDG_RUNTIME_DIR:-/run/user/$(id -u)}";`;

export function restartWorkerCmd(os: ServiceOs): string {
  return os === "linux"
    ? `${XDG} systemctl --user restart ${WORKER_UNIT}`
    : `launchctl kickstart -k gui/$(id -u)/${WORKER_AGENT}`;
}

export function verifyWorkerCmd(os: ServiceOs): string {
  return os === "linux"
    ? `${XDG} systemctl --user show ${WORKER_UNIT} -p ActiveState -p SubState -p MainPID`
    : `set -o pipefail; launchctl print gui/$(id -u)/${WORKER_AGENT} 2>&1 | grep -E '^\\s*(state|pid|active count)' | head -5`;
}
export function restartCoordCmd(os: ServiceOs): string {
  return os === "linux"
    ? `${XDG} systemctl --user daemon-reload && systemctl --user restart ${COORD_UNIT}`
    : `launchctl kickstart -k gui/$(id -u)/${COORD_AGENT}`;
}

export function verifyCoordCmd(os: ServiceOs): string {
  return os === "linux"
    ? `${XDG} systemctl --user is-active ${COORD_UNIT}`
    : `launchctl print gui/$(id -u)/${COORD_AGENT} 2>&1 | grep -E '^\\s*(state|pid|active count)' | head -5`;
}

export function stopServicesCmd(os: ServiceOs): string {
  return os === "linux"
    ? `${XDG} systemctl --user stop ${COORD_UNIT} ${WORKER_UNIT}`
    : `launchctl bootout gui/$(id -u)/${COORD_AGENT} 2>/dev/null; launchctl bootout gui/$(id -u)/${WORKER_AGENT} 2>/dev/null; true`;
}

// Every caller that needs a dialect for *this* box asks here rather than
// re-writing the same process.platform ternary.
export function currentServiceOs(): ServiceOs {
  return process.platform === "linux" ? "linux" : "darwin";
}
