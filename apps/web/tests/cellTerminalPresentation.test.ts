// Pane-local DOM recovery reaches escalation only after its renderer target.
// Browser-Solid fixtures isolate view status, holds, pointer gestures, and
// content-free terminal diagnostics without a terminal stream.

import { afterEach, describe, expect, mock, test, vi } from "bun:test";
import type * as SolidApi from "solid-js";
import type { RendererEpochSeq } from "../src/lib/cellRendererPresentation.ts";
import type { CellTerminalPresentation } from "../src/components/cell-terminal-presentation.ts";
import type { TerminalViewHandleStatus } from "../src/store/terminal-stream-types.ts";

interface PresentationOptions {
  onCatchUpStalled(watermark: RendererEpochSeq): void;
}

interface OfflineUpdate {
  viewed: boolean;
  hasFrame: boolean;
}

const browserGlobals = globalThis as unknown as Record<string, unknown>;
const fakeDocument = Object.assign(new EventTarget(), {
  visibilityState: "visible" as DocumentVisibilityState,
});
const fakeWindow = new EventTarget();
Object.assign(browserGlobals, { document: fakeDocument, window: fakeWindow });

// Browser Solid must load after fake DOM globals exist.
const solidClientUrl = new URL("./solid.js", import.meta.resolve("solid-js"));
const Solid = await import(solidClientUrl.href) as typeof SolidApi;
mock.module("solid-js", () => ({ ...Solid }));
const [pageVisible, setPageVisible] = Solid.createSignal(true);

let presentationOptions: PresentationOptions | null = null;
let prepareLiveInteraction = (): void => undefined;
const offlineUpdates: OfflineUpdate[] = [];
const stallSignals: Array<Record<string, unknown>> = [];

mock.module("@roost/shared/diag", () => ({
  diag: () => undefined,
  signal: (_kind: string, fields: Record<string, unknown>) => stallSignals.push(fields),
}));
mock.module("../src/lib/terminalPresentation.ts", () => ({
  FOREGROUND_DOM_STALL_MS: 1_000,
  preservesForegroundReaderHold: (reason: string | null) => (
    reason === "native_scroll"
    || reason === "wheel"
    || reason === "touch"
    || reason === "selection"
    || reason === "find"
  ),
  createTerminalPresentationController: (options: PresentationOptions) => {
    presentationOptions = options;
    return {
      state: () => "catching_up",
      clearFrameActivity: () => undefined,
      refreshTerminalPresentation: () => undefined,
      noteFrameActivity: () => undefined,
      clearCursorBlink: () => undefined,
      refreshCursorBlink: () => undefined,
    };
  },
}));
mock.module("../src/lib/terminalSelectionGuard.ts", () => ({
  createTerminalSelectionGuard: () => ({
    captureTerminalSelection: () => undefined,
    notifyBackfill: () => undefined,
    syncNativeSelectionHold: () => undefined,
    prepareLiveInteraction: () => prepareLiveInteraction(),
    releasePaintHolds: () => undefined,
  }),
}));
mock.module("../src/lib/offlineWatch.ts", () => ({
  createOfflineWatch: () => ({
    update: (viewed: boolean, hasFrame: boolean) => offlineUpdates.push({ viewed, hasFrame }),
    dispose: () => undefined,
  }),
}));
mock.module("../src/lib/pageVisible.ts", () => ({
  isPageVisible: pageVisible,
  pageVisible,
}));
mock.module("../src/lib/folderKey.ts", () => ({ folderKeyOf: () => null }));
mock.module("../src/store/selectors.ts", () => ({
  newestOpenSessionForFolderKey: () => null,
}));
mock.module("../src/lib/attachDiagnosis.ts", () => ({
  attachDiagnosisWaitKey: () => null,
  startAttachDiagnosis: () => ({ dispose: () => undefined }),
}));
mock.module("../src/components/TerminalOfflineNotice.tsx", () => ({
  terminalViewportLoadingNotice: () => null,
}));
mock.module("../src/store/terminal-stream.ts", () => ({
  terminalStreamDiagnosticSnapshot: () => ({
    view: { stream_id: "stream-a", revision: "7" },
    sync: { socket_generation: 3, domain_generation: "11" },
    replica: { grid_epoch: "epoch-a", seq: 2 },
  }),
}));

