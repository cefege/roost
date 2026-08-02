// Demand-driven history backfill controller (cell mode). Attach/reframe full
// frames carry only a SB_SNAPSHOT_TAIL_ROWS scrollback tail (worker
// session-emit.ts); this controller pulls the remaining [0, sbBase) rows
// per-viewer via the SessionsGetScrollbackCells unary RPC — OFF the broadcast
// Sync stream — and splices them above the painted history
// (CellGridRenderer.prependScrollback). Browser anchoring preserves an
// inspected row; the renderer pins only a pre-mutation literal-bottom viewport
// to the new bottom.
//
// Epoch model: scrollback row indices are absolute only within one grid epoch
// (a reframe — width change / alt toggle / reset — restarts numbering). Every
// FULL frame bumps the local epoch, cancelling any in-flight loop. A full frame
// restarts backfill only when its renderer has an unpainted [0, sbBase) hole
// and the reader is already near that boundary; otherwise the next user scroll
// toward the painted top starts it. Each response is re-validated against the
// renderer's live anchor (cols + overlap-row text identity). History remains
// reachable through the reserved spacer and is fetched only on reader demand.
//
// Owner: CellTerminal.tsx (one controller per pane; onFullFrame from the cell
// handler, onUserScrollUp from the container scroll listener).

import { coordClient } from "../connect.ts";
import { diag } from "@roost/shared/diag";
import { cellRowFromProto } from "@roost/shared/cell/cell-proto";
import type { CellRow } from "@roost/shared/cell";
import { MAX_HELD_SCROLLBACK_ROWS, type CellGridRenderer } from "./cellRenderer.ts";
import { rowText } from "./cellRow.ts";

// Rows per RPC. The server caps at 2000 (browser-command-terminal.ts); 1000
// nets 999 rows after the overlap row, so draining 10k rows of history costs
// ~10 round trips instead of ~40.
const BACKFILL_FETCH_ROWS = 1000;
// Rows per prependScrollback call, one per animation frame. This is the number
// the DOM cares about: a large splice builds thousands of row/span nodes
// synchronously at ~0.15ms/row (the deep-scrollback attach freeze — the whole
// UI stalls while history materializes). Decoupling the two means fewer round
// trips at exactly the same per-frame main-thread cost.
const BACKFILL_SPLICE_ROWS = 250;
// One retry after a transient RPC failure (coord 8s timeout, reconnect),
// then park until the next trigger.
const BACKFILL_RETRY_MS = 2000;

export interface ScrollbackBackfill {
  /** Cancel stale work on every FULL frame, then restart only when the reader
   *  is already near an unpainted [0, sbBase) history boundary. */
  onFullFrame(): void;
  /** Start filling immediately when the reader nears the painted history top. */
  onUserScrollUp(): void;
  /** Pull history down until `absIndex` is PAINTED, for a find jump: a match in
   *  the reserved-but-unpainted [0, sbBase) region would otherwise land the
   *  reader on blank spacer. Deliberately ignores the demand loop's
   *  MAX_HELD_SCROLLBACK_ROWS guard — an explicit gesture asked for that exact
   *  row. false = the epoch moved (a full frame arrived) or the pull failed, in
   *  which case the caller must re-run its search rather than jump to a stale
   *  index. */
  ensureRowPainted(absIndex: number): Promise<boolean>;
  dispose(): void;
}

