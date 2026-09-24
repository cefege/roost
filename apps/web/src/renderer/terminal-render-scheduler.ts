// Owns deferred DOM application for one terminal renderer.
// Terminal stream subscribers queue sparse deltas here so one browser frame
// folds and paints a bounded batch. Parked renderers discard sparse work while
// retaining the latest canonical frame and raw delivery metadata. Renderer-owned
// full application supplies the sole full-frame clone.

import { type CellGridFrame } from "@roost/protocol/cell";
import { diag, isDiagEnabled } from "@roost/observability/diag";
import type { CellGridRenderer } from "./cellRenderer.ts";
import {
  noteTerminalRenderApplied,
  noteTerminalRenderApply,
  noteTerminalRendererDisposed,
} from "./terminalIncidentCapture.ts";

const MAX_PENDING_DELTA_FRAMES = 64;
const MAX_PENDING_SCROLLBACK_ROWS = 250;
const MAX_PENDING_DELTA_SPANS = 65_536;

type FullRenderSource = "wire_full" | "fallback_full";

type PendingTerminalRender =
  | {
      mode: "delta";
      canonical: CellGridFrame;
      deltas: CellGridFrame[];
      appendedRows: number;
      spanCount: number;
      queuedAt: number | null;
    }
  | {
      mode: "full";
      canonical: CellGridFrame;
      /** Raw wire frame retained for activity and mode consumers. */
      deliveryFrame: CellGridFrame;
      source: FullRenderSource;
      appendedRows: number;
      batchFrames: number;
      hadWireFull: boolean;
      queuedAt: number | null;
    };

/** Queued deltas outlive later canonical folding, which may renumber their rows. */
function ownQueuedDelta(frame: CellGridFrame): CellGridFrame {
  return {
    ...frame,
    viewportRows: frame.viewportRows.map((row) => ({
      index: row.index,
      spans: row.spans,
    })),
    scrollbackAppend: frame.scrollbackAppend.map((row) => ({
      index: row.index,
      spans: row.spans,
    })),
  };
}

function countIncomingSpans(frame: CellGridFrame, limit: number): number {
  let spanCount = 0;
  for (const row of frame.viewportRows) {
    spanCount += row.spans.length;
    if (spanCount > limit) return spanCount;
  }
  for (const row of frame.scrollbackAppend) {
    spanCount += row.spans.length;
    if (spanCount > limit) return spanCount;
  }
  return spanCount;
}

/** Schedules one renderer's latest canonical terminal state for DOM application. */
export class TerminalRenderScheduler {
  private pending: PendingTerminalRender | null = null;
  private animationFrame: number | null = null;
  private foreground = false;
  private disposed = false;
  private streamId: string | null = null;
  private gridEpoch: string | null = null;
  private cols: number | null = null;
  private rows: number | null = null;
  private altScreen: boolean | null = null;
  private seq: number | null = null;

  constructor(
    private readonly renderer: CellGridRenderer,
    private readonly sessionId: string,
    private readonly onApplied?: (
      frame: CellGridFrame,
      canonical: CellGridFrame,
      scrollbackAppended: boolean,
      hadWireFull: boolean,
    ) => void,
  ) {}

  enqueue(frame: CellGridFrame, canonical: CellGridFrame): void {
    if (this.disposed) return;
    if (frame.full) {
      if (this.fullConflictsWithKnownCanonical(canonical)) return;
      this.pending = this.fullPending(
        canonical,
        frame,
        "wire_full",
        this.pending?.appendedRows ?? 0,
      );
    } else if (!this.enqueueDelta(frame, canonical)) {
      const pending = this.pending;
      let batchFrames = 1;
      let hadWireFull = false;
      if (pending) {
        batchFrames = pending.mode === "delta"
          ? pending.deltas.length + 1
          : pending.batchFrames + 1;
        hadWireFull = pending.mode === "full" && pending.hadWireFull;
      }
      this.pending = this.fullPending(
        canonical,
        ownQueuedDelta(frame),
        "fallback_full",
        (pending?.appendedRows ?? 0) + frame.scrollbackAppend.length,
        batchFrames,
        hadWireFull,
        pending?.queuedAt,
      );
    }
    this.schedule();
  }