// Browser-Solid and module mocks must settle before this controller evaluates.
const { createCellTerminalPresentation } = await import(
  "../src/components/cell-terminal-presentation.ts"
);

const accepted: TerminalViewHandleStatus = {
  status: "accepted",
  revision: 7n,
  active: true,
  streamId: "stream-a",
  effectiveCols: 80,
  effectiveRows: 24,
  baselineReady: true,
};

interface PresentationFixture {
  readonly display: EventTarget;
  readonly localReconciliations: () => number;
  readonly onCatchUpStalled: (watermark: RendererEpochSeq) => void;
  readonly presentation: CellTerminalPresentation;
  readonly recoveryCalls: () => number;
  readonly renderer: {
    canonical: RendererEpochSeq;
    reconciled: RendererEpochSeq;
    readerReason: string | null;
  };
  dispose(): void;
}

function createPresentationFixture(): PresentationFixture {
  const display = new EventTarget();
  const renderer = {
    canonical: { grid_epoch: "epoch-a", seq: 2 },
    reconciled: { grid_epoch: "epoch-a", seq: 1 },
    readerReason: null as string | null,
    canonicalEpochSeq(): RendererEpochSeq { return { ...this.canonical }; },
    reconciledEpochSeq(): RendererEpochSeq { return { ...this.reconciled }; },
    reconcileBlockReason: () => "not_reconciled",
    setPredictedCursor: () => undefined,
  };
  let localReconciliations = 0;
  let recoveryCalls = 0;
  prepareLiveInteraction = () => { localReconciliations++; };
  presentationOptions = null;
  const [viewActive] = Solid.createSignal(true);
  let disposeRoot = (): void => undefined;
  let presentation: CellTerminalPresentation | null = null;
  Solid.createRoot((dispose) => {
    disposeRoot = dispose;
    presentation = createCellTerminalPresentation(
      { focused: true, session: { id: "presentation-session" } } as never,
      {
        sessionId: "presentation-session",
        display: () => display as unknown as HTMLDivElement,
        renderer,
        backfill: null,
        linkAttachment: null,
        predictor: { clear: () => undefined },
        view: {
          viewId: "view-a",
          refresh: () => undefined,
          recoverUnreconciledDom: () => { recoveryCalls++; },
        },
      } as never,
      () => false,
      viewActive,
      () => undefined,
    );
  });
  const mounted = presentation as CellTerminalPresentation | null;
  const fixtureOptions = presentationOptions as unknown as PresentationOptions | null;
  if (mounted === null || fixtureOptions === null) throw new Error("presentation fixture did not mount");
  return {
    display,
    localReconciliations: () => localReconciliations,
    onCatchUpStalled: fixtureOptions.onCatchUpStalled,
    presentation: mounted,
    recoveryCalls: () => recoveryCalls,
    renderer,
    dispose: () => {
      mounted.dispose();
      disposeRoot();
    },
  };
}

function pointerEvent(type: string, pointerId: number): Event {
  return Object.assign(new Event(type), { pointerId });
}

afterEach(() => {
  vi.useRealTimers();
  offlineUpdates.length = 0;
  stallSignals.length = 0;
  prepareLiveInteraction = () => undefined;
  presentationOptions = null;
  setPageVisible(true);
});

