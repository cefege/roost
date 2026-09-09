// Owns deferred DOM application for one terminal renderer.
// Terminal stream subscribers enqueue folded canonical state here instead of
// mutating a renderer during Sync dispatch. It coalesces to one animation frame
// and repairs skipped sparse deltas with the newest canonical full frame.

import { cloneCellGridFrame, type CellGridFrame } from "@roost/shared/cell";
import { diag, isDiagEnabled } from "@roost/shared/diag";
import type { CellGridRenderer } from "./cellRenderer.ts";

type PendingTerminalRender = {
  frame: CellGridFrame;
  canonical: CellGridFrame;
  mode: "delta" | "full";
};

/** Schedules one renderer's latest canonical terminal state for DOM application. */
export class TerminalRenderScheduler {
  private pending: PendingTerminalRender | null = null;
  private animationFrame: number | null = null;
  private foreground = false;
  private disposed = false;
  private streamId: string | null = null;
  private gridEpoch: string | null = null;
  private seq: number | null = null;

  constructor(
    private readonly renderer: CellGridRenderer,
    private readonly sessionId: string,
    private readonly onApplied?: (frame: CellGridFrame) => void,
  ) {}

  enqueue(frame: CellGridFrame, canonical: CellGridFrame): void {
    if (this.disposed) return;
    this.pending = this.canApplyDelta(frame)
      ? { frame, canonical, mode: "delta" }
      : { frame: canonical, canonical, mode: "full" };
    this.schedule();
  }

  setForeground(active: boolean): void {
    if (this.disposed) return;
    this.foreground = active;
    if (!active) {
      this.cancelScheduledFrame();
      return;
    }
    this.schedule();
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.pending = null;
    this.cancelScheduledFrame();
  }

  private canApplyDelta(frame: CellGridFrame): boolean {
    return !frame.full
      && this.pending === null
      && this.streamId === frame.streamId
      && this.gridEpoch === frame.gridEpoch
      && this.seq === frame.baseSeq;
  }

  private schedule(): void {
    if (
      this.disposed
      || !this.foreground
      || this.pending === null
      || this.animationFrame !== null
    ) return;
    if (typeof requestAnimationFrame !== "function") {
      this.applyPendingFrame();
      return;
    }
    this.animationFrame = requestAnimationFrame(() => {
      this.animationFrame = null;
      this.applyPendingFrame();
    });
  }

  private cancelScheduledFrame(): void {
    if (this.animationFrame === null) return;
    if (typeof cancelAnimationFrame === "function") {
      cancelAnimationFrame(this.animationFrame);
    }
    this.animationFrame = null;
  }

  private applyPendingFrame(): void {
    if (this.disposed || !this.foreground) return;
    const pending = this.pending;
    if (!pending) return;
    this.pending = null;

    const frame = cloneCellGridFrame(pending.frame);
    if (pending.mode === "full") {
      frame.full = true;
      frame.baseSeq = 0;
    }
    const diagnostics = isDiagEnabled();
    const startedAt = diagnostics ? performance.now() : 0;
    const applied = pending.mode === "full"
      ? this.renderer.applyFullFrame(frame)
      : this.renderer.applyDeltaFrame(frame);
    if (!applied) {
      if (pending.mode === "delta") {
        this.pending = { ...pending, frame: pending.canonical, mode: "full" };
        this.schedule();
      } else {
        this.pending = pending;
      }
      return;
    }

    this.streamId = frame.streamId;
    this.gridEpoch = frame.gridEpoch;
    this.seq = frame.seq;
    if (diagnostics) {
      diag("cell.apply_dur", {
        sid: this.sessionId,
        seq: frame.seq,
        full: frame.full,
        dur_ms: performance.now() - startedAt,
      });
    }
    this.onApplied?.(frame);
    this.schedule();
  }
}
