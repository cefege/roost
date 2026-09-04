// Shared terminal-view geometry and lease policy. The coordinator's
// TerminalViewHub is the only SCD/membership owner; browsers clamp trusted
// measurements to these core limits and every later trust boundary rejects an
// out-of-range value.

export const TERMINAL_MAX_COLS = 256;
export const TERMINAL_MAX_ROWS = 256;

export const TERMINAL_VIEW_LEASE_MS = 15_000;
export const TERMINAL_VIEW_HEARTBEAT_MS = 5_000;
export const TERMINAL_VIEW_SWEEP_MS = 1_000;
export const TERMINAL_FOREGROUND_IDLE_PROBE_MS = 20_000;
export const TERMINAL_FOREGROUND_PROBE_DEADLINE_MS = 10_000;

export interface TerminalGeometry {
  cols: number;
  rows: number;
}

/** UUID shape used by view, stream and snapshot generations. IDs are opaque;
 * accepting every RFC-4122 version keeps validation independent of how a
 * caller obtained its collision-resistant UUID. */
const TERMINAL_UUID_RE =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;

export function isTerminalUuid(value: string): boolean {
  return TERMINAL_UUID_RE.test(value);
}

export function isTerminalGeometry(value: Readonly<TerminalGeometry>): boolean {
  return Number.isInteger(value.cols)
    && value.cols >= 1
    && value.cols <= TERMINAL_MAX_COLS
    && Number.isInteger(value.rows)
    && value.rows >= 1
    && value.rows <= TERMINAL_MAX_ROWS;
}

/** Validate untrusted wire/API geometry without mutating it. */
export function assertTerminalGeometry(
  value: Readonly<TerminalGeometry>,
): TerminalGeometry {
  if (!isTerminalGeometry(value)) {
    throw new RangeError(
      `terminal geometry ${String(value.cols)}x${String(value.rows)} is outside `
      + `1..${TERMINAL_MAX_COLS}x1..${TERMINAL_MAX_ROWS}`,
    );
  }
  return { cols: value.cols, rows: value.rows };
}

/** Bound a trusted positive browser measurement before it enters the wire. */
export function clampTerminalGeometry(
  value: Readonly<TerminalGeometry>,
): TerminalGeometry {
  const cols = Number.isFinite(value.cols) ? Math.floor(value.cols) : 1;
  const rows = Number.isFinite(value.rows) ? Math.floor(value.rows) : 1;
  return {
    cols: Math.min(Math.max(cols, 1), TERMINAL_MAX_COLS),
    rows: Math.min(Math.max(rows, 1), TERMINAL_MAX_ROWS),
  };
}

/** Independent-axis smallest-common dimensions. Empty input means unwatched. */
export function minimumTerminalGeometry(
  geometries: Iterable<Readonly<TerminalGeometry>>,
): TerminalGeometry | null {
  let cols = TERMINAL_MAX_COLS + 1;
  let rows = TERMINAL_MAX_ROWS + 1;
  let seen = false;
  for (const geometry of geometries) {
    assertTerminalGeometry(geometry);
    if (geometry.cols < cols) cols = geometry.cols;
    if (geometry.rows < rows) rows = geometry.rows;
    seen = true;
  }
  return seen ? { cols, rows } : null;
}
