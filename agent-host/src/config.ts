import { exit } from "node:process";

export interface HostConfig {
  bind: string;
  secret: string;
  dataDir: string;
  coordinatorInternalUrl: string;
}

export function readConfig(env: NodeJS.ProcessEnv = process.env): HostConfig {
  const secret = env.ROOST_AGENT_HOST_SECRET;
  const dataDir = env.ROOST_AGENT_HOST_DATA_DIR;
  const bind = env.ROOST_AGENT_HOST_BIND ?? "127.0.0.1:4115";
  const coordinatorInternalUrl = env.ROOST_COORDINATOR_INTERNAL_URL ?? "ws://127.0.0.1:4113";
  const errors: string[] = [];
  if (!secret || secret.length < 32) errors.push("ROOST_AGENT_HOST_SECRET must be at least 32 characters");
  if (!dataDir) errors.push("ROOST_AGENT_HOST_DATA_DIR is required");
  const bindMatch = bind.match(/^(?:\[[^\]]+\]|[^:]+):(\d+)$/);
  const port = bindMatch ? Number(bindMatch[1]) : Number.NaN;
  if (!bindMatch || !Number.isInteger(port) || port < 0 || port > 65535) errors.push("ROOST_AGENT_HOST_BIND must be a host:port address");
  try {
    const url = new URL(coordinatorInternalUrl);
    if (url.protocol !== "ws:" && url.protocol !== "wss:") errors.push("ROOST_COORDINATOR_INTERNAL_URL must use ws:// or wss://");
  } catch { errors.push("ROOST_COORDINATOR_INTERNAL_URL must be a valid WebSocket URL"); }
  if (errors.length) {
    process.stderr.write(`${JSON.stringify({ level: "error", message: errors.join("; ") })}\n`);
    exit(1);
  }
  return { bind, secret: secret!, dataDir: dataDir!, coordinatorInternalUrl };
}
