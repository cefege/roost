// DOM half of predictive local echo: one absolutely-positioned overlay div
// inside a CellTerminal viewport, painting one span per predicted cell.
// predictiveEcho.ts owns the state machine and calls paint()/clear(); this file
// owns element creation, re-attachment after the renderer's replaceChildren
// detaches it, and the glyph-vs-erase child shape.

export interface PredictedCell {
  row: number;
  col: number;
  /** The predicted glyph, or "" for an ERASE cell (paint the terminal bg). */
  ch: string;
}

export class PredictiveEchoOverlay {
  private readonly overlay: HTMLDivElement;

  constructor(private readonly viewportEl: HTMLElement) {
    this.overlay = viewportEl.ownerDocument.createElement("div");
    this.overlay.className = "cell-predict";
    this.overlay.style.position = "absolute";
    this.overlay.style.top = "0";
    this.overlay.style.left = "0";
    this.overlay.style.pointerEvents = "none";
    viewportEl.appendChild(this.overlay);
  }

  /** Repaint every visible prediction. `flagged` underlines GLYPH cells only —
   *  an underlined eraser paints a line the terminal never drew. */
  paint(cells: readonly PredictedCell[], flagged: boolean): void {
    this.attach();
    const doc = this.viewportEl.ownerDocument;
    const frag = doc.createDocumentFragment();
    for (const cell of cells) {
      const el = doc.createElement("span");
      el.style.position = "absolute";
      el.style.top = `${cell.row}lh`;
      el.style.left = `${cell.col}ch`;
      if (cell.ch === "") {
        el.className = "cell-predict-erase";
      } else {
        el.className = "cell-predict-ch";
        el.textContent = cell.ch;
        if (flagged) el.style.textDecoration = "underline";
      }
      frag.appendChild(el);
    }
    this.overlay.replaceChildren(frag);
  }

  clear(): void {
    this.attach();
    this.overlay.replaceChildren();
  }

  dispose(): void {
    this.overlay.remove();
  }

  /** The renderer rebuilds viewport children on a full repair, which detaches
   *  this overlay; re-append instead of silently painting into a dead node. */
  private attach(): void {
    if (this.overlay.parentNode !== this.viewportEl) {
      this.viewportEl.appendChild(this.overlay);
    }
  }
}
