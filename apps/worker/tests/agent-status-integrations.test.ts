// Focused lifecycle tests for the installed OMP and Pi status integrations.
// A local collector observes only session-authorized state fields while event
// mappings retain their debounce, blocker, heartbeat, and shutdown behavior.
import { afterEach, describe, expect, test, vi } from "bun:test";
import net from "node:net";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

type Handler = (event?: unknown, context?: unknown) => unknown;
interface HarnessApi {
  events: { on(name: string, handler: Handler): void };
  on(name: string, handler: Handler): void;
}
interface IntegrationModule { default(api: HarnessApi): void }
interface StatusParams {
  session_id: string;
  state: "working" | "blocked" | "idle";
  message?: string;
  active: boolean;
}

const SESSION_ID = "11111111-1111-4111-8111-111111111111";
const originalEnv: Record<string, string | undefined> = {};
const envNames = [
  "ROOST_AGENT_ENDPOINT", "ROOST_AGENT_CAPABILITY", "ROOST_SESSION_ID", "ROOST_AGENT_STATUS_DISABLED",
  "ROOST_AGENT_HEARTBEAT_MS", "ROOST_OMP_IDLE_DEBOUNCE_MS", "ROOST_OMP_RETRY_GRACE_MS",
];
for (const name of envNames) originalEnv[name] = process.env[name];
const cleanupDirs: string[] = [];
let reportServer: net.Server | null = null;

afterEach(async () => {
  if (reportServer) {
    const closed = Promise.withResolvers<void>();
    reportServer.close(() => closed.resolve());
    await closed.promise;
  }
  reportServer = null;
  await Promise.all(cleanupDirs.splice(0).map((path) => rm(path, { recursive: true, force: true })));
  for (const name of envNames) {
    const value = originalEnv[name];
    if (value === undefined) delete process.env[name];
    else process.env[name] = value;
  }
  vi.useRealTimers();
});

function createHarness(): {
  api: HarnessApi;
  handlers: Map<string, Handler>;
  eventHandlers: Map<string, Handler>;
} {
  const handlers = new Map<string, Handler>();
  const eventHandlers = new Map<string, Handler>();
  return {
    handlers,
    eventHandlers,
    api: {
      on: (name, handler) => { handlers.set(name, handler); },
      events: { on: (name, handler) => { eventHandlers.set(name, handler); } },
    },
  };
}


async function startCollector(): Promise<{
  reports: StatusParams[];
  path: string;
  waitFor(count: number): Promise<void>;
}> {
  const dir = await mkdtemp(join(tmpdir(), "roost-agent-integration-"));
  cleanupDirs.push(dir);
  const path = join(dir, "agent.sock");
  const reports: StatusParams[] = [];
  const waiters: Array<{ count: number; resolve: () => void }> = [];
  reportServer = net.createServer((socket) => {
    socket.setEncoding("utf8");
    let buffer = "";
    socket.on("data", (chunk) => {
      buffer += chunk;
      const newline = buffer.indexOf("\n");
      if (newline < 0) return;
      const request = JSON.parse(buffer.slice(0, newline)) as { params: StatusParams };
      reports.push(request.params);
      for (let index = waiters.length - 1; index >= 0; index--) {
        const waiter = waiters[index]!;
        if (reports.length < waiter.count) continue;
        waiters.splice(index, 1);
        waiter.resolve();
      }
      socket.end('{"ok":true}\n');
    });
  });
  const listening = Promise.withResolvers<void>();
  reportServer.listen(path, listening.resolve);
  await listening.promise;
  process.env.ROOST_AGENT_ENDPOINT = path;
  process.env.ROOST_AGENT_CAPABILITY = "test-capability";
  process.env.ROOST_SESSION_ID = SESSION_ID;
  return {
    reports,
    path,
    waitFor(count: number): Promise<void> {
      if (reports.length >= count) return Promise.resolve();
      const pending = Promise.withResolvers<void>();
      waiters.push({ count, resolve: pending.resolve });
      return pending.promise;
    },
  };
}

// Cache-busted imports are intentional: each test must evaluate an installed
// extension after setting its process-level socket and timer environment.
async function importIntegration(agent: "omp" | "pi"): Promise<IntegrationModule> {
  const url = new URL(
    `../src/agent-status/integrations/${agent}/roost-agent-state.ts?test=${Date.now()}-${Math.random()}`,
    import.meta.url,
  );
  return await import(url.href) as unknown as IntegrationModule;
}

