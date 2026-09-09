// Maps one bounded coordinator diagnosis snapshot to a loading-card reason.
// The document scheduler owns request cadence; this module owns no browser state.
// Callers retain the prior sequence to distinguish server progress from local assembly.

const WORKER_GATE_LABELS = {
  resize_capture: "Resizing grid",
  baseline: "Building baseline",
  sync_output: "App is buffering output (synchronized output)",
} as const;

type WorkerGateLabel = keyof typeof WORKER_GATE_LABELS;

export interface AttachDiagnosisReasonContext {
  previousTerminalScreenSeq: string | null;
}

export interface AttachDiagnosisReasonOutcome {
  reason: string | null;
  terminalScreenSeq: string | null;
}

/** First matching rule wins; null clears any previously shown line. */
export function attachDiagnosisReasonFromSnapshot(
  snapshot: unknown,
  sessionId: string,
  context: AttachDiagnosisReasonContext,
): AttachDiagnosisReasonOutcome {
  const root = asRecord(snapshot);
  const coord = asRecord(root?.coord);
  const sessions = asRecord(coord?.sessions);
  const workers = asRecord(root?.workers);
  // A coordinator without this session carries no evidence for its loading card.
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

  const routeWorkerFp = asString(route.worker_fp);
  const worker = routeWorkerFp && workers
    ? asRecord(workers[routeWorkerFp])
    : null;
  if (worker?.status === "ok") {
    const workerSnapshot = asRecord(worker.snapshot);
    const workerSessions = asRecord(workerSnapshot?.sessions);
    const workerSession = workerSessions
      ? asRecord(workerSessions[sessionId])
      : null;
    const gate = workerSession ? asRecord(workerSession.gate) : null;
    if (gate?.active === true) {
      const gateKind = asString(gate.gate) ?? asString(gate.reason);
      if (gateKind && gateKind in WORKER_GATE_LABELS) {
        const ageMs = typeof gate.age_ms === "number" ? gate.age_ms : 0;
        const ageSeconds = Math.max(0, Math.round(ageMs / 1000));
        const label = WORKER_GATE_LABELS[gateKind as WorkerGateLabel];
        return {
          reason: `${label} (${ageSeconds}s)`,
          terminalScreenSeq: currentSeq,
        };
      }
    }
  }

  if (
    context.previousTerminalScreenSeq !== null
    && currentSeq !== null
    && currentSeq !== context.previousTerminalScreenSeq
  ) {
    return {
      reason: "Frames flowing — assembling on this device",
      terminalScreenSeq: currentSeq,
    };
  }

  return { reason: null, terminalScreenSeq: currentSeq };
}

function asRecord(value: unknown): Record<string, unknown> | null {
  return typeof value === "object" && value !== null
    ? value as Record<string, unknown>
    : null;
}

function asString(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}
