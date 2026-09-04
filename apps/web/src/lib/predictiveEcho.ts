// Predictive (speculative) local echo — the prediction engine adapted to
// Roost's cell
// stream. On a high-latency link (mobile over cellular → tailnet), every typed
// char otherwise waits a full round-trip to echo. This paints the predicted
// char IMMEDIATELY, reconciled against the authoritative CellGridFrame that
// arrives ~1 RTT later.
//
// Safety (why it can never corrupt the terminal):
//  - Pure CLIENT overlay; never touches the byte/cell stream or the worker.
//  - Two-epoch confidence gate: a prediction stays INVISIBLE until an
//    earlier prediction in its epoch is server-CONFIRMED → no flicker, and the
//    first keystroke of a burst is never shown on a guess.
//  - A SHOWN prediction that the next authoritative frame contradicts → reset
//    ALL predictions (fall back to authoritative). A wrong guess lives ≤1 frame.
//  - SRTT-gated (Adaptive): predictions only SHOW when echo RTT > ~60ms; on a
//    fast LAN/tailnet link it's a no-op (no benefit, so no risk).
//  - Suppressed entirely in alt-screen (claude/vim) — can't predict a TUI.
//
// Driven by CellTerminal: predict() on each keystroke, onFrame() on each
// authoritative CellGridFrame. The display preference is an injected typed
// accessor, so this hot path performs no storage reads.

import { columnText, type CellGridFrame } from "@roost/shared/cell";
import { diag } from "@roost/shared/diag";
import type { PredictMode } from "./predictPref.ts";
// SRTT/2 thresholds. send_interval = SRTT/2. Lowered so local echo engages on a
// LAN/tailnet link (~15-30ms RTT) — where every keystroke otherwise visibly
// trails one full round-trip (the everyday typing lag) — and only a true
// single-digit-ms loopback stays a no-op. Validated: roostPredict="always"
// makes typing feel instant on this link; this makes it the default, per
// browser, with no localStorage toggle. (Still suppressed in alt-screen TUIs.)
const SHOW_ON_MS = 5;     // srtt/2 > 5 (≈ RTT > 10ms) → engage predictions
const SHOW_OFF_MS = 3;    // srtt/2 ≤ 3 (≈ RTT < 6ms) & idle → disengage
const FLAG_ON_MS = 80;    // srtt/2 > 80 (≈ RTT > 160ms) → underline the guess
const GLITCH_MS = 250;    // a prediction pending this long → force-show (link stalled)

interface Pred {
  row: number;
  col: number;
  ch: string;
  epoch: number;        // tentative_until_epoch
  bornMs: number;       // for RTT sampling + glitch
  bornSeq: number;      // frame seq when predicted — confirm only against a LATER frame
}

/** Text painted at viewport (row,col) in a frame's run-length spans, or "" if
 *  blank/oob. Looks the row up by its `.index`, NOT array position: a DELTA
 *  frame's viewportRows holds only the CHANGED rows, so array position ≠ grid
 *  row — indexing by position would read the wrong cell for any cursor off row 0
 *  (the common bottom-prompt case) and block confirmation entirely. For full
 *  frames index === position, so this matches the old behavior. Column lookup
 *  goes through columnText because a span's text length is not its width. */
function cellCharAt(frame: CellGridFrame, row: number, col: number): string {
  const r = frame.viewportRows.find((rr) => rr.index === row);
  if (!r) return "";
  return columnText(r.spans, col);
}

export class PredictiveEcho {
  private overlay: HTMLDivElement;
  private preds: Pred[] = [];
  private predictionEpoch = 1;
  private confirmedEpoch = 0;
  private srtt = 0;            // EWMA of echo RTT (ms); 0 = unmeasured
  private glitch = false;
  private srttTrigger = false;  // srtt_trigger (armed/hysteresis): once engaged, stays on through the dead-band until idle + low SRTT

