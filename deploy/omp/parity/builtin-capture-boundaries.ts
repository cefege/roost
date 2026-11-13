import { promisify } from "node:util";
import { execFile } from "node:child_process";
import { realpath } from "node:fs/promises";
import type { MCPServerConfig } from "@omp-source/packages/coding-agent/src/mcp/types";
import type { CaptureState } from "./builtin-capture-state";
import { boundaryValue, objectValue, ownedPath, readBoundaryRows, requestedPointers } from "./builtin-capture-state";
import { probe } from "./probe";

export async function captureMcp(state: CaptureState): Promise<unknown> {
  const manager = probe.requireInteractiveContext().mcpManager;
  if (!manager) throw new Error("MCP capture requires the actual interactive MCPManager");
  await manager.waitForPendingConnections();
  const writer = await state.context.load<typeof import("@omp-source/packages/coding-agent/src/mcp/config-writer")>("packages/coding-agent/src/mcp/config-writer.ts");
  const utilities = await state.context.load<typeof import("@omp-source/packages/utils/src/index")>("packages/utils/src/index.ts");
  const credentials = await state.context.load<typeof import("@omp-source/packages/coding-agent/src/mcp/oauth-credentials")>("packages/coding-agent/src/mcp/oauth-credentials.ts");
  const project = await writer.readMCPConfigFile(await ownedPath(state, utilities.getMCPConfigPath("project", state.context.workspace)));
  const user = await writer.readMCPConfigFile(await ownedPath(state, utilities.getMCPConfigPath("user", state.context.workspace)));
  const configs: Record<string, MCPServerConfig> = { ...user.mcpServers, ...project.mcpServers };
  const names = new Set([...Object.keys(configs), ...manager.getConnectedServers(), ...requestedPointers(state, "builtin.mcp").map(parts => parts[0])]);
  const result: Record<string, unknown> = {};
  for (const name of names) {
    const present = Object.hasOwn(configs, name);
    const config = configs[name];
    const connection = manager.getConnection(name);
    const tools = manager.getTools().filter(tool => tool.mcpServerName === name).map(tool => {
      if (tool.mcpToolName) return tool.mcpToolName;
      const original = connection?.tools?.find(candidate => tool.name.endsWith(`_${candidate.name}`));
      if (!original) throw new Error(`Missing actual MCP tool namespace metadata: ${name}/${tool.name}`);
      return original.name;
    });
    result[name] = {
      status: manager.getConnectionStatus(name),
      config: { ...config, exists: present, ...(config ? { enabled: config.enabled !== false && !user.disabledServers?.includes(name) } : {}) },
      removed: !present,
      oauthCredentialPresent: config !== undefined && credentials.lookupMcpOAuthCredential(probe.requireSession().modelRegistry.authStorage, config) !== undefined,
      tools,
      resources: manager.getServerResources(name),
      prompts: manager.getServerPrompts(name),
    };
  }
  return result;
}

export async function captureHttp(state: CaptureState): Promise<unknown> {
  const rows = await readBoundaryRows(state, "OMP_PARITY_SERVICE_TRACE");
  const result: Record<string, unknown> = {};
  for (const row of rows) {
    if (row.channel === "builtin.http") return boundaryValue(state, "OMP_PARITY_SERVICE_TRACE", "builtin.http");
    if (row.channel !== "builtin.service") continue;
    const event = objectValue(row.value, "Actual service trace event");
    if (event.direction !== "request" || !["http", "websocket"].includes(String(event.transport))) continue;
    if (typeof event.service !== "string") throw new Error("Actual service trace lacks service name");
    const prior = Object.hasOwn(result, event.service) ? objectValue(result[event.service], "Service count") : undefined;
    result[event.service] = {
      ...prior,
      requests: Number(prior?.requests ?? 0) + 1,
      ...(typeof event.contentType === "string" && typeof event.bytes === "number"
        ? { bodyIsBinary: event.contentType.split(";")[0].trim().toLowerCase() === "application/octet-stream" && event.bytes > 0 }
        : {}),
    };
  }
  for (const parts of requestedPointers(state, "builtin.http")) {
    if (!Object.hasOwn(result, parts[0])) throw new Error(`Missing real HTTP request trace for service ${parts[0]}`);
    let observation: unknown = result;
    for (const part of parts) {
      const fields = objectValue(observation, `HTTP capture /${parts.join("/")}`);
      if (!Object.hasOwn(fields, part)) throw new Error(`Missing real HTTP trace field /${parts.join("/")}`);
      observation = fields[part];
    }
  }
  return result;
}

export async function captureGit(state: CaptureState): Promise<unknown> {
  const cwd = await ownedPath(state, probe.requireSession().sessionManager.getCwd());
  const execute = promisify(execFile);
  const options = { cwd, encoding: "utf8" as const, env: { ...process.env, GIT_OPTIONAL_LOCKS: "0" } };
  const staged = await execute("git", ["diff", "--cached", "--name-only", "-z"], options);
  const status = await execute("git", ["status", "--porcelain=v1", "-z"], options);
  const head = await execute("git", ["log", "-1", "--format=%s"], options);
  const branch = await execute("git", ["branch", "--show-current"], options);
  const root = await execute("git", ["rev-parse", "--show-toplevel"], options);
  return { stagedFiles: staged.stdout.split("\0").filter(Boolean), clean: status.stdout.length === 0, headSubject: head.stdout.trimEnd(), branch: branch.stdout.trimEnd(), root: await realpath(await ownedPath(state, root.stdout.trimEnd())) };
}
