import { createHash, randomBytes } from "node:crypto";
import type { JsonRpcResponse, MCPGetPromptResult, MCPHttpServerConfig, MCPInitializeResult, MCPPromptsListResult, MCPResourceReadResult, MCPResourcesListResult, MCPToolCallResult, MCPToolsListResult } from "@omp-source/packages/coding-agent/src/mcp/types";
import type { BuiltinSeedContext } from "./builtin-seed";
import { startHttpService, type LocalHttpService, type ServiceTrace } from "./fixture-service-http";
import type { McpServiceInput } from "./fixture-service-mcp";

export interface McpHttpServiceInput extends McpServiceInput {
  oauth: boolean;
  authorization?: "complete" | "hold";
}

export interface RunningMcpHttpService {
  service: LocalHttpService;
  environment: Record<string, string>;
  config: MCPHttpServerConfig;
}

export const MCP_OAUTH_AUTHORIZATION_ORIGIN = "https://parity-mcp.invalid";

export async function startMcpHttpService(input: McpHttpServiceInput, context: BuiltinSeedContext, trace: ServiceTrace): Promise<RunningMcpHttpService> {
  const production = await context.load<typeof import("@omp-source/packages/coding-agent/src/mcp/types")>("packages/coding-agent/src/mcp/types.ts");
  if (!production.MCP_PROTOCOL_VERSION) throw new Error("Production MCP HTTP protocol is unavailable");
  const clients = new Map<string, string[]>();
  const codes = new Map<string, { clientId: string; redirectUri: string; challenge: string }>();
  const accessTokens = new Set<string>();
  const refreshTokens = new Set<string>();
  const sessions = new Map<string, { initialized: boolean; subscriptions: Set<string> }>();
  let origin: string;
  const service = startHttpService("mcp-http", trace, async request => {
    const url = new URL(request.url);
    if (request.method === "GET" && url.pathname === "/.well-known/oauth-protected-resource/mcp") {
      return Response.json({ resource: `${origin}/mcp`, authorization_servers: [origin], scopes_supported: ["mcp:tools"] });
    }
    if (request.method === "GET" && (url.pathname === "/.well-known/oauth-authorization-server" || url.pathname === "/.well-known/openid-configuration")) {
      return Response.json({ issuer: origin, authorization_endpoint: `${MCP_OAUTH_AUTHORIZATION_ORIGIN}/authorize`, token_endpoint: `${origin}/token`, registration_endpoint: `${origin}/register`, response_types_supported: ["code"], grant_types_supported: ["authorization_code", "refresh_token"], code_challenge_methods_supported: ["S256"], token_endpoint_auth_methods_supported: ["none"], scopes_supported: ["mcp:tools"] });
    }
    if (request.method === "POST" && url.pathname === "/register") {
      const body: unknown = await request.json();
      if (typeof body !== "object" || body === null || !("redirect_uris" in body) || !Array.isArray(body.redirect_uris)) return Response.json({ error: "invalid_client_metadata" }, { status: 400 });
      const redirects = body.redirect_uris.filter((entry): entry is string => typeof entry === "string");
      if (!redirects.length || redirects.some(redirect => !isLoopbackCallback(redirect))) return Response.json({ error: "invalid_redirect_uri" }, { status: 400 });
      const clientId = `parity-client-${randomBytes(12).toString("hex")}`;
      clients.set(clientId, redirects);
      trace("mcp-http", { direction: "oauth-client-registered", redirectUris: redirects });
      return Response.json({ client_id: clientId, redirect_uris: redirects, token_endpoint_auth_method: "none" }, { status: 201 });
    }
    if (request.method === "GET" && url.pathname === "/authorize") {
      const clientId = url.searchParams.get("client_id") ?? "";
      const redirectUri = url.searchParams.get("redirect_uri") ?? "";
      const state = url.searchParams.get("state");
      const challenge = url.searchParams.get("code_challenge");
      if (!clients.get(clientId)?.includes(redirectUri) || !state || !challenge || url.searchParams.get("code_challenge_method") !== "S256" || url.searchParams.get("response_type") !== "code") return Response.json({ error: "invalid_request" }, { status: 400 });
      if (input.authorization === "hold") {
        trace("mcp-http", { direction: "oauth-authorization-pending", callbackOrigin: new URL(redirectUri).origin });
        return new Response("Owned authorization awaits consent", { headers: { "Content-Type": "text/plain" } });
      }
      const code = randomBytes(24).toString("base64url");
      codes.set(code, { clientId, redirectUri, challenge });
      const callback = new URL(redirectUri);
      callback.searchParams.set("code", code);
      callback.searchParams.set("state", state);
      trace("mcp-http", { direction: "oauth-authorized", callbackOrigin: callback.origin, callbackPath: callback.pathname });
      return Response.redirect(callback.href, 302);
    }
    if (request.method === "POST" && url.pathname === "/token") {
      const body = new URLSearchParams(await request.text());
      const grantType = body.get("grant_type");
      if (grantType === "authorization_code") {
        const code = body.get("code") ?? "";
        const grant = codes.get(code);
        const verifier = body.get("code_verifier") ?? "";
        if (!grant || body.get("client_id") !== grant.clientId || body.get("redirect_uri") !== grant.redirectUri || createHash("sha256").update(verifier).digest("base64url") !== grant.challenge) return Response.json({ error: "invalid_grant" }, { status: 400 });
        codes.delete(code);
      } else if (grantType === "refresh_token") {
        const token = body.get("refresh_token") ?? "";
        if (!refreshTokens.delete(token)) return Response.json({ error: "invalid_grant" }, { status: 400 });
      } else return Response.json({ error: "unsupported_grant_type" }, { status: 400 });
      const access = `parity-access-${randomBytes(24).toString("base64url")}`;
      const refresh = `parity-refresh-${randomBytes(24).toString("base64url")}`;
      accessTokens.add(access);
      refreshTokens.add(refresh);
      trace("mcp-http", { direction: "oauth-token-issued", grantType });
      return Response.json({ access_token: access, refresh_token: refresh, token_type: "Bearer", expires_in: 3600, scope: "mcp:tools" }, { headers: { "Cache-Control": "no-store" } });
    }
    if (url.pathname !== "/mcp") return new Response("Unknown parity MCP route", { status: 404 });
    if (input.oauth && !accessTokens.has((request.headers.get("authorization") ?? "").replace(/^Bearer /, ""))) {
      return new Response("OAuth authorization required", { status: 401, headers: { "WWW-Authenticate": `Bearer resource_metadata="${origin}/.well-known/oauth-protected-resource/mcp"`, "Mcp-Auth-Server": origin } });
    }
    const sessionId = request.headers.get("mcp-session-id");
    if (request.method === "GET") return new Response("SSE is not supported", { status: 405 });
    if (request.method === "DELETE") {
      if (sessionId) sessions.delete(sessionId);
      return new Response(null, { status: 204 });
    }
    if (request.method !== "POST") return new Response("Method not allowed", { status: 405 });
    const body: unknown = await request.json();
    if (typeof body !== "object" || body === null || !("method" in body) || typeof body.method !== "string") return Response.json({ error: "Invalid JSON-RPC request" }, { status: 400 });
    const id = "id" in body && (typeof body.id === "string" || typeof body.id === "number") ? body.id : undefined;
    const params = "params" in body && typeof body.params === "object" && body.params !== null ? body.params as Record<string, unknown> : {};
    trace("mcp-http", { direction: "rpc-request", method: body.method, hasAuthorization: request.headers.has("authorization") });
    const currentSession = sessionId ? sessions.get(sessionId) : undefined;
    if (body.method === "initialize" && id !== undefined) {
      const createdId = randomBytes(16).toString("hex");
      sessions.set(createdId, { initialized: false, subscriptions: new Set() });
      const result: MCPInitializeResult = { protocolVersion: production.MCP_PROTOCOL_VERSION, serverInfo: { name: input.name, version: input.version }, capabilities: { tools: { listChanged: true }, resources: { subscribe: true, listChanged: true }, prompts: { listChanged: true } } };
      const response: JsonRpcResponse = { jsonrpc: "2.0", id, result };
      return Response.json(response, { headers: { "Mcp-Session-Id": createdId } });
    }
    if (!currentSession) return new Response("Unknown MCP session", { status: 404 });
    if (body.method === "notifications/initialized") {
      currentSession.initialized = true;
      return new Response(null, { status: 202 });
    }
    if (id === undefined) return new Response(null, { status: 202 });
    if (!currentSession.initialized) return Response.json({ jsonrpc: "2.0", id, error: { code: -32002, message: "Server not initialized" } } satisfies JsonRpcResponse);
    let result: unknown;
    switch (body.method) {
      case "ping": result = {}; break;
      case "tools/list": result = { tools: [{ name: input.tool, description: "Echo the supplied text", inputSchema: { type: "object", properties: { text: { type: "string" } }, required: ["text"] } }] } satisfies MCPToolsListResult; break;
      case "tools/call": {
        const argumentsValue = params.arguments;
        if (params.name !== input.tool || typeof argumentsValue !== "object" || argumentsValue === null || !("text" in argumentsValue) || typeof argumentsValue.text !== "string") return Response.json({ jsonrpc: "2.0", id, error: { code: -32602, message: "Expected echo with string text" } } satisfies JsonRpcResponse);
        result = { content: [{ type: "text", text: argumentsValue.text }] } satisfies MCPToolCallResult;
        break;
      }
      case "resources/list": result = { resources: [{ uri: input.resource, name: "Parity resource", title: "Parity resource", mimeType: "text/plain" }] } satisfies MCPResourcesListResult; break;
      case "resources/templates/list": result = { resourceTemplates: [] }; break;
      case "resources/read":
        if (params.uri !== input.resource) return Response.json({ jsonrpc: "2.0", id, error: { code: -32602, message: "Unknown resource" } } satisfies JsonRpcResponse);
        result = { contents: [{ uri: input.resource, text: "Parity resource\n", mimeType: "text/plain" }] } satisfies MCPResourceReadResult;
        break;
      case "resources/subscribe":
      case "resources/unsubscribe":
        if (params.uri !== input.resource) return Response.json({ jsonrpc: "2.0", id, error: { code: -32602, message: "Unknown resource" } } satisfies JsonRpcResponse);
        if (body.method === "resources/subscribe") currentSession.subscriptions.add(input.resource);
        else currentSession.subscriptions.delete(input.resource);
        result = {};
        break;
      case "prompts/list": result = { prompts: [{ name: input.prompt, title: "Parity prompt", description: "Parity prompt" }] } satisfies MCPPromptsListResult; break;
      case "prompts/get":
        if (params.name !== input.prompt) return Response.json({ jsonrpc: "2.0", id, error: { code: -32602, message: "Unknown prompt" } } satisfies JsonRpcResponse);
        result = { description: "Parity prompt", messages: [{ role: "user", content: { type: "text", text: "Parity prompt" } }] } satisfies MCPGetPromptResult;
        break;
      default: return Response.json({ jsonrpc: "2.0", id, error: { code: -32601, message: `Method not found: ${body.method}` } } satisfies JsonRpcResponse);
    }
    return Response.json({ jsonrpc: "2.0", id, result } satisfies JsonRpcResponse);
  });
  origin = service.url;
  const config: MCPHttpServerConfig = { type: "http", url: `${origin}/mcp`, ...(input.oauth ? { oauth: { callbackPort: 0 } } : {}) };
  return { service, environment: { OMP_PARITY_MCP_HTTP_URL: config.url, ...(input.oauth ? { OMP_PARITY_MCP_OAUTH_CALLBACKS: "1" } : {}) }, config };
}

function isLoopbackCallback(value: string): boolean {
  try {
    const url = new URL(value);
    return url.protocol === "http:" && ["127.0.0.1", "localhost", "[::1]"].includes(url.hostname) && !url.username && !url.password;
  } catch { return false; }
}
