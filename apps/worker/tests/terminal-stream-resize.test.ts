import { afterEach, describe, expect, test } from "bun:test";
import { createWtermCore } from "@roost/shared/wterm-core-factory";
import { MuxFrameType } from "../src/keeper/protocol.ts";
import { installAutoKeeper } from "./keeper-fake-pool.ts";
import {
  cleanupStreamHarnesses,
  coreRowText,
  enableStream,
  frameRowText,
  makeHarness,
  paintRows,
  STREAM_A,
  STREAM_B,
  STREAM_C,
  TEST_COLS,
  TEST_ROWS,
  trackKeeper,
} from "./terminal-stream-state-harness.ts";

afterEach(cleanupStreamHarnesses);

describe("worker live resize boundary", () => {
  test("keeps one core allocation and preserves primary static rows across shrink and grow", async () => {
    const keeper = trackKeeper(installAutoKeeper({ cols: TEST_COLS, rows: TEST_ROWS }));
    const core = await createWtermCore(TEST_COLS, TEST_ROWS);
    const originalRows = Array.from({ length: TEST_ROWS }, (_, row) => `P${row}-STATIC`);
    paintRows(core, originalRows);
    const harness = await makeHarness(core);
    const originalCore = harness.record.wtermCore;

    const initial = await enableStream(harness.manager, STREAM_A);
    expect(initial).toMatchObject({ status: "committed", resized: false });
    const shrink = await enableStream(harness.manager, STREAM_B, 10, 3);
    expect(shrink).toMatchObject({
      status: "committed",
      streamId: STREAM_B,
      resized: true,
      channelResizeSeq: 1,
      cols: 10,
      rows: 3,
    });
    expect(harness.record.wtermCore).toBe(originalCore);
    expect(Array.from({ length: 3 }, (_, row) => coreRowText(core, row))).toEqual(
      originalRows.slice(3),
    );
    expect([0, 1, 2].map((row) => frameRowText(harness.frameAttempts.at(-1)!, row))).toEqual(
      originalRows.slice(3),
    );

    core.writeString("\x1b[2;4HCHANGED");
    const grow = await enableStream(harness.manager, STREAM_C, TEST_COLS, TEST_ROWS);
    expect(grow).toMatchObject({
      status: "committed",
      streamId: STREAM_C,
      resized: true,
      channelResizeSeq: 2,
      cols: TEST_COLS,
      rows: TEST_ROWS,
    });
    expect(harness.record.wtermCore).toBe(originalCore);
    expect(Array.from({ length: TEST_ROWS }, (_, row) => coreRowText(core, row))).toEqual([
      originalRows[0],
      originalRows[1],
      originalRows[2],
      originalRows[3],
      "P4-CHANGED",
      originalRows[5],
    ]);
    expect(keeper.seqOf(MuxFrameType.ResizeRequest)).toEqual([1, 2]);
    expect(harness.frameAttempts.map((frame) => [frame.streamId, frame.full, frame.seq])).toEqual([
      [STREAM_A, true, 1n],
      [STREAM_B, true, 1n],
      [STREAM_C, true, 1n],
    ]);
  });

  test("preserves fitting alternate cells and the complete saved primary grid on the same core", async () => {
    const keeper = trackKeeper(installAutoKeeper({ cols: TEST_COLS, rows: TEST_ROWS }));
    const core = await createWtermCore(TEST_COLS, TEST_ROWS);
    const primary = Array.from({ length: TEST_ROWS }, (_, row) => `M${row}-static`);
    const alternate = Array.from({ length: TEST_ROWS }, (_, row) => `A${row}-static`);
    paintRows(core, primary);
    const primaryCursor = core.getCursor();
    core.writeString("\x1b[?1049h");
    paintRows(core, alternate);
    const harness = await makeHarness(core);
    const originalCore = harness.record.wtermCore;

    await enableStream(harness.manager, STREAM_A);
    await enableStream(harness.manager, STREAM_B, 10, 3);
    expect(harness.record.wtermCore).toBe(originalCore);
    expect(core.usingAltScreen()).toBe(true);
    expect(Array.from({ length: 3 }, (_, row) => coreRowText(core, row))).toEqual(
      alternate.slice(0, 3),
    );

    core.writeString("\x1b[2;1HALT-DIFF");
    await enableStream(harness.manager, STREAM_C, TEST_COLS, TEST_ROWS);
    expect(harness.record.wtermCore).toBe(originalCore);
    expect(core.usingAltScreen()).toBe(true);
    expect(Array.from({ length: TEST_ROWS }, (_, row) => coreRowText(core, row))).toEqual([
      alternate[0],
      "ALT-DIFFc",
      alternate[2],
      "",
      "",
      "",
    ]);

    core.writeString("\x1b[?1049l");
    expect(core.usingAltScreen()).toBe(false);
    expect(Array.from({ length: TEST_ROWS }, (_, row) => coreRowText(core, row))).toEqual(primary);
    expect(core.getCursor()).toEqual(primaryCursor);
    expect(keeper.seqOf(MuxFrameType.ResizeRequest)).toEqual([1, 2]);
  });
});
