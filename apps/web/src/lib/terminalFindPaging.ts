// Validates and decodes coordinator search pages for terminal find.
// TerminalFindController uses these pure guards before publishing worker rows.
// Every row remains fenced to a JSON-safe absolute index and one grid epoch.

import type { SessionsSearchScrollbackResponse } from "@roost/shared/proto/coordinator_pb";
import { TERMINAL_SEARCH_MAX_ROWS } from "@roost/shared/terminal-search";

const MAX_SAFE_ROW = BigInt(Number.MAX_SAFE_INTEGER);
const TERMINAL_SEARCH_MAX_ROWS_BIGINT = BigInt(TERMINAL_SEARCH_MAX_ROWS);

export interface FindMatch {
  row: number;
  col: number;
  len: number;
  preview: string;
  /** Grid numbering that owns `row`. */
  epoch: string;
}

function safeRow(row: bigint): boolean {
  return row >= 0n && row <= MAX_SAFE_ROW;
}

export function searchPageRangeIsValid(
  response: SessionsSearchScrollbackResponse,
  beforeRow: bigint | undefined,
): boolean {
  const start = response.scannedStartRow;
  const end = response.scannedEndRow;
  if (!safeRow(start) || !safeRow(end) || start > end) return false;
  if (end - start > TERMINAL_SEARCH_MAX_ROWS_BIGINT) return false;
  if (beforeRow !== undefined && end !== beforeRow) return false;
  return response.matches.every((match) => (
    safeRow(match.row) && match.row >= start && match.row < end
  ));
}

export function searchContinuationIsValid(
  response: SessionsSearchScrollbackResponse,
  beforeRow: bigint | undefined,
): boolean {
  const next = response.nextBeforeRow;
  if (next === undefined || !safeRow(next)) return false;
  if (next !== response.scannedStartRow || next >= response.scannedEndRow) return false;
  return beforeRow === undefined || next < beforeRow;
}

export function decodePageMatches(
  response: SessionsSearchScrollbackResponse,
  epoch: string,
): FindMatch[] | null {
  const decoded: FindMatch[] = [];
  for (const match of response.matches) {
    if (!safeRow(match.row)) return null;
    decoded.push({
      row: Number(match.row),
      col: match.col,
      len: match.len,
      preview: match.preview,
      epoch,
    });
  }
  return decoded;
}
