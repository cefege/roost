// Attach-stall diagnosis for the terminal loading card ("where are we stuck").
// Started by CellTerminal once a pane has sat in its viewport/frame stage past
// the grace window; polls the coordinator's session-scoped DiagSnapshot every
// ATTACH_DIAGNOSIS_POLL_MS and maps each snapshot to one human-readable stuck
// reason. The pure wire-to-reason mapping lives in
// attachDiagnosisReasonFromSnapshot so the contract is unit-testable without
// the RPC loop.

import { diag } from "@roost/shared/diag";
import { coordClient } from "../connect.ts";

export const ATTACH_DIAGNOSIS_POLL_MS = 750;

export interface AttachDiagnosisHandle {
  dispose(): void;
}

const WORKER_GATE_LABELS = {
  resize_capture: "Resizing grid",
  baseline: "Building baseline",
  sync_output: "App is buffering output (synchronized output)",
} as const;

type WorkerGateLabel = keyof typeof WORKER_GATE_LABELS;

function asRecord(value: unknown): Record<string, unknown> | null {
  return typeof value === "object" && value !== null
    ? value as Record<string, unknown>
    : null;
}

function asString(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

function truncateReason(reason: string): string {
  return reason.length > 200 ? `${reason.slice(0, 200)}…` : reason;
}

/** First matching rule wins; null clears any previously shown line. The
 * returned seq feeds the caller's next-context so "frames are flowing" can be
 * observed across polls while the browser itself stays unpainted. */
export function attachDiagnosisReasonFromSnapshot(
  snapshot: unknown,
  sessionId: string,
  context: { previousTerminalScreenSeq: string | null },
): { reason: string | null; terminalScreenSeq: string | null } {
  const root = asRecord(snapshot);
  const coord = asRecord(root?.coord);
  const sessions = asRecord(coord?.sessions);
  const workers = asRecord(root?.workers);
  // A coordinator older than the session_filter_id contract answers with an
  // unfiltered dump; either way a missing session entry carries no evidence,
  // so stay silent rather than guess.
  if (!sessions || !(sessionId in sessions)) {
    return { reason: null, terminalScreenSeq: context.previousTerminalScreenSeq };
  }
  const session = asRecord(sessions[sessionId]);
  if (!session) {
    return { reason: null, terminalScreenSeq: context.previousTerminalScreenSeq };
  }

  const route = asRecord(session.route);
  if (!route || route.connected === false) {
    return {
      reason: "Worker offline — waiting for it to reconnect",
      terminalScreenSeq: null,
    };
  }

  const terminalView = asRecord(session.terminal_view);
  if (terminalView?.unavailable === true) {
    // The view-hub snapshot carries availability as a boolean only; there is
    // no coordinator reason string on the wire to append.
    return {
      reason: "Coordinator: terminal view marked unavailable",
      terminalScreenSeq: null,
    };
  }

  const terminalScreen = asRecord(session.terminal_screen);
  const currentSeq = terminalScreen === null ? null : asString(terminalScreen.seq);
  if (terminalScreen && terminalScreen.valid === false) {
    return {
      reason: "Repairing the stream (resync requested)",
      terminalScreenSeq: currentSeq,
    };
  }

  if (workers) {
    for (const entry of Object.values(workers)) {
      const worker = asRecord(entry);
      if (!worker || worker.status !== "ok") continue;
      const workerSnapshot = asRecord(worker.snapshot);
      const workerSessions = asRecord(workerSnapshot?.sessions);
      const workerSession = workerSessions
        ? asRecord(workerSessions[sessionId])
        : null;
      const gate = workerSession ? asRecord(workerSession.gate) : null;
      if (!gate || gate.active !== true) continue;
      const gateKind = asString(gate.gate) ?? asString(gate.reason);
      if (gateKind === null || !(gateKind in WORKER_GATE_LABELS)) continue;
      const ageMs = typeof gate.age_ms === "number" ? gate.age_ms : 0;
      const ageSeconds = Math.max(0, Math.round(ageMs / 1000));
      const label = WORKER_GATE_LABELS[gateKind as WorkerGateLabel];
      return {
        reason: `${label} (${ageSeconds}s)`,
        terminalScreenSeq: currentSeq,
      };
    }
  }

  if (
    context.previousTerminalScreenSeq !== null
    && currentSeq !== null
    && currentSeq !== context.previousTerminalScreenSeq
  ) {
    // This poller only runs while the local browser is still unpainted, so a
    // moving coordinator-side sequence means delivery works and assembly or
    // paint is what remains.
    return {
      reason: "Frames flowing — assembling on this device",
      terminalScreenSeq: currentSeq,
    };
  }

  return { reason: null, terminalScreenSeq: currentSeq };
}

/** Poll the session-scoped coordinator snapshot until dispose(). Each poll
 * reports at most one stuck reason through onReason (null clears). Polling
 * errors never surface: a broken diagnosis channel must not add noise to an
 * attach that is already struggling. */
export function startAttachDiagnosis(
  sessionId: string,
  onReason: (reason: string | null) => void,
): AttachDiagnosisHandle {
  let disposed = false;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let inFlight = false;
  let previousTerminalScreenSeq: string | null = null;

  const scheduleNext = (): void => {
    if (disposed) return;
    timer = setTimeout(() => void poll(), ATTACH_DIAGNOSIS_POLL_MS);
  };

  const poll = async (): Promise<void> => {
    timer = null;
    if (disposed || inFlight) return;
    inFlight = true;
    try {
      const response = await coordClient.diagSnapshot({
        sessionFilterId: sessionId,
      });
      if (disposed) return;
      let parsed: unknown = null;
      try { parsed = JSON.parse(response.snapshotJson); } catch { /* null below */ }
      const outcome = parsed === null
        ? { reason: null, terminalScreenSeq: previousTerminalScreenSeq }
        : attachDiagnosisReasonFromSnapshot(parsed, sessionId, {
          previousTerminalScreenSeq,
        });
      previousTerminalScreenSeq = outcome.terminalScreenSeq;
      diag("attach.stuck_reason", { sid: sessionId, reason: outcome.reason });
      onReason(outcome.reason);
    } catch {
      // Leave the previous line standing; the next tick retries.
    } finally {
      inFlight = false;
      scheduleNext();
    }
  };

  void poll();
  return {
    dispose(): void {
      disposed = true;
      if (timer !== null) clearTimeout(timer);
      timer = null;
    },
  };
}