  // Authoritative grid state, updated each frame.
  private cursorRow = 0;
  private cursorCol = 0;
  private cols = 0;
  private rows = 0;            // last-seen viewport HEIGHT (frame.rows) for resize detection, mirrors cols
  private altScreen = false;
  private lastSeq = 0;
  private predCursorCol = -1;  // predicted cursor col (−1 = none ahead of auth)

  private readonly now: () => number;
  private readonly onCursor: (col: number | null) => void;
  private readonly sid: string;
  private readonly getMode: () => PredictMode;

  constructor(
    private readonly viewportEl: HTMLElement,
    opts: {
      mode: () => PredictMode;
      now?: () => number;
      onCursor?: (col: number | null) => void;
      sid?: string;
    },
  ) {
    this.now = opts.now ?? nowMs;
    this.sid = opts.sid ?? "";
    this.onCursor = opts.onCursor ?? (() => {});
    this.getMode = opts.mode;
    this.overlay = viewportEl.ownerDocument.createElement("div");
    this.overlay.className = "cell-predict";
    this.overlay.style.position = "absolute";
    this.overlay.style.top = "0";
    this.overlay.style.left = "0";
    this.overlay.style.pointerEvents = "none";
    viewportEl.appendChild(this.overlay);
  }

  /** The display gate: Always/Experimental
   *  paint unconditionally; Adaptive paints only when srtt_trigger||glitch_trigger
   *  (the SRTT hysteresis) — i.e. only on a slow link. The epoch
   *  confidence gate (isTentative) is applied separately in repaint. */
  private shouldShow(mode: PredictMode): boolean {
    if (mode === "always" || mode === "experimental") return true;
    const half = this.srtt / 2;
    // srtt_trigger hysteresis: arm at
    // SRTT/2 > SHOW_ON, stay armed through the 20–30 ms dead-band, disarm only
    // when SRTT/2 ≤ SHOW_OFF AND idle (no pending preds). The old stateless
    // recomputation returned false in that band, so nothing ever painted there.
    if (this.glitch) this.srttTrigger = true;
    else if (half > SHOW_ON_MS) this.srttTrigger = true;
    else if (half <= SHOW_OFF_MS && this.preds.length === 0) this.srttTrigger = false;
    return this.srttTrigger;
  }
  private shouldFlag(): boolean {
    return this.glitch || this.srtt / 2 > FLAG_ON_MS;
  }

  private becomeTentative(): void { this.predictionEpoch++; }

  private resetAll(): void {
    this.preds = [];
    this.predCursorCol = -1;
    this.glitch = false;
    this.srttTrigger = false;
    this.repaint();
  }

  /** A keystroke the user typed. Predict its echo (always — to measure RTT;
   *  display is gated in repaint). Only printable width-1 + backspace; anything
   *  ambiguous bumps the epoch so later predictions stay hidden until reproven. */
  predict(bytes: Uint8Array): void {
    if (this.getMode() === "never" || this.altScreen) { if (this.preds.length) this.resetAll(); return; }
    // Paste guard (paste = bytes > 100 →
    // reset): never predict a paste — it floods the overlay and its echo is
    // unguessable. Reset any pending predictions and drop.
    if (bytes.length > 100) { this.resetAll(); return; }
    // Experimental mode: no tentative epoch — predictions
    // show IMMEDIATELY (predictionEpoch == confirmedEpoch ⇒ not tentative),
    // trading the no-flicker guarantee for zero-latency display.
    if (this.getMode() === "experimental") this.predictionEpoch = this.confirmedEpoch;
    for (let i = 0; i < bytes.length; i++) {
      const b = bytes[i]!;
      if (b === 0x7f || b === 0x08) {            // backspace
        const col = (this.predCursorCol >= 0 ? this.predCursorCol : this.cursorCol) - 1;
        if (col < 0) { this.becomeTentative(); continue; }
        this.predCursorCol = col;
        // drop a prediction sitting at the now-deleted col
        this.preds = this.preds.filter((p) => !(p.row === this.cursorRow && p.col === col));
        continue;
      }
      // Left/right arrow (CSI 'C'/'D'): predict
      // the cursor move only (no glyph). ESC[C = right, ESC[D = left.
      if (b === 0x1b && bytes[i + 1] === 0x5b && (bytes[i + 2] === 0x43 || bytes[i + 2] === 0x44)) {
        const base = this.predCursorCol >= 0 ? this.predCursorCol : this.cursorCol;
        const dir = bytes[i + 2] === 0x43 ? 1 : -1;
        this.predCursorCol = Math.max(0, Math.min(this.cols - 1, base + dir));
        i += 2; // consumed '[' and 'C'/'D'
        continue;
      }
      // Refuse: control (<0x20), DEL handled above, ESC/CSI start, high bytes
      // (multi-byte UTF-8 / wide) — predicting those is ambiguous.
      if (b < 0x20 || b > 0x7e) { this.becomeTentative(); continue; }
      const col = this.predCursorCol >= 0 ? this.predCursorCol : this.cursorCol;
      if (col + 1 >= this.cols) { this.becomeTentative(); continue; } // last-col wrap ambiguous
      this.preds.push({
        row: this.cursorRow, col, ch: String.fromCharCode(b),
        epoch: this.predictionEpoch, bornMs: this.now(), bornSeq: this.lastSeq,
      });
      this.predCursorCol = col + 1;
    }
    this.repaint();
  }

