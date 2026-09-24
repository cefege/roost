// Predictive (speculative) local echo — the prediction state machine over
// Roost's cell stream. Paints a typed char IMMEDIATELY instead of waiting a
// full round-trip, then reconciles it against the authoritative CellGridFrame.
// Driven by CellTerminal: predict() per keystroke, noteInputWritten() per input
// admission ack, onFrame() per frame. Grid predicates live in
// predictiveEchoGrid.ts, DOM in predictiveEchoOverlay.ts.
//
// A pure client overlay: it never touches the byte/cell stream or the worker,
// and the epoch/ack/grace gates below are what keep a wrong guess off screen.

import { diag } from "@roost/observability/diag";
import { columnText, type CellGridFrame, type CellSpan } from "@roost/protocol/cell";
import type { PredictMode } from "./predictPref.ts";
import { PredictionExpiryTimer } from "./predictiveEchoExpiry.ts";
import {
  cellCharAt,
  erasableCell,
  judgePrediction,
  type Pred,
} from "./predictiveEchoGrid.ts";
import {
  PredictiveEchoOverlay,
  type PredictedCell,
} from "./predictiveEchoOverlay.ts";
import { PredictionPainter } from "./predictiveEchoPaint.ts";

// SRTT/2 thresholds, low enough that local echo engages on a LAN/tailnet link
// (~15-30ms RTT) and only a single-digit-ms loopback stays a no-op.
const SHOW_ON_MS = 5;     // srtt/2 > 5 (≈ RTT > 10ms) → engage predictions
const SHOW_OFF_MS = 3;    // srtt/2 ≤ 3 (≈ RTT < 6ms) & idle → disengage
const FLAG_ON_MS = 80;    // srtt/2 > 80 (≈ RTT > 160ms) → underline the guess
const GLITCH_MS = 250;    // a prediction pending this long → force-show (link stalled)

export class PredictiveEcho {
  private overlay: PredictiveEchoOverlay;
  private painter: PredictionPainter;
  private preds: Pred[] = [];
  private predictionEpoch = 1;
  private confirmedEpoch = 0;
  private srtt = 0;            // EWMA of echo RTT (ms); 0 = unmeasured
  private glitch = false;
  private srttTrigger = false;  // hysteresis: once engaged, stays on through the dead-band until idle + low SRTT
  // Wipe accounting, read by the smoke tier: a burst that resets itself is the
  // felt-latency defect, and it is invisible in painted state alone.
  // clearedCount is tracked apart from resetCount because only clear() — the
  // pane's DOM-stall watchdog — is an EXTERNAL wipe of correct predictions; a
  // contradiction or expiry reset is the engine's own documented rule firing.
  private resetCount = 0;
  private clearedCount = 0;
  private lastReset: string | null = null;

  // Authoritative grid state, updated each frame.
  private cursorRow = 0;
  private cursorCol = 0;
  private cols = 0;
  private rows = 0;            // last-seen viewport HEIGHT (frame.rows) for resize detection, mirrors cols
  private altScreen = false;
  private cursorRowSpans: readonly CellSpan[] = [];
  private predCursorCol = -1;  // predicted cursor col (−1 = none ahead of auth)
  private readonly expiry: PredictionExpiryTimer;

  private readonly now: () => number;
  private readonly onCursor: (col: number | null) => void;
  private readonly sid: string;
  private readonly getMode: () => PredictMode;

  constructor(
    viewportEl: HTMLElement,
    opts: {
      mode: () => PredictMode;
      now?: () => number;
      onCursor?: (col: number | null) => void;
      sid?: string;
      /** Arms a deferred expiry pass; returns its canceller. Tests inject a
       *  no-op scheduler and drive _expirePredictions() against their clock. */
      schedule?: (callback: () => void, delayMs: number) => () => void;
    },
  ) {
    this.now = opts.now ?? nowMs;
    this.sid = opts.sid ?? "";
    this.onCursor = opts.onCursor ?? (() => {});
    this.getMode = opts.mode;
    this.expiry = new PredictionExpiryTimer({
      now: this.now,
      schedule: opts.schedule ?? ((callback, delayMs) => {
        const id = setTimeout(callback, delayMs);
        return () => clearTimeout(id);
      }),
      oldestBornMs: () => {
        let oldest: number | null = null;
        for (const pred of this.preds) {
          if (oldest === null || pred.bornMs < oldest) oldest = pred.bornMs;
        }
        return oldest;
      },
      srtt: () => this.srtt,
      onExpire: () => this.resetAll("expired"),
    });
    this.overlay = new PredictiveEchoOverlay(viewportEl);
    this.painter = new PredictionPainter(this.overlay, this.onCursor);
  }

