// Detects which coding agents are actually running on this machine by
// combining a process scan with manifest evaluation over terminal screen text
// (spans/viewport rows), so a pane showing Claude/Codex activity maps to a
// concrete BuiltinAgentId even when no extension reported in.
import { log } from "@roost/shared/log";
import { spansText, viewportRowSpans } from "@roost/shared/cell";
import type { SessionId } from "@roost/shared/wire";
import type { TerminalCore } from "@wterm/core";
import type { SessionManager } from "../session-manager.ts";
import { evaluateManifest } from "./manifest-engine.ts";
import { AGENT_MANIFESTS } from "./manifests.ts";
import {
  AgentProcessScanner,
  type AgentProcessIdentity,
  type BuiltinAgentId,
  type SessionProcessRoot,
} from "./process-scan.ts";
import { AgentStatusRegistry } from "./registry.ts";
import { StableScreenDetector } from "./stable-detection.ts";
import { releaseAgentStatusCapabilities } from "./environment.ts";
import {
  emitDurableAgentReference,
  type AgentReferenceAdmissionGate,
  type AgentReferenceEventSink,
} from "./reference-admission.ts";
import { monoNowMs } from "../util/mono.ts";
import { clearAgentOscEvidence } from "../terminal-stream-scan.ts";

/** The identity a session's last scan resolved, for both the OSC-evidence
 *  transition clear and the one-shot reference clear on agent exit. */
interface ObservedAgentIdentity {
  agentId: BuiltinAgentId;
  processId: number;
}

const PROCESS_SCAN_INTERVAL_MS = 250;
const OUTPUT_SCAN_COALESCE_MS = 40;
/** Minimum gap between visible-grid reads for one session. The grid-text read
 *  (rows×cols cell reads) is the expensive part of a status scan, and output
 *  bursts arm scans every OUTPUT_SCAN_COALESCE_MS — ungated, a fleet of chatty
 *  sessions starves PTY parsing and baseline emission on the same loop.
 *  OSC title/progress inputs stay cheap; badge latency stays under one
 *  process-scan interval. */
const SCREEN_RESCAN_MIN_MS = 200;

/** Durable clear path for the conversation reference of a session whose
 *  reporting agent is gone. Absent in isolated tests that exercise screen
 *  detection only. */
export interface AgentReferenceClearDeps {
  readonly eventSink: AgentReferenceEventSink;
  readonly referenceAdmission: Pick<AgentReferenceAdmissionGate, "runExclusive">;
}

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
  private readonly lastObservedAgentId = new Map<string, ObservedAgentIdentity>();
  private readonly interval: ReturnType<typeof setInterval>;
  private running: Promise<void> | null = null;
  private rerun = false;
  private disposed = false;
  private readonly now: () => number;
  private readonly referenceClear: AgentReferenceClearDeps | null;

  constructor(
    private readonly sessions: SessionManager,
    private readonly registry: AgentStatusRegistry,
    scanner = new AgentProcessScanner(),
    options: {
      now?: () => number;
      referenceClear?: AgentReferenceClearDeps;
    } = {},
  ) {
    this.scanner = scanner;
    this.now = options.now ?? monoNowMs;
    this.referenceClear = options.referenceClear ?? null;
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
    for (const sessionId of this.lastObservedAgentId.keys()) {
      if (!liveSessionIds.has(sessionId)) this.lastObservedAgentId.delete(sessionId);
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
        this.clearReferenceOnAgentExit(record.sessionId);
        continue;
      }
      const previousIdentity = this.lastObservedAgentId.get(sessionId);
      this.lastObservedAgentId.set(sessionId, {
        agentId: identity.agentId,
        processId: identity.pid,
      });
      if (
        previousIdentity !== undefined
        && (previousIdentity.agentId !== identity.agentId
          || previousIdentity.processId !== identity.pid)
      ) {
        clearAgentOscEvidence(record);
        log.info("agent-status", "osc_evidence_cleared_on_agent_change", {
          session_id: sessionId,
          previous_agent_id: previousIdentity.agentId,
          agent_id: identity.agentId,
        });
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
        const report = this.stable.observe(sessionId, identity, detection);
        if (report) {
          this.registry.reportScreen(sessionId, report);
        } else if (!this.stable.current(sessionId)) {
          this.registry.clearScreen(sessionId);
        }
      } catch (error) {
        log.warn("agent-status", "screen_detection_failed", {
          session_id: sessionId,
          agent_id: identity.agentId,
          error: String(error),
        });
      }
    }
  }

  /** An agent process that disappears from a live session takes its
   *  conversation reference with it: a reference left behind would later be
   *  typed into that session's shell as a resume for a conversation the user
   *  already ended. */
  private clearReferenceOnAgentExit(sessionId: SessionId): void {
    const clear = this.referenceClear;
    if (!clear || this.lastObservedAgentId.get(sessionId)?.agentId !== "omp") return;
    // Forgotten before the append is attempted, so one exit clears exactly once.
    this.lastObservedAgentId.delete(sessionId);
    void clear.referenceAdmission.runExclusive(async () => {
      emitDurableAgentReference(clear.eventSink, sessionId, null);
      log.info("agent-reference", "reference_cleared_on_agent_exit", {
        session_id: sessionId,
        agent_id: "omp",
      });
    }).catch((error: unknown) => {
      log.warn("agent-reference", "reference_clear_failed", {
        session_id: sessionId,
        error: String(error),
      });
    });
  }

  async reportingAgentForSession(
    sessionId: string,
    reporterPid: number,
    signal?: AbortSignal,
  ): Promise<AgentProcessIdentity | null> {
    const record = this.sessions.getBySessionId(sessionId);
    const childPid = record?.childPid;
    if (!childPid || childPid <= 0) return null;
    const identity = await this.scanner.scanReportingAgent(
      { sessionId, childPid },
      reporterPid,
      signal,
    );
    const current = this.sessions.getBySessionId(sessionId);
    return current?.childPid === childPid ? identity : null;
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
    this.lastObservedAgentId.delete(sessionId);
    this.stable.release(sessionId);
    this.registry.closeSession(sessionId);
  }

  dispose(): void {
    this.disposed = true;
    clearInterval(this.interval);
    for (const timer of this.outputTimers.values()) clearTimeout(timer);
    this.outputTimers.clear();
    this.lastScreenReadAtMs.clear();
    this.lastObservedAgentId.clear();
  }
}