  setForeground(active: boolean): void {
    if (this.disposed) return;
    this.foreground = active;
    if (!active) {
      this.parkPendingDeltas();
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
    noteTerminalRendererDisposed(this.sessionId, this.renderer);
  }

  private fullPending(
    canonical: CellGridFrame,
    deliveryFrame: CellGridFrame,
    source: FullRenderSource,
    appendedRows = 0,
    batchFrames = 1,
    hadWireFull = source === "wire_full",
    queuedAt = isDiagEnabled() ? performance.now() : null,
  ): PendingTerminalRender {
    return {
      mode: "full",
      canonical,
      deliveryFrame,
      source,
      appendedRows,
      batchFrames,
      hadWireFull,
      queuedAt,
    };
  }

  private fullConflictsWithKnownCanonical(canonical: CellGridFrame): boolean {
    const known = this.pending?.canonical;
    const knownStreamId = known?.streamId ?? this.streamId;
    const knownSeq = known?.seq ?? this.seq;
    if (knownStreamId !== canonical.streamId || knownSeq === null) return false;
    if (canonical.seq !== knownSeq) return canonical.seq < knownSeq;
    return (known?.gridEpoch ?? this.gridEpoch) !== canonical.gridEpoch
      || (known?.cols ?? this.cols) !== canonical.cols
      || (known?.rows ?? this.rows) !== canonical.rows
      || (known?.altScreen ?? this.altScreen) !== canonical.altScreen;
  }

  private enqueueDelta(frame: CellGridFrame, canonical: CellGridFrame): boolean {
    if (!this.canQueueDelta(frame)) return false;
    const queued = this.pending;
    const pending = queued?.mode === "delta" ? queued : null;
    const frameCount = (pending?.deltas.length ?? 0) + 1;
    const appendedRows = (pending?.appendedRows ?? 0) + frame.scrollbackAppend.length;
    const priorSpanCount = pending?.spanCount ?? 0;
    const remainingSpans = MAX_PENDING_DELTA_SPANS - priorSpanCount;
    const spanCount = countIncomingSpans(frame, remainingSpans);
    if (
      frameCount > MAX_PENDING_DELTA_FRAMES
      || appendedRows > MAX_PENDING_SCROLLBACK_ROWS
      || spanCount > remainingSpans
    ) return false;

    const owned = ownQueuedDelta(frame);
    if (pending) {
      pending.deltas.push(owned);
      pending.canonical = canonical;
      pending.appendedRows = appendedRows;
      pending.spanCount = priorSpanCount + spanCount;
      return true;
    }
    this.pending = {
      mode: "delta",
      canonical,
      deltas: [owned],
      appendedRows,
      spanCount,
      queuedAt: isDiagEnabled() ? performance.now() : null,
    };
    return true;
  }

  private canQueueDelta(frame: CellGridFrame): boolean {
    if (
      !this.foreground
      || frame.full
      || frame.seq !== frame.baseSeq + 1
      || this.pending?.mode === "full"
    ) return false;
    const pending = this.pending;
    if (pending?.mode === "delta") {
      const previous = pending.deltas[pending.deltas.length - 1]!;
      return previous.streamId === frame.streamId
        && previous.gridEpoch === frame.gridEpoch
        && previous.cols === frame.cols
        && previous.rows === frame.rows
        && previous.altScreen === frame.altScreen
        && previous.seq === frame.baseSeq;
    }
    return this.streamId === frame.streamId
      && this.gridEpoch === frame.gridEpoch
      && this.cols === frame.cols
      && this.rows === frame.rows
      && this.altScreen === frame.altScreen
      && this.seq === frame.baseSeq;
  }

  private parkPendingDeltas(): void {
    const pending = this.pending;
    if (pending?.mode !== "delta") return;
    this.pending = this.fullPending(
      pending.canonical,
      pending.deltas[pending.deltas.length - 1]!,
      "fallback_full",
      pending.appendedRows,
      pending.deltas.length,
      false,
      pending.queuedAt,
    );
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

    const canonical = pending.canonical;
    const applyMode = pending.mode === "delta"
      ? "delta"
      : pending.source === "wire_full" ? "full" : "fallback_full";
    const diagnostics = isDiagEnabled();
    const startedAt = diagnostics ? performance.now() : 0;
    noteTerminalRenderApply(this.sessionId, this.renderer, applyMode);
    const applied = pending.mode === "full"
      ? this.renderer.applyFullFrame(canonical)
      : this.renderer.applyDeltaFrames(pending.deltas);
    noteTerminalRenderApplied(this.sessionId, applyMode, applied);
    if (!applied) {
      this.pending = pending.mode === "delta"
        ? this.fullPending(
          canonical,
          pending.deltas[pending.deltas.length - 1]!,
          "fallback_full",
          pending.appendedRows,
          pending.deltas.length,
          false,
          pending.queuedAt,
        )
        : pending;
      this.schedule();
      return;
    }

    this.streamId = canonical.streamId;
    this.gridEpoch = canonical.gridEpoch;
    this.cols = canonical.cols;
    this.rows = canonical.rows;
    this.altScreen = canonical.altScreen;
    this.seq = canonical.seq;
    if (diagnostics) {
      diag("cell.apply_dur", {
        sid: this.sessionId,
        seq: canonical.seq,
        full: pending.mode === "full",
        source: pending.mode === "full" ? pending.source : "delta_batch",
        batch_frames: pending.mode === "delta" ? pending.deltas.length : pending.batchFrames,
        appended_rows: pending.appendedRows,
        queue_ms: pending.queuedAt === null ? null : startedAt - pending.queuedAt,
        dur_ms: performance.now() - startedAt,
      });
    }
    this.onApplied?.(
      pending.mode === "delta"
        ? pending.deltas[pending.deltas.length - 1]!
        : pending.deliveryFrame,
      canonical,
      pending.appendedRows > 0,
      pending.mode === "full" && pending.hadWireFull,
    );
    this.schedule();
  }
}
