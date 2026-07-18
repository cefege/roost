// Lazy-history backfill controller (cell mode). Attach/reframe full frames
// carry only a SB_SNAPSHOT_TAIL_ROWS scrollback tail (worker session-emit.ts);
// this controller pulls the remaining [0, sbBase) rows per-viewer via the
// SessionsGetScrollbackCells unary RPC — OFF the broadcast Sync stream — and
// splices them above the painted history (CellGridRenderer.prependScrollback,
// distance-from-bottom preserved, invisible to the user).
//
// Epoch model: scrollback row indices are absolute only within one grid epoch
// (a reframe — width change / alt toggle / reset — restarts numbering). Every
// FULL frame bumps the local epoch, cancelling any in-flight loop; the loop
// re-validates each response against the renderer's live anchor (cols +
// overlap-row text identity) and parks on any mismatch — the reframe that
// moved the epoch re-arms it. History always arrives; only its timing is lazy
// (CLAUDE.md L11: never trade history away).
//
// Owner: CellTerminal.tsx (one controller per pane; onFullFrame from the cell
// handler, onUserScrollUp from the container scroll listener).

import { coordClient } from "../connect.ts";
import { diag } from "@roost/shared/diag";
import { cellRowFromProto } from "@roost/shared/cell/cell-proto";
import { rowText, type CellGridRenderer } from "./cellRenderer.ts";

// Grace after a full frame before fetching — lets the attach burst (N panes
// re-claiming on tab-visible) settle; a user scroll-up starts immediately.
const BACKFILL_DELAY_MS = 300;
// Rows per RPC chunk. Server caps at 2000 (browser-command-terminal.ts);
// 1000 keeps each response + prepend a few-ms task.
const BACKFILL_CHUNK_ROWS = 1000;
// One retry after a transient RPC failure (coord 8s timeout, reconnect),
// then park until the next trigger.
const BACKFILL_RETRY_MS = 2000;

export interface ScrollbackBackfill {
  /** Call on every FULL cell frame: cancels the in-flight loop (epoch moved)
   *  and re-arms the delayed fetch when the frame left a [0, sbBase) hole. */
  onFullFrame(): void;
  /** Call when the user scrolls off the bottom: start filling immediately. */
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
  let timer: ReturnType<typeof setTimeout> | null = null;
  let disposed = false;

  const clearTimer = (): void => {
    if (timer !== null) {
      clearTimeout(timer);
      timer = null;
    }
  };

  const start = (): void => {
    if (disposed || activeLoop === epoch) return;
    void loop(epoch);
  };

  async function loop(myEpoch: number): Promise<void> {
    activeLoop = myEpoch;
    try {
      let retried = false;
      while (!disposed && myEpoch === epoch) {
        const anchor = opts.renderer()?.backfillAnchor();
        if (!anchor || anchor.sbBase <= 0) return;
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
        retried = false;
      }
    } finally {
      if (activeLoop === myEpoch) activeLoop = -1;
    }
  }

  return {
    onFullFrame(): void {
      epoch++; // in-flight loop aborts at its next check
      clearTimer();
      if (disposed) return;
      const anchor = opts.renderer()?.backfillAnchor();
      if (!anchor || anchor.sbBase <= 0) return;
      timer = setTimeout(() => {
        timer = null;
        start();
      }, BACKFILL_DELAY_MS);
    },
    onUserScrollUp(): void {
      if (disposed) return;
      const anchor = opts.renderer()?.backfillAnchor();
      if (!anchor || anchor.sbBase <= 0) return;
      clearTimer();
      start();
    },
    dispose(): void {
      disposed = true;
      epoch++;
      clearTimer();
    },
  };
}
