import { afterEach, describe, expect, test } from "bun:test";
import net from "node:net";
import { mkdtemp, rm, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { AgentStatusUpdate } from "@roost/shared";
import { startAgentReportServer, type AgentReportServer } from "../src/agent-status/report-server.ts";
import { AgentStatusRegistry } from "../src/agent-status/registry.ts";
import { withAgentStatusEnvironment } from "../src/agent-status/environment.ts";

const sessionId = "11111111-1111-4111-8111-111111111111";
const cleanupDirs: string[] = [];
let server: AgentReportServer | null = null;

afterEach(async () => {
  if (server) await server.close();
  server = null;
  await Promise.all(cleanupDirs.splice(0).map((path) => rm(path, { recursive: true, force: true })));
});

async function request(path: string, body: string): Promise<Record<string, unknown>> {
  const { promise, resolve, reject } = Promise.withResolvers<Record<string, unknown>>();
  const socket = net.createConnection(path);
  let buffer = "";
  socket.setEncoding("utf8");
  socket.on("connect", () => socket.write(body));
  socket.on("data", (chunk) => {
    buffer += chunk;
    const newline = buffer.indexOf("\n");
    if (newline < 0) return;
    socket.destroy();
    try { resolve(JSON.parse(buffer.slice(0, newline)) as Record<string, unknown>); }
    catch (error) { reject(error); }
  });
  socket.on("error", reject);
  return promise;
}

function report(seq: number, patch: Record<string, unknown> = {}): string {
  if (!server) throw new Error("agent report server is not running");
  const params = {
    session_id: sessionId,
    pid: 42,
    agent: "omp",
    state: "working",
    seq,
    active: true,
    ...patch,
  };
  const capability = withAgentStatusEnvironment(
    {},
    String(params.session_id),
  ).ROOST_AGENT_CAPABILITY;
  return `${JSON.stringify({
    version: 1,
    capability,
    method: "agent.report",
    params,
  })}\n`;
}

describe("agent report environment", () => {
  test("exports the report endpoint under the documented POSIX socket name", () => {
    const shellEnv = withAgentStatusEnvironment({}, sessionId);
    expect(shellEnv.ROOST_SESSION_ID).toBe(sessionId);
    if (process.platform === "win32") {
      expect(shellEnv.ROOST_AGENT_ENDPOINT_KIND).toBe("named-pipe");
      expect(shellEnv.ROOST_AGENT_SOCKET_PATH).toBeUndefined();
    } else {
      expect(shellEnv.ROOST_AGENT_ENDPOINT_KIND).toBe("uds");
      expect(shellEnv.ROOST_AGENT_SOCKET_PATH).toBe(shellEnv.ROOST_AGENT_ENDPOINT);
    }
  });
});

describe("agent report server", () => {
  test("creates a 0600 socket and accepts owned pid reports", async () => {
    const dir = await mkdtemp(join(tmpdir(), "roost-agent-report-"));
    cleanupDirs.push(dir);
    const published: AgentStatusUpdate[] = [];
    const registry = new AgentStatusRegistry({
      publish: (status) => published.push(status), startLeaseTimer: false,
    });
    server = await startAgentReportServer({
      socketPath: join(dir, "agent.sock"),
      detector: { sessionForPid: async (pid) => pid === 42 ? sessionId : null },
      registry,
    });
    expect((await stat(server.path)).mode & 0o777).toBe(0o600);
    expect(await request(server.path, report(1))).toEqual({ ok: true });
    expect(published.at(-1)).toMatchObject({
      session_id: sessionId, agent_id: "omp", state: "working", active: true,
    });
    registry.dispose();
  });

  test("rejects malformed, mismatched, and stale requests without escaping", async () => {
    const dir = await mkdtemp(join(tmpdir(), "roost-agent-report-"));
    cleanupDirs.push(dir);
    const registry = new AgentStatusRegistry({ publish: () => undefined, startLeaseTimer: false });
    server = await startAgentReportServer({
      socketPath: join(dir, "agent.sock"),
      detector: { sessionForPid: async (pid) => pid === 42 ? sessionId : null },
      registry,
    });
    expect(await request(server.path, "not json\n")).toMatchObject({ ok: false, error: "invalid_json" });
    expect(await request(server.path, report(1, { session_id: "22222222-2222-4222-8222-222222222222" })))
      .toMatchObject({ ok: false, error: "pid_session_mismatch" });
    expect(await request(server.path, report(2))).toEqual({ ok: true });
    expect(await request(server.path, report(2))).toMatchObject({ ok: false, error: "stale_seq" });
    expect(await request(server.path, report(3, { message: "x".repeat(513) })))
      .toMatchObject({ ok: false, error: "invalid_request" });
    registry.dispose();
  });

  test("caps oversized local input", async () => {
    const dir = await mkdtemp(join(tmpdir(), "roost-agent-report-"));
    cleanupDirs.push(dir);
    server = await startAgentReportServer({
      socketPath: join(dir, "agent.sock"),
      detector: { sessionForPid: async () => sessionId },
      registry: { reportIntegration: () => true },
    });
    expect(await request(server.path, `${"x".repeat(8_300)}\n`))
      .toMatchObject({ ok: false, error: "request_too_large" });
  });
});
