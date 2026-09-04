// Terminal presentation policy — separates stream receipt from painted state.
// CellTerminal owns the pane's view activity and renderer; this controller
// owns the bounded receiving indicator, reconciliation hold state, and cursor
// blink presentation without changing the terminal frame protocol.
import {
  createEffect,
  createSignal,
  onCleanup,
  type Accessor,
} from "solid-js";
import type { CellGridFrame } from "@roost/shared/cell";
import type { CellGridRenderer } from "./cellRenderer.ts";
import { isPageVisible, pageVisible } from "./pageVisible.ts";
import type { TerminalViewHandleStatus } from "../store/terminal-stream-types.ts";
import {
  deriveTerminalPresentationState,
  FRAME_ACTIVITY_WINDOW_MS,
  type TerminalPresentationActivity,
  type TerminalPresentationState,
} from "../store/terminal-stream-types.ts";
import type {
  ReaderIntentReason,
  RendererEpochSeq,
} from "./cellRendererPresentation.ts";
export const FOREGROUND_DOM_STALL_MS = 10_000;
export function preservesForegroundReaderHold(
  reason: ReaderIntentReason | null,
): boolean {
  return reason === "native_scroll"
    || reason === "wheel"
    || reason === "touch"
    || reason === "selection"
    || reason === "find";
}

export interface TerminalPresentationController {
  readonly state: Accessor<TerminalPresentationState>;
  clearFrameActivity(): void;
  refreshTerminalPresentation(): void;
  noteFrameActivity(frame: Pick<CellGridFrame, "full" | "gridEpoch" | "seq">): void;
  clearCursorBlink(): void;
  refreshCursorBlink(): void;
}

