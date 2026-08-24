// Screen-read gate — pins that back-to-back status scans inside
// SCREEN_RESCAN_MIN_MS read each session's visible grid once, that the grid is
// re-read after the window, and that a vanished session's gate entry is
// dropped so its return re-reads immediately. The grid-text read is the
// expensive part of a status scan; this gate keeps chatty sessions from
// starving PTY parsing and baseline emission.
import { describe, expect, test, vi } from "bun:test";
import { DEFAULT_COLOR } from "@roost/shared/cell";
import type { CellData, TerminalCore } from "@wterm/core";
import type { SessionManager } from "../src/session-manager.ts";
import { AgentScreenDetector } from "../src/agent-status/detector.ts";
import {
  releaseAgentStatusCapabilities,
  withAgentStatusEnvironment,
} from "../src/agent-status/environment.ts";
import {
  AgentProcessScanner,
  type BuiltinAgentId,
} from "../src/agent-status/process-scan.ts";
import { AgentStatusRegistry } from "../src/agent-status/registry.ts";

const SESSION_ID = "11111111-1111-4111-8111-111111111111";

function blankCell(): CellData {
  return {
    char: " ".codePointAt(0)!,
    fg: DEFAULT_COLOR,
    bg: DEFAULT_COLOR,
    flags: 0,
    fgRgb: undefined,
    bgRgb: undefined,
  };
}

/** Minimal core whose getRows() call count IS the visible-grid read count:
 *  readVisibleScreen calls getCols()/getRows() exactly once per read. */
function makeGridCore(reads: { count: number }): TerminalCore {
  return {
    getCols: () => 8,
    getRows: () => {
      reads.count++;
      return 2;
    },
    getCell: () => blankCell(),
  } as unknown as TerminalCore;
}

function makeDetector(nowMs: { value: number }) {
  const reads = { count: 0 };
  const scans = { count: 0 };
  const records: Array<Record<string, unknown>> = [];
  const addSession = () => {
    records.push({
      sessionId: SESSION_ID,
      channelId: 7,
      childPid: 4321,
      wtermCore: makeGridCore(reads),
      rawOscTitle: null,
      rawOscProgress: null,
    });
  };
  const sessions = {
    allSessions: () => records,
    getBySessionId: (id: string) => records.find((r) => r.sessionId === id),
  } as unknown as SessionManager;
  const registry = new AgentStatusRegistry({
    publish: () => {},
    startLeaseTimer: false,
  });
  const identities = new Map<string, { agentId: BuiltinAgentId }>([
    [SESSION_ID, { agentId: "omp" }],
  ]);
  const scanner = {
    scanAgents: async () => {
      scans.count++;
      return identities;
    },
    sessionForPid: async () => null,
  } as unknown as AgentProcessScanner;
  const detector = new AgentScreenDetector(sessions, registry, scanner, {
    now: () => nowMs.value,
  });
  return {
    detector,
    reads,
    scans,
    addSession,
    clearSessions: () => records.splice(0, records.length),
  };
}

describe("agent-status screen-read gate", () => {
  test("back-to-back scans inside the window read the visible grid once", async () => {
    const now = { value: 1_000 };
    const { detector, reads, addSession } = makeDetector(now);
    addSession();
    await detector.scanNow();
    expect(reads.count).toBe(1);
    now.value += 150;
    await detector.scanNow();
    expect(reads.count).toBe(1);
    now.value += 49;
    await detector.scanNow();
    expect(reads.count).toBe(1);
    // Exactly 200 ms elapsed is not "newer than" the minimum: re-read.
    now.value += 1;
    await detector.scanNow();
    expect(reads.count).toBe(2);
    detector.dispose();
  });

  test("a vanished session's gate entry is pruned so its return re-reads immediately", async () => {
    const now = { value: 1_000 };
    const { detector, reads, addSession, clearSessions } = makeDetector(now);
    addSession();
    await detector.scanNow();
    expect(reads.count).toBe(1);
    // Session disappears: the next scan prunes its gate entry while it is
    // absent, so its quick return cannot hide behind the old timestamp.
    clearSessions();
    await detector.scanNow();
    addSession();
    now.value += 10;
    await detector.scanNow();
    expect(reads.count).toBe(2);
    detector.dispose();
  });

  test("closeSession drops the gate entry so a reused session id re-reads", async () => {
    const now = { value: 1_000 };
    const { detector, reads, addSession } = makeDetector(now);
    addSession();
    await detector.scanNow();
    expect(reads.count).toBe(1);
    detector.closeSession(SESSION_ID);
    now.value += 10;
    await detector.scanNow();
    expect(reads.count).toBe(2);
    detector.dispose();
  });

  test("a coalesce timer armed for a closing session is cancelled, not fired", async () => {
    vi.useFakeTimers();
    try {
      const now = { value: 1_000 };
      const { detector, reads, scans, addSession } = makeDetector(now);
      addSession();
      await detector.scanNow();
      expect(reads.count).toBe(1);
      const scansBeforeClose = scans.count;
      detector.schedule(7);
      detector.closeSession(SESSION_ID);
      // Past OUTPUT_SCAN_COALESCE_MS (40) but before the process-scan
      // interval tick (250), so a fired timer would show up here and the
      // interval itself cannot.
      vi.advanceTimersByTime(200);
      await Promise.resolve();
      expect(scans.count).toBe(scansBeforeClose);
      detector.dispose();
    } finally {
      vi.useRealTimers();
    }
  });

  test("closeSession evicts the session's cached report capability", () => {
    const otherId = "22222222-2222-4222-8222-222222222222";
    withAgentStatusEnvironment({}, SESSION_ID);
    withAgentStatusEnvironment({}, otherId);
    const now = { value: 1_000 };
    const { detector } = makeDetector(now);
    detector.closeSession(SESSION_ID);
    // The closed session's entry is already gone (release finds nothing to
    // drop); the unclosed session's entry survives eviction.
    expect(releaseAgentStatusCapabilities(SESSION_ID)).toBe(0);
    expect(releaseAgentStatusCapabilities(otherId)).toBe(1);
    detector.dispose();
  });
});
