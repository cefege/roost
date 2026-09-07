// Pins screen authority over a non-full-lifecycle integration. An integration
// that only proves identity and activity can claim `working` while the grid
// shows a prompt waiting on a human; a visible blocker corrects it. Reporters
// for runtimes whose integration covers the whole lifecycle keep their state.
import { describe, expect, test } from "bun:test";
import type { AgentStatusUpdate } from "@roost/shared/wire";
import type { BuiltinAgentId } from "../src/agent-status/process-scan.ts";
import { AgentStatusRegistry } from "../src/agent-status/registry.ts";

const SESSION_ID = "11111111-1111-4111-8111-111111111111";

function harness() {
  const published: AgentStatusUpdate[] = [];
  const registry = new AgentStatusRegistry({
    publish: (status) => published.push(status),
    startLeaseTimer: false,
  });
  return { published, registry };
}

function reportPair(
  registry: AgentStatusRegistry,
  agentId: string,
  integrationState: "working" | "blocked" | "idle",
  visibleBlocker: boolean,
): void {
  registry.reportScreen(SESSION_ID, {
    agentId: agentId as BuiltinAgentId,
    processId: 40,
    state: "idle",
    visibleBlocker,
  });
  registry.reportIntegration({
    sessionId: SESSION_ID,
    agentId: agentId as BuiltinAgentId,
    processId: 40,
    state: integrationState,
    seq: 1,
    active: true,
  });
}

describe("visible-blocker override of integration state", () => {
  test("a visible blocker corrects an identity-only integration", () => {
    const { published, registry } = harness();
    reportPair(registry, "codex", "working", true);
    expect(published.at(-1)?.state).toBe("blocked");
    expect(published.at(-1)?.source).toBe("screen");
    registry.dispose();
  });

  test("a full-lifecycle integration keeps its own state", () => {
    for (const agentId of ["omp", "pi"]) {
      const { published, registry } = harness();
      reportPair(registry, agentId, "working", true);
      expect(published.at(-1)?.state).toBe("working");
      expect(published.at(-1)?.source).toBe("integration");
      registry.dispose();
    }
  });

  test("no visible blocker leaves the integration state alone", () => {
    const { published, registry } = harness();
    reportPair(registry, "codex", "working", false);
    expect(published.at(-1)?.state).toBe("working");
    expect(published.at(-1)?.source).toBe("integration");
    registry.dispose();
  });

  test("an inherited prototype key is not a full-lifecycle authority", () => {
    const { published, registry } = harness();
    reportPair(registry, "constructor", "working", true);
    expect(published.at(-1)?.state).toBe("blocked");
    registry.dispose();
  });

  test("a screen blocker for a different agent does not override", () => {
    const { published, registry } = harness();
    registry.reportScreen(SESSION_ID, {
      agentId: "gemini",
      processId: 41,
      state: "idle",
      visibleBlocker: true,
    });
    registry.reportIntegration({
      sessionId: SESSION_ID,
      agentId: "codex",
      processId: 40,
      state: "working",
      seq: 1,
      active: true,
    });
    expect(published.at(-1)?.state).toBe("working");
    registry.dispose();
  });
});
