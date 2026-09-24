// Renderer-reconciliation check for one captured browser painted state: does
// the ACTUAL painted DOM agree with the committed painted model at the SAME
// boundary?
// Called by scripts/replay-terminal-incident.ts. Reads the bundle's row text to
// fingerprint it and reports coordinates only — a finding names an absolute
// row, a viewport row and a DOM position, never characters.

import {
  paintedRowFingerprint,
  paintedTextFingerprint,
  type TerminalBrowserPaintedState,
  type TerminalDomRow,
} from "../packages/protocol/src/terminal-capture.ts";

export type RendererFindingKind =
  | "duplicate_history_index"
  | "out_of_order_history_index"
  | "history_text_mismatch"
  | "viewport_row_count"
  | "viewport_text_mismatch";

export interface RendererFinding {
  readonly kind: RendererFindingKind;
  readonly absoluteRow: number | null;
  readonly viewportRow: number | null;
  readonly domOrder: number | null;
}

export interface RendererReport {
  readonly findings: readonly RendererFinding[];
  /** States whose history invariants were evaluated. */
  readonly historyStates: number;
  /** States whose viewport could be compared (renderer caught up to canonical). */
  readonly viewportStates: number;
  /** States whose viewport was NOT comparable, so its absence is visible. */
  readonly viewportSkipped: number;
}

/** A row with zero grid columns paints one blank to keep its line box, so its
 *  painted identity is the empty string — not that padding space. The browser
 *  recorder fingerprints the DOM by this same rule; diverging here would make
 *  every blank row of every grid look corrupted. */
function domIdentity(row: TerminalDomRow): number {
  return paintedTextFingerprint(row.columns === 0 ? "" : row.text, row.columns);
}

/** True when the renderer had caught up to this canonical frame, i.e. the DOM
 *  and the model are the SAME instant. A pre-apply or reader-held snapshot
 *  holds an older DOM beside a newer canonical frame; comparing those two
 *  reports every intervening keystroke as corruption. */
function sameInstant(state: TerminalBrowserPaintedState): boolean {
  const committed = state.committed;
  const canonical = state.canonical;
  return committed !== null
    && canonical !== null
    && committed.stream_id === canonical.streamId
    && committed.grid_epoch === canonical.gridEpoch
    && committed.seq === String(canonical.seq)
    && committed.cols === canonical.cols
    && committed.rows === canonical.rows;
}

export function rendererReport(
  states: readonly (TerminalBrowserPaintedState | null)[],
): RendererReport {
  const findings: RendererFinding[] = [];
  let historyStates = 0;
  let viewportStates = 0;
  let viewportSkipped = 0;
  for (const state of states) {
    if (!state) continue;
    historyStates++;
    findings.push(...historyFindings(state));
    if (state.canonical === null || state.dom_viewport.length === 0) continue;
    if (!sameInstant(state)) {
      viewportSkipped++;
      continue;
    }
    viewportStates++;
    findings.push(...viewportFindings(state));
  }
  return { findings, historyStates, viewportStates, viewportSkipped };
}

/** A duplicate ABSOLUTE history index is the invariant violation — it is the
 *  duplicated-tail class itself. The same TEXT at two distinct indices is
 *  legitimate output (a footer redrawn on a new line) and stays silent.
 *  History is compared against the COMMITTED painted model, which the renderer
 *  retains per row, so it is clock-correct at every phase. */
function historyFindings(state: TerminalBrowserPaintedState): RendererFinding[] {
  const findings: RendererFinding[] = [];
  const seen = new Set<number>();
  let previousIndex: number | null = null;
  for (const row of state.dom_history) {
    if (row.index === null) continue;
    if (seen.has(row.index)) {
      findings.push(finding("duplicate_history_index", row.index, null, row.order));
    } else {
      seen.add(row.index);
    }
    if (previousIndex !== null && row.index <= previousIndex) {
      findings.push(finding("out_of_order_history_index", row.index, null, row.order));
    }
    previousIndex = row.index;
  }

  const modelByIndex = new Map<number, number>();
  for (const row of state.painted_model_history) {
    modelByIndex.set(row.index, paintedRowFingerprint(row.spans));
  }
  for (const row of state.dom_history) {
    if (row.index === null) continue;
    const expected = modelByIndex.get(row.index);
    if (expected === undefined) continue;
    if (expected !== domIdentity(row)) {
      findings.push(finding("history_text_mismatch", row.index, null, row.order));
    }
  }
  return findings;
}

function viewportFindings(state: TerminalBrowserPaintedState): RendererFinding[] {
  const canonical = state.canonical!;
  const findings: RendererFinding[] = [];
  if (state.dom_viewport.length !== canonical.rows) {
    findings.push(finding("viewport_row_count", null, null, null));
  }
  for (const row of state.dom_viewport) {
    const modelRow = canonical.viewportRows[row.order];
    if (!modelRow) continue;
    if (paintedRowFingerprint(modelRow.spans) !== domIdentity(row)) {
      findings.push(finding("viewport_text_mismatch", null, row.order, row.order));
    }
  }
  return findings;
}

function finding(
  kind: RendererFindingKind,
  absoluteRow: number | null,
  viewportRow: number | null,
  domOrder: number | null,
): RendererFinding {
  return { kind, absoluteRow, viewportRow, domOrder };
}
