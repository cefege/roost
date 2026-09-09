// These tests own the browser-frame boundary for terminal DOM delivery.
// A controllable requestAnimationFrame proves coalescing, parking, and disposal
// without involving Sync transport or a real DOM. Each frame is a valid one-cell
// terminal frame so full/delta continuity is asserted at the renderer boundary.

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import type { CellGridFrame } from "@roost/shared/cell";
import { TerminalRenderScheduler } from "../src/lib/terminal-render-scheduler.ts";
import type { CellGridRenderer } from "../src/lib/cellRenderer.ts";

const frameHost = globalThis as typeof globalThis & {
  cancelAnimationFrame?: (id: number) => void;
  requestAnimationFrame?: (callback: FrameRequestCallback) => number;
};
const originalRequestAnimationFrame = frameHost.requestAnimationFrame;
const originalCancelAnimationFrame = frameHost.cancelAnimationFrame;
let nextAnimationFrame = 1;
const queuedAnimationFrames = new Map<number, FrameRequestCallback>();
const cancelledAnimationFrames: number[] = [];

class RecordingRenderer {
  readonly fullFrames: CellGridFrame[] = [];
  readonly deltaFrames: CellGridFrame[] = [];

  applyFullFrame(frame: CellGridFrame): boolean {
    this.fullFrames.push(frame);
    return true;
  }

  applyDeltaFrame(frame: CellGridFrame): boolean {
    this.deltaFrames.push(frame);
    return true;
  }
}

function asCellGridRenderer(renderer: RecordingRenderer): CellGridRenderer {
  return renderer as unknown as CellGridRenderer;
}

function fullFrame(seq: number, text: string): CellGridFrame {
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

function deltaFrame(seq: number, text: string): CellGridFrame {
  return { ...fullFrame(seq, text), full: false, baseSeq: seq - 1 };
}

function flushNextAnimationFrame(): void {
  const next = queuedAnimationFrames.entries().next().value as
    | [number, FrameRequestCallback]
    | undefined;
  if (!next) throw new Error("test expected an animation frame");
  const [id, callback] = next;
  queuedAnimationFrames.delete(id);
  callback(0);
}

beforeEach(() => {
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
});

afterEach(() => {
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
});
describe("TerminalRenderScheduler", () => {
  test("coalesces skipped deltas into the newest canonical full repair", () => {
    const renderer = new RecordingRenderer();
    const deliveries: CellGridFrame[] = [];
    const scheduler = new TerminalRenderScheduler(
      asCellGridRenderer(renderer),
      "session-a",
      (frame) => deliveries.push(frame),
    );
    scheduler.setForeground(true);

    const first = fullFrame(1, "A");
    scheduler.enqueue(first, first);
    expect(queuedAnimationFrames.size).toBe(1);
    expect(renderer.fullFrames).toHaveLength(0);
    expect(deliveries).toHaveLength(0);
    flushNextAnimationFrame();

    const secondCanonical = fullFrame(2, "B");
    scheduler.enqueue(deltaFrame(2, "B"), secondCanonical);
    expect(queuedAnimationFrames.size).toBe(1);
    flushNextAnimationFrame();
    expect(renderer.deltaFrames.map((frame) => frame.seq)).toEqual([2]);

    const thirdCanonical = fullFrame(3, "C");
    const fourthCanonical = fullFrame(4, "D");
    scheduler.enqueue(deltaFrame(3, "C"), thirdCanonical);
    scheduler.enqueue(deltaFrame(4, "D"), fourthCanonical);
    expect(queuedAnimationFrames.size).toBe(1);
    expect(renderer.deltaFrames.map((frame) => frame.seq)).toEqual([2]);
    flushNextAnimationFrame();

    expect(renderer.fullFrames.map((frame) => frame.seq)).toEqual([1, 4]);
    expect(renderer.deltaFrames.map((frame) => frame.seq)).toEqual([2]);
    expect(deliveries.map((frame) => ({ seq: frame.seq, full: frame.full }))).toEqual([
      { seq: 1, full: true },
      { seq: 2, full: false },
      { seq: 4, full: true },
    ]);
  });

  test("parks pending canonical work without DOM application and wakes with the newest frame", () => {
    const renderer = new RecordingRenderer();
    const scheduler = new TerminalRenderScheduler(asCellGridRenderer(renderer), "session-a");
    scheduler.setForeground(true);
    const first = fullFrame(1, "A");
    scheduler.enqueue(first, first);
    expect(queuedAnimationFrames.size).toBe(1);

    scheduler.setForeground(false);
    expect(cancelledAnimationFrames).toEqual([1]);
    expect(queuedAnimationFrames.size).toBe(0);
    const latest = fullFrame(2, "B");
    scheduler.enqueue(latest, latest);
    expect(queuedAnimationFrames.size).toBe(0);
    expect(renderer.fullFrames).toHaveLength(0);

    scheduler.setForeground(true);
    expect(queuedAnimationFrames.size).toBe(1);
    flushNextAnimationFrame();
    expect(renderer.fullFrames.map((frame) => frame.seq)).toEqual([2]);
  });

  test("cancels a queued frame on disposal", () => {
    const renderer = new RecordingRenderer();
    const scheduler = new TerminalRenderScheduler(asCellGridRenderer(renderer), "session-a");
    scheduler.setForeground(true);
    const first = fullFrame(1, "A");
    scheduler.enqueue(first, first);

    scheduler.dispose();
    expect(cancelledAnimationFrames).toEqual([1]);
    expect(queuedAnimationFrames.size).toBe(0);
    expect(renderer.fullFrames).toHaveLength(0);
  });
});
