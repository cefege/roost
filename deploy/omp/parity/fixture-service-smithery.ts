import type { BuiltinSeedContext } from "./builtin-seed";
import { startHttpService, type LocalHttpService, type ServiceTrace } from "./fixture-service-http";
import { startMcpHttpService } from "./fixture-service-mcp-http";

export type SmitheryServiceInput =
  | { mode: "failure"; status: number }
  | { mode: "success"; apiKey: string; scope: string; name: string; displayName: string; browserLogin?: "complete" | "failure" };

export const SMITHERY_APPROVAL_ORIGIN = "https://parity-smithery.invalid";

export async function startSmitheryService(input: SmitheryServiceInput, context: BuiltinSeedContext, trace: ServiceTrace): Promise<{ service: LocalHttpService; environment: Record<string, string> }> {
  if (input.mode === "failure") {
    const service = startHttpService("smithery", trace, request => request.method === "POST" && new URL(request.url).pathname === "/api/auth/cli/session" ? new Response("Parity Smithery auth unavailable", { status: input.status }) : new Response("Unknown Smithery boundary route", { status: 403 }));
    return { service, environment: { SMITHERY_URL: service.url, OMP_PARITY_ALLOWED_ORIGINS: service.url } };
  }
  const mcp = await startMcpHttpService({ name: input.name, version: "1.0.0", tool: "echo", resource: "parity://resource", prompt: "parity", oauth: false }, context, trace);
  const sessions = new Map<string, { approved: boolean }>();
  const qualifiedName = `@${input.scope}/${input.name}`;
  let origin: string;
  let auth: LocalHttpService;
  try {
    auth = startHttpService("smithery", trace, request => {
      const url = new URL(request.url);
      if (request.method === "POST" && url.pathname === "/api/auth/cli/session") {
        if (input.browserLogin === "failure") return new Response("Parity Smithery browser auth unavailable", { status: 503 });
        const sessionId = crypto.randomUUID();
        sessions.set(sessionId, { approved: false });
        return Response.json({ sessionId, authUrl: `${SMITHERY_APPROVAL_ORIGIN}/approve?session=${encodeURIComponent(sessionId)}` });
      }
      if (request.method === "GET" && url.pathname === "/approve") {
        const sessionId = url.searchParams.get("session") ?? "";
        const session = sessions.get(sessionId);
        if (!session) return new Response("Unknown owned Smithery session", { status: 404 });
        session.approved = true;
        trace("smithery", { direction: "approval", sessionId });
        return new Response("Owned Smithery authorization approved", { headers: { "Content-Type": "text/plain" } });
      }
      if (request.method === "GET" && url.pathname.startsWith("/api/auth/cli/poll/")) {
        const sessionId = decodeURIComponent(url.pathname.slice("/api/auth/cli/poll/".length));
        const session = sessions.get(sessionId);
        if (!session) return new Response("Unknown owned Smithery session", { status: 404 });
        return session.approved ? Response.json({ status: "success", apiKey: input.apiKey }) : Response.json({ status: "pending" });
      }
      if (!url.pathname.startsWith("/servers")) return new Response("Unknown Smithery boundary route", { status: 404 });
      if (request.headers.get("authorization") !== `Bearer ${input.apiKey}`) return Response.json({ error: "Unauthorized" }, { status: 401 });
      if (request.method !== "GET") return new Response("Method not allowed", { status: 405 });
      if (url.pathname === "/servers") {
        const query = url.searchParams.get("q") ?? "";
        const matches = query.toLowerCase() !== "mcp" && `${qualifiedName} ${input.displayName}`.toLowerCase().includes(query.toLowerCase());
        return Response.json({ servers: matches ? [{ id: `${input.scope}-${input.name}`, namespace: input.scope, slug: input.name, qualifiedName, displayName: input.displayName, description: "Owned parity MCP endpoint", remote: true, isDeployed: true, verified: true, useCount: 1 }] : [] });
      }
      const identifier = decodeURIComponent(url.pathname.slice("/servers/".length)).replace(/^@/, "");
      if (identifier !== `${input.scope}/${input.name}` && identifier !== input.name) return Response.json({ error: "Server not found" }, { status: 404 });
      return Response.json({ qualifiedName, displayName: input.displayName, description: "Owned parity MCP endpoint", remote: true, deploymentUrl: mcp.config.url, connections: [{ type: "http", deploymentUrl: mcp.config.url }], tools: [{ name: "echo", description: "Echo the supplied text", inputSchema: { type: "object", properties: { text: { type: "string" } }, required: ["text"] } }] });
    });
    origin = auth.url;
  } catch (error) {
    await mcp.service.dispose();
    throw error;
  }
  let disposed = false;
  const service: LocalHttpService = {
    url: auth.url,
    port: auth.port,
    async dispose() {
      if (disposed) return;
      disposed = true;
      const outcomes = await Promise.allSettled([auth.dispose(), mcp.service.dispose()]);
      const failures = outcomes.flatMap(outcome => outcome.status === "rejected" ? [outcome.reason] : []);
      if (failures.length) throw new AggregateError(failures, "Smithery fixture disposal failed");
    },
  };
  return { service, environment: { SMITHERY_URL: auth.url, OMP_PARITY_SMITHERY_REGISTRY_URL: auth.url, OMP_PARITY_SMITHERY_APPROVAL_ORIGIN: auth.url, OMP_PARITY_ALLOWED_ORIGINS: [auth.url, mcp.service.url].join(",") } };
}
