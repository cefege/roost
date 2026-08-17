// Platform-aware worker, coordinator, version, and service paths.
import { homedir } from "node:os";
import { join, win32 } from "node:path";
import {
  assertNeverPlatform,
  supportedHostPlatform,
  type SupportedHostPlatform,
} from "./platform.ts";

export type PathEnv = Record<string, string | undefined>;

export const COORD_LABEL_DARWIN = "com.roost.coordinator-v2";
export const COORD_LABEL_LINUX = "roost-coord";
export const COORD_SERVICE_WINDOWS = "RoostCoordinatorV2";
export const WORKER_LABEL_DARWIN = "com.roost.worker-v2";
export const WORKER_LABEL_LINUX = "roost-worker";
export const WORKER_SERVICE_WINDOWS = "RoostWorkerV2";
export const KEEPER_SERVICE_WINDOWS = "RoostKeeperV2";
export const UPDATER_SERVICE_WINDOWS = "RoostUpdaterV2";

function windowsLocalAppData(env: PathEnv): string {
  const root = env.LOCALAPPDATA
    ?? (env.USERPROFILE ? win32.join(env.USERPROFILE, "AppData", "Local") : undefined);
  if (!root) throw new Error("LOCALAPPDATA or USERPROFILE is required on Windows");
  return root;
}

function windowsProgramData(env: PathEnv): string {
  const root = env.ProgramData ?? env.PROGRAMDATA;
  if (!root) throw new Error("ProgramData is required on Windows");
  return root;
}

export function workerDataDir(
  env: PathEnv = process.env as PathEnv,
  platform: SupportedHostPlatform = supportedHostPlatform(),
): string {
  if (env.ROOST_WORKER_DATA_DIR) return env.ROOST_WORKER_DATA_DIR;
  switch (platform) {
    case "darwin": return join(homedir(), "Library", "Application Support", "RoostWorkerV2");
    case "linux": return join(env.XDG_DATA_HOME ?? join(homedir(), ".local", "share"), "RoostWorkerV2");
    case "win32": return win32.join(windowsLocalAppData(env), "Roost", "WorkerV2");
    default: return assertNeverPlatform(platform);
  }
}

export function workerLogDir(
  env: PathEnv = process.env as PathEnv,
  platform: SupportedHostPlatform = supportedHostPlatform(),
): string {
  if (env.ROOST_WORKER_LOG_DIR) return env.ROOST_WORKER_LOG_DIR;
  switch (platform) {
    case "darwin": return join(homedir(), "Library", "Logs", "RoostWorker");
    case "linux": return join(env.XDG_STATE_HOME ?? join(homedir(), ".local", "state"), "RoostWorker");
    case "win32": return win32.join(windowsLocalAppData(env), "Roost", "Logs", "Worker");
    default: return assertNeverPlatform(platform);
  }
}

export function coordDataDir(
  env: PathEnv = process.env as PathEnv,
  platform: SupportedHostPlatform = supportedHostPlatform(),
): string {
  if (env.ROOST_COORD_DATA_DIR) return env.ROOST_COORD_DATA_DIR;
  switch (platform) {
    case "darwin": return join(homedir(), "Library", "Application Support", "RoostCoordinatorV2");
    case "linux": return join(env.XDG_DATA_HOME ?? join(homedir(), ".local", "share"), "RoostCoordinatorV2");
    case "win32": return win32.join(windowsLocalAppData(env), "Roost", "CoordinatorV2");
    default: return assertNeverPlatform(platform);
  }
}

export function coordLogDir(
  env: PathEnv = process.env as PathEnv,
  platform: SupportedHostPlatform = supportedHostPlatform(),
): string {
  if (env.ROOST_COORD_LOG_DIR) return env.ROOST_COORD_LOG_DIR;
  switch (platform) {
    case "darwin": return join(homedir(), "Library", "Logs", "RoostCoord");
    case "linux": return join(env.XDG_STATE_HOME ?? join(homedir(), ".local", "state"), "RoostCoord");
    case "win32": return win32.join(windowsLocalAppData(env), "Roost", "Logs", "Coordinator");
    default: return assertNeverPlatform(platform);
  }
}