// The reference asset has the same process-environment evaluation boundary.
async function importOmpReferenceIntegration(): Promise<IntegrationModule> {
  const url = new URL(
    `../src/agent-status/integrations/omp/roost-agent-reference.ts?test=${Date.now()}-${Math.random()}`,
    import.meta.url,
  );
  return await import(url.href) as unknown as IntegrationModule;
}

/** OMP's real SessionStopEvent, per SessionStopEvent in
 *  @oh-my-pi/pi-coding-agent dist/types/extensibility/shared-events.d.ts. */
function sessionStopEvent(sessionFile: string): Record<string, unknown> {
  return {
    type: "session_stop",
    messages: [],
    turn_id: 3,
    session_id: "stopped-id",
    session_file: sessionFile,
    stop_hook_active: false,
    signal: new AbortController().signal,
  };
}

describe("OMP lifecycle integration", () => {
  test("preserves debounce, nested blockers, ask events, and release", async () => {
    const { reports, waitFor } = await startCollector();
    vi.useFakeTimers();
    process.env.ROOST_AGENT_HEARTBEAT_MS = "1000";
    process.env.ROOST_OMP_IDLE_DEBOUNCE_MS = "250";
    const { api, handlers } = createHarness();
    (await importIntegration("omp")).default(api);
    const context = { hasUI: true, isIdle: () => true };

    handlers.get("session_start")?.({}, context);
    await waitFor(1);
    expect(reports.at(-1)).toMatchObject({ state: "idle", active: true });
    expect(Object.keys(reports.at(-1)!)).not.toContain("pid");
    expect(Object.keys(reports.at(-1)!)).not.toContain("agent");
    expect(Object.keys(reports.at(-1)!)).not.toContain("seq");
    handlers.get("agent_start")?.({}, context);
    await waitFor(2);
    expect(reports.at(-1)?.state).toBe("working");

    const approval = { reason: "Approve command", toolName: "bash" };
    handlers.get("tool_approval_requested")?.(approval, context);
    await waitFor(3);
    handlers.get("tool_approval_requested")?.(approval, context);
    handlers.get("tool_approval_resolved")?.({}, context);
    expect(reports).toHaveLength(3);
    expect(reports.at(-1)).toMatchObject({ state: "blocked", message: "Approve command" });
    handlers.get("tool_approval_resolved")?.({}, context);
    await waitFor(4);
    expect(reports.at(-1)?.state).toBe("working");

    handlers.get("tool_execution_start")?.({ toolName: "ask", args: { questions: [{ question: "Choose one" }] } }, context);
    await waitFor(5);
    expect(reports.at(-1)).toMatchObject({ state: "blocked", message: "Choose one" });
    handlers.get("tool_execution_end")?.({ toolName: "ask" }, context);
    await waitFor(6);
    handlers.get("agent_end")?.({ messages: [] });
    vi.advanceTimersByTime(250);
    await waitFor(7);
    expect(reports.at(-1)?.state).toBe("idle");

    handlers.get("session_shutdown")?.();
    await waitFor(8);
    expect(reports.at(-1)).toMatchObject({ state: "idle", active: false });
  });

  test("holds retryable failures and ignores duplicate late end", async () => {
    const { reports, waitFor } = await startCollector();
    vi.useFakeTimers();
    process.env.ROOST_AGENT_HEARTBEAT_MS = "1000";
    process.env.ROOST_OMP_RETRY_GRACE_MS = "250";
    const { api, handlers } = createHarness();
    (await importIntegration("omp")).default(api);
    const context = { hasUI: true, isIdle: () => true };
    handlers.get("session_start")?.({}, context);
    handlers.get("agent_start")?.({}, context);
    await waitFor(2);
    handlers.get("agent_end")?.({
      messages: [{ role: "assistant", stopReason: "error", errorMessage: "provider returned 503" }],
    });
    handlers.get("agent_end")?.({ messages: [] });
    expect(reports.at(-1)?.state).toBe("working");
    vi.advanceTimersByTime(250);
    await waitFor(3);
    expect(reports.at(-1)).toMatchObject({ state: "blocked", message: "provider returned 503" });
    handlers.get("session_shutdown")?.();
  });

  test("heartbeats the current state and can be disabled", async () => {
    const { reports, waitFor } = await startCollector();
    vi.useFakeTimers();
    process.env.ROOST_AGENT_HEARTBEAT_MS = "1000";
    const first = createHarness();
    (await importIntegration("omp")).default(first.api);
    first.handlers.get("session_start")?.({}, { hasUI: true, isIdle: () => true });
    await waitFor(1);
    vi.advanceTimersByTime(1000);
    await waitFor(2);
    expect(reports[0]).toMatchObject({ state: "idle", active: true });
    expect(reports[1]).toMatchObject({ state: "idle", active: true });
    first.handlers.get("session_shutdown")?.();
    await waitFor(3);

    process.env.ROOST_AGENT_STATUS_DISABLED = "1";
    const disabled = createHarness();
    (await importIntegration("omp")).default(disabled.api);
    expect(disabled.handlers.size).toBe(0);
  });

  test("prefers an absolute session path and falls back to the id", async () => {
    const { reports, waitFor } = await startCollector();
    process.env.ROOST_AGENT_STATUS_DISABLED = "1";
    const { api, handlers } = createHarness();
    (await importOmpReferenceIntegration()).default(api);
    const referenceReports = reports as unknown as Array<{
      session_id: string;
      reference: { kind: "id" | "path"; value: string } | null;
    }>;

    await handlers.get("session_start")?.({}, {
      sessionManager: {
        getSessionFile: () => "/opaque/start.jsonl",
        getSessionId: () => "start-id",
      },
    });
    await waitFor(1);
    await handlers.get("session_switch")?.({}, {
      sessionManager: {
        getSessionFile: () => undefined,
        getSessionId: () => "switched-id",
      },
    });
    await waitFor(2);
    // A relative session file resumes nothing, so it must not shadow the id
    // offered in the same call.
    await handlers.get("session_switch")?.({}, {
      sessionManager: {
        getSessionFile: () => "relative/session.jsonl",
        getSessionId: () => "relative-file-id",
      },
    });
    await waitFor(3);
    await handlers.get("session_stop")?.(
      sessionStopEvent("/opaque/stopped.jsonl"),
      {},
    );
    await waitFor(4);
    await handlers.get("session_stop")?.(
      sessionStopEvent("stopped-relative.jsonl"),
      {},
    );
    await waitFor(5);

    expect(referenceReports).toEqual([
      {
        session_id: SESSION_ID,
        reference: { kind: "path", value: "/opaque/start.jsonl" },
      },
      {
        session_id: SESSION_ID,
        reference: { kind: "id", value: "switched-id" },
      },
      {
        session_id: SESSION_ID,
        reference: { kind: "id", value: "relative-file-id" },
      },
      {
        session_id: SESSION_ID,
        reference: { kind: "path", value: "/opaque/stopped.jsonl" },
      },
      {
        session_id: SESSION_ID,
        reference: { kind: "id", value: "stopped-id" },
      },
    ]);
    expect(handlers.size).toBe(3);
  });
});

