// Pure absolute scrollback interval arithmetic.
// CellGridRenderer owns the row array and DOM; pagers use these helpers to
// ask which immutable rows are actually covered without inferring from sbBase.

export interface CellHistoryIndex {
  readonly index: number;
}

export interface CellHistoryRange {
  readonly start: number;
  readonly end: number;
}


/** True when a half-open absolute range belongs to one history total. */
export function isCellHistoryRange(
  total: number,
  start: number,
  end: number,
): boolean {
  return Number.isSafeInteger(total)
    && Number.isSafeInteger(start)
    && Number.isSafeInteger(end)
    && start >= 0
    && start < end
    && end <= total;
}

/** The renderer keeps these rows sorted and unique before asking interval queries. */
export function hasSortedCellHistoryRows(
  rows: readonly CellHistoryIndex[],
  total: number,
): boolean {
  if (!Number.isSafeInteger(total) || total < 0) return false;
  let previous = -1;
  for (const row of rows) {
    if (
      !Number.isSafeInteger(row.index)
      || row.index < 0
      || row.index >= total
      || row.index <= previous
    ) return false;
    previous = row.index;
  }
  return true;
}

/** Lower-bound position for one absolute row in sorted immutable history. */
export function cellHistoryInsertionIndex(
  rows: readonly CellHistoryIndex[],
  index: number,
): number {
  let low = 0;
  let high = rows.length;
  while (low < high) {
    const middle = (low + high) >>> 1;
    if (rows[middle]!.index < index) low = middle + 1;
    else high = middle;
  }
  return low;
}

/** Missing half-open interval containing `row`, or null when it is painted. */
export function missingCellHistoryRange(
  rows: readonly CellHistoryIndex[],
  total: number,
  row: number,
): CellHistoryRange | null {
  if (
    !Number.isSafeInteger(total)
    || total < 0
    || !Number.isSafeInteger(row)
    || row < 0
    || row >= total
  ) return null;
  const insertion = cellHistoryInsertionIndex(rows, row);
  if (rows[insertion]?.index === row) return null;
  const start = (rows[insertion - 1]?.index ?? -1) + 1;
  const end = rows[insertion]?.index ?? total;
  return start < end ? { start, end } : null;
}

/** True only when every row in a nonempty historical range is painted. */
export function hasCellHistoryRange(
  rows: readonly CellHistoryIndex[],
  total: number,
  start: number,
  end: number,
): boolean {
  if (!isCellHistoryRange(total, start, end)) return false;
  let position = cellHistoryInsertionIndex(rows, start);
  for (let index = start; index < end; index++, position++) {
    if (rows[position]?.index !== index) return false;
  }
  return true;
}

/** Missing intervals inside one requested historical range, in ascending order. */
export function missingCellHistoryRanges(
  rows: readonly CellHistoryIndex[],
  total: number,
  start: number,
  end: number,
): CellHistoryRange[] {
  if (!isCellHistoryRange(total, start, end)) return [];
  const ranges: CellHistoryRange[] = [];
  let cursor = start;
  let position = cellHistoryInsertionIndex(rows, start);
  while (cursor < end) {
    const painted = rows[position];
    if (!painted || painted.index >= end) {
      ranges.push({ start: cursor, end });
      break;
    }
    if (painted.index > cursor) ranges.push({ start: cursor, end: painted.index });
    cursor = painted.index + 1;
    position++;
  }
  return ranges;
}

/** A page is safe to insert only when its row shells name exactly this interval. */
export function hasContiguousCellHistoryRows(
  rows: readonly CellHistoryIndex[],
  start: number,
  end: number,
): boolean {
  if (
    !Number.isSafeInteger(start)
    || !Number.isSafeInteger(end)
    || start < 0
    || start >= end
    || rows.length !== end - start
  ) return false;
  for (let offset = 0; offset < rows.length; offset++) {
    if (rows[offset]?.index !== start + offset) return false;
  }
  return true;
}