  /** The display gate: Always/Experimental paint unconditionally; Adaptive
   *  paints only on a slow link (the SRTT hysteresis). The epoch confidence gate
   *  (isTentative) is applied separately in repaint. */
  private shouldShow(mode: PredictMode): boolean {
    if (mode === "always" || mode === "experimental") return true;
    const half = this.srtt / 2;
    // Arm at SRTT/2 > SHOW_ON and stay armed through the 20–30 ms dead-band,
    // disarming only when SRTT/2 ≤ SHOW_OFF AND idle: a stateless recomputation
    // returns false inside that band, so nothing would ever paint there.
    if (this.glitch) this.srttTrigger = true;
    else if (half > SHOW_ON_MS) this.srttTrigger = true;
    else if (half <= SHOW_OFF_MS && this.preds.length === 0) this.srttTrigger = false;
    return this.srttTrigger;
  }
  private shouldFlag(): boolean {
    return this.glitch || this.srtt / 2 > FLAG_ON_MS;
  }

  private becomeTentative(): void { this.predictionEpoch++; }

  /** Drop every prediction AND re-arm the confidence gate. Without the re-arm,
   *  the next keystroke is shown on an authoritative cursor column that still
   *  lags the un-echoed input — the wrong glyph the user sees snap back. */
  private resetAll(reason: string): void {
    this.preds = [];
    this.predCursorCol = -1;
    this.glitch = false;
    this.srttTrigger = false;
    this.resetCount++;
    if (reason === "cleared") this.clearedCount++;
    this.lastReset = reason;
    this.becomeTentative();
    this.expiry.arm();
    diag("echo.reset", { sid: this.sid, reason });
    this.repaint();
  }

  /** A keystroke the user typed. Predict its echo (always — to measure RTT;
   *  display is gated in repaint). Only printable width-1 + backspace; anything
   *  ambiguous bumps the epoch so later predictions stay hidden until reproven.
   *  `inputSeq` is the admission sequence of the batch carrying these bytes;
   *  noteInputWritten(seq) later proves the worker wrote them to the PTY. */
  predict(bytes: Uint8Array, inputSeq: bigint): void {
    if (this.getMode() === "never" || this.altScreen) {
      if (this.preds.length) this.resetAll("suppressed");
      return;
    }
    // Never predict a paste: it floods the overlay and its echo is unguessable.
    if (bytes.length > 100) { this.resetAll("paste"); return; }
    // Experimental mode has no tentative epoch — predictions show IMMEDIATELY
    // (predictionEpoch == confirmedEpoch ⇒ not tentative), trading the
    // no-flicker guarantee for zero-latency display.
    if (this.getMode() === "experimental") this.predictionEpoch = this.confirmedEpoch;
    for (let i = 0; i < bytes.length; i++) {
      const b = bytes[i]!;
      if (b === 0x7f || b === 0x08) {            // backspace
        this.predictErase(inputSeq);
        continue;
      }
      // Left/right arrow (CSI 'C'/'D'): predict the cursor move only (no glyph).
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
      // A glyph typed over our own eraser supersedes it; keeping both would make
      // the eraser contradict the very echo that confirms the glyph.
      const stale = this.preds.findIndex(
        (pred) => pred.ch === "" && pred.row === this.cursorRow && pred.col === col,
      );
      if (stale >= 0) this.preds.splice(stale, 1);
      this.preds.push({
        row: this.cursorRow, col, ch: String.fromCharCode(b),
        originalCh: columnText(this.cursorRowSpans, col),
        epoch: this.predictionEpoch, bornMs: this.now(), inputSeq, ackedMs: null,
      });
      this.predCursorCol = col + 1;
    }

    this.expiry.arm();
    this.repaint();
  }

