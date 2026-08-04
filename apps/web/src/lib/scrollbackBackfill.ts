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
// FULL frame bumps the local epoch, cancelling any in-flight loop, and then
// restarts the drain: a claim snapshot is always SB_SNAPSHOT_TAIL_ROWS, so a
// reveal of a pane that fell behind lands on the live bottom holding only the
// tail and this controller is what refills the held window behind the reader.
// Each response is re-validated against the renderer's live anchor (cols +
// overlap-row text identity). History remains reachable through the reserved
// spacer while it is in flight.
//
// Owner: CellTerminal.tsx (one controller per pane; onFullFrame from the cell
// handler, onUserScrollUp from the container scroll listener, onReveal from the
// in-layout / tab-visible claim sites).

import { coordClient } from "../connect.ts";
import type { SessionsGetScrollbackCellsResponse } from "@roost/shared/proto/coordinator_pb";
import { diag } from "@roost/shared/diag";
import { cellRowFromProto } from "@roost/shared/cell/cell-proto";
import type { CellRow } from "@roost/shared/cell";
import { MAX_HELD_SCROLLBACK_ROWS, type BackfillAnchor, type CellGridRenderer } from "./cellRenderer.ts";
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
// Concurrent get-scrollback-cells RPCs per wave. 3 × 999 rows covers the whole
// MAX_HELD_SCROLLBACK_ROWS window in ONE round trip, so a reveal that collapsed
// to the 250-row tail refills without the reader ever seeing a partial history.
// Higher only adds worker WASM-walk pressure for rows the evictor trims back off.
const BACKFILL_CONCURRENCY = 3;


