import { describe, expect, test } from "bun:test";
import type { AgentStatusUpdate } from "@roost/shared";
import { evaluateManifest, type ManifestDetection } from "../src/agent-status/manifest-engine.ts";
import { AGENT_MANIFESTS } from "../src/agent-status/manifests.ts";
import {
  AgentProcessScanner,
  BUILTIN_AGENT_COMMANDS,
  findAgentProcessIdentity,
  identifyAgentProcess,
  type BuiltinAgentId,
  type ProcessRecord,
} from "../src/agent-status/process-scan.ts";
import { AgentStatusRegistry } from "../src/agent-status/registry.ts";
import { StableScreenDetector } from "../src/agent-status/stable-detection.ts";
import { _scanAgentOsc } from "../src/terminal-stream-scan.ts";

const sessionId = "11111111-1111-4111-8111-111111111111";

function processRecord(patch: Partial<ProcessRecord> = {}): ProcessRecord {
  return { pid: 20, ppid: 10, pgid: 20, tpgid: 20, comm: "bash", args: "/bin/bash", ...patch };
}

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

describe("agent process identity", () => {
  test("identifies every built-in from a live descendant command", () => {
    const root = processRecord({ pid: 10, ppid: 1 });
    for (const [agentId, commands] of Object.entries(BUILTIN_AGENT_COMMANDS) as Array<
      [BuiltinAgentId, readonly string[]]
    >) {
      const child = processRecord({ comm: commands[0], args: commands[0] });
      expect(findAgentProcessIdentity([root, child], 10)).toEqual({ agentId, pid: 20 });
    }
  });

  test("recognizes runtime package launchers without reading shell command text", () => {
    expect(identifyAgentProcess(processRecord({
      comm: "bun",
      args: "bun /home/me/node_modules/@oh-my-pi/pi-coding-agent/dist/cli.js",
    }))).toBe("omp");
    expect(identifyAgentProcess(processRecord({
      comm: "bash",
      args: "/bin/bash -c echo codex gemini omp",
    }))).toBeNull();
  });

  test("requires two consecutive misses before releasing identity", async () => {
    const root = processRecord({ pid: 10, ppid: 1 });
    let records: ProcessRecord[] = [root, processRecord({ comm: "codex", args: "codex" })];
    const scanner = new AgentProcessScanner(async () => records, 0);
    const roots = [{ sessionId, childPid: 10 }];
    expect((await scanner.scanAgents(roots)).get(sessionId)?.agentId).toBe("codex");
    records = [root];
    expect((await scanner.scanAgents(roots)).get(sessionId)?.agentId).toBe("codex");
    expect((await scanner.scanAgents(roots)).has(sessionId)).toBe(false);
  });

  test("maps a reporter pid to its open session root", async () => {
    const records = [
      processRecord({ pid: 10, ppid: 1 }),
      processRecord({ pid: 20, ppid: 10 }),
      processRecord({ pid: 30, ppid: 20 }),
    ];
    const scanner = new AgentProcessScanner(async () => records, 0);
    expect(await scanner.sessionForPid(30, [{ sessionId, childPid: 10 }])).toBe(sessionId);
    expect(await scanner.sessionForPid(99, [{ sessionId, childPid: 10 }])).toBeNull();
  });

  test("refreshes a throttled snapshot for a newly started reporter", async () => {
    const root = processRecord({ pid: 10, ppid: 1 });
    let records: ProcessRecord[] = [root];
    let snapshots = 0;
    const scanner = new AgentProcessScanner(async () => {
      snapshots++;
      return records;
    }, 10_000);
    const roots = [{ sessionId, childPid: 10 }];
    await scanner.scanAgents(roots);
    records = [root, processRecord({ pid: 30, ppid: 10 })];

    expect(await scanner.sessionForPid(30, roots)).toBe(sessionId);
    expect(snapshots).toBe(2);
  });
});

describe("pinned manifest engine", () => {
  const workingFixtures: Record<BuiltinAgentId, { screen?: string; oscTitle?: string; oscProgress?: string }> = {
    codex: { oscTitle: "codex ⠋ task" },
    gemini: { screen: "esc to cancel" },
    opencode: { screen: "press esc to interrupt" },
    cursor: { screen: "ctrl+c to stop" },
    amp: { oscTitle: "⠋ task" },
    copilot: { screen: "esc again to cancel" },
    droid: { screen: "⠋ Running\nesc to stop" },
    grok: { oscProgress: "4;1;-1" },
    pi: { screen: "Working..." },
    omp: { oscTitle: "π ⠋ task" },
  };

  test("detects working fixtures for all ten built-ins", () => {
    for (const [agentId, fixture] of Object.entries(workingFixtures) as Array<
      [BuiltinAgentId, (typeof workingFixtures)[BuiltinAgentId]]
    >) {
      expect(evaluateManifest(AGENT_MANIFESTS[agentId], {
        screen: fixture.screen ?? "",
        oscTitle: fixture.oscTitle,
        oscProgress: fixture.oscProgress,
      }).state).toBe("working");
    }
  });

  test("honors blocker priority, visible idle, and skip-state screens", () => {
    expect(evaluateManifest(AGENT_MANIFESTS.codex, {
      screen: "• Working (esc to interrupt)", oscTitle: "Action Required",
    }).state).toBe("blocked");
    const idle = evaluateManifest(AGENT_MANIFESTS.omp, { screen: "", oscTitle: "π > repo" });
    expect(idle).toMatchObject({ state: "idle", visibleIdle: true });
    const skipped = evaluateManifest(AGENT_MANIFESTS.codex, {
      screen: "› prompt\n↑/↓ to scroll pgup/pgdn to move home/end to jump q to quit esc to edit prev",
    });
    expect(skipped).toMatchObject({ state: "unknown", skipStateUpdate: true });
  });

  test("defaults a known process to idle rather than matching transcript identity", () => {
    expect(evaluateManifest(AGENT_MANIFESTS.gemini, {
      screen: "old output: codex esc to interrupt",
    }).state).toBe("idle");
  });
});

