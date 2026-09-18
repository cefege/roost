// The single monotone percent model behind the terminal startup card: one
// contiguous 4→99 band per startup step, shared by the bootstrap surface and
// the pane surface so their hand-off reads as one journey.
// Consumed by TerminalStartupOverlay.tsx. Depends on nothing — pure, no Solid
// and no DOM, because bun test resolves Solid components to their SSR build.

export type TerminalStartupStage =
  | "identity"
  | "sync"
  | "sessions"
  | "spawn"
  | "measure"
  | "viewport"
  | "frame"
  | "render"
  | "retry";

export interface TerminalStartupStep {
  /** Percent this step owns from (inclusive) and up to (exclusive). */
  start: number;
  end: number;
  /** One short human line; never jargon, never a wire term. */
  label: string;
}

// Bands are contiguous and ordered across BOTH surfaces on purpose: the
// bootstrap card owns identity/sync/sessions (4→46) and the pane card owns the
// rest (46→99), so the hand-off between two component instances still reads as
// one journey. start: 4 because a zero-width bar reads as broken. retry is a
// zero-width band: a retrying step must not advance.
export const TERMINAL_STARTUP_STEPS: Record<TerminalStartupStage, TerminalStartupStep> = {
  identity: { start: 4, end: 14, label: "Reaching your coordinator" },
  sync: { start: 14, end: 30, label: "Opening the live connection" },
  sessions: { start: 30, end: 46, label: "Finding your terminals" },
  spawn: { start: 46, end: 62, label: "Starting the shell" },
  measure: { start: 62, end: 70, label: "Fitting the screen" },
  viewport: { start: 70, end: 82, label: "Claiming the screen" },
  frame: { start: 82, end: 96, label: "Loading your screen" },
  render: { start: 96, end: 99, label: "Almost ready" },
  retry: { start: 82, end: 82, label: "Reconnecting" },
};

const STEP_TIME_CONSTANT_MS = 900;

export interface TerminalStartupSample {
  stage: TerminalStartupStage;
  /** ms since this stage became current. */
  stageElapsedMs: number;
  /** Chunked-baseline assembly when the frame step reports one. */
  chunks?: { received: number; total: number } | null;
  /** Percent already shown; the meter never moves backwards. */
  floor: number;
}

/** Percent to paint for one sample: never below the floor already shown, never
 * past 99 (only completion reaches 100). */
export function terminalStartupPercent(sample: TerminalStartupSample): number {
  const step = TERMINAL_STARTUP_STEPS[sample.stage];
  const span = step.end - step.start;
  // Asymptotic, so a step never parks at its own ceiling and the bar is always
  // visibly moving early (≈63% of the band at 900ms, ≈95% at 2.7s). The
  // asymptote saturates in floating point, so cap it a tenth below the band's
  // end: publishing the end would publish the NEXT step's starting percent.
  // The chunk path below may reach the end — a complete baseline really is done.
  const eased = Math.min(
    span * (1 - Math.exp(-Math.max(sample.stageElapsedMs, 0) / STEP_TIME_CONSTANT_MS)),
    Math.max(span - 0.1, 0),
  );
  const chunks = sample.chunks;
  const usableChunks = chunks && Number.isFinite(chunks.total) && chunks.total > 0
    ? Math.min(Math.max(chunks.received, 0), chunks.total) / chunks.total
    : 0;
  const byChunks = span * usableChunks;
  // max, so a stalled chunk stream still creeps and a fast chunk stream still
  // overtakes the creep.
  const raw = step.start + Math.max(eased, byChunks);
  const bounded = Math.min(99, Math.max(raw, sample.floor, 0));
  return Math.round(bounded * 10) / 10;
}

/** "part 3 of 7", or null when there is no usable chunk count. */
export function terminalStartupChunkDetail(
  chunks: { received: number; total: number } | null | undefined,
): string | null {
  if (!chunks || !Number.isFinite(chunks.total) || chunks.total <= 0) return null;
  const received = Math.min(Math.max(chunks.received, 0), chunks.total);
  return `part ${received} of ${chunks.total}`;
}

/** Only a pane step's disappearance means the terminal actually painted, so
 *  only those end the journey at 100%. The three bootstrap steps hand off to
 *  the pane's own card and must unmount silently, or the user would watch the
 *  meter finish at 100% and then restart at 46%. */
export function terminalStartupCompletesJourney(stage: TerminalStartupStage): boolean {
  return stage !== "identity" && stage !== "sync" && stage !== "sessions";
}
