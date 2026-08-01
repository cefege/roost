// Platform-aware worker + coordinator paths. darwin keeps the historical
// ~/Library layout so existing Macs need no migration; linux uses XDG.
import { homedir } from "node:os";
import { join } from "node:path";

type Env = Record<string, string | undefined>;

export function workerDataDir(env: Env = process.env as Env): string {
  if (env.ROOST_WORKER_DATA_DIR) return env.ROOST_WORKER_DATA_DIR;
  if (process.platform === "darwin")
    return join(homedir(), "Library", "Application Support", "RoostWorkerV2");
  return join(env.XDG_DATA_HOME ?? join(homedir(), ".local", "share"), "RoostWorkerV2");
}

export function workerLogDir(env: Env = process.env as Env): string {
  if (env.ROOST_WORKER_LOG_DIR) return env.ROOST_WORKER_LOG_DIR;
  if (process.platform === "darwin")
    return join(homedir(), "Library", "Logs", "RoostWorker");
  return join(env.XDG_STATE_HOME ?? join(homedir(), ".local", "state"), "RoostWorker");
}
/** launchd label on darwin, systemd unit basename on linux. */
export const COORD_LABEL_DARWIN = "com.roost.coordinator-v2";
export const COORD_LABEL_LINUX = "roost-coord";
export const WORKER_LABEL_DARWIN = "com.roost.worker-v2";
export const WORKER_LABEL_LINUX = "roost-worker";

export function coordServiceLabel(env: Env = process.env as Env): string {
  return env.ROOST_COORD_LABEL ?? (process.platform === "darwin" ? COORD_LABEL_DARWIN : COORD_LABEL_LINUX);
}
export function workerServiceLabel(env: Env = process.env as Env): string {
  return env.ROOST_WORKER_AGENT_LABEL ?? (process.platform === "darwin" ? WORKER_LABEL_DARWIN : WORKER_LABEL_LINUX);
}
export function coordDataDir(env: Env = process.env as Env): string {
  if (env.ROOST_COORD_DATA_DIR) return env.ROOST_COORD_DATA_DIR;
  if (process.platform === "darwin")
    return join(homedir(), "Library", "Application Support", "RoostCoordinatorV2");
  return join(env.XDG_DATA_HOME ?? join(homedir(), ".local", "share"), "RoostCoordinatorV2");
}

export function coordLogDir(env: Env = process.env as Env): string {
  if (env.ROOST_COORD_LOG_DIR) return env.ROOST_COORD_LOG_DIR;
  if (process.platform === "darwin") return join(homedir(), "Library", "Logs", "RoostCoord");
  return join(env.XDG_STATE_HOME ?? join(homedir(), ".local", "state"), "RoostCoord");
}

/** launchd plist on darwin, systemd user unit on linux. */
export function coordServicePath(env: Env = process.env as Env): string {
  if (process.platform === "darwin")
    return env.ROOST_COORD_PLIST
      ?? join(homedir(), "Library", "LaunchAgents", `${env.ROOST_COORD_LABEL ?? "com.roost.coordinator-v2"}.plist`);
  return env.ROOST_COORD_UNIT
    ?? join(homedir(), ".config", "systemd", "user", `${env.ROOST_COORD_LABEL ?? "roost-coord"}.service`);
}

export function workerServicePath(env: Env = process.env as Env): string {
  if (process.platform === "darwin")
    return env.ROOST_WORKER_PLIST
      ?? join(homedir(), "Library", "LaunchAgents", `${workerServiceLabel(env)}.plist`);
  return env.ROOST_WORKER_UNIT
    ?? join(homedir(), ".config", "systemd", "user", `${workerServiceLabel(env)}.service`);
}
