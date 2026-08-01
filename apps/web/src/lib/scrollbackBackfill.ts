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
import { rowText, MAX_HELD_SCROLLBACK_ROWS, type CellGridRenderer } from "./cellRenderer.ts";

// Rows per RPC chunk. Server caps at 2000 (browser-command-terminal.ts). Kept
// small (~one content-visibility block) so each prependScrollback is a short
// DOM task: a large chunk builds thousands of row/span nodes synchronously,
// blocking the main thread ~0.15ms/row (the deep-scrollback attach/reconnect
// freeze — whole UI stalls while history materializes). Deep history just
// arrives over a few more RPCs instead of one long freeze.
const BACKFILL_CHUNK_ROWS = 250;
// One retry after a transient RPC failure (coord 8s timeout, reconnect),
// then park until the next trigger.
const BACKFILL_RETRY_MS = 2000;

export interface ScrollbackBackfill {
  /** Cancel stale work on every FULL frame, then restart only when the reader
   *  is already near an unpainted [0, sbBase) history boundary. */
  onFullFrame(): void;
  /** Start filling immediately when the reader nears the painted history top. */
  onUserScrollUp(): void;
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
        // walks history back to them, BACKFILL_CHUNK_ROWS per frame. Blank-then-
        // fill is the deliberate trade — honest about depth and self-healing,
        // versus a scrollbar that lied about depth and moved under the reader.
        //
        // While a reader is off the bottom _evictScrollback does not run
        // (cellRenderer.ts: it returns unless wasAtBottom), so a deep drag can
        // hold more than MAX_HELD_SCROLLBACK_ROWS of DOM in that pane until the
        // reader returns to the bottom, where eviction trims back to the cap.
        // Pre-existing for any deep scroll-up; do not add a second cap.
        if (anchor.total - anchor.sbBase >= MAX_HELD_SCROLLBACK_ROWS && !r0.nearHistoryTop()) return;
        let resp;
        try {
          // +1: re-fetch the first held row as the OVERLAP row — its text
          // identity proves the response belongs to the epoch we hold.
          resp = await coordClient.sessionsGetScrollbackCells({
            sessionId: opts.sessionId,
            endRow: BigInt(anchor.sbBase + 1),
            maxRows: BACKFILL_CHUNK_ROWS,
          });
        } catch {
          if (retried) return; // park until the next full frame / scroll
          retried = true;
          const { promise, resolve } = Promise.withResolvers<void>();
          setTimeout(resolve, BACKFILL_RETRY_MS);
          await promise;
          continue;
        }
        if (disposed || myEpoch !== epoch) return;
        const renderer = opts.renderer();
        const now = renderer?.backfillAnchor();
        // Re-validate against the LIVE anchor: a delta may have appended rows
        // meanwhile (fine — sbBase untouched) but a reframe or a shrunk grid
        // means these rows describe a dead epoch.
        if (!renderer || !now || now.sbBase !== anchor.sbBase) return;
        if (resp.cols !== now.cols) return;
        if (Number(resp.scrollbackTotal) < now.total) return; // reset — reframe incoming
        const rows = resp.rows.map(cellRowFromProto);
        const overlap = rows[rows.length - 1];
        if (
          !overlap ||
          overlap.index !== anchor.sbBase ||
          now.firstHeldText === null ||
          rowText(overlap) !== now.firstHeldText
        )
          return;
        const prefix = rows.slice(0, -1);
        if (prefix.length === 0) return; // server had nothing below the overlap
        renderer.prependScrollback(prefix);
        diag("scrollback.backfill", {
          sid: opts.sessionId,
          start_row: prefix[0]!.index,
          end_row: anchor.sbBase,
          rows: prefix.length,
          sb_base_after: anchor.sbBase - prefix.length,
        });
        // Yield a frame between chunks: a deep drain paints + handles input
        // between prepends instead of stacking into one long blocking task.
        await new Promise<void>((r) =>
          typeof requestAnimationFrame === "function"
            ? requestAnimationFrame(() => r())
            : setTimeout(r, 0),
        );
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
    dispose(): void {
      disposed = true;
      epoch++;
    },
  };
}
