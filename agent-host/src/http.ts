import { createHash, timingSafeEqual } from "node:crypto";
import { createServer, type IncomingMessage, type Server, type ServerResponse } from "node:http";
import type { AddressInfo } from "node:net";
import type { HostConfig } from "./config.ts";
import type { Conversations } from "./conversations.ts";
import type { EventHub } from "./event-hub.ts";
import type { Logins } from "./logins.ts";
import type { ModelsService } from "./models.ts";

export interface AgentHost { server: Server; listen(): Promise<AddressInfo>; close(): Promise<void> }
export interface AgentHostOptions { config: HostConfig; conversations: Conversations; hub: EventHub; models: ModelsService; logins: Logins; onInternalError?: (error: unknown) => void }
const BODY_LIMIT = 1024 * 1024;
const digest = (value: string) => createHash("sha256").update(value).digest();

async function readBody(request: IncomingMessage): Promise<Record<string, any>> {
  const chunks: Buffer[] = [];
  let size = 0;
  for await (const chunk of request) {
    size += Buffer.byteLength(chunk);
    if (size > BODY_LIMIT) throw Object.assign(new Error("request body too large"), { status: 413, code: "invalid" });
    chunks.push(Buffer.from(chunk));
  }
  if (!size) return {};
  try { return JSON.parse(Buffer.concat(chunks).toString("utf8")) as Record<string, any>; }
  catch { throw Object.assign(new Error("invalid JSON body"), { status: 400, code: "invalid" }); }
}
function sendJson(response: ServerResponse, value: unknown, status = 200): void {
  response.writeHead(status, { "content-type": "application/json; charset=utf-8", "cache-control": "no-store" });
  response.end(JSON.stringify(value));
}
function sendError(response: ServerResponse, error: unknown): number {
  const issue = error as { status?: number; code?: string; message?: string };
  const status = issue.status ?? 500;
  const code = ["not_found", "invalid", "busy", "unavailable", "internal"].includes(issue.code ?? "") ? issue.code : status === 404 ? "not_found" : status < 500 ? "invalid" : "internal";
  sendJson(response, { error: { code, message: issue.message ?? "internal error" } }, status);
  return status;
}
function authorized(request: IncomingMessage, secret: string): boolean {
  const authorization = request.headers.authorization ?? "";
  const provided = authorization.startsWith("Bearer ") ? authorization.slice(7) : "";
  return timingSafeEqual(digest(provided), digest(secret));
}

export function createAgentHost(options: AgentHostOptions): AgentHost {
  const { config, conversations, hub, models, logins, onInternalError } = options;
  const server = createServer(async (request, response) => {
    const url = new URL(request.url ?? "/", "http://localhost");
    if (request.method === "GET" && url.pathname === "/healthz") { response.writeHead(200, { "content-type": "text/plain" }); response.end("ok"); return; }
    if (!authorized(request, config.secret)) { sendJson(response, { error: { code: "unavailable", message: "unauthorized" } }, 401); return; }
    try {
      const body = await readBody(request);
      const path = url.pathname;
      if (request.method === "GET" && path === "/v1/models") { sendJson(response, await models.catalog()); return; }
      if (request.method === "GET" && path === "/v1/events") {
        response.writeHead(200, { "content-type": "application/x-ndjson; charset=utf-8", "cache-control": "no-cache", connection: "keep-alive" });
        const write = (line: Record<string, unknown>) => response.write(`${JSON.stringify(line)}\n`);
        const disconnect = await hub.connect(write);
        const ping = setInterval(() => write({ type: "ping" }), 15_000);
        response.on("close", () => { clearInterval(ping); disconnect(); });
        return;
      }
      if (request.method === "POST" && path === "/v1/conversations") { sendJson(response, await conversations.create(body as never), 201); return; }
      let match = path.match(/^\/v1\/conversations\/([^/]+)(?:\/(submit|abort|configure))?$/);
      if (match) {
        const id = decodeURIComponent(match[1]!);
        if (request.method === "DELETE" && !match[2]) { await conversations.delete(id); sendJson(response, {}); return; }
        if (request.method === "POST" && match[2] === "submit") { await conversations.submit(id, String(body.text ?? ""), String(body.request_id ?? "")); sendJson(response, {}); return; }
        if (request.method === "POST" && match[2] === "abort") { await conversations.abort(id); sendJson(response, {}); return; }
        if (request.method === "POST" && match[2] === "configure") { sendJson(response, await conversations.configure(id, body)); return; }
      }
      if (request.method === "POST" && path === "/v1/auth/logins") { sendJson(response, logins.start(String(body.provider ?? "")), 201); return; }
      match = path.match(/^\/v1\/auth\/logins\/([^/]+)(?:\/(respond))?$/);
      if (match) {
        const id = decodeURIComponent(match[1]!);
        if (request.method === "GET" && !match[2]) { sendJson(response, logins.poll(id)); return; }
        if (request.method === "DELETE" && !match[2]) { logins.cancel(id); sendJson(response, {}); return; }
        if (request.method === "POST" && match[2] === "respond") { logins.respond(id, String(body.prompt_id ?? ""), String(body.value ?? "")); sendJson(response, {}); return; }
      }
      match = path.match(/^\/v1\/auth\/api-keys\/([^/]+)$/);
      if (request.method === "PUT" && match) {
        const provider = decodeURIComponent(match[1]!);
        await models.setApiKey(provider, String(body.api_key ?? ""));
        sendJson(response, {}); return;
      }
      match = path.match(/^\/v1\/auth\/credentials\/([^/]+)$/);
      if (request.method === "DELETE" && match) { await models.models.logout(decodeURIComponent(match[1]!)); sendJson(response, {}); return; }
      sendJson(response, { error: { code: "not_found", message: "not found" } }, 404);
    } catch (error) { if (sendError(response, error) >= 500) onInternalError?.(error); }
  });
  return {
    server,
    listen: () => new Promise((resolve, reject) => { server.once("error", reject); server.listen(config.bind.split(":").at(-1) ? Number(config.bind.split(":").at(-1)) : 4115, config.bind.slice(0, config.bind.lastIndexOf(":")), () => { server.off("error", reject); resolve(server.address() as AddressInfo); }); }),
    close: () => new Promise((resolve, reject) => server.close(error => error ? reject(error) : resolve())),
  };
}
