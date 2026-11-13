import { appendFileSync } from "node:fs";
import { chmod, mkdir, open } from "node:fs/promises";
import { delimiter, join } from "node:path";
import { fileURLToPath } from "node:url";
import type { MCPStdioServerConfig } from "@omp-source/packages/coding-agent/src/mcp/types";
import type { BuiltinRequirements } from "./builtin-requirements";
import type { BuiltinSeedContext } from "./builtin-seed";
import { serviceTrace, startDenyProxy, startHttpService, startRefusingRelay } from "./fixture-service-http";
import type { McpExecutableConfiguration, McpServiceInput } from "./fixture-service-mcp";
import { startSkillRegistry, type SkillRegistryInput } from "./fixture-service-skills";
import { startLiveFixtureService, type LiveFixtureInput } from "./live-fixture-service";
import { prepareControlledLiveAudio, seedControlledLiveAuth } from "./live-controlled-audio";
import { MCP_OAUTH_AUTHORIZATION_ORIGIN, startMcpHttpService, type McpHttpServiceInput } from "./fixture-service-mcp-http";
import { SMITHERY_APPROVAL_ORIGIN, startSmitheryService, type SmitheryServiceInput } from "./fixture-service-smithery";
import { startOwnedOAuthFollower, type OwnedOAuthFollowRule } from "./fixture-service-oauth-follow";
import { parseMemoryHttpServiceInput, startBuiltinMemoryHttpService, type MemoryHttpServiceInput } from "./builtin-memory-http-service";
import type { AuthWorkflowHook } from "./builtin-auth-workflow-requirements";
import { startBuiltinAuthService } from "./fixture-service-auth";
import { startCanonicalCollabRelay, type CollabRelayServiceInput } from "./fixture-service-collab";
import type { SecurityWorkflowHook } from "./builtin-security-workflow-requirements";

export type BuiltinServiceSpec =
  | { kind: "skill-registry"; behavior: string; input: SkillRegistryInput }
  | { kind: "share"; behavior: string; input: { status: number; body: string } }
  | { kind: "collab-relay"; behavior: string; input: CollabRelayServiceInput }
  | { kind: "mcp-stdio"; behavior: string; input: McpServiceInput }
  | { kind: "smithery"; behavior: string; input: SmitheryServiceInput }
  | { kind: "mcp-http"; behavior: string; input: McpHttpServiceInput }
  | { kind: "live"; behavior: string; input: LiveFixtureInput }
  | { kind: "hindsight"; behavior: string; input: MemoryHttpServiceInput };

export interface RunningBuiltinServices {
  environment: Record<string, string>;
  settings: Record<string, unknown>;
  tracePath: string;
  dispose(): Promise<void>;
}

