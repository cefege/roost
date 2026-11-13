import { appendFileSync } from "node:fs";
import { createInterface } from "node:readline";
import type { JsonRpcNotification, JsonRpcRequest, JsonRpcResponse, MCPGetPromptResult, MCPInitializeResult, MCPPromptsListResult, MCPResourceReadResult, MCPResourcesListResult, MCPResourceTemplatesListResult, MCPToolCallResult, MCPToolsListResult } from "@omp-source/packages/coding-agent/src/mcp/types";

export interface McpServiceInput {
  name: string;
  version: string;
  tool: string;
  resource: string;
  prompt: string;
}

export interface McpExecutableConfiguration {
  input: McpServiceInput;
  protocolVersion: string;
  tracePath: string;
}

export async function serveMcpStdio(configuration: McpExecutableConfiguration): Promise<void> {
  const { input, tracePath } = configuration;
  const trace = (direction: string, value: unknown) => appendFileSync(tracePath, `${JSON.stringify({ channel: "builtin.service", value: { service: "mcp-stdio", transport: "stdio", direction, pid: process.pid, message: value } })}\n`);
  const send = (message: JsonRpcResponse | JsonRpcNotification) => {
    process.stdout.write(`${JSON.stringify(message)}\n`);
    trace("response", message);
  };
  let initialized = false;
  const subscriptions = new Set<string>();
  const lines = createInterface({ input: process.stdin, crlfDelay: Infinity });
  trace("start", { argv: process.argv });
  process.once("exit", code => trace("exit", { code }));
  for await (const line of lines) {
    let decoded: unknown;
    try { decoded = JSON.parse(line); }
    catch { trace("invalid-request", line); continue; }
    if (typeof decoded !== "object" || decoded === null || !("method" in decoded) || typeof decoded.method !== "string") {
      trace("invalid-request", decoded);
      continue;
    }
    const id = "id" in decoded && (typeof decoded.id === "string" || typeof decoded.id === "number") ? decoded.id : undefined;
    const params = "params" in decoded && typeof decoded.params === "object" && decoded.params !== null && !Array.isArray(decoded.params) ? decoded.params as Record<string, unknown> : undefined;
    const request: JsonRpcRequest | JsonRpcNotification = id === undefined ? { jsonrpc: "2.0", method: decoded.method, params } : { jsonrpc: "2.0", id, method: decoded.method, params };
    trace("request", request);
    if (request.method === "notifications/initialized") { initialized = true; continue; }
    if (!("id" in request)) continue;
    if (request.method !== "initialize" && request.method !== "ping" && !initialized) {
      send({ jsonrpc: "2.0", id: request.id, error: { code: -32002, message: "Server not initialized" } });
      continue;
    }
    let result: unknown;
    switch (request.method) {
      case "initialize": {
        const response: MCPInitializeResult = { protocolVersion: configuration.protocolVersion, capabilities: { tools: { listChanged: true }, resources: { subscribe: true, listChanged: true }, prompts: { listChanged: true } }, serverInfo: { name: input.name, version: input.version } };
        result = response;
        break;
      }
      case "ping": result = {}; break;
      case "tools/list": {
        const response: MCPToolsListResult = { tools: [{ name: input.tool, description: "Echo the supplied text", inputSchema: { type: "object", properties: { text: { type: "string" } }, required: ["text"] } }] };
        result = response;
        break;
      }
      case "tools/call": {
        const argumentsValue = request.params?.arguments;
        if (request.params?.name !== input.tool || typeof argumentsValue !== "object" || argumentsValue === null || !("text" in argumentsValue) || typeof argumentsValue.text !== "string") {
          send({ jsonrpc: "2.0", id: request.id, error: { code: -32602, message: "Expected the configured echo tool with string text" } });
          continue;
        }
        const response: MCPToolCallResult = { content: [{ type: "text", text: argumentsValue.text }] };
        result = response;
        break;
      }
      case "resources/list": {
        const response: MCPResourcesListResult = { resources: [{ uri: input.resource, name: "Parity resource", title: "Parity resource", mimeType: "text/plain" }] };
        result = response;
        break;
      }
      case "resources/templates/list": {
        const response: MCPResourceTemplatesListResult = { resourceTemplates: [] };
        result = response;
        break;
      }
      case "resources/read": {
        if (request.params?.uri !== input.resource) { send({ jsonrpc: "2.0", id: request.id, error: { code: -32602, message: "Unknown resource" } }); continue; }
        const response: MCPResourceReadResult = { contents: [{ uri: input.resource, mimeType: "text/plain", text: "Parity resource\n" }] };
        result = response;
        break;
      }
      case "resources/subscribe":
      case "resources/unsubscribe": {
        if (request.params?.uri !== input.resource) { send({ jsonrpc: "2.0", id: request.id, error: { code: -32602, message: "Unknown resource" } }); continue; }
        if (request.method === "resources/subscribe") subscriptions.add(input.resource);
        else subscriptions.delete(input.resource);
        trace("subscriptions", [...subscriptions]);
        result = {};
        break;
      }
      case "prompts/list": {
        const response: MCPPromptsListResult = { prompts: [{ name: input.prompt, title: "Parity prompt", description: "Parity prompt" }] };
        result = response;
        break;
      }
      case "prompts/get": {
        if (request.params?.name !== input.prompt) { send({ jsonrpc: "2.0", id: request.id, error: { code: -32602, message: "Unknown prompt" } }); continue; }
        const response: MCPGetPromptResult = { description: "Parity prompt", messages: [{ role: "user", content: { type: "text", text: "Parity prompt" } }] };
        result = response;
        break;
      }
      default:
        send({ jsonrpc: "2.0", id: request.id, error: { code: -32601, message: `Method not found: ${request.method}` } });
        continue;
    }
    send({ jsonrpc: "2.0", id: request.id, result });
  }
}

if (import.meta.main) {
  const configPath = process.argv[2];
  if (!configPath) throw new Error("parity-mcp requires its owned configuration file");
  const configuration: McpExecutableConfiguration = await Bun.file(configPath).json();
  await serveMcpStdio(configuration);
}
