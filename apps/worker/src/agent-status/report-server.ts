import net from "node:net";
import { z } from "zod";
import {
  cleanupLocalEndpoint,
  LOCAL_ENDPOINT_MAX_UNAUTHENTICATED_CONNECTIONS,
  LOCAL_ENDPOINT_UNAUTHENTICATED_MAX_BYTES,
  LOCAL_ENDPOINT_UNAUTHENTICATED_TIMEOUT_MS,
  prepareLocalEndpoint,
  secureLocalEndpoint,
  type LocalEndpoint,
} from "@roost/shared/local-endpoint";
import {
  AGENT_STATUS_MESSAGE_MAX_LENGTH,
  AgentRuntimeState,
  SessionId,
  log,
} from "@roost/shared";
import { supportedHostPlatform } from "@roost/shared/platform";
import type { AgentScreenDetector } from "./detector.ts";
import type { BuiltinAgentId } from "./process-scan.ts";
import type { AgentStatusRegistry } from "./registry.ts";
import {
  resolveAgentReportEndpoint,
  verifyAgentReportCapability,
} from "./environment.ts";

const MAX_LINE_BYTES = 4_096;
const MAX_REQUESTS_PER_CONNECTION = 32;
const BuiltinAgent = z.enum([
  "codex", "gemini", "opencode", "cursor", "amp",
  "copilot", "droid", "grok", "pi", "omp",
]);

export const AgentReportRequest = z.object({
  version: z.literal(1),
  method: z.literal("agent.report"),
  capability: z.string().regex(/^[a-f0-9]{64}$/),
  params: z.object({
    session_id: SessionId,
    pid: z.number().int().positive().max(Number.MAX_SAFE_INTEGER),
    agent: BuiltinAgent,
    state: AgentRuntimeState,
    message: z.string().max(AGENT_STATUS_MESSAGE_MAX_LENGTH).optional(),
    seq: z.number().int().nonnegative().max(Number.MAX_SAFE_INTEGER),
    active: z.boolean(),
  }),
});
export type AgentReportRequest = z.infer<typeof AgentReportRequest>;


export interface AgentReportServerOptions {
  detector: Pick<AgentScreenDetector, "sessionForPid">;
  registry: Pick<AgentStatusRegistry, "reportIntegration">;
  endpoint?: LocalEndpoint;
  /** POSIX-only explicit address seam for isolated callers. */
  socketPath?: string;
}

export interface AgentReportServer {
  endpoint: LocalEndpoint;
  path: string;
  close(): Promise<void>;
}

function response(ok: boolean, error?: string): string {
  return `${JSON.stringify(ok ? { ok: true } : { ok: false, error })}\n`;
}