export function serviceSpecsForRequirements(requirement: BuiltinRequirements): BuiltinServiceSpec[] {
  return requirement.services.map(service => {
    const { input, behavior, kind } = service;
    switch (kind) {
      case "skill-registry": return { kind, behavior, input: { scope: stringInput(input, "scope"), name: stringInput(input, "name"), version: stringInput(input, "version"), body: stringInput(input, "body") } };
      case "mcp-stdio": return { kind, behavior, input: { name: stringInput(input, "name"), version: stringInput(input, "version"), tool: stringInput(input, "tool"), resource: stringInput(input, "resource"), prompt: stringInput(input, "prompt") } };
      case "share": return { kind, behavior, input: { status: numberInput(input, "status"), body: stringInput(input, "body") } };
      case "collab-relay": {
        if (input.mode === "canonical") return { kind, behavior, input: { mode: "canonical" } };
        if (input.mode !== undefined && input.mode !== "refusal") throw new Error("Parity collaboration relay mode must be canonical or refusal");
        return { kind, behavior, input: { mode: "refusal", closeCode: numberInput(input, "closeCode"), reason: stringInput(input, "reason") } };
      }
      case "smithery": {
        if (input.mode === undefined || input.mode === "failure") return { kind, behavior, input: { mode: "failure", status: numberInput(input, "status") } };
        if (input.mode !== "success") throw new Error(`Unsupported owned Smithery fixture mode ${String(input.mode)}`);
        if (input.browserLogin !== undefined && input.browserLogin !== "complete" && input.browserLogin !== "failure") throw new Error("Parity Smithery browserLogin must be complete or failure");
        return { kind, behavior, input: { mode: "success", apiKey: stringInput(input, "apiKey"), scope: stringInput(input, "scope"), name: stringInput(input, "name"), displayName: stringInput(input, "displayName"), ...(input.browserLogin === undefined ? {} : { browserLogin: input.browserLogin }) } };
      }
      case "mcp-http": {
        if (typeof input.oauth !== "boolean") throw new Error("Parity MCP HTTP oauth input must be a boolean");
        if (input.authorization !== undefined && input.authorization !== "complete" && input.authorization !== "hold") throw new Error("Parity MCP HTTP authorization must be complete or hold");
        return { kind, behavior, input: { name: stringInput(input, "name"), version: stringInput(input, "version"), tool: stringInput(input, "tool"), resource: stringInput(input, "resource"), prompt: stringInput(input, "prompt"), oauth: input.oauth, ...(input.authorization === undefined ? {} : { authorization: input.authorization }) } };
      }
      case "live": {
        const mode = stringInput(input, "mode");
        if (mode !== "conversation" && mode !== "failure") throw new Error(`Unsupported owned live fixture mode ${mode}`);
        return { kind, behavior, input: { mode, userTranscript: stringInput(input, "userTranscript"), assistantTranscript: stringInput(input, "assistantTranscript"), failureMessage: stringInput(input, "failureMessage") } };
      }
      case "hindsight": return { kind, behavior, input: parseMemoryHttpServiceInput(input) };
      default: throw new Error(`Unsupported parity service kind: ${String(kind)}`);
    }
  });
}

export function builtinMcpConfiguration(): MCPStdioServerConfig {
  return { type: "stdio", command: "parity-mcp", args: [] };
}

export async function prepareBuiltinServiceFiles(specs: readonly BuiltinServiceSpec[], context: BuiltinSeedContext): Promise<Record<string, string>> {
  const directory = join(context.home, ".parity");
  await mkdir(directory, { recursive: true });
  const tracePath = join(directory, "service-trace.jsonl");
  const traceFile = await open(tracePath, "a");
  await traceFile.close();
  const environment: Record<string, string> = { OMP_PARITY_SERVICE_TRACE: tracePath };
  const mcp = specs.filter(spec => spec.kind === "mcp-stdio");
  if (mcp.length > 1) throw new Error("Only one parity-mcp executable may belong to an isolated leg");
  const specification = mcp[0];
  if (specification) {
    const production = await context.load<typeof import("@omp-source/packages/coding-agent/src/mcp/types")>("packages/coding-agent/src/mcp/types.ts");
    if (!production.MCP_PROTOCOL_VERSION) throw new Error("Production MCP initialization protocol is unavailable");
    const bin = join(directory, "bin");
    await mkdir(bin, { recursive: true });
    const configPath = join(directory, "mcp-service.json");
    const configuration: McpExecutableConfiguration = { input: specification.input, protocolVersion: production.MCP_PROTOCOL_VERSION, tracePath };
    const configContent = `${JSON.stringify(configuration)}\n`;
    if (await Bun.file(configPath).exists()) {
      if (await Bun.file(configPath).text() !== configContent) throw new Error("Owned MCP fixture configuration changed between seed and startup");
    } else await Bun.write(configPath, configContent);
    const executable = join(bin, "parity-mcp");
    const quote = (value: string) => `'${value.replaceAll("'", "'\\''")}'`;
    const script = `#!/bin/sh\nexec ${quote(process.execPath)} ${quote(fileURLToPath(new URL("./fixture-service-mcp.ts", import.meta.url)))} ${quote(configPath)}\n`;
    if (await Bun.file(executable).exists()) {
      if (await Bun.file(executable).text() !== script) throw new Error("Owned parity-mcp executable changed between seed and startup");
    } else {
      await Bun.write(executable, script);
      await chmod(executable, 0o700);
    }
    environment.PATH = [bin, process.env.PATH ?? ""].filter(Boolean).join(delimiter);
  }
  const live = specs.filter(spec => spec.kind === "live");
  if (live.length > 1) throw new Error("Only one controlled live service may belong to an isolated leg");
  if (live.length === 1) {
    Object.assign(environment, await prepareControlledLiveAudio(context));
    if (process.env.HOME === context.home) await seedControlledLiveAuth(context);
  }
  return environment;
}