describe("stable screen transitions", () => {
  test("holds transient working-to-plain-idle spinner loss", () => {
    const stable = new StableScreenDetector();
    expect(stable.observe(sessionId, "codex", detection("working", true), 0)).toEqual({ agentId: "codex", state: "working" });
    expect(stable.observe(sessionId, "codex", detection("idle"), 100)).toBeNull();
    expect(stable.observe(sessionId, "codex", detection("idle"), 200)).toBeNull();
    expect(stable.observe(sessionId, "codex", detection("working", true), 250)).toBeNull();
    expect(stable.current(sessionId)?.state).toBe("working");
  });

  test("confirms sustained plain idle but accepts visible idle immediately", () => {
    const stable = new StableScreenDetector();
    stable.observe(sessionId, "codex", detection("working", true), 0);
    expect(stable.observe(sessionId, "codex", detection("idle"), 100)).toBeNull();
    expect(stable.observe(sessionId, "codex", detection("idle"), 200)).toBeNull();
    expect(stable.observe(sessionId, "codex", detection("idle"), 300)).toBeNull();
    expect(stable.observe(sessionId, "codex", detection("idle"), 400)).toEqual({ agentId: "codex", state: "idle" });

    stable.observe(sessionId, "codex", detection("working", true), 500);
    expect(stable.observe(sessionId, "codex", detection("idle", true), 501)).toEqual({ agentId: "codex", state: "idle" });
  });

  test("holds the previous state on skip-state screens", () => {
    const stable = new StableScreenDetector();
    stable.observe(sessionId, "codex", detection("working", true), 0);
    expect(stable.observe(sessionId, "codex", {
      ...detection("idle"), state: "unknown", skipStateUpdate: true,
    }, 100)).toBeNull();
    expect(stable.current(sessionId)?.state).toBe("working");
  });
});

describe("integration and screen arbitration", () => {
  test("live integration wins, expires to screen, and derives completion revisions", () => {
    let now = 1_000;
    const published: AgentStatusUpdate[] = [];
    const registry = new AgentStatusRegistry({
      publish: (status) => published.push(status), now: () => now,
      leaseMs: 100, startLeaseTimer: false,
    });
    registry.reportScreen(sessionId, { agentId: "omp", state: "working" });
    expect(registry.reportIntegration({
      sessionId, agentId: "omp", state: "blocked", seq: 1, active: true,
    })).toBe(true);
    registry.reportScreen(sessionId, { agentId: "omp", state: "idle" });
    expect(published.at(-1)?.state).toBe("blocked");
    expect(registry.reportIntegration({
      sessionId, agentId: "omp", state: "working", seq: 1, active: true,
    })).toBe(false);
    now += 101;
    registry.expireLeases();
    const completed = published.at(-1)!;
    expect(completed.state).toBe("idle");
    expect(completed.completed_revision).toBe(completed.revision);
    registry.dispose();
  });

  test("heartbeats do not fan out; reconnect resend preserves revision", () => {
    let now = 2_000;
    const published: AgentStatusUpdate[] = [];
    const registry = new AgentStatusRegistry({
      publish: (status) => published.push(status), now: () => now,
      startLeaseTimer: false,
    });
    registry.reportIntegration({ sessionId, agentId: "pi", state: "working", seq: 1, active: true });
    const revision = published[0]!.revision;
    now++;
    registry.reportIntegration({ sessionId, agentId: "pi", state: "working", seq: 2, active: true });
    expect(published).toHaveLength(1);
    registry.resend();
    expect(published).toHaveLength(2);
    expect(published[1]!.revision).toBe(revision);
    registry.closeSession(sessionId);
    expect(published.at(-1)).toMatchObject({ active: false });
    registry.dispose();
  });
});

describe("agent OSC scanner", () => {
  test("bridges split title and progress sequences", () => {
    const first = _scanAgentOsc("text\x1b]0;π ⠋ bu");
    expect(first.title).toBeNull();
    const second = _scanAgentOsc(first.carry + "ild\x07\x1b]9;4;1;-1\x1b\\");
    expect(second).toMatchObject({ title: "π ⠋ build", progress: "4;1;-1", carry: "" });
  });
});