  /** An authoritative cell frame landed. Update grid state, reconcile every
   *  prediction against it, then repaint survivors. */
  onFrame(frame: CellGridFrame): void {
    const prevAlt = this.altScreen;
    const prevCols = this.cols;            // capture BEFORE overwrite (resize detect)
    const prevRows = this.rows;
    this.cursorRow = frame.cursorRow;
    this.cursorCol = frame.cursorCol;
    this.cols = frame.cols;
    this.altScreen = frame.altScreen;
    this.lastSeq = frame.seq;
    this.rows = frame.rows;

    // Wipe ONLY when prediction coordinates are actually invalidated: alt-screen
    // entry/toggle, content scroll, or a detected resize (cols / viewport HEIGHT
    // changed). A non-resize full OR delta frame keeps the same viewport coords,
    // so RECONCILE against it — there's no "full frame wipes
    // predictions" concept (cull judges every framebuffer
    // update). The old `|| frame.full` wiped on every attach/claim/force/first-
    // emit full frame, killing predictions before the echo delta could confirm
    // them, so SRTT was never sampled when full frames interleaved the first
    // keystrokes. NOTE: resize is detected via frame.rows (the viewport height,
    // stable across non-resize deltas) — NOT frame.viewportRows.length, which on
    // a DELTA is the dirty-ROW COUNT (types.ts:58-61, grid-to-cells.ts:112-114)
    // and drifts every frame, which would wipe on every delta and keep SRTT at 0.
    const resized = prevCols !== 0 &&
      (frame.cols !== prevCols || frame.rows !== prevRows);
    if (this.altScreen || prevAlt !== this.altScreen || frame.scrollbackAppend.length > 0 || resized) {
      this.resetAll();
      this.predCursorCol = -1;
      return;
    }

    const now = this.now();
    const survivors: Pred[] = [];
    let hardReset = false;
    for (const p of this.preds) {
      const shownBefore = !this.isTentative(p);
      // Only judge against a frame produced AFTER the prediction (echo had a
      // chance to land); a same/older frame is "pending".
      if (frame.seq <= p.bornSeq) {
        if (now - p.bornMs >= GLITCH_MS) this.glitch = true;
        survivors.push(p);
        continue;
      }
      const actual = cellCharAt(frame, p.row, p.col);
      if (actual === p.ch) {
        // Correct → confirm: unlock display for this epoch, sample RTT, retire.
        this.confirmedEpoch = Math.max(this.confirmedEpoch, p.epoch);
        this.sampleRtt(now - p.bornMs);
        this.glitch = false;
      } else if (this.getMode() === "experimental") {
        // Experimental mode: reset just the wrong cell — never a
        // hard reset, never an epoch kill. Flickerier, but each cell
        // self-corrects independently.
        p.epoch = -1;
      } else {
        // Server reached a later frame but the cell differs → wrong guess.
        if (shownBefore) { hardReset = true; break; }   // a SHOWN guess was wrong → nuke all
        // hidden/tentative wrong guess → drop just this epoch
        this.preds.forEach((q) => { if (q.epoch === p.epoch) q.epoch = -1; });
      }
    }
    if (hardReset) { this.resetAll(); return; }
    this.preds = survivors.filter((p) => p.epoch >= 0);
    // Re-anchor predicted cursor: authoritative col + count of pending preds on
    // the cursor row to the right of the authoritative cursor.
    const ahead = this.preds.filter((p) => p.row === this.cursorRow && p.col >= this.cursorCol).length;
    this.predCursorCol = ahead > 0 ? this.cursorCol + ahead : -1;
    this.repaint();
  }