export async function startAgentReportServer(
  options: AgentReportServerOptions,
): Promise<AgentReportServer> {
  let endpoint = options.endpoint ?? resolveAgentReportEndpoint();
  if (options.socketPath) {
    if (supportedHostPlatform() === "win32") {
      throw new Error("socketPath cannot select a UDS on Windows");
    }
    endpoint = {
      ...endpoint,
      kind: "uds",
      address: options.socketPath,
      isFilesystemPath: true,
    };
  }
  await prepareLocalEndpoint(endpoint);
  let unauthenticatedConnections = 0;
  const server = net.createServer((socket) => {
    if (
      unauthenticatedConnections >=
      LOCAL_ENDPOINT_MAX_UNAUTHENTICATED_CONNECTIONS
    ) {
      socket.destroy();
      return;
    }
    unauthenticatedConnections++;
    let awaitingAuthentication = true;
    let authenticatedSessionId: string | null = null;
    let unauthenticatedBytes = 0;
    const releaseUnauthenticatedSlot = () => {
      if (!awaitingAuthentication) return;
      awaitingAuthentication = false;
      unauthenticatedConnections = Math.max(0, unauthenticatedConnections - 1);
    };
    const authenticationTimer = setTimeout(() => {
      if (awaitingAuthentication) socket.destroy();
    }, LOCAL_ENDPOINT_UNAUTHENTICATED_TIMEOUT_MS);
    authenticationTimer.unref?.();
    socket.once("close", () => {
      clearTimeout(authenticationTimer);
      releaseUnauthenticatedSlot();
    });
    socket.setEncoding("utf8");
    let buffer = "";
    let requestCount = 0;
    let chain = Promise.resolve();
    const send = (body: string) => {
      if (!socket.destroyed && socket.writable) socket.write(body);
    };
    const handleLine = async (line: string) => {
      if (Buffer.byteLength(line) > MAX_LINE_BYTES) {
        send(response(false, "request_too_large"));
        socket.destroy();
        return;
      }
      let raw: unknown;
      try { raw = JSON.parse(line); }
      catch {
        send(response(false, "invalid_json"));
        return;
      }
      const parsed = AgentReportRequest.safeParse(raw);
      if (!parsed.success) {
        send(response(false, "invalid_request"));
        return;
      }
      const claimed = parsed.data.params.session_id;
      if (authenticatedSessionId === null) {
        if (!verifyAgentReportCapability(endpoint, claimed, parsed.data.capability)) {
          send(response(false, "authentication_failed"));
          socket.destroy();
          return;
        }
        authenticatedSessionId = claimed;
        clearTimeout(authenticationTimer);
        releaseUnauthenticatedSlot();
      } else if (
        claimed !== authenticatedSessionId ||
        !verifyAgentReportCapability(endpoint, claimed, parsed.data.capability)
      ) {
        send(response(false, "authentication_failed"));
        socket.destroy();
        return;
      }
      try {
        const resolved = await options.detector.sessionForPid(parsed.data.params.pid);
        if (!resolved || claimed !== resolved) {
          send(response(false, "pid_session_mismatch"));
          return;
        }
        const accepted = options.registry.reportIntegration({
          sessionId: resolved,
          agentId: parsed.data.params.agent as BuiltinAgentId,
          state: parsed.data.params.state,
          message: parsed.data.params.message,
          seq: parsed.data.params.seq,
          active: parsed.data.params.active,
        });
        send(accepted ? response(true) : response(false, "stale_seq"));
      } catch (error) {
        log.warn("agent-status", "report_request_failed", { error: String(error) });
        send(response(false, "internal_error"));
      }
    };
    socket.on("data", (chunk: string) => {
      try {
        if (awaitingAuthentication) {
          unauthenticatedBytes += Buffer.byteLength(chunk);
          if (unauthenticatedBytes > LOCAL_ENDPOINT_UNAUTHENTICATED_MAX_BYTES) {
            send(response(false, "request_too_large"));
            socket.destroy();
            return;
          }
        }
        buffer += chunk;
        if (Buffer.byteLength(buffer) > MAX_LINE_BYTES * 2) {
          send(response(false, "request_too_large"));
          socket.destroy();
          return;
        }
        let newline = buffer.indexOf("\n");
        while (newline >= 0) {
          const line = buffer.slice(0, newline);
          buffer = buffer.slice(newline + 1);
          requestCount++;
          if (requestCount > MAX_REQUESTS_PER_CONNECTION) {
            send(response(false, "too_many_requests"));
            socket.destroy();
            return;
          }
          if (line.trim()) chain = chain.then(() => handleLine(line)).catch(() => undefined);
          newline = buffer.indexOf("\n");
        }
      } catch {
        send(response(false, "invalid_request"));
        socket.destroy();
      }
    });
    socket.on("error", () => undefined);
  });
  const { promise, resolve, reject } = Promise.withResolvers<void>();
  server.once("error", reject);
  server.listen(endpoint.address, resolve);
  try {
    await promise;
    server.removeListener("error", reject);
    server.on("error", (error) => {
      log.warn("agent-status", "report_server_error", { error: String(error) });
    });
    await secureLocalEndpoint(endpoint);
  } catch (error) {
    try { server.close(); } catch { /* listen failed before binding */ }
    await cleanupLocalEndpoint(endpoint);
    throw error;
  }
  return {
    endpoint,
    path: endpoint.address,
    async close(): Promise<void> {
      const closed = Promise.withResolvers<void>();
      server.close(() => closed.resolve());
      await closed.promise;
      await cleanupLocalEndpoint(endpoint);
    },
  };
}
