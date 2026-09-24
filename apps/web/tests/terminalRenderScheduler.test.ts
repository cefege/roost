// These tests own the browser-frame boundary for terminal DOM delivery.
// A controllable requestAnimationFrame proves bounded batching, parking, and
// fallback repair without involving Sync transport or a real DOM.

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import type { CellGridFrame } from "@roost/protocol/cell";
import { TerminalRenderScheduler } from "../src/renderer/terminal-render-scheduler.ts";
import {
  RecordingRenderer,
  asCellGridRenderer,
  cancelledAnimationFrames,
  deltaFrame,
  flushNextAnimationFrame,
  fullFrame,
  installAnimationFrameHarness,
  queuedAnimationFrames,
  restoreAnimationFrameHarness,
} from "./helpers/terminalRenderSchedulerHarness.ts";

beforeEach(installAnimationFrameHarness);
afterEach(restoreAnimationFrameHarness);

describe("TerminalRenderScheduler", () => {
  test("folds contiguous deltas into one sparse paint", () => {
    const renderer = new RecordingRenderer();
    const deliveries: Array<{
      frame: CellGridFrame;
      canonical: CellGridFrame;
      scrollbackAppended: boolean;
    }> = [];
    const scheduler = new TerminalRenderScheduler(
      asCellGridRenderer(renderer),
      "session-a",
      (frame, canonical, scrollbackAppended) => {
        deliveries.push({ frame, canonical, scrollbackAppended });
      },
    );
    scheduler.setForeground(true);

    const first = fullFrame(1, "A");
    scheduler.enqueue(first, first);
    flushNextAnimationFrame();

    const secondCanonical = fullFrame(2, "B");
    scheduler.enqueue(deltaFrame(2, "B"), secondCanonical);
    flushNextAnimationFrame();
    expect(renderer.deltaBatches.map((batch) => batch.map((frame) => frame.seq))).toEqual([[2]]);

    const thirdCanonical = fullFrame(3, "C");
    const fourthCanonical = {
      ...fullFrame(4, "D"),
      cursorKeysApp: true,
      bracketedPaste: true,
    };
    scheduler.enqueue(deltaFrame(3, "C"), thirdCanonical);
    scheduler.enqueue({
      ...deltaFrame(4, "D"),
      cursorKeysApp: true,
      bracketedPaste: true,
    }, fourthCanonical);
    expect(queuedAnimationFrames.size).toBe(1);
    flushNextAnimationFrame();

    expect(renderer.fullFrames.map((frame) => frame.seq)).toEqual([1]);
    expect(renderer.deltaBatches.map((batch) => batch.map((frame) => frame.seq)))
      .toEqual([[2], [3, 4]]);
    expect(deliveries.map(({ frame, canonical, scrollbackAppended }) => ({
      seq: frame.seq,
      full: frame.full,
      cursorKeysApp: frame.cursorKeysApp,
      bracketedPaste: frame.bracketedPaste,
      canonicalSeq: canonical.seq,
      canonicalFull: canonical.full,
      scrollbackAppended,
    }))).toEqual([
      {
        seq: 1,
        full: true,
        cursorKeysApp: false,
        bracketedPaste: false,
        canonicalSeq: 1,
        canonicalFull: true,
        scrollbackAppended: false,
      },
      {
        seq: 2,
        full: false,
        cursorKeysApp: false,
        bracketedPaste: false,
        canonicalSeq: 2,
        canonicalFull: true,
        scrollbackAppended: false,
      },
      {
        seq: 4,
        full: false,
        cursorKeysApp: true,
        bracketedPaste: true,
        canonicalSeq: 4,
        canonicalFull: true,
        scrollbackAppended: false,
      },
    ]);
  });

  test("preserves a queued rebaseline full through its first delta", () => {
    const renderer = new RecordingRenderer();
    const deliveries: Array<{
      frame: CellGridFrame;
      canonical: CellGridFrame;
      scrollbackAppended: boolean;
      hadWireFull: boolean;
    }> = [];
    const scheduler = new TerminalRenderScheduler(
      asCellGridRenderer(renderer),
      "session-a",
      (frame, canonical, scrollbackAppended, hadWireFull) => {
        deliveries.push({ frame, canonical, scrollbackAppended, hadWireFull });
      },
    );
    scheduler.setForeground(true);
    const first = fullFrame(1, "A");
    scheduler.enqueue(first, first);
    flushNextAnimationFrame();

    const second = { ...fullFrame(1, "B"), streamId: "stream-b", gridEpoch: "epoch-b" };
    const third = {
      ...deltaFrame(2, "C"),
      gridEpoch: "epoch-b", streamId: "stream-b",
      scrollbackAppend: [{ index: 0, spans: [] }],
      scrollbackTotal: 1,
    };
    const thirdCanonical = {
      ...fullFrame(2, "C"),
      gridEpoch: "epoch-b", streamId: "stream-b",
      scrollbackTotal: 1,
      sbBase: 1,
    };
    scheduler.enqueue(second, second);
    scheduler.enqueue(third, thirdCanonical);
    flushNextAnimationFrame();

    expect(deliveries.at(-1)).toMatchObject({
      frame: { streamId: "stream-b", seq: 2, full: false },
      canonical: { streamId: "stream-b", gridEpoch: "epoch-b", seq: 2, full: true },
      scrollbackAppended: true,
      hadWireFull: true,
    });
  });
  test("repairs a queued sequence gap after ignoring stale or conflicting fulls", () => {
    const renderer = new RecordingRenderer();
    const scheduler = new TerminalRenderScheduler(asCellGridRenderer(renderer), "session-a");
    scheduler.setForeground(true);
    scheduler.enqueue(fullFrame(1, "A"), fullFrame(1, "A"));
    flushNextAnimationFrame();
    scheduler.enqueue(deltaFrame(2, "B"), fullFrame(2, "B"));
    const stale = fullFrame(1, "S");
    scheduler.enqueue(stale, stale);
    const conflicting = { ...fullFrame(2, "X"), gridEpoch: "epoch-b" };
    scheduler.enqueue(conflicting, conflicting);
    flushNextAnimationFrame();
    scheduler.enqueue(deltaFrame(4, "D"), fullFrame(4, "D"));
    flushNextAnimationFrame();
    expect(renderer.fullFrames.map((frame) => frame.seq)).toEqual([1, 4]);
    expect(renderer.deltaBatches).toHaveLength(1);
    expect(renderer.deltaBatches[0]!.map((frame) => frame.seq)).toEqual([2]);
  });

  test("repairs from canonical full when a pending batch exceeds a bound", () => {
    const frameBoundRenderer = new RecordingRenderer();
    const frameBoundScheduler = new TerminalRenderScheduler(
      asCellGridRenderer(frameBoundRenderer),
      "session-a",
    );
    frameBoundScheduler.setForeground(true);
    const frameBoundBaseline = fullFrame(1, "A");
    frameBoundScheduler.enqueue(frameBoundBaseline, frameBoundBaseline);
    flushNextAnimationFrame();
    for (let seq = 2; seq <= 66; seq++) {
      const canonical = fullFrame(seq, String(seq));
      frameBoundScheduler.enqueue(deltaFrame(seq, String(seq)), canonical);
    }
    flushNextAnimationFrame();
    expect(frameBoundRenderer.fullFrames.map((frame) => frame.seq)).toEqual([1, 66]);
    expect(frameBoundRenderer.deltaBatches).toEqual([]);

    const historyBoundRenderer = new RecordingRenderer();
    const historyBoundScheduler = new TerminalRenderScheduler(
      asCellGridRenderer(historyBoundRenderer),
      "session-a",
    );
    historyBoundScheduler.setForeground(true);
    const historyBoundBaseline = fullFrame(1, "A");
    historyBoundScheduler.enqueue(historyBoundBaseline, historyBoundBaseline);
    flushNextAnimationFrame();
    const tooMuchHistory = {
      ...deltaFrame(2, "B"),
      scrollbackAppend: Array.from({ length: 251 }, (_, index) => ({
        index,
        spans: [],
      })),
      scrollbackTotal: 251,
    };
    historyBoundScheduler.enqueue(tooMuchHistory, fullFrame(2, "B"));
    flushNextAnimationFrame();
    expect(historyBoundRenderer.fullFrames.map((frame) => frame.seq)).toEqual([1, 2]);
    expect(historyBoundRenderer.deltaBatches).toEqual([]);

    const spanBoundRenderer = new RecordingRenderer();
    const spanBoundScheduler = new TerminalRenderScheduler(
      asCellGridRenderer(spanBoundRenderer),
      "session-a",
    );
    spanBoundScheduler.setForeground(true);
    const spanBoundBaseline = fullFrame(1, "A");
    spanBoundScheduler.enqueue(spanBoundBaseline, spanBoundBaseline);
    flushNextAnimationFrame();
    const tooManySpans = {
      ...deltaFrame(2, "B"),
      viewportRows: [{
        index: 0,
        spans: Array.from({ length: 65_537 }, () => ({
          text: "x",
          columns: 1,
          fg: 256,
          bg: 256,
          flags: 0,
        })),
      }],
    };
    spanBoundScheduler.enqueue(tooManySpans, fullFrame(2, "B"));
    flushNextAnimationFrame();
    expect(spanBoundRenderer.fullFrames.map((frame) => frame.seq)).toEqual([1, 2]);
    expect(spanBoundRenderer.deltaBatches).toEqual([]);
  });

  test("owns queued delta row shells before later replica folding", () => {
    const renderer = new RecordingRenderer();
    const scheduler = new TerminalRenderScheduler(asCellGridRenderer(renderer), "session-a");
    scheduler.setForeground(true);
    const first = fullFrame(1, "A");
    scheduler.enqueue(first, first);
    flushNextAnimationFrame();

    const second = {
      ...deltaFrame(2, "B"),
      scrollbackAppend: [{
        index: 0,
        spans: [{ text: "A", columns: 1, fg: 256, bg: 256, flags: 0 }],
      }],
      scrollbackTotal: 1,
    };
    scheduler.enqueue(second, fullFrame(2, "B"));
    second.viewportRows[0]!.index = 99;
    second.scrollbackAppend[0]!.index = 99;
    flushNextAnimationFrame();

    expect(renderer.deltaBatches[0]![0]!.viewportRows[0]!.index).toBe(0);
    expect(renderer.deltaBatches[0]![0]!.scrollbackAppend[0]!.index).toBe(0);
  });

  test("parks canonical state without DOM application", () => {
    const renderer = new RecordingRenderer();
    let delivered: CellGridFrame | null = null;
    let scrollbackAppended = false;
    const scheduler = new TerminalRenderScheduler(
      asCellGridRenderer(renderer),
      "session-a",
      (frame, _canonical, appended) => {
        delivered = frame;
        scrollbackAppended = appended;
      },
    );
    scheduler.setForeground(true);
    const first = fullFrame(1, "A");
    scheduler.enqueue(first, first);
    expect(queuedAnimationFrames.size).toBe(1);

    scheduler.setForeground(false);
    expect(cancelledAnimationFrames).toEqual([1]);
    const latestCanonical = { ...fullFrame(2, "B"), scrollbackTotal: 1, sbBase: 1 };
    scheduler.enqueue({
      ...deltaFrame(2, "B"),
      scrollbackAppend: [{ index: 0, spans: [] }],
      scrollbackTotal: 1,
    }, latestCanonical);
    expect(queuedAnimationFrames.size).toBe(0);
    expect(renderer.fullFrames).toHaveLength(0);
    expect(renderer.deltaBatches).toEqual([]);

    scheduler.setForeground(true);
    expect(queuedAnimationFrames.size).toBe(1);
    flushNextAnimationFrame();
    expect(renderer.fullFrames.map((frame) => frame.seq)).toEqual([2]);
    expect(renderer.deltaBatches).toEqual([]);
    expect(delivered).toMatchObject({ seq: 2, full: false });
    expect(scrollbackAppended).toBe(true);
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