export function roostServiceDir(
  env: PathEnv = process.env as PathEnv,
  platform: SupportedHostPlatform = supportedHostPlatform(),
): string {
  if (env.ROOST_SERVICE_DIR) return env.ROOST_SERVICE_DIR;
  switch (platform) {
    case "darwin":
    case "linux": return join(workerDataDir(env, platform), "service");
    case "win32": return win32.join(windowsProgramData(env), "Roost", "service");
    default: return assertNeverPlatform(platform);
  }
}

export function roostVersionsDir(
  env: PathEnv = process.env as PathEnv,
  platform: SupportedHostPlatform = supportedHostPlatform(),
): string {
  if (env.ROOST_VERSIONS_DIR) return env.ROOST_VERSIONS_DIR;
  switch (platform) {
    case "darwin":
    case "linux": return join(workerDataDir(env, platform), "versions");
    case "win32": return win32.join(windowsProgramData(env), "Roost", "versions");
    default: return assertNeverPlatform(platform);
  }
}

export function windowsVersionedBinaryPath(
  version: string,
  env: PathEnv = process.env as PathEnv,
): string {
  if (!version || /[\\/\0]/.test(version)) throw new Error("invalid Roost version");
  return win32.join(roostVersionsDir(env, "win32"), version, "roost.exe");
}

export function coordServiceLabel(
  env: PathEnv = process.env as PathEnv,
  platform: SupportedHostPlatform = supportedHostPlatform(),
): string {
  if (env.ROOST_COORD_LABEL) return env.ROOST_COORD_LABEL;
  switch (platform) {
    case "darwin": return COORD_LABEL_DARWIN;
    case "linux": return COORD_LABEL_LINUX;
    case "win32": return COORD_SERVICE_WINDOWS;
    default: return assertNeverPlatform(platform);
  }
}

export function workerServiceLabel(
  env: PathEnv = process.env as PathEnv,
  platform: SupportedHostPlatform = supportedHostPlatform(),
): string {
  if (env.ROOST_WORKER_AGENT_LABEL) return env.ROOST_WORKER_AGENT_LABEL;
  switch (platform) {
    case "darwin": return WORKER_LABEL_DARWIN;
    case "linux": return WORKER_LABEL_LINUX;
    case "win32": return WORKER_SERVICE_WINDOWS;
    default: return assertNeverPlatform(platform);
  }
}

export function coordServicePath(
  env: PathEnv = process.env as PathEnv,
  platform: SupportedHostPlatform = supportedHostPlatform(),
): string {
  switch (platform) {
    case "darwin":
      return env.ROOST_COORD_PLIST
        ?? join(homedir(), "Library", "LaunchAgents", `${env.ROOST_COORD_LABEL ?? COORD_LABEL_DARWIN}.plist`);
    case "linux":
      return env.ROOST_COORD_UNIT
        ?? join(homedir(), ".config", "systemd", "user", `${env.ROOST_COORD_LABEL ?? COORD_LABEL_LINUX}.service`);
    case "win32":
      return env.ROOST_COORD_SERVICE_CONFIG
        ?? win32.join(roostServiceDir(env, platform), "coordinator.json");
    default:
      return assertNeverPlatform(platform);
  }
}

export function workerServicePath(
  env: PathEnv = process.env as PathEnv,
  platform: SupportedHostPlatform = supportedHostPlatform(),
): string {
  switch (platform) {
    case "darwin":
      return env.ROOST_WORKER_PLIST
        ?? join(homedir(), "Library", "LaunchAgents", `${workerServiceLabel(env, platform)}.plist`);
    case "linux":
      return env.ROOST_WORKER_UNIT
        ?? join(homedir(), ".config", "systemd", "user", `${workerServiceLabel(env, platform)}.service`);
    case "win32":
      return env.ROOST_WORKER_SERVICE_CONFIG
        ?? win32.join(roostServiceDir(env, platform), "worker.json");
    default:
      return assertNeverPlatform(platform);
  }
}
