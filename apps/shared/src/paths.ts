// Platform-aware worker paths. darwin keeps the historical ~/Library
// layout so existing Macs need no migration; linux uses XDG.
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