export interface ScrollbackBackfill {
  /** Cancel stale work on every FULL frame, then restart the drain when this
   *  pane is live and its held window is below the evictor's cap. */
  onFullFrame(): void;
  /** Start filling immediately when the reader nears the painted history top. */
  onUserScrollUp(): void;
  /** A pane re-promoted into the deck may receive NO new full frame (the
   *  worker's held_cell_seq fast path skips the claim snapshot), so the reveal
   *  itself has to be a trigger — otherwise a pane that collapsed to the tail
   *  on an earlier catch-up never refills until the reader scrolls. */
  onReveal(): void;
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
  /** Pull only for a pane that is in layout on a foregrounded tab. */
  active: () => boolean;
}): ScrollbackBackfill {
  let epoch = 0;
  let activeLoop = -1; // epoch of the running loop; -1 = none
  let disposed = false;

  const start = (): void => {
    if (disposed || activeLoop === epoch) return;
    void loop(epoch);
  };

  /** Start a drain when this pane is live and its held window is below the cap
   *  the evictor enforces, OR the reader is against the painted top. */
  function maybeStart(): void {
    if (disposed || !opts.active()) return;
    const r = opts.renderer();
    const a = r?.backfillAnchor();
    if (!r || !a || a.sbBase <= 0) return;
    if (a.total - a.sbBase >= MAX_HELD_SCROLLBACK_ROWS && !r.nearHistoryTop()) return;
    start();
  }

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

  /** One RPC for rows [endRow - BACKFILL_FETCH_ROWS, endRow). `endRow` is
   *  EXCLUSIVE and its predecessor is the OVERLAP row — re-fetched so its text
   *  identity can prove the chunk abuts what the renderer already holds. */
  const requestChunk = (endRow: number): Promise<SessionsGetScrollbackCellsResponse> =>
    coordClient.sessionsGetScrollbackCells({
      sessionId: opts.sessionId,
      endRow: BigInt(endRow),
      maxRows: BACKFILL_FETCH_ROWS,
    });

  /** Validate one get-scrollback-cells response against the anchor the wave was
   *  planned from. Returns the rows BELOW the overlap row (oldest → newest), or
   *  null when the response must be discarded: a delta may have appended rows
   *  meanwhile (fine, sbBase is untouched) but a reframe, a width change, or an
   *  overlap-row text mismatch means these rows describe a dead epoch.
   *  `endRow` is EXCLUSIVE; its predecessor is the overlap row, whose text must
   *  equal `expectText` — the identity proof that this chunk abuts what the
   *  renderer (or the previously spliced chunk) holds. */
  function validateChunk(
    resp: SessionsGetScrollbackCellsResponse,
    anchor: BackfillAnchor,
    endRow: number,
    expectText: string | null,
  ): CellRow[] | null {
    if (resp.cols !== anchor.cols) return null;
    if (Number(resp.scrollbackTotal) < anchor.total) return null; // reset — reframe incoming
    const rows = resp.rows.map(cellRowFromProto);
    const overlap = rows[rows.length - 1];
    if (
      !overlap ||
      overlap.index !== endRow - 1 ||
      expectText === null ||
      rowText(overlap) !== expectText
    )
      return null;
    const prefix = rows.slice(0, -1);
    return prefix.length === 0 ? null : prefix; // server had nothing below the overlap
  }

  /** Fetch rows [endRow - BACKFILL_FETCH_ROWS, endRow) as ONE chunk and
   *  validate it. Only the RPC itself throws; the caller decides whether to
   *  retry. Sequential callers only — the wave issues its RPCs itself so they
   *  overlap on the wire. */
  async function fetchChunk(
    myEpoch: number,
    anchor: BackfillAnchor,
    endRow: number,
    expectText: string | null,
  ): Promise<CellRow[] | null> {
    const resp = await requestChunk(endRow);
    if (disposed || myEpoch !== epoch) return null;
    return validateChunk(resp, anchor, endRow, expectText);
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
        const nearTop = r0.nearHistoryTop();
        if (anchor.total - anchor.sbBase >= MAX_HELD_SCROLLBACK_ROWS && !nearTop) return;
        // Size the wave so it never overshoots the evictor's cap, then issue
        // every chunk CONCURRENTLY: the chunks are disjoint and their end rows
        // are a pure function of the anchor, so the responses can be validated
        // and spliced strictly newest-first while they are all in flight. One
        // 3-wide wave covers MAX_HELD_SCROLLBACK_ROWS in a single round trip.
        const want = nearTop
          ? anchor.sbBase
          : Math.min(anchor.sbBase, MAX_HELD_SCROLLBACK_ROWS - (anchor.total - anchor.sbBase));
        const stride = BACKFILL_FETCH_ROWS - 1; // rows netted per chunk (one is the overlap)
        const waves = Math.min(BACKFILL_CONCURRENCY, Math.max(1, Math.ceil(want / stride)));
        const ends: number[] = [];
        for (let k = 0; k < waves; k++) {
          const end = anchor.sbBase + 1 - k * stride;
          if (end <= 0) break;
          ends.push(end);
        }
        if (ends.length === 0) return;
        const pending = ends.map((end) =>
          requestChunk(end).then((r) => r, () => "retry" as const),
        );
        // Chunk 0 must reproduce the renderer's first held row; chunk k>0 must
        // reproduce chunk k-1's OLDEST row, which is only known once k-1 has
        // been validated — hence issue-all-then-validate-in-order.
        let expectText = anchor.firstHeldText;
        let transient = false;
        for (let k = 0; k < ends.length; k++) {
          const resp = await pending[k]!;
          if (disposed || myEpoch !== epoch) return;
          if (resp === "retry") { transient = true; break; }
          const prefix = validateChunk(resp, anchor, ends[k]!, expectText);
          if (!prefix) return;
          diag("scrollback.backfill", {
            sid: opts.sessionId,
            start_row: prefix[0]!.index,
            end_row: ends[k]! - 1,
            rows: prefix.length,
            sb_base_after: ends[k]! - 1 - prefix.length,
            chunk: k,
            wave: ends.length,
          });
          if (!(await spliceBatch(prefix, myEpoch))) return;
          // prependScrollback silently drops a misaligned splice; without this
          // the wave would re-request the same rows forever. The rest of the
          // wave describes rows a splice already refused, so abandon it and let
          // the next trigger re-plan from the live anchor.
          const after = opts.renderer()?.backfillAnchor();
          if (!after || after.sbBase >= ends[k]! - 1) return;
          expectText = rowText(prefix[0]!);
        }
        if (transient) {
          if (retried) return; // park until the next full frame / scroll / reveal
          retried = true;
          const { promise, resolve } = Promise.withResolvers<void>();
          setTimeout(resolve, BACKFILL_RETRY_MS);
          await promise;
          continue; // re-plan the whole wave from a fresh anchor
        }
        retried = false;
      }
    } finally {
      if (activeLoop === myEpoch) activeLoop = -1;
    }
  }

  return {
    onFullFrame(): void {
      epoch++; // in-flight loop aborts at its next check
      maybeStart();
    },
    onUserScrollUp(): void {
      maybeStart();
    },
    onReveal(): void {
      maybeStart();
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
        try { prefix = await fetchChunk(myEpoch, anchor, anchor.sbBase + 1, anchor.firstHeldText); }
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
