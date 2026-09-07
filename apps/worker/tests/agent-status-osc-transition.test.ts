// Pins the OSC-evidence clear on agent transition. Retained OSC title/progress
// is the top-priority input in several manifests, so a replacement process in
// the same PTY must not be judged by the previous agent's final title. Drives
// AgentScreenDetector against a scripted process scanner, the real manifests
// and a recording registry.
import { describe, expect, test } from "bun:test";
import { DEFAULT_COLOR } from "@roost/shared/cell";
import type { AgentStatusUpdate } from "@roost/shared/wire";
import type { CellData, TerminalCore } from "@wterm/core";
import type { SessionManager } from "../src/session-manager.ts";
import { AgentScreenDetector } from "../src/agent-status/detector.ts";
import {
  AgentProcessScanner,
  type BuiltinAgentId,
} from "../src/agent-status/process-scan.ts";
import { AgentStatusRegistry } from "../src/agent-status/registry.ts";

const SESSION_ID = "11111111-1111-4111-8111-111111111111";
const BLOCKED_TITLE = "Action Required — review the patch";

function makeGridCore(): TerminalCore {
  const blank: CellData = {
    char: " ".codePointAt(0)!,
    fg: DEFAULT_COLOR,
    bg: DEFAULT_COLOR,
    flags: 0,
    fgRgb: undefined,
    bgRgb: undefined,
  };
  return {
    getCols: () => 8,
    getRows: () => 2,
    getCell: () => blank,
  } as unknown as TerminalCore;
}

function makeHarness(agentId: BuiltinAgentId, oscTitle: string) {
  const record: Record<string, unknown> = {
    sessionId: SESSION_ID,
    channelId: 7,
    childPid: 4_321,
    wtermCore: makeGridCore(),
    rawOscTitle: oscTitle,
    rawOscProgress: "",
  };
  const sessions = {
    allSessions: () => [record],
    getBySessionId: (id: string) => (record.sessionId === id ? record : undefined),
  } as unknown as SessionManager;
  const published: AgentStatusUpdate[] = [];
  const registry = new AgentStatusRegistry({
    publish: (status) => published.push(status),
    startLeaseTimer: false,
  });
  const identities = new Map<string, { agentId: BuiltinAgentId; pid: number }>([
    [SESSION_ID, { agentId, pid: 4_321 }],
  ]);
  const scanner = {
    scanAgents: async () => identities,
    scanReportingAgent: async () => null,
  } as unknown as AgentProcessScanner;
  let nowMs = 1_000;
  const detector = new AgentScreenDetector(sessions, registry, scanner, {
    now: () => nowMs,
  });
  /** Advances past the screen-rescan gate and the acquisition grace window, so
   *  a settled identity has actually published. */
  const settle = async () => {
    for (let pass = 0; pass < 3; pass++) {
      nowMs += 4_000;
      await detector.scanNow();
      await detector.scanNow();
    }
  };
  return { detector, identities, published, record, settle };
}

describe("agent-status OSC evidence on agent transition", () => {
  test("a replacement agent is not judged by the previous agent's title", async () => {
    const { detector, identities, published, record, settle } = makeHarness(
      "codex",
      BLOCKED_TITLE,
    );
    await settle();
    expect(published.at(-1)?.state).toBe("blocked");

    identities.set(SESSION_ID, { agentId: "omp", pid: 5_555 });
    await settle();
    expect(record.rawOscTitle).toBe("");
    expect(String(published.at(-1)?.agent_id)).toBe("omp");
    expect(published.at(-1)?.state).toBe("idle");
    detector.dispose();
  });

  test("a stale idle title cannot hold a working replacement at idle", async () => {
    const { detector, identities, published, record, settle } = makeHarness(
      "grok",
      "session - grok",
    );
    await settle();
    expect(published.at(-1)?.state).toBe("idle");

    identities.set(SESSION_ID, { agentId: "omp", pid: 5_555 });
    await settle();
    expect(record.rawOscTitle).toBe("");
    record.rawOscTitle = "π ⠋ building";
    await settle();
    expect(published.at(-1)?.state).toBe("working");
    detector.dispose();
  });

  test("a same-agent PID replacement also drops the retained title", async () => {
    const { detector, identities, record, settle } = makeHarness("codex", BLOCKED_TITLE);
    await settle();
    identities.set(SESSION_ID, { agentId: "codex", pid: 9_999 });
    await settle();
    expect(record.rawOscTitle).toBe("");
    detector.dispose();
  });

  test("first acquisition keeps evidence the new process already emitted", async () => {
    const { detector, published, record, settle } = makeHarness("codex", BLOCKED_TITLE);
    await settle();
    expect(record.rawOscTitle).toBe(BLOCKED_TITLE);
    expect(published.at(-1)?.state).toBe("blocked");
    detector.dispose();
  });

  test("a stable identity does not clear evidence on every pass", async () => {
    const { detector, record, settle } = makeHarness("codex", BLOCKED_TITLE);
    await settle();
    await settle();
    await settle();
    expect(record.rawOscTitle).toBe(BLOCKED_TITLE);
    detector.dispose();
  });
});
