// Provides deterministic browser-frame control and recording renderer fixtures.
// TerminalRenderScheduler tests use this harness to assert queue admission without a DOM.
// It restores global requestAnimationFrame hooks after every isolated Bun test.

import type { CellGridFrame } from "@roost/shared/cell";
import type { CellGridRenderer } from "../../src/lib/cellRenderer.ts";

const frameHost = globalThis as typeof globalThis & {
  cancelAnimationFrame?: (id: number) => void;
  requestAnimationFrame?: (callback: FrameRequestCallback) => number;
};
const originalRequestAnimationFrame = frameHost.requestAnimationFrame;
const originalCancelAnimationFrame = frameHost.cancelAnimationFrame;
let nextAnimationFrame = 1;
export const queuedAnimationFrames = new Map<number, FrameRequestCallback>();
export const cancelledAnimationFrames: number[] = [];

export class RecordingRenderer {
  readonly fullFrames: CellGridFrame[] = [];
  readonly deltaBatches: CellGridFrame[][] = [];

  applyFullFrame(frame: CellGridFrame): boolean {
    this.fullFrames.push(frame);
    return true;
  }

  applyDeltaFrames(frames: readonly CellGridFrame[]): boolean {
    this.deltaBatches.push([...frames]);
    return true;
  }
}

export function asCellGridRenderer(renderer: RecordingRenderer): CellGridRenderer {
  return renderer as unknown as CellGridRenderer;
}

export function fullFrame(seq: number, text: string): CellGridFrame {
  return {
    streamId: "stream-a",
    gridEpoch: "epoch-a",
    cols: 1,
    rows: 1,
    full: true,
    viewportRows: [{
      index: 0,
      spans: [{ text, columns: 1, fg: 256, bg: 256, flags: 0 }],
    }],
    scrollbackRows: [],
    scrollbackAppend: [],
    scrollbackTotal: 0,
    sbBase: 0,
    baseSeq: 0,
    seq,
    cursorRow: 0,
    cursorCol: 0,
    cursorVisible: true,
    altScreen: false,
    cursorKeysApp: false,
    bracketedPaste: false,
    mouseTracking: 0,
    mouseSgr: false,
    focusEvents: false,
  };
}

export function deltaFrame(seq: number, text: string): CellGridFrame {
  return { ...fullFrame(seq, text), full: false, baseSeq: seq - 1 };
}

export function flushNextAnimationFrame(): void {
  const next = queuedAnimationFrames.entries().next().value as
    | [number, FrameRequestCallback]
    | undefined;
  if (!next) throw new Error("test expected an animation frame");
  const [id, callback] = next;
  queuedAnimationFrames.delete(id);
  callback(0);
}

export function installAnimationFrameHarness(): void {
  nextAnimationFrame = 1;
  queuedAnimationFrames.clear();
  cancelledAnimationFrames.length = 0;
  frameHost.requestAnimationFrame = (callback) => {
    const id = nextAnimationFrame++;
    queuedAnimationFrames.set(id, callback);
    return id;
  };
  frameHost.cancelAnimationFrame = (id) => {
    cancelledAnimationFrames.push(id);
    queuedAnimationFrames.delete(id);
  };
}

export function restoreAnimationFrameHarness(): void {
  queuedAnimationFrames.clear();
  if (originalRequestAnimationFrame) {
    frameHost.requestAnimationFrame = originalRequestAnimationFrame;
  } else {
    Reflect.deleteProperty(frameHost, "requestAnimationFrame");
  }
  if (originalCancelAnimationFrame) {
    frameHost.cancelAnimationFrame = originalCancelAnimationFrame;
  } else {
    Reflect.deleteProperty(frameHost, "cancelAnimationFrame");
  }
}
