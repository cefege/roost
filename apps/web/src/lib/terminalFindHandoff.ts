// Types and pure selection validation for global-result handoff to terminal find.
// A coordinator coordinate is only a preference: pane-local fresh results and
// the pane's current grid epoch remain the reveal authority.

import type { FindMatch } from "./terminalFindPaging.ts";

export interface TerminalFindPreferredMatch {
  readonly gridEpoch: string;
  readonly row: bigint;
  readonly col: number;
}

export interface TerminalFindQueryOptions {
  /** Reset regex state before searching an externally supplied literal. */
  readonly literal?: boolean;
  readonly caseSensitive?: boolean;
  readonly preferredMatch?: TerminalFindPreferredMatch;
}

/** Return a 1-based active index, defaulting to the newest fresh pane match. */
export function preferredTerminalFindIndex(
  matches: readonly FindMatch[],
  paneEpoch: string,
  preferred: TerminalFindPreferredMatch | null,
): number {
  if (preferred && paneEpoch === preferred.gridEpoch) {
    const preferredIndex = matches.findIndex((match) =>
      match.epoch === preferred.gridEpoch
      && BigInt(match.row) === preferred.row
      && match.col === preferred.col
    );
    if (preferredIndex >= 0) return preferredIndex + 1;
  }
  return matches.length;
}