describe("Pi lifecycle integration", () => {
  test("reports TUI lifecycle, blockers, heartbeat, and release", async () => {
    const { reports, waitFor } = await startCollector();
    process.env.ROOST_AGENT_HEARTBEAT_MS = "1000";
    const { api, handlers, eventHandlers } = createHarness();
    (await importIntegration("pi")).default(api);
    const context = { mode: "tui", isIdle: () => true };
    handlers.get("session_start")?.({}, context);
    await waitFor(1);
    handlers.get("agent_start")?.({}, context);
    await waitFor(2);
    expect(reports.at(-1)?.state).toBe("working");
    eventHandlers.get("herdr:blocked")?.({ active: true, label: "Input needed" });
    await waitFor(3);
    expect(reports.at(-1)).toMatchObject({ state: "blocked", message: "Input needed" });
    eventHandlers.get("herdr:blocked")?.({ active: false });
    await waitFor(4);
    handlers.get("agent_settled")?.({}, context);
    await waitFor(5);
    expect(reports.at(-1)?.state).toBe("idle");
    handlers.get("session_shutdown")?.();
    await waitFor(6);
    expect(reports.at(-1)?.active).toBe(false);
  });

  test("ignores non-TUI sessions", async () => {
    const { reports } = await startCollector();
    const { api, handlers } = createHarness();
    (await importIntegration("pi")).default(api);
    handlers.get("session_start")?.({}, { mode: "rpc", isIdle: () => true });
    expect(reports).toHaveLength(0);
    expect(handlers.has("agent_start")).toBe(true);
  });
});
