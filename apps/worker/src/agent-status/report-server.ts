import net from "node:net";
import { chmod, mkdir, rm } from "node:fs/promises";
import { dirname } from "node:path";
import { z } from "zod";
import {
  AGENT_STATUS_MESSAGE_MAX_LENGTH,
  AgentRuntimeState,
  SessionId,
  log,
} from "@roost/shared";
import type { AgentScreenDetector } from "./detector.ts";
import type { BuiltinAgentId } from "./process-scan.ts";
import type { AgentStatusRegistry } from "./registry.ts";
import { defaultAgentReportSocketPath } from "./environment.ts";

const MAX_LINE_BYTES = 4_096;
const MAX_REQUESTS_PER_CONNECTION = 32;
const BuiltinAgent = z.enum([
  "codex", "gemini", "opencode", "cursor", "amp",
  "copilot", "droid", "grok", "pi", "omp",
]);

export const AgentReportRequest = z.object({
  version: z.literal(1),
  method: z.literal("agent.report"),
  params: z.object({
    session_id: SessionId.optional(),
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
  socketPath?: string;
}

export interface AgentReportServer {
  path: string;
  close(): Promise<void>;
}

function response(ok: boolean, error?: string): string {
  return `${JSON.stringify(ok ? { ok: true } : { ok: false, error })}\n`;
}

export async function startAgentReportServer(
  options: AgentReportServerOptions,
): Promise<AgentReportServer> {
  const socketPath = options.socketPath ?? defaultAgentReportSocketPath();
  await mkdir(dirname(socketPath), { recursive: true, mode: 0o700 });
  await rm(socketPath, { force: true });
  const server = net.createServer((socket) => {
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
      try {
        const claimed = parsed.data.params.session_id;
        const resolved = await options.detector.sessionForPid(parsed.data.params.pid);
        if (!resolved || (claimed !== undefined && claimed !== resolved)) {
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
  server.listen(socketPath, resolve);
  await promise;
  server.removeListener("error", reject);
  await chmod(socketPath, 0o600);
  return {
    path: socketPath,
    async close(): Promise<void> {
      const closed = Promise.withResolvers<void>();
      server.close(() => closed.resolve());
      await closed.promise;
      await rm(socketPath, { force: true });
    },
  };
}
