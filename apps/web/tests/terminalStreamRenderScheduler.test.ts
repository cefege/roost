// These tests connect terminal canonical folding to per-renderer rAF delivery.
// They prove hidden subscribers retain canonical state without DOM work and that
// stream reset cancels queued renderer work before a stale callback can paint.
// The shared fixture supplies a generation-matched terminal stream and fake renderer.

import { describe, expect, test } from "bun:test";
import type { CellGridFrame } from "@roost/shared/cell";
import {
  RecordingRenderer,
  SESSION_ID,
  acceptView,
  cellFrameToProto,
  delta,
  full,
  latestViewCommand,
  renderer,
  terminalStream,
} from "./helpers/terminalStreamFixture.ts";
import { terminalDropNextFrames } from "../src/store/terminal-stream-state.ts";

type AnimationFrameQueue = {
  cancelled: number[];
  flush(): void;
  pending(): number;
  restore(): void;
};

function installAnimationFrameQueue(): AnimationFrameQueue {
  const frameHost = globalThis as unknown as {
    cancelAnimationFrame?: (id: number) => void;
    requestAnimationFrame?: (callback: FrameRequestCallback) => number;
  };
  const priorRequest = frameHost.requestAnimationFrame;
  const priorCancel = frameHost.cancelAnimationFrame;
  const frames = new Map<number, FrameRequestCallback>();
  const cancelled: number[] = [];
  let nextId = 1;
  frameHost.requestAnimationFrame = (callback) => {
    const id = nextId++;
    frames.set(id, callback);
    return id;
  };
  frameHost.cancelAnimationFrame = (id) => {
    cancelled.push(id);
    frames.delete(id);
  };
  return {
    cancelled,
    flush(): void {
      const next = frames.entries().next().value as
        | [number, FrameRequestCallback]
        | undefined;
      if (!next) throw new Error("test expected a scheduled animation frame");
      const [id, callback] = next;
      frames.delete(id);
      callback(0);
    },
    pending(): number {
      return frames.size;
    },
    restore(): void {
      if (priorRequest) frameHost.requestAnimationFrame = priorRequest;
      else Reflect.deleteProperty(frameHost, "requestAnimationFrame");
      if (priorCancel) frameHost.cancelAnimationFrame = priorCancel;
      else Reflect.deleteProperty(frameHost, "cancelAnimationFrame");
    },
  };
}

describe("terminal stream renderer scheduling", () => {
  test("folds parked deliveries canonically and repairs with the latest full on activation", () => {
    const frames = installAnimationFrameQueue();
    try {
      let foreground = true;
      const view = terminalStream.createTerminalView(SESSION_ID);
      const sink = new RecordingRenderer();
      const deliveries: CellGridFrame[] = [];
      view.subscribeRenderer(
        renderer(sink),
        ({ frame }) => deliveries.push(frame),
        () => foreground,
      );
      view.setViewport({ cols: 1, rows: 1 });
      acceptView(view.viewId, latestViewCommand().value.revision as bigint);
      view.setInactive();
      foreground = false;

      terminalStream.dispatchTerminalCellFrame(cellFrameToProto(full(), SESSION_ID));
      terminalStream.dispatchTerminalCellFrame(cellFrameToProto(delta(2, "B"), SESSION_ID));
      expect(terminalStream.terminalStreamDiagnosticSnapshot(SESSION_ID).replica.seq).toBe(2);
      expect(frames.pending()).toBe(0);
      expect(sink.fullFrames).toHaveLength(0);
      expect(sink.deltaFrames).toHaveLength(0);
      expect(deliveries).toHaveLength(0);

      foreground = true;
      view.setViewport({ cols: 1, rows: 1 });
      expect(frames.pending()).toBe(1);
      frames.flush();
      expect(sink.fullFrames).toHaveLength(1);
      expect(sink.fullFrames[0]).toMatchObject({ full: true, baseSeq: 0, seq: 2 });
      expect(sink.fullFrames[0]!.viewportRows[0]!.spans[0]!.text).toBe("B");
      expect(sink.deltaFrames).toHaveLength(0);
      expect(deliveries).toHaveLength(1);
      view.dispose();
    } finally {
      frames.restore();
    }
  });

  test("repairs a renderer-only dropped delta with retained scrollback", () => {
    const frames = installAnimationFrameQueue();
    try {
      const view = terminalStream.createTerminalView(SESSION_ID);
      const sink = new RecordingRenderer();
      view.subscribeRenderer(renderer(sink));
      view.setViewport({ cols: 1, rows: 1 });
      acceptView(view.viewId, latestViewCommand().value.revision as bigint);

      const baseline = full();
      terminalStream.dispatchTerminalCellFrame(cellFrameToProto(baseline, SESSION_ID));
      frames.flush();
      terminalDropNextFrames.add(SESSION_ID);
      const dropped = {
        ...delta(2, "B"),
        scrollbackAppend: [baseline.viewportRows[0]!],
        scrollbackTotal: 1,
      };
      terminalStream.dispatchTerminalCellFrame(cellFrameToProto(dropped, SESSION_ID));
      expect(frames.pending()).toBe(0);

      const following = {
        ...delta(3, "C"),
        scrollbackAppend: [{ ...dropped.viewportRows[0]!, index: 1 }],
        scrollbackTotal: 2,
      };
      terminalStream.dispatchTerminalCellFrame(cellFrameToProto(following, SESSION_ID));
      expect(frames.pending()).toBe(1);
      frames.flush();

      const repair = sink.fullFrames.at(-1);
      expect(repair).toMatchObject({
        full: true,
        baseSeq: 0,
        seq: 3,
        sbBase: 0,
        scrollbackTotal: 2,
      });
      expect(repair?.scrollbackRows.map((row) => row.spans[0]?.text)).toEqual(["A", "B"]);
      view.dispose();
    } finally {
      frames.restore();
    }
  });

  test("cancels a queued renderer frame when stream state resets", () => {
    const frames = installAnimationFrameQueue();
    try {
      const view = terminalStream.createTerminalView(SESSION_ID);
      const sink = new RecordingRenderer();
      view.subscribeRenderer(renderer(sink));
      view.setViewport({ cols: 1, rows: 1 });
      acceptView(view.viewId, latestViewCommand().value.revision as bigint);
      terminalStream.dispatchTerminalCellFrame(cellFrameToProto(full(), SESSION_ID));

      expect(frames.pending()).toBe(1);
      terminalStream._resetTerminalStreamForTest();
      expect(frames.cancelled).toEqual([1]);
      expect(frames.pending()).toBe(0);
      expect(sink.fullFrames).toHaveLength(0);
    } finally {
      frames.restore();
    }
  });
});