  /** Backspace: paint an ERASE over the column the echo is about to clear.
   *  Moving the caret alone leaves the authoritative glyph under it for a full
   *  round-trip, which reads as "my correction did nothing". */
  private predictErase(inputSeq: bigint): void {
    const col = (this.predCursorCol >= 0 ? this.predCursorCol : this.cursorCol) - 1;
    if (col < 0) { this.becomeTentative(); return; }
    this.predCursorCol = col;
    const owned = this.preds.findIndex(
      (pred) => pred.row === this.cursorRow && pred.col === col,
    );
    if (owned >= 0) { this.preds.splice(owned, 1); return; }  // erasing our own guess
    const erasable = erasableCell(this.cursorRowSpans, col);
    if (erasable === "blank") return;                         // nothing painted there
    if (erasable === "refuse") { this.becomeTentative(); return; }
    this.preds.push({
      row: this.cursorRow, col, ch: "",
      originalCh: columnText(this.cursorRowSpans, col),
      epoch: this.predictionEpoch, bornMs: this.now(), inputSeq, ackedMs: null,
    });
  }

  /** The worker acknowledged writing every byte up to `inputSeq` to the PTY.
   *  Predictions from those batches become judgeable after ECHO_GRACE_MS. */
  noteInputWritten(inputSeq: bigint): void {
    for (const pred of this.preds) {
      if (pred.ackedMs === null && pred.inputSeq <= inputSeq) pred.ackedMs = this.now();
    }
    this.expiry.arm();
  }

  /** An authoritative cell frame landed. Update grid state, reconcile every
   *  prediction against it, then repaint survivors. */
  onFrame(
    frame: CellGridFrame,
    scrollbackAppended = frame.scrollbackAppend.length > 0,
  ): void {
    const prevAlt = this.altScreen;
    const prevCols = this.cols;            // capture BEFORE overwrite (resize detect)
    const prevRows = this.rows;
    this.cursorRow = frame.cursorRow;
    this.cursorCol = frame.cursorCol;
    this.cols = frame.cols;
    this.altScreen = frame.altScreen;
    this.rows = frame.rows;
    this.cursorRowSpans = frame.viewportRows.find(
      (row) => row.index === frame.cursorRow,
    )?.spans ?? [];

    // Wipe ONLY when prediction coordinates are actually invalidated: alt-screen
    // entry/toggle, content scroll, or a detected resize. A non-resize full OR
    // delta frame keeps the same viewport coords, so RECONCILE against it —
    // wiping on every full frame kills predictions before the echo delta can
    // confirm them, and SRTT is then never sampled. Resize is detected via
    // frame.rows (the viewport height, stable across non-resize deltas) — NOT
    // frame.viewportRows.length, which on a DELTA is the dirty-ROW COUNT
    // (types.ts:58-61, grid-to-cells.ts:112-114) and drifts every frame.
    const resized = prevCols !== 0 &&
      (frame.cols !== prevCols || frame.rows !== prevRows);
    if (this.altScreen || prevAlt !== this.altScreen || scrollbackAppended || resized) {
      this.resetAll(
        this.altScreen || prevAlt !== this.altScreen
          ? "alt_screen"
          : resized ? "resized" : "scrolled",
      );
      return;
    }

    this.reconcileAgainst(frame, this.now());
  }

  /** Judge every prediction against one frame. `frameAtMs` is when the frame
   *  ARRIVED: RTT is sampled from it so a deferred pass can never inflate SRTT,
   *  and the ack/grace comparisons are meaningless against any other clock. */
  private reconcileAgainst(frame: CellGridFrame, frameAtMs: number): void {
    const survivors: Pred[] = [];
    const firstTentativeByEpoch = new Map<number, Pred>();
    for (const pred of this.preds) {
      if (this.isTentative(pred) && !firstTentativeByEpoch.has(pred.epoch)) {
        firstTentativeByEpoch.set(pred.epoch, pred);
      }
    }
    let hardReset = false;
    for (const pred of this.preds) {
      const shownBefore = !this.isTentative(pred);
      const verdict = judgePrediction(pred, cellCharAt(frame, pred.row, pred.col), frameAtMs);
      if (verdict === "credit" || verdict === "retire") {
        if (verdict === "credit" && this.isTentative(pred) && firstTentativeByEpoch.get(pred.epoch) !== pred) {
          survivors.push(pred);
          continue;
        }
        if (verdict === "credit") {
          this.confirmedEpoch = Math.max(this.confirmedEpoch, pred.epoch);
          this.sampleRtt(frameAtMs - pred.bornMs);
        }
        this.glitch = false;
        continue;
      }
      if (verdict === "unproven") {
        if (this.now() - pred.bornMs >= GLITCH_MS) this.glitch = true;
        survivors.push(pred);
        continue;
      }
      if (verdict === "echoing") { survivors.push(pred); continue; }
      // Experimental mode drops just the wrong cell — never a hard reset, never
      // an epoch kill. Flickerier, but each cell self-corrects independently.
      if (this.getMode() === "experimental") continue;
      if (shownBefore) { hardReset = true; break; }   // a SHOWN guess was wrong → nuke all
      // hidden/tentative wrong guess → drop just this epoch, and re-arm the gate
      this.preds.forEach((other) => { if (other.epoch === pred.epoch) other.epoch = -1; });
      this.becomeTentative();
    }
    if (hardReset) { this.resetAll("contradicted"); return; }
    this.preds = survivors.filter((pred) => pred.epoch >= 0);
    this.reanchorPredictedCursor();
    this.expiry.arm();
    this.repaint();
  }