export function createTerminalPresentationController(options: {
  active: Accessor<boolean>;
  focused: Accessor<boolean>;
  status: Accessor<TerminalViewHandleStatus | null>;
  renderer: Accessor<CellGridRenderer | null>;
  onCatchUpStalled(watermark: RendererEpochSeq): void;
}): TerminalPresentationController {
  const [state, setState] = createSignal<TerminalPresentationState>("idle");
  let activityTimer: number | null = null;
  let activityAt: number | null = null;
  let activityEpoch: string | null = null;
  let activitySeq: number | null = null;
  let catchUpTimer: ReturnType<typeof setTimeout> | null = null;
  let catchUpWatermark: RendererEpochSeq | null = null;
  let notifiedCatchUpWatermark: string | null = null;

  const watermarkKey = (watermark: RendererEpochSeq): string =>
    `${watermark.grid_epoch ?? ""}\u0000${watermark.seq ?? ""}`;

  function clearCatchUpStall(resetNotification: boolean): void {
    clearTimeout(catchUpTimer ?? undefined);
    catchUpTimer = null;
    catchUpWatermark = null;
    if (resetNotification) notifiedCatchUpWatermark = null;
  }

  function armCatchUpStall(watermark: RendererEpochSeq): void {
    const key = watermarkKey(watermark);
    if (notifiedCatchUpWatermark === key) return;
    if (catchUpTimer !== null && catchUpWatermark !== null) {
      const sameEpoch = catchUpWatermark.grid_epoch === watermark.grid_epoch;
      const stillAhead = catchUpWatermark.seq !== null
        && watermark.seq !== null
        && watermark.seq >= catchUpWatermark.seq;
      if (sameEpoch && stillAhead) return;
      clearCatchUpStall(false);
    }
    const captured = { ...watermark };
    catchUpWatermark = captured;
    const timer = setTimeout(() => {
      if (catchUpTimer !== timer || catchUpWatermark !== captured) return;
      catchUpTimer = null;
      catchUpWatermark = null;
      const renderer = options.renderer();
      const status = options.status();
      const current = renderer?.canonicalEpochSeq();
      const reconciled = renderer?.reconciledEpochSeq();
      const stillActive = options.active()
        && isPageVisible()
        && status?.status === "accepted"
        && status.active
        && status.baselineReady;
      const stillOwnsWatermark = current?.grid_epoch === captured.grid_epoch
        && current.seq !== null
        && captured.seq !== null
        && current.seq >= captured.seq;
      const remainsUnreconciled = reconciled?.grid_epoch !== captured.grid_epoch
        || reconciled.seq === null
        || captured.seq === null
        || reconciled.seq < captured.seq;
      if (!renderer || !stillActive || !stillOwnsWatermark || !remainsUnreconciled) {
        refreshTerminalPresentation();
        return;
      }
      if (preservesForegroundReaderHold(renderer.readerReason)) {
        refreshTerminalPresentation();
        return;
      }
      notifiedCatchUpWatermark = key;
      options.onCatchUpStalled(captured);
    }, FOREGROUND_DOM_STALL_MS);
    catchUpTimer = timer;
  }

  function clearFrameActivity(): void {
    clearTimeout(activityTimer ?? undefined);
    activityTimer = null;
    activityAt = null;
    activityEpoch = null;
    activitySeq = null;
    clearCatchUpStall(true);
  }

  function armActivityExpiry(delayMs: number): void {
    clearTimeout(activityTimer ?? undefined);
    activityTimer = window.setTimeout(() => {
      activityTimer = null;
      refreshTerminalPresentation();
    }, delayMs);
  }

  function refreshTerminalPresentation(): void {
    const active = options.active() && pageVisible();
    const status = options.status();
    const acceptedWithBaseline = status?.status === "accepted"
      && status.active
      && status.baselineReady;
    const renderer = options.renderer();
    if (!active || !acceptedWithBaseline || renderer === null) {
      clearFrameActivity();
      setState("idle");
      return;
    }
    const nowMs = Date.now();
    const canonical = renderer.canonicalEpochSeq();
    const reconciled = renderer.reconciledEpochSeq();
    let activity: TerminalPresentationActivity | null = null;
    if (activityAt !== null && activityEpoch !== null && activitySeq !== null) {
      activity = {
        grid_epoch: activityEpoch,
        seq: activitySeq,
        started_at_ms: activityAt,
      };
    }
    const nextState = deriveTerminalPresentationState({
      active,
      acceptedWithBaseline,
      canonical,
      reconciled,
      activity,
      nowMs,
    });
    if (nextState === "catching_up") {
      clearTimeout(activityTimer ?? undefined);
      activityTimer = null;
      setState(nextState);
      armCatchUpStall(canonical);
      return;
    }
    clearCatchUpStall(true);
    if (nextState === "receiving" && activity !== null) {
      setState(nextState);
      armActivityExpiry(Math.max(
        0,
        FRAME_ACTIVITY_WINDOW_MS - (Date.now() - activity.started_at_ms),
      ));
      return;
    }
    clearTimeout(activityTimer ?? undefined);
    activityTimer = null;
    setState("idle");
  }

  function noteFrameActivity(frame: Pick<CellGridFrame, "full" | "gridEpoch" | "seq">): void {
    if (!frame.full) {
      activityAt = Date.now();
      activityEpoch = frame.gridEpoch;
      activitySeq = frame.seq;
    }
    refreshTerminalPresentation();
  }

  function clearCursorBlink(): void {
    options.renderer()?.setCursorBlinkEnabled(false);
  }

  function refreshCursorBlink(): void {
    options.renderer()?.setCursorBlinkEnabled(
      options.active() && options.focused() && pageVisible(),
    );
  }

  createEffect(() => {
    options.active();
    pageVisible();
    options.status();
    refreshTerminalPresentation();
  });
  createEffect(() => {
    options.active();
    options.focused();
    pageVisible();
    refreshCursorBlink();
  });
  onCleanup(() => {
    clearFrameActivity();
    clearCursorBlink();
    clearCatchUpStall(true);
  });

  return {
    state,
    clearFrameActivity,
    refreshTerminalPresentation,
    noteFrameActivity,
    clearCursorBlink,
    refreshCursorBlink,
  };
}
