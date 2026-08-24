// Detects which coding agents are actually running on this machine by
// combining a process scan with manifest evaluation over terminal screen text
// (spans/viewport rows), so a pane showing Claude/Codex activity maps to a
// concrete BuiltinAgentId even when no extension reported in.
import { log } from "@roost/shared/log";
import { spansText, viewportRowSpans } from "@roost/shared/cell";
import type { TerminalCore } from "@wterm/core";
import type { SessionManager } from "../session-manager.ts";
import { evaluateManifest } from "./manifest-engine.ts";
import { AGENT_MANIFESTS } from "./manifests.ts";
import {
  AgentProcessScanner,
  type SessionProcessRoot,
} from "./process-scan.ts";
import { AgentStatusRegistry } from "./registry.ts";
import { StableScreenDetector } from "./stable-detection.ts";
import { releaseAgentStatusCapabilities } from "./environment.ts";
import { monoNowMs } from "../util/mono.ts";

const PROCESS_SCAN_INTERVAL_MS = 250;
const OUTPUT_SCAN_COALESCE_MS = 40;
/** Minimum gap between visible-grid reads for one session. The grid-text read
 *  (rows×cols cell reads) is the expensive part of a status scan, and output
 *  bursts arm scans every OUTPUT_SCAN_COALESCE_MS — ungated, a fleet of chatty
 *  sessions starves PTY parsing and baseline emission on the same loop.
 *  OSC title/progress inputs stay cheap; badge latency stays under one
 *  process-scan interval. */
const SCREEN_RESCAN_MIN_MS = 200;

/** The visible grid as text for manifest matching. Rows come from the cell
 *  encoder, not a private per-column read: a wide glyph's width-0 continuation
 *  cell must contribute NOTHING, or every pattern that spans one sees a phantom
 *  space ("中 文") and stops matching. */
export function readVisibleScreen(core: TerminalCore): string {
  const cols = core.getCols();
  const rows = core.getRows();
  const lines = new Array<string>(rows);
  for (let row = 0; row < rows; row++) {
    lines[row] = spansText(viewportRowSpans(core, row, cols)).trimEnd();
  }
  return lines.join("\n");
}

export class AgentScreenDetector {
  private readonly scanner: AgentProcessScanner;
  private readonly stable = new StableScreenDetector();
  private readonly outputTimers = new Map<number, ReturnType<typeof setTimeout>>();
  private readonly lastScreenReadAtMs = new Map<string, number>();
  private readonly interval: ReturnType<typeof setInterval>;
  private running: Promise<void> | null = null;
  private rerun = false;
  private disposed = false;
  private readonly now: () => number;

  constructor(
    private readonly sessions: SessionManager,
    private readonly registry: AgentStatusRegistry,
    scanner = new AgentProcessScanner(),
    options: { now?: () => number } = {},
  ) {
    this.scanner = scanner;
    this.now = options.now ?? monoNowMs;
    this.interval = setInterval(() => void this.scanNow(), PROCESS_SCAN_INTERVAL_MS);
    this.interval.unref?.();
    void this.scanNow();
  }

  schedule(channelId: number): void {
    if (this.disposed || this.outputTimers.has(channelId)) return;
    const timer = setTimeout(() => {
      this.outputTimers.delete(channelId);
      void this.scanNow();
    }, OUTPUT_SCAN_COALESCE_MS);
    timer.unref?.();
    this.outputTimers.set(channelId, timer);
  }

  async scanNow(): Promise<void> {
    if (this.disposed) return;
    if (this.running) {
      this.rerun = true;
      return this.running;
    }
    this.running = this.scanOnce().finally(() => {
      this.running = null;
      if (this.rerun && !this.disposed) {
        this.rerun = false;
        void this.scanNow();
      }
    });
    return this.running;
  }

  private async scanOnce(): Promise<void> {
    const records = this.sessions.allSessions();
    const liveSessionIds = new Set(records.map((record) => String(record.sessionId)));
    this.stable.retain(liveSessionIds);
    this.registry.retainSessions(liveSessionIds);
    for (const sessionId of this.lastScreenReadAtMs.keys()) {
      if (!liveSessionIds.has(sessionId)) this.lastScreenReadAtMs.delete(sessionId);
    }
    const roots: SessionProcessRoot[] = [];
    for (const record of records) {
      if (record.childPid && record.childPid > 0) {
        roots.push({ sessionId: String(record.sessionId), childPid: record.childPid });
      }
    }
    const identities = await this.scanner.scanAgents(roots);
    for (const record of records) {
      const sessionId = String(record.sessionId);
      const identity = identities.get(sessionId);
      if (!identity) {
        this.lastScreenReadAtMs.delete(sessionId);
        this.stable.release(sessionId);
        this.registry.clearScreen(sessionId);
        continue;
      }
      const nowMs = this.now();
      const lastReadAtMs = this.lastScreenReadAtMs.get(sessionId);
      if (lastReadAtMs !== undefined && nowMs - lastReadAtMs < SCREEN_RESCAN_MIN_MS) continue;
      this.lastScreenReadAtMs.set(sessionId, nowMs);
      try {
        const detection = evaluateManifest(AGENT_MANIFESTS[identity.agentId], {
          screen: readVisibleScreen(record.wtermCore),
          oscTitle: record.rawOscTitle,
          oscProgress: record.rawOscProgress,
        });
        const report = this.stable.observe(sessionId, identity.agentId, detection);
        if (report) this.registry.reportScreen(sessionId, report);
      } catch (error) {
        log.warn("agent-status", "screen_detection_failed", {
          session_id: sessionId,
          agent_id: identity.agentId,
          error: String(error),
        });
      }
    }
  }

  sessionForPid(pid: number): Promise<string | null> {
    const roots: SessionProcessRoot[] = [];
    for (const record of this.sessions.allSessions()) {
      if (record.childPid && record.childPid > 0) {
        roots.push({ sessionId: String(record.sessionId), childPid: record.childPid });
      }
    }
    return this.scanner.sessionForPid(pid, roots);
  }

  closeSession(sessionId: string): void {
    // _dropChannelState fires this before dropping the record, so the channel
    // is still resolvable; a pending coalesce timer must not fire a scan for
    // a session that can no longer be scanned.
    const record = this.sessions.getBySessionId(sessionId);
    if (record) {
      const timer = this.outputTimers.get(record.channelId);
      if (timer !== undefined) {
        clearTimeout(timer);
        this.outputTimers.delete(record.channelId);
      }
    }
    // Respawns mint fresh session ids forever, so a cached capability for a
    // closed session can never be read again — pure memory growth.
    releaseAgentStatusCapabilities(sessionId);
    this.lastScreenReadAtMs.delete(sessionId);
    this.stable.release(sessionId);
    this.registry.closeSession(sessionId);
  }

  dispose(): void {
    this.disposed = true;
    clearInterval(this.interval);
    for (const timer of this.outputTimers.values()) clearTimeout(timer);
    this.outputTimers.clear();
    this.lastScreenReadAtMs.clear();
  }
}