  /** The final surviving prediction already carries the absolute predicted
   *  caret. Deriving from authoritative cursor + survivor count double-counts
   *  echoes when a sparse cursor-only frame omits the row that changed. */
  private reanchorPredictedCursor(): void {
    const last = this.preds.at(-1);
    if (!last) {
      this.predCursorCol = -1;
      return;
    }
    const predicted = last.col + (last.ch === "" ? 0 : 1);
    this.predCursorCol = predicted >= 0 && predicted < this.cols ? predicted : -1;
  }

  /** Test seam — the deferred expiry pass, driveable against an injected clock
   *  instead of a real timer. */
  _expirePredictions(): void {
    this.expiry.check();
  }

  private isTentative(pred: Pred): boolean { return pred.epoch > this.confirmedEpoch; }

  private sampleRtt(rttMs: number): void {
    if (rttMs <= 0 || rttMs > 5000) return;          // ignore absurd samples
    this.srtt = this.srtt === 0 ? rttMs : this.srtt * 0.875 + rttMs * 0.125; // α=1/8
    diag("echo.rtt_sample", { sid: this.sid, rtt_ms: rttMs });
  }

  /** Hand the painter the visible, non-tentative predictions. */
  private repaint(): void {
    const mode = this.getMode();
    if (mode === "never" || this.altScreen || !this.shouldShow(mode)) {
      this.painter.request(null);
      return;
    }
    // The caret leads the echoed chars / an arrow move, but never leads text
    // that is still tentative (hidden).
    const blocked = this.preds.some((pred) => this.isTentative(pred));
    const cells: PredictedCell[] = [];
    for (const pred of this.preds) {
      if (this.isTentative(pred)) continue;          // hidden until epoch confirmed
      cells.push({ row: pred.row, col: pred.col, ch: pred.ch });
    }
    this.painter.request({
      cells,
      flagged: this.shouldFlag(),
      caretCol: this.predCursorCol >= 0 && !blocked ? this.predCursorCol : null,
    });
  }

  /** Apply a reactive Settings change immediately, even while the terminal is
   * idle and no keystroke/frame would otherwise trigger repaint. */
  refreshPreference(): void {
    if (this.getMode() === "never") this.resetAll("preference");
    else this.repaint();
  }

  clear(): void {
    this.resetAll("cleared");
  }

  dispose(): void {
    this.expiry.cancel();
    this.painter.cancel();
    this.overlay.dispose();
    this.preds = [];
  }

  /** Test seam — internal state for unit tests (no DOM assertions needed). */
  _debug(): { total: number; visible: number; srtt: number; confirmedEpoch: number; predictionEpoch: number; mode: string; predCursorCol: number; resetCount: number; clearedCount: number; lastReset: string | null } {
    const mode = this.getMode();
    const showing = mode !== "never" && !this.altScreen && this.shouldShow(mode);
    const visible = showing ? this.preds.filter((pred) => !this.isTentative(pred)).length : 0;
    return {
      total: this.preds.length, visible, srtt: this.srtt,
      confirmedEpoch: this.confirmedEpoch, predictionEpoch: this.predictionEpoch, mode,
      predCursorCol: this.predCursorCol,
      resetCount: this.resetCount, clearedCount: this.clearedCount, lastReset: this.lastReset,
    };
  }
}

function nowMs(): number {
  return typeof performance !== "undefined" ? performance.now() : Date.now();
}
