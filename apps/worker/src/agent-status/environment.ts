// Provisions the local environment an installed integration needs to talk to
// this worker: a capability-checked local endpoint plus the ROOST_* env block
// (endpoint path, HMAC'd capability token, session id) that gets baked into
// every agent config. The HMAC is what stops other local users from spoofing
// reports onto our socket.
import { createHmac } from "node:crypto";
import { homedir } from "node:os";
import { isAbsolute, join } from "node:path";
import {
  resolveLocalEndpoint,
  verifyLocalEndpointCapability,
  type LocalEndpoint,
} from "@roost/shared/local-endpoint";
import {
  assertNeverPlatform,
  supportedHostPlatform,
  type SupportedHostPlatform,
} from "@roost/shared/platform";
import { workerDataDir } from "@roost/shared/paths";

const sessionCapabilities = new Map<string, string>();

export function resolveAgentReportEndpoint(
  env: NodeJS.ProcessEnv = process.env,
  home = homedir(),
  platform: SupportedHostPlatform = supportedHostPlatform(),
): LocalEndpoint {
  const dataDir = platform === "win32" ? workerDataDir(env, platform) : join(home, ".roost");
  const endpoint = resolveLocalEndpoint({
    name: "agent-report",
    dataDir,
    platform,
    env,
  });
  const configured = env.ROOST_AGENT_ENDPOINT
    ?? (platform === "win32" ? undefined : env.ROOST_AGENT_SOCKET_PATH);
  if (!configured) return endpoint;
  switch (platform) {
    case "darwin":
    case "linux":
      if (!isAbsolute(configured)) throw new Error("ROOST_AGENT_ENDPOINT must be an absolute UDS path");
      return { ...endpoint, address: configured, isFilesystemPath: true, kind: "uds" };
    case "win32":
      if (!configured.startsWith("\\\\.\\pipe\\")) {
        throw new Error("ROOST_AGENT_ENDPOINT must be a Windows named-pipe path");
      }
      return { ...endpoint, address: configured, isFilesystemPath: false, kind: "named-pipe" };
    default:
      return assertNeverPlatform(platform);
  }
}

function capabilityForSession(
  sessionId: string,
  endpoint: LocalEndpoint,
): string {
  const cacheKey = `${endpoint.capability}\0${sessionId}`;
  let capability = sessionCapabilities.get(cacheKey);
  if (capability) return capability;
  // HMAC output is a distinct pseudorandom capability per session while
  // remaining stable across worker restarts for keeper-surviving agents.
  capability = createHmac("sha256", endpoint.capability)
    .update("roost-agent-report-session\0")
    .update(sessionId)
    .digest("hex");
  sessionCapabilities.set(cacheKey, capability);
  return capability;
}

export function verifyAgentReportCapability(
  endpoint: LocalEndpoint,
  sessionId: string,
  received: unknown,
): boolean {
  return verifyLocalEndpointCapability(
    capabilityForSession(sessionId, endpoint),
    received,
  );
}

/** Evict cached capabilities for a closed session. Keys embed the session id
 *  (`${capability}\0${sessionId}`), and respawns mint fresh ids forever, so a
 *  closed session's entries can never be hit again — unbounded-map growth
 *  without eviction. Returns how many entries were dropped (diag/test oracle). */
export function releaseAgentStatusCapabilities(sessionId: string): number {
  const suffix = `\0${sessionId}`;
  let dropped = 0;
  for (const key of sessionCapabilities.keys()) {
    if (key.endsWith(suffix)) {
      sessionCapabilities.delete(key);
      dropped++;
    }
  }
  return dropped;
}

export function withAgentStatusEnvironment(
  base: Record<string, string>,
  sessionId: string,
  env: NodeJS.ProcessEnv = process.env,
): Record<string, string> {
  const platform = supportedHostPlatform();
  const endpoint = resolveAgentReportEndpoint(env, homedir(), platform);
  const overlay: Record<string, string> = {
    ROOST_AGENT_ENDPOINT: endpoint.address,
    ROOST_AGENT_ENDPOINT_KIND: endpoint.kind,
    ROOST_AGENT_CAPABILITY: capabilityForSession(sessionId, endpoint),
    ROOST_SESSION_ID: sessionId,
  };
  switch (platform) {
    case "darwin":
    case "linux":
      return {
        ...base,
        ...overlay,
        // Keep the documented POSIX name available to shells and lightweight
        // clients while ROOST_AGENT_ENDPOINT remains the cross-platform name.
        ROOST_AGENT_SOCKET_PATH: endpoint.address,
      };
    case "win32": {
      // Windows environment names are case-insensitive. Replace an existing
      // differently-cased key instead of emitting an ambiguous duplicate.
      const merged = { ...base };
      const keyByFolded = new Map(
        Object.keys(merged).map((key) => [key.toLocaleLowerCase("en-US"), key]),
      );
      for (const [key, value] of Object.entries(overlay)) {
        const oldKey = keyByFolded.get(key.toLocaleLowerCase("en-US"));
        if (oldKey && oldKey !== key) delete merged[oldKey];
        merged[key] = value;
      }
      return merged;
    }
    default:
      return assertNeverPlatform(platform);
  }
}
