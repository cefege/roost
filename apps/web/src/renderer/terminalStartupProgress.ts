// The single monotone percent model behind the terminal startup card: one
// contiguous band per pane startup step, ending at 99 (only completion paints
// 100).
// Consumed by TerminalStartupOverlay.tsx. Depends on nothing — pure, no Solid
// and no DOM, because bun test resolves Solid components to their SSR build.

export type TerminalStartupStage =
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

// Bands are contiguous and ordered so the meter only ever moves forward as a
// pane advances from spawn to render. They begin at 46 because the pane card
// continues the coordinator connection that ran before the workbench mounted.
// retry is a zero-width band: a retrying step must not advance.
export const TERMINAL_STARTUP_STEPS: Record<TerminalStartupStage, TerminalStartupStep> = {
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