  private isTentative(p: Pred): boolean { return p.epoch > this.confirmedEpoch; }

  private sampleRtt(rttMs: number): void {
    if (rttMs <= 0 || rttMs > 5000) return;          // ignore absurd samples
    this.srtt = this.srtt === 0 ? rttMs : this.srtt * 0.875 + rttMs * 0.125; // α=1/8
    diag("echo.rtt_sample", { sid: this.sid, rtt_ms: rttMs });
  }

  /** Paint visible, non-tentative predictions into the overlay (re-attached
   *  because the renderer's replaceChildren wipes viewport children). */
  private repaint(): void {
    // Re-attach after an explicit full repair may have rebuilt the viewport.
    if (this.overlay.parentNode !== this.viewportEl) this.viewportEl.appendChild(this.overlay);
    const mode = this.getMode();
    const show = mode !== "never" && !this.altScreen && this.shouldShow(mode);
    if (!show) { this.overlay.replaceChildren(); this.onCursor(null); return; }
    // Predicted cursor (ConditionalCursorMove): the caret leads the
    // echoed chars / an arrow move. Suppressed while any char prediction is
    // still tentative (hidden) — don't jump the caret ahead of unshown text.
    const blocked = this.preds.some((p) => this.isTentative(p));
    this.onCursor(this.predCursorCol >= 0 && !blocked ? this.predCursorCol : null);
    const flag = this.shouldFlag();
    const doc = this.viewportEl.ownerDocument;
    const frag = doc.createDocumentFragment();
    for (const p of this.preds) {
      if (this.isTentative(p)) continue;             // hidden until epoch confirmed
      const el = doc.createElement("span");
      el.className = "cell-predict-ch";
      el.textContent = p.ch;
      el.style.position = "absolute";
      el.style.top = `${p.row}lh`;
      el.style.left = `${p.col}ch`;
      if (flag) el.style.textDecoration = "underline";
      frag.appendChild(el);
    }
    this.overlay.replaceChildren(frag);
  }

  /** Apply a reactive Settings change immediately, even while the terminal is
   * idle and no keystroke/frame would otherwise trigger repaint. */
  refreshPreference(): void {
    if (this.getMode() === "never") this.resetAll();
    else this.repaint();
  }

  clear(): void {
    this.resetAll();
  }

  dispose(): void {
    this.overlay.remove();
    this.preds = [];
  }

  /** Test seam — internal state for unit tests (no DOM assertions needed). */
  _debug(): { total: number; visible: number; srtt: number; confirmedEpoch: number; predictionEpoch: number; mode: string; predCursorCol: number } {
    const mode = this.getMode();
    const showing = mode !== "never" && !this.altScreen && this.shouldShow(mode);
    const visible = showing ? this.preds.filter((p) => !this.isTentative(p)).length : 0;
    return {
      total: this.preds.length, visible, srtt: this.srtt,
      confirmedEpoch: this.confirmedEpoch, predictionEpoch: this.predictionEpoch, mode,
      predCursorCol: this.predCursorCol,
    };
  }
}

function nowMs(): number {
  return typeof performance !== "undefined" ? performance.now() : Date.now();
}
