// Local agent-report socket authentication and admission tests. The caller
// supplies only session-authorized state; a fresh detector identity and the
// server's serialized monotonic sequence are the registry input.

import { afterEach, describe, expect, test } from "bun:test";
import net from "node:net";
import { mkdtemp, rm, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { AgentStatusUpdate } from "@roost/shared/wire";
import { startAgentReportServer, type AgentReportServer } from "../src/agent-status/report-server.ts";
import {
  AgentStatusRegistry,
  type IntegrationStatusReport,
} from "../src/agent-status/registry.ts";
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

function report(patch: Record<string, unknown> = {}): string {
  if (!server) throw new Error("agent report server is not running");
  const params = {
    session_id: sessionId,
    state: "working",
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
  test("accepts state with fresh worker-derived identity and ordering", async () => {
    const dir = await mkdtemp(join(tmpdir(), "roost-agent-report-"));
    cleanupDirs.push(dir);
    const published: AgentStatusUpdate[] = [];
    const received: IntegrationStatusReport[] = [];
    let identity: { agentId: "omp" | "pi"; pid: number } = {
      agentId: "omp",
      pid: 42,
    };
    let peerPid = identity.pid;
    const registry = new AgentStatusRegistry({
      publish: (status) => published.push(status), startLeaseTimer: false,
    });
    server = await startAgentReportServer({
      socketPath: join(dir, "agent.sock"),
      detector: {
        reportingAgentForSession: async (claimed, attestedPid) => (
          claimed === sessionId && attestedPid === identity.pid ? identity : null
        ),
      },
      peerProcessIdReader: { read: () => peerPid },
      registry: {
        reportIntegration: (candidate) => {
          received.push(candidate);
          return registry.reportIntegration(candidate);
        },
      },
    });
    expect((await stat(server.path)).mode & 0o777).toBe(0o600);
    expect(await request(server.path, report())).toEqual({ ok: true });
    identity = { agentId: "pi", pid: 84 };
    peerPid = identity.pid;
    expect(await request(server.path, report({ state: "blocked" }))).toEqual({ ok: true });
    expect(received.map(({ agentId, processId }) => [agentId, processId])).toEqual([
      ["omp", 42],
      ["pi", 84],
    ]);
    expect(received[1]!.seq).toBeGreaterThan(received[0]!.seq);
    expect(published.at(-1)).toMatchObject({
      session_id: sessionId,
      agent_id: "pi",
      state: "blocked",
      active: true,
      source: "integration",
    });
    registry.dispose();
  });

  test("rejects unavailable identity, caller-selected identity, and malformed state", async () => {
    const dir = await mkdtemp(join(tmpdir(), "roost-agent-report-"));
    cleanupDirs.push(dir);
    const received: IntegrationStatusReport[] = [];
    let identityAvailable = false;
    let peerPid = 42;
    server = await startAgentReportServer({
      socketPath: join(dir, "agent.sock"),
      detector: {
        reportingAgentForSession: async (claimed, attestedPid) => (
          identityAvailable && claimed === sessionId && attestedPid === 42
            ? { agentId: "omp", pid: 42 }
            : null
        ),
      },
      peerProcessIdReader: { read: () => peerPid },
      registry: {
        reportIntegration: (candidate) => {
          received.push(candidate);
          return true;
        },
      },
    });
    expect(await request(server.path, "not json\n")).toMatchObject({
      ok: false,
      error: "invalid_json",
    });
    expect(await request(server.path, report())).toMatchObject({
      ok: false,
      error: "reporter_identity_mismatch",
    });
    expect(await request(server.path, report({
      session_id: "22222222-2222-4222-8222-222222222222",
    }))).toMatchObject({ ok: false, error: "reporter_identity_mismatch" });
    identityAvailable = true;
    peerPid = 7;
    expect(await request(server.path, report())).toMatchObject({
      ok: false,
      error: "reporter_identity_mismatch",
    });
    peerPid = 42;
    expect(await request(server.path, report({
      pid: 7,
      agent: "pi",
      seq: Number.MAX_SAFE_INTEGER,
    }))).toMatchObject({ ok: false, error: "invalid_request" });
    expect(await request(server.path, report({ message: "x".repeat(513) })))
      .toMatchObject({ ok: false, error: "invalid_request" });
    expect(received).toEqual([]);
  });

  test("rejects a different peer process reporting for the live agent", async () => {
    if (process.platform === "win32") return;
    const dir = await mkdtemp(join(tmpdir(), "roost-agent-report-"));
    cleanupDirs.push(dir);
    let attestedPeerPid: number | undefined;
    let registryCalls = 0;
    server = await startAgentReportServer({
      socketPath: join(dir, "agent.sock"),
      detector: {
        reportingAgentForSession: async (_claimed, peerPid) => {
          attestedPeerPid = peerPid;
          return peerPid === process.pid ? { agentId: "omp", pid: peerPid } : null;
        },
      },
      registry: {
        reportIntegration: () => {
          registryCalls++;
          return true;
        },
      },
    });
    const child = Bun.spawn([
      process.execPath,
      "-e",
      [
        'const net = require("node:net");',
        'const socket = net.createConnection(process.env.ROOST_TEST_ENDPOINT);',
        'socket.setEncoding("utf8");',
        'socket.on("connect", () => socket.write(',
        'Buffer.from(process.env.ROOST_TEST_BODY, "base64").toString("utf8")));',
        'socket.on("data", (chunk) => { process.stdout.write(chunk); socket.end(); });',
        'socket.on("error", () => process.exit(2));',
      ].join(""),
    ], {
      env: {
        ...process.env,
        ROOST_TEST_ENDPOINT: server.path,
        ROOST_TEST_BODY: Buffer.from(report()).toString("base64"),
      },
      stdout: "pipe",
      stderr: "ignore",
    });

    const [output, exitCode] = await Promise.all([
      new Response(child.stdout).text(),
      child.exited,
    ]);
    expect(exitCode).toBe(0);
    expect(JSON.parse(output.trim())).toEqual({
      ok: false,
      error: "reporter_identity_mismatch",
    });
    expect(attestedPeerPid).toBe(child.pid);
    expect(registryCalls).toBe(0);
  });

  test("caps oversized local input", async () => {
    const dir = await mkdtemp(join(tmpdir(), "roost-agent-report-"));
    cleanupDirs.push(dir);
    server = await startAgentReportServer({
      socketPath: join(dir, "agent.sock"),
      detector: {
        reportingAgentForSession: async (_claimed, attestedPid) => (
          attestedPid === 42 ? { agentId: "omp", pid: 42 } : null
        ),
      },
      peerProcessIdReader: { read: () => 42 },
      registry: { reportIntegration: () => true },
    });
    expect(await request(server.path, `${"x".repeat(8_300)}\n`))
      .toMatchObject({ ok: false, error: "request_too_large" });
  });
});