export async function startBuiltinServices(specs: readonly BuiltinServiceSpec[], context: BuiltinSeedContext, authHook?: AuthWorkflowHook, securityHook?: SecurityWorkflowHook): Promise<RunningBuiltinServices> {
  const environment = await prepareBuiltinServiceFiles(specs, context);
  const tracePath = environment.OMP_PARITY_SERVICE_TRACE;
  if (!tracePath) throw new Error("Parity service trace path was not prepared");
  const trace = serviceTrace(tracePath);
  const settings: Record<string, unknown> = {};
  const services: Array<{ dispose(): Promise<void> }> = [];
  const origins: string[] = [];
  const followRules: OwnedOAuthFollowRule[] = [];
  let disposed = false;
  const dispose = async () => {
    if (disposed) return;
    disposed = true;
    const outcomes = await Promise.allSettled(services.toReversed().map(service => service.dispose()));
    const errors = outcomes.flatMap(outcome => outcome.status === "rejected" ? [outcome.reason] : []);
    if (errors.length) throw new AggregateError(errors, "Failed to dispose parity services");
  };
  try {
    const deny = await startDenyProxy(trace);
    services.push(deny);
    origins.push(deny.url);
    Object.assign(environment, { HTTP_PROXY: deny.url, HTTPS_PROXY: deny.url, ALL_PROXY: deny.url, http_proxy: deny.url, https_proxy: deny.url, all_proxy: deny.url, NO_PROXY: "127.0.0.1,localhost,::1", no_proxy: "127.0.0.1,localhost,::1", PI_PROXY: deny.url });
    const kinds = new Set<string>();
    for (const specification of specs) {
      if (kinds.has(specification.kind)) throw new Error(`Duplicate parity service ${specification.kind}`);
      kinds.add(specification.kind);
      switch (specification.kind) {
        case "mcp-stdio": break;
        case "skill-registry": {
          const registry = await startSkillRegistry(specification.input, context, trace);
          services.push(registry.service);
          origins.push(registry.service.url);
          Object.assign(settings, registry.settings);
          break;
        }
        case "share": {
          const configuration = await context.load<typeof import("@omp-source/packages/coding-agent/src/commands/settings")>("packages/coding-agent/src/commands/settings.ts");
          if (!configuration.cfgShareServerUrl?.id || !configuration.cfgShareStore?.id) throw new Error("Production has no configurable share upload route");
          const service = startHttpService("share", trace, request => request.method === "POST" && new URL(request.url).pathname === "/" ? new Response(specification.input.body, { status: specification.input.status }) : new Response("Unknown share boundary route", { status: 404 }));
          services.push(service);
          origins.push(service.url);
          settings[configuration.cfgShareServerUrl.id] = service.url;
          settings[configuration.cfgShareStore.id] = "blob";
          break;
        }
        case "collab-relay": {
          const service = specification.input.mode === "canonical" ? await startCanonicalCollabRelay(context, trace) : startRefusingRelay(specification.input, trace);
          services.push(service);
          origins.push(service.url);
          environment.OMP_PARITY_RELAY_PORT = String(service.port);
          environment.OMP_PARITY_RELAY_URL = service.url;
          const configuration = await context.load<typeof import("@omp-source/packages/coding-agent/src/collab/settings")>("packages/coding-agent/src/collab/settings.ts");
          if (!configuration.cfgCollabRelayUrl?.id || !configuration.cfgCollabWebUrl?.id) throw new Error("Production collaboration routing settings are unavailable");
          settings[configuration.cfgCollabRelayUrl.id] = service.url;
          settings[configuration.cfgCollabWebUrl.id] = "https://parity-collab.invalid";
          break;
        }
        case "smithery": {
          const smithery = await startSmitheryService(specification.input, context, trace);
          services.push(smithery.service);
          origins.push(...(smithery.environment.OMP_PARITY_ALLOWED_ORIGINS ?? smithery.service.url).split(","));
          Object.assign(environment, smithery.environment);
          if (specification.input.mode === "success" && specification.input.browserLogin !== "failure") followRules.push({ origin: SMITHERY_APPROVAL_ORIGIN, targetOrigin: smithery.service.url, path: "/approve" });
          break;
        }
        case "mcp-http": {
          const mcp = await startMcpHttpService(specification.input, context, trace);
          services.push(mcp.service);
          origins.push(mcp.service.url);
          Object.assign(environment, mcp.environment);
          const directories = await context.load<typeof import("@omp-source/packages/utils/src/dirs")>("packages/utils/src/dirs.ts");
          const configuration = await context.load<typeof import("@omp-source/packages/coding-agent/src/mcp/config-writer")>("packages/coding-agent/src/mcp/config-writer.ts");
          await configuration.addMCPServer(directories.getMCPConfigPath("project", context.workspace), specification.input.name, mcp.config);
          if (specification.input.oauth) followRules.push({ origin: MCP_OAUTH_AUTHORIZATION_ORIGIN, targetOrigin: mcp.service.url, path: "/authorize" });
          break;
        }
        case "live": {
          const live = await startLiveFixtureService(specification.input, context, trace);
          services.push(live.service);
          origins.push(live.service.url);
          Object.assign(environment, live.environment);
          break;
        }
        case "hindsight": {
          const configuration = await context.load<typeof import("@omp-source/packages/coding-agent/src/hindsight/settings")>("packages/coding-agent/src/hindsight/settings.ts");
          if (!configuration.cfgHindsightApiUrl?.id || !configuration.cfgHindsightApiToken?.id) throw new Error("Production has no configurable Hindsight HTTP route");
          const memory = await startBuiltinMemoryHttpService(specification.input, snapshot => {
            appendFileSync(tracePath, `${JSON.stringify({ channel: "builtin.memory-http", value: snapshot })}\n`);
          });
          services.push({ dispose: () => memory.stop() });
          origins.push(memory.url);
          settings[configuration.cfgHindsightApiUrl.id] = memory.url;
          settings[configuration.cfgHindsightApiToken.id] = specification.input.apiToken;
          environment.HINDSIGHT_API_URL = memory.url;
          environment.HINDSIGHT_API_TOKEN = specification.input.apiToken;
          break;
        }
        default: throw new Error("Unsupported parity service specification");
      }
    }
    if (authHook?.service && securityHook) throw new Error("A parity leg must declare exactly one auth service profile");
    const authProfile = authHook?.service ? authHook : securityHook;
    if (authProfile) {
      const auth = await startBuiltinAuthService(authProfile, context, trace);
      services.push(auth);
      origins.push(auth.url);
      Object.assign(environment, auth.environment);
    }
    if (followRules.length) services.push(startOwnedOAuthFollower(context, followRules, trace));
    environment.OMP_PARITY_ALLOWED_ORIGINS = origins.join(",");
    return { environment, settings, tracePath, dispose };
  } catch (error) {
    try { await dispose(); }
    catch (cleanupError) { throw new AggregateError([error, cleanupError], "Parity service startup and cleanup failed"); }
    throw error;
  }
}

function stringInput(input: Record<string, unknown>, key: string): string {
  const value = input[key];
  if (typeof value !== "string") throw new Error(`Parity service input ${key} must be a string`);
  return value;
}

function numberInput(input: Record<string, unknown>, key: string): number {
  const value = input[key];
  if (typeof value !== "number" || !Number.isInteger(value)) throw new Error(`Parity service input ${key} must be an integer`);
  return value;
}
