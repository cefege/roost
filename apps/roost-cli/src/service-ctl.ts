// Single source of truth for the two worker-service dialects: launchd on
// macOS, systemd --user on Linux. Callers (deploy-local, deploy-linux,
// status) hand the resulting string to `bash -c` locally or over ssh, so
// the $(id -u) / XDG_RUNTIME_DIR expansion happens on the target box.

export type ServiceOs = "darwin" | "linux";

export const WORKER_UNIT = "roost-worker.service"; // linux
export const WORKER_AGENT = "com.roost.worker-v2"; // darwin

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
    : `launchctl print gui/$(id -u)/${WORKER_AGENT} 2>&1 | grep -E '^\\s*(state|pid|active count)' | head -5`;
}