export function createScrollbackBackfill(opts: {
  sessionId: string;
  /** Live accessor — the pane may unmount mid-await. */
  renderer: () => CellGridRenderer | null;
}): ScrollbackBackfill {
  let epoch = 0;
  let activeLoop = -1; // epoch of the running loop; -1 = none
  let disposed = false;

  const start = (): void => {
    if (disposed || activeLoop === epoch) return;
    void loop(epoch);
  };

  /** Paint one fetched batch, NEWEST slice first so every splice lands directly
   *  above the rows already painted and the reader's row never moves. One slice
   *  per animation frame keeps the per-frame DOM cost identical to the old
   *  250-rows-per-RPC behavior. Returns false when the epoch moved mid-batch,
   *  in which case the un-spliced remainder is discarded — those rows describe a
   *  dead numbering. */
  async function spliceBatch(prefix: readonly CellRow[], myEpoch: number): Promise<boolean> {
    for (let end = prefix.length; end > 0; end -= BACKFILL_SPLICE_ROWS) {
      const slice = prefix.slice(Math.max(0, end - BACKFILL_SPLICE_ROWS), end);
      const renderer = opts.renderer();
      if (!renderer) return false;
      renderer.prependScrollback(slice);
      // Yield a frame between slices: a deep drain paints + handles input
      // between prepends instead of stacking into one long blocking task.
      await new Promise<void>((r) =>
        typeof requestAnimationFrame === "function"
          ? requestAnimationFrame(() => r())
          : setTimeout(r, 0),
      );
      if (disposed || myEpoch !== epoch) return false;
    }
    return true;
  }

  /** Fetch the batch of rows immediately BELOW the painted window and validate
   *  it against the LIVE anchor. Returns the rows to splice, or null when this
   *  response must be discarded: a delta may have appended rows meanwhile (fine,
   *  sbBase is untouched) but a reframe, a width change, or an overlap-row text
   *  mismatch means these rows describe a dead epoch. Only the RPC itself can
   *  throw; the caller decides whether to retry. */
  async function fetchOlderPrefix(myEpoch: number): Promise<CellRow[] | null> {
    const before = opts.renderer()?.backfillAnchor();
    if (!before || before.sbBase <= 0) return null;
    // +1: re-fetch the first held row as the OVERLAP row — its text identity
    // proves the response belongs to the epoch we hold.
    const resp = await coordClient.sessionsGetScrollbackCells({
      sessionId: opts.sessionId,
      endRow: BigInt(before.sbBase + 1),
      maxRows: BACKFILL_FETCH_ROWS,
    });
    if (disposed || myEpoch !== epoch) return null;
    const now = opts.renderer()?.backfillAnchor();
    if (!now || now.sbBase !== before.sbBase) return null;
    if (resp.cols !== now.cols) return null;
    if (Number(resp.scrollbackTotal) < now.total) return null; // reset — reframe incoming
    const rows = resp.rows.map(cellRowFromProto);
    const overlap = rows[rows.length - 1];
    if (
      !overlap ||
      overlap.index !== before.sbBase ||
      now.firstHeldText === null ||
      rowText(overlap) !== now.firstHeldText
    )
      return null;
    const prefix = rows.slice(0, -1);
    return prefix.length === 0 ? null : prefix; // server had nothing below the overlap
  }

  async function loop(myEpoch: number): Promise<void> {
    activeLoop = myEpoch;
    try {
      let retried = false;
      while (!disposed && myEpoch === epoch) {
        const r0 = opts.renderer();
        const anchor = r0?.backfillAnchor();
        if (!r0 || !anchor || anchor.sbBase <= 0) return;
        // The demand-driven drain stops at the window the evictor enforces.
        // Pulling past MAX_HELD_SCROLLBACK_ROWS while the pane follows the live
        // tail only feeds _evictScrollback, which trims the rows straight back
        // off and moves sbBase. Past the cap, only a reader within one viewport
        // of the painted top pulls more, one chunk per animation frame.
        //
        // Reachability model (CellGridRenderer._syncSpacer): the [0, sbBase)
        // hole is RESERVED in the scroll space by the .cell-sb-spacer sibling,
        // so one gesture can land the reader anywhere in [0, scrollbackTotal).
        // That region is blank until this sequential backward drain reaches it —
        // and it does, because nearHistoryTop() measures against
        // scrollbackEl.offsetTop, which now includes the spacer: a reader
        // inside reserved-but-unpainted space keeps the guard true and the loop
        // walks history back to them, BACKFILL_SPLICE_ROWS per frame. Blank-then-
        // fill is the deliberate trade — honest about depth and self-healing,
        // versus a scrollbar that lied about depth and moved under the reader.
        //
        // While a reader is off the bottom _evictScrollback does not run
        // (cellRenderer.ts: it returns unless wasAtBottom), so a deep drag can
        // hold more than MAX_HELD_SCROLLBACK_ROWS of DOM in that pane until the
        // reader returns to the bottom, where eviction trims back to the cap.
        // Pre-existing for any deep scroll-up; do not add a second cap.
        if (anchor.total - anchor.sbBase >= MAX_HELD_SCROLLBACK_ROWS && !r0.nearHistoryTop()) return;
        const prefix = await (async () => {
          try { return await fetchOlderPrefix(myEpoch); }
          catch { return "retry" as const; }
        })();
        if (prefix === "retry") {
          if (retried) return; // park until the next full frame / scroll
          retried = true;
          const { promise, resolve } = Promise.withResolvers<void>();
          setTimeout(resolve, BACKFILL_RETRY_MS);
          await promise;
          continue;
        }
        if (!prefix) return;
        diag("scrollback.backfill", {
          sid: opts.sessionId,
          start_row: prefix[0]!.index,
          end_row: anchor.sbBase,
          rows: prefix.length,
          sb_base_after: anchor.sbBase - prefix.length,
        });
        if (!(await spliceBatch(prefix, myEpoch))) return;
        retried = false;
      }
    } finally {
      if (activeLoop === myEpoch) activeLoop = -1;
    }
  }

  return {
    onFullFrame(): void {
      epoch++; // in-flight loop aborts at its next check
      if (disposed) return;
      const renderer = opts.renderer();
      const anchor = renderer?.backfillAnchor();
      if (!renderer || !anchor || anchor.sbBase <= 0 || !renderer.nearHistoryTop()) return;
      start();
    },
    onUserScrollUp(): void {
      if (disposed) return;
      const anchor = opts.renderer()?.backfillAnchor();
      if (!anchor || anchor.sbBase <= 0) return;
      start();
    },
    async ensureRowPainted(absIndex: number): Promise<boolean> {
      if (disposed) return false;
      const myEpoch = epoch;
      for (;;) {
        if (disposed || myEpoch !== epoch) return false;
        const anchor = opts.renderer()?.backfillAnchor();
        if (!anchor) return false;
        if (anchor.sbBase <= absIndex) return true;
        let prefix: CellRow[] | null;
        try { prefix = await fetchOlderPrefix(myEpoch); }
        catch { return false; }
        if (!prefix) return false;
        if (!(await spliceBatch(prefix, myEpoch))) return false;
      }
    },
    dispose(): void {
      disposed = true;
      epoch++;
    },
  };
}
