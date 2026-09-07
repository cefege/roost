// Acquisition grace and screen-state stabilization for one observed agent
// process. Exercises StableScreenDetector directly; process recognition and
// registry arbitration live in agent-status.test.ts, and the registry's
// visible-blocker override lives in agent-status-visible-blocker.test.ts.

import { describe, expect, test } from "bun:test";
import type { ManifestDetection } from "../src/agent-status/manifest-engine.ts";
import type {
  AgentProcessIdentity,
  BuiltinAgentId,
} from "../src/agent-status/process-scan.ts";
import { StableScreenDetector } from "../src/agent-status/stable-detection.ts";

const sessionId = "11111111-1111-4111-8111-111111111111";

function detection(state: "working" | "blocked" | "idle", visible = false): ManifestDetection {
  return {
    state,
    visibleIdle: visible && state === "idle",
    visibleBlocker: visible && state === "blocked",
    visibleWorking: visible && state === "working",
    skipStateUpdate: false,
    matchedRuleId: visible ? "visible" : null,
  };
}

function agentIdentity(agentId: BuiltinAgentId, pid = 20): AgentProcessIdentity {
  return { agentId, pid };
}

describe("stable screen transitions", () => {
  /** Closes the acquisition grace window so a test can exercise the working→idle
   *  stabilizer from a settled identity. */
  function acquire(stable: StableScreenDetector): void {
    expect(stable.observe(sessionId, agentIdentity("codex"), detection("working", true), 0))
      .toBeNull();
    expect(stable.observe(sessionId, agentIdentity("codex"), detection("working", true), 1))
      .toEqual({ agentId: "codex", processId: 20, state: "working", visibleBlocker: false });
  }

  test("withholds the first evaluation of a newly acquired identity", () => {
    const stable = new StableScreenDetector();
    expect(stable.observe(sessionId, agentIdentity("codex"), detection("blocked", true), 0))
      .toBeNull();
    expect(stable.current(sessionId)?.state).toBe("blocked");
    expect(stable.observe(sessionId, agentIdentity("codex"), detection("working", true), 100))
      .toBeNull();
    expect(stable.observe(sessionId, agentIdentity("codex"), detection("working", true), 200))
      .toEqual({ agentId: "codex", processId: 20, state: "working", visibleBlocker: false });
  });

  test("publishes a disagreeing acquisition once the grace window expires", () => {
    const stable = new StableScreenDetector();
    expect(stable.observe(sessionId, agentIdentity("codex"), detection("blocked", true), 0))
      .toBeNull();
    expect(stable.observe(sessionId, agentIdentity("codex"), detection("working", true), 2_999))
      .toBeNull();
    expect(stable.observe(sessionId, agentIdentity("codex"), detection("blocked", true), 3_000))
      .toEqual({ agentId: "codex", processId: 20, state: "blocked", visibleBlocker: true });
  });

  test("re-arms the grace window when the process behind an agent is replaced", () => {
    const stable = new StableScreenDetector();
    acquire(stable);
    expect(stable.observe(sessionId, agentIdentity("codex", 21), detection("blocked", true), 2))
      .toBeNull();
  });

  test("holds transient working-to-plain-idle spinner loss", () => {
    const stable = new StableScreenDetector();
    acquire(stable);
    expect(stable.observe(sessionId, agentIdentity("codex"), detection("idle"), 100)).toBeNull();
    expect(stable.observe(sessionId, agentIdentity("codex"), detection("idle"), 200)).toBeNull();
    expect(stable.observe(sessionId, agentIdentity("codex"), detection("working", true), 250)).toBeNull();
    expect(stable.current(sessionId)?.state).toBe("working");
  });

  test("confirms sustained plain idle but accepts visible idle immediately", () => {
    const stable = new StableScreenDetector();
    acquire(stable);
    expect(stable.observe(sessionId, agentIdentity("codex"), detection("idle"), 100)).toBeNull();
    expect(stable.observe(sessionId, agentIdentity("codex"), detection("idle"), 200)).toBeNull();
    expect(stable.observe(sessionId, agentIdentity("codex"), detection("idle"), 300)).toBeNull();
    expect(stable.observe(sessionId, agentIdentity("codex"), detection("idle"), 400))
      .toEqual({ agentId: "codex", processId: 20, state: "idle", visibleBlocker: false });

    stable.observe(sessionId, agentIdentity("codex"), detection("working", true), 500);
    expect(stable.observe(sessionId, agentIdentity("codex"), detection("idle", true), 501))
      .toEqual({ agentId: "codex", processId: 20, state: "idle", visibleBlocker: false });
  });

  test("holds the previous state on skip-state screens", () => {
    const stable = new StableScreenDetector();
    acquire(stable);
    expect(stable.observe(sessionId, agentIdentity("codex"), {
      ...detection("idle"), state: "unknown", skipStateUpdate: true,
    }, 100)).toBeNull();
    expect(stable.current(sessionId)?.state).toBe("working");
  });
});
