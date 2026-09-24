// Frame batching for predictive local echo's DOM writes: predictiveEcho.ts
// calls request() on every keystroke and every frame, and this collapses a
// burst of those into ONE overlay paint plus one caret write per animation
// frame. Writes through predictiveEchoOverlay.ts and the renderer's
// predicted-cursor callback; owns no prediction state of its own.
//
// Writing the overlay synchronously inside keydown cost a forced reflow per
// keystroke: the write dirtied layout, and the NEXT keydown's bottom-pin
// scrollHeight read flushed it, priced by painted rows plus scrollback.

import type {
  PredictedCell,
  PredictiveEchoOverlay,
} from "./predictiveEchoOverlay.ts";

export interface PredictionPaint {
  cells: PredictedCell[];
  flagged: boolean;
  caretCol: number | null;
}

export class PredictionPainter {
  private pending: PredictionPaint | null = null;
  private armed = false;
  private animationFrame: number | null = null;

  constructor(
    private readonly overlay: PredictiveEchoOverlay,
    private readonly onCursor: (col: number | null) => void,
  ) {}

  /** `null` clears the overlay. Latest state wins; one flush per frame. */
  request(paint: PredictionPaint | null): void {
    this.pending = paint;
    this.armed = true;
    if (this.animationFrame !== null) return;
    // No rAF (unit tier, fake DOM) → paint inline, so callers can assert the
    // overlay synchronously without a frame pump.
    if (typeof requestAnimationFrame !== "function") {
      this.flush();
      return;
    }
    this.animationFrame = requestAnimationFrame(() => {
      this.animationFrame = null;
      this.flush();
    });
  }

  /** Drop any queued frame: a flush after dispose() writes a removed node. */
  cancel(): void {
    if (this.animationFrame !== null && typeof cancelAnimationFrame === "function") {
      cancelAnimationFrame(this.animationFrame);
    }
    this.animationFrame = null;
    this.pending = null;
    this.armed = false;
  }

  private flush(): void {
    if (!this.armed) return;              // a null paint is a request; no request is not
    const paint = this.pending;
    this.armed = false;
    this.pending = null;
    if (paint === null) {
      this.overlay.clear();
      this.onCursor(null);
      return;
    }
    this.onCursor(paint.caretCol);
    this.overlay.paint(paint.cells, paint.flagged);
  }
}