describe("cell terminal presentation DOM recovery", () => {
  test("waits for a non-null view status before arming offline watch", () => {
    const fixture = createPresentationFixture();
    try {
      expect(offlineUpdates.at(-1)).toEqual({ viewed: false, hasFrame: false });
      fixture.presentation.setViewStatus(accepted);
      expect(offlineUpdates.at(-1)).toEqual({ viewed: true, hasFrame: false });
    } finally {
      fixture.dispose();
    }
  });

  test("escalates only after the three-second pane-local DOM target", () => {
    vi.useFakeTimers();
    const fixture = createPresentationFixture();
    try {
      fixture.presentation.setViewStatus(accepted);
      fixture.onCatchUpStalled({ grid_epoch: "epoch-a", seq: 2 });
      expect(fixture.localReconciliations()).toBe(1);
      vi.advanceTimersByTime(2_999);
      expect(fixture.recoveryCalls()).toBe(0);
      vi.advanceTimersByTime(1);
      expect(fixture.recoveryCalls()).toBe(1);
      expect(stallSignals).toEqual([expect.objectContaining({
        stream_id: "stream-a",
        generation_socket: 3,
        generation_domain: "11",
        checkpoint_epoch: "epoch-a",
        checkpoint_seq: 2,
        replica_epoch: "epoch-a",
        replica_seq: 2,
        dom_reconciled_epoch: "epoch-a",
        dom_reconciled_seq: 1,
        reader_reason: null,
        block_reason: "not_reconciled",
        layer: "dom_reconcile",
        action: "redial",
      })]);
    } finally {
      fixture.dispose();
    }
  });

  test("clears the target only after the renderer reaches its captured watermark", () => {
    vi.useFakeTimers();
    const fixture = createPresentationFixture();
    try {
      fixture.presentation.setViewStatus(accepted);
      fixture.onCatchUpStalled({ grid_epoch: "epoch-a", seq: 2 });
      vi.advanceTimersByTime(2_999);
      fixture.presentation.noteRendererReconciled();
      expect(fixture.recoveryCalls()).toBe(0);
      fixture.renderer.reconciled = { grid_epoch: "epoch-a", seq: 2 };
      fixture.presentation.noteRendererReconciled();
      vi.advanceTimersByTime(1);
      expect(fixture.recoveryCalls()).toBe(0);
    } finally {
      fixture.dispose();
    }
  });

  test("preserves reader holds and pointer gestures through the DOM deadline", () => {
    vi.useFakeTimers();
    const held = createPresentationFixture();
    const pointer = createPresentationFixture();
    try {
      held.presentation.setViewStatus(accepted);
      held.renderer.readerReason = "selection";
      held.onCatchUpStalled({ grid_epoch: "epoch-a", seq: 2 });
      vi.advanceTimersByTime(3_000);
      expect(held.localReconciliations()).toBe(0);
      expect(held.recoveryCalls()).toBe(0);

      pointer.presentation.setViewStatus(accepted);
      const detachPointerGuard = pointer.presentation.attachPointerGestureGuard();
      pointer.onCatchUpStalled({ grid_epoch: "epoch-a", seq: 2 });
      expect(pointer.localReconciliations()).toBe(1);
      pointer.display.dispatchEvent(pointerEvent("pointerdown", 1));
      vi.advanceTimersByTime(3_000);
      expect(pointer.recoveryCalls()).toBe(0);
      fakeWindow.dispatchEvent(pointerEvent("pointerup", 1));
      expect(pointer.recoveryCalls()).toBe(1);
      detachPointerGuard();
    } finally {
      held.dispose();
      pointer.dispose();
    }
  });

  test("keeps an active pointer guard through foreground recovery", () => {
    vi.useFakeTimers();
    const fixture = createPresentationFixture();
    try {
      fixture.presentation.setViewStatus(accepted);
      const detachPointerGuard = fixture.presentation.attachPointerGestureGuard();
      fixture.display.dispatchEvent(pointerEvent("pointerdown", 1));
      fixture.presentation.setViewStatus(null);
      fixture.presentation.setViewStatus(accepted);
      fixture.onCatchUpStalled({ grid_epoch: "epoch-a", seq: 2 });
      expect(fixture.localReconciliations()).toBe(0);
      fakeWindow.dispatchEvent(pointerEvent("pointerup", 1));
      expect(fixture.localReconciliations()).toBe(1);
      detachPointerGuard();
    } finally {
      fixture.dispose();
    }
  });

  test("cancels a pointer guard when the document hides", () => {
    const fixture = createPresentationFixture();
    try {
      fixture.presentation.setViewStatus(accepted);
      fixture.presentation.attachPointerGestureGuard();
      fixture.display.dispatchEvent(pointerEvent("pointerdown", 1));
      setPageVisible(false);
      setPageVisible(true);
      fixture.onCatchUpStalled({ grid_epoch: "epoch-a", seq: 2 });
      expect(fixture.localReconciliations()).toBe(1);
    } finally {
      fixture.dispose();
    }
  });
});
