// Independent per-sink cell delivery: one built frame, many transports.
// Each case asserts on the frames a sink actually received, never on internal
// delivery flags, because "what reached this browser" is the only contract a
// renderer can fold.

import { afterEach, describe, expect, test } from "bun:test";
import { createWtermCore } from "@roost/wterm/wterm-core-factory";
import {
  COORD_CELL_SINK_ID,
  resumeCellSink,
  suspendCellSink,
} from "../src/session-cell-sinks.ts";
import { installAutoKeeper } from "./keeper-fake-pool.ts";
import {
  attachExtraCellSink,
  CHANNEL_ID,
  cleanupStreamHarnesses,
  enableStream,
  frameRowText,
  makeHarness,
  STREAM_A,
  TEST_COLS,
  TEST_ROWS,
  trackKeeper,
} from "./terminal-stream-state-harness.ts";

afterEach(cleanupStreamHarnesses);

describe("worker cell sink fan-out", () => {
  test("keeps delivering to a local sink while the coordinator is suspended", async () => {
    trackKeeper(installAutoKeeper({ cols: TEST_COLS, rows: TEST_ROWS }));
    const core = await createWtermCore(TEST_COLS, TEST_ROWS);
    const harness = await makeHarness(core);
    const local = attachExtraCellSink(harness.manager, "local:socket-1");
    await enableStream(harness.manager, STREAM_A);
    expect(local.frames.map((frame) => frame.full)).toEqual([true]);

    suspendCellSink(harness.manager, COORD_CELL_SINK_ID);
    core.writeString("\x1b[2;1HLOCAL-ONLY");
    harness.manager.emitCellFrame(CHANNEL_ID, false);

    // The local pane took a delta; a suspended coordinator neither received a
    // frame nor forced anybody back onto a fresh baseline.
    expect(local.frames.map((frame) => frame.full)).toEqual([true, false]);
    expect(frameRowText(local.frames[1]!, 1)).toContain("LOCAL-ONLY");
    expect(harness.frameAttempts.map((frame) => frame.full)).toEqual([true]);
    expect(harness.manager.pendingCellRepairs.has(CHANNEL_ID)).toBe(false);

    core.writeString("\x1b[3;1HSTILL-LOCAL");
    harness.manager.emitCellFrame(CHANNEL_ID, false);
    expect(local.frames.map((frame) => frame.full)).toEqual([true, false, false]);
  });

  test("repairs with exactly one forced full when an active sink drops a delta", async () => {
    trackKeeper(installAutoKeeper({ cols: TEST_COLS, rows: TEST_ROWS }));
    const core = await createWtermCore(TEST_COLS, TEST_ROWS);
    let dropNextDelta = false;
    const harness = await makeHarness(core, {
      sendFrame: (frame) => {
        if (frame.full || !dropNextDelta) return "sent";
        dropNextDelta = false;
        return "dropped";
      },
    });
    await enableStream(harness.manager, STREAM_A);
    expect(harness.frameAttempts.map((frame) => frame.full)).toEqual([true]);

    dropNextDelta = true;
    core.writeString("\x1b[2;1HDROPPED");
    harness.manager.emitCellFrame(CHANNEL_ID, false);

    // The dropped delta is followed by ONE full, and nothing after it.
    expect(harness.frameAttempts.map((frame) => frame.full)).toEqual([true, false, true]);
    expect(frameRowText(harness.frameAttempts[2]!, 1)).toContain("DROPPED");
    expect(harness.manager.pendingCellRepairs.has(CHANNEL_ID)).toBe(false);

    core.writeString("\x1b[3;1HAFTER-REPAIR");
    harness.manager.emitCellFrame(CHANNEL_ID, false);
    expect(harness.frameAttempts.map((frame) => frame.full)).toEqual([
      true,
      false,
      true,
      false,
    ]);
  });

  test("unregisters only the local sink that overflows", async () => {
    trackKeeper(installAutoKeeper({ cols: TEST_COLS, rows: TEST_ROWS }));
    const core = await createWtermCore(TEST_COLS, TEST_ROWS);
    const harness = await makeHarness(core);
    let overflow = false;
    const local = attachExtraCellSink(harness.manager, "local:socket-1", {
      sendFrame: () => (overflow ? "overflow" : "sent"),
    });
    await enableStream(harness.manager, STREAM_A);

    overflow = true;
    core.writeString("\x1b[2;1HOVERFLOW");
    harness.manager.emitCellFrame(CHANNEL_ID, false);

    // The overflowing socket is dropped and told to close exactly once.
    expect(local.overflowNotices()).toBe(1);
    expect(harness.manager.cellSinks.has(local.id)).toBe(false);
    const localFramesAtOverflow = local.frames.length;

    // The coordinator's delivery is untouched: it took that same delta and
    // keeps taking the next one, with no repair full in between.
    expect(harness.frameAttempts.map((frame) => frame.full)).toEqual([true, false]);
    core.writeString("\x1b[3;1HCOORD-LIVE");
    harness.manager.emitCellFrame(CHANNEL_ID, false);
    expect(harness.frameAttempts.map((frame) => frame.full)).toEqual([true, false, false]);
    expect(frameRowText(harness.frameAttempts[2]!, 2)).toContain("COORD-LIVE");
    expect(local.frames).toHaveLength(localFramesAtOverflow);
  });

  test("repairs stream-wide when one sink drops a delta its sibling took", async () => {
    trackKeeper(installAutoKeeper({ cols: TEST_COLS, rows: TEST_ROWS }));
    const core = await createWtermCore(TEST_COLS, TEST_ROWS);
    let dropNextCoordDelta = false;
    const harness = await makeHarness(core, {
      sendFrame: (frame) => {
        if (frame.full || !dropNextCoordDelta) return "sent";
        dropNextCoordDelta = false;
        return "dropped";
      },
    });
    const local = attachExtraCellSink(harness.manager, "local:socket-1");
    await enableStream(harness.manager, STREAM_A);

    dropNextCoordDelta = true;
    core.writeString("\x1b[2;1HSPLIT");
    harness.manager.emitCellFrame(CHANNEL_ID, false);

    // The local sink kept the delta; the coordinator's drop costs exactly one
    // stream-wide full, which both sinks receive as the same frame.
    expect(harness.frameAttempts.map((frame) => frame.full)).toEqual([true, false, true]);
    expect(local.frames.map((frame) => frame.full)).toEqual([true, false, true]);
    expect(local.frames[2]!.seq).toBe(harness.frameAttempts[2]!.seq);
    // The accepted delta advanced the sequence, so the repair does not reuse it.
    expect(local.frames.map((frame) => frame.seq)).toEqual([1n, 2n, 3n]);
    expect(harness.manager.pendingCellRepairs.has(CHANNEL_ID)).toBe(false);

    core.writeString("\x1b[3;1HAFTER");
    harness.manager.emitCellFrame(CHANNEL_ID, false);
    expect(local.frames.map((frame) => frame.full)).toEqual([true, false, true, false]);
    expect(harness.frameAttempts.map((frame) => frame.full)).toEqual([true, false, true, false]);
  });

  test("a sink registering mid-snapshot does not darken the channel", async () => {
    trackKeeper(installAutoKeeper({ cols: TEST_COLS, rows: TEST_ROWS }));
    const core = await createWtermCore(TEST_COLS, TEST_ROWS);
    core.writeString("BASE");
    let coordWritable = false;
    const harness = await makeHarness(core, {
      sendFrame: () => (coordWritable ? "sent" : "dropped"),
    });
    await enableStream(harness.manager, STREAM_A);
    // The coordinator's baseline is parked on its own cursor.
    expect(harness.frameAttempts).toHaveLength(1);

    // A local socket arrives while that cursor is still parked. It must get a
    // complete baseline: a forced full withheld here would leave every sink
    // owing one with nothing left to trigger it.
    const local = attachExtraCellSink(harness.manager, "local:socket-1");
    expect(local.frames.map((frame) => frame.full)).toEqual([true]);

    coordWritable = true;
    harness.manager.resumeTerminalSnapshots();
    core.writeString("\x1b[2;1HAFTER");
    harness.manager.emitCellFrame(CHANNEL_ID, false);

    // Both sinks are live again on the same stream.
    expect(local.frames.map((frame) => frame.full)).toEqual([true, false]);
    expect(frameRowText(local.frames[1]!, 1)).toContain("AFTER");
    expect(harness.frameAttempts.at(-1)).toMatchObject({
      full: false,
      streamId: STREAM_A,
    });
  });

  test("builds one frame per tick and fans that same frame to every sink", async () => {
    trackKeeper(installAutoKeeper({ cols: TEST_COLS, rows: TEST_ROWS }));
    const core = await createWtermCore(TEST_COLS, TEST_ROWS);
    const harness = await makeHarness(core);
    const local = attachExtraCellSink(harness.manager, "local:socket-1");
    await enableStream(harness.manager, STREAM_A);

    core.writeString("\x1b[2;1HONE-BUILDER");
    harness.manager.emitCellFrame(CHANNEL_ID, false);

    // A second CellEmitState over one core would steal the first frame's dirty
    // rows: identical seq AND identical row content prove a single build.
    expect(harness.frameAttempts.map((frame) => frame.seq)).toEqual(
      local.frames.map((frame) => frame.seq),
    );
    expect(harness.frameAttempts.at(-1)!.seq).toBe(2n);
    expect(frameRowText(local.frames.at(-1)!, 1)).toBe(
      frameRowText(harness.frameAttempts.at(-1)!, 1),
    );
    expect(frameRowText(local.frames.at(-1)!, 1)).toContain("ONE-BUILDER");

    resumeCellSink(harness.manager, COORD_CELL_SINK_ID);
    // An already-active sink resumes delivery without minting a new frame.
    expect(harness.frameAttempts).toHaveLength(2);
  });
});
