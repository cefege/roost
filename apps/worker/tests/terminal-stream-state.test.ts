import { afterEach, describe, expect, test } from "bun:test";
import type { PbCellGridFrame } from "@roost/shared/proto/cell_pb";
import { createWtermCore } from "@roost/shared/wterm-core-factory";
import { installAutoKeeper } from "./keeper-fake-pool.ts";
import {
  attachExtraCellSink,
  CHANNEL_ID,
  cleanupStreamHarnesses,
  enableStream,
  flushLeadingCellEmit,
  frameRowText,
  makeHarness,
  SESSION_ID,
  STREAM_A,
  STREAM_B,
  TEST_COLS,
  TEST_ROWS,
  trackKeeper,
} from "./terminal-stream-state-harness.ts";
import {
  aggregateStreamDelivery,
  COORD_CELL_SINK_ID,
  resumeCellSink,
  suspendCellSink,
  unregisterCellSink,
} from "../src/session-cell-sinks.ts";

afterEach(cleanupStreamHarnesses);

describe("worker terminal stream baseline and sequence contract", () => {
  test("commits while a blocked full later admits its exact successor delta", async () => {
    trackKeeper(installAutoKeeper({ cols: TEST_COLS, rows: TEST_ROWS }));
    const core = await createWtermCore(TEST_COLS, TEST_ROWS);
    core.writeString("BASELINE");
    let writable = false;
    const delivered: PbCellGridFrame[] = [];
    const harness = await makeHarness(core, {
      sendFrame: (frame) => {
        if (!writable) return "dropped";
        delivered.push(frame);
        return "sent";
      },
    });

    const resultPromise = enableStream(harness.manager, STREAM_A);
    await flushLeadingCellEmit();
    expect(harness.frameAttempts).toHaveLength(1);
    expect(harness.frameAttempts[0]).toMatchObject({
      full: true,
      streamId: STREAM_A,
      seq: 1n,
      baseSeq: 0n,
    });
    expect(delivered).toHaveLength(0);
    const stream = harness.manager.terminalStreams.get(CHANNEL_ID)!;
    expect(stream).toMatchObject({ streamId: STREAM_A, coreValid: true });
    expect(aggregateStreamDelivery(harness.manager, stream)).toMatchObject({
      baselineReady: false,
      snapshotPending: true,
    });
    const result = await resultPromise;
    expect(result).toMatchObject({
      status: "committed",
      streamId: STREAM_A,
      resized: false,
      cols: TEST_COLS,
      rows: TEST_ROWS,
    });

    core.writeString("\x1b[2;1HEXACT-DELTA");
    harness.manager.emitCellFrame(CHANNEL_ID, false);
    expect(harness.frameAttempts).toHaveLength(1);

    writable = true;
    harness.manager.resumeTerminalSnapshots();
    await flushLeadingCellEmit();
    expect(harness.frameAttempts.map((frame) => frame.full)).toEqual([
      true,
      true,
      false,
    ]);
    expect(delivered).toHaveLength(2);
    const [baseline, delta] = delivered;
    expect(baseline).toMatchObject({
      full: true,
      streamId: STREAM_A,
      seq: 1n,
      baseSeq: 0n,
    });
    expect(delta).toMatchObject({
      full: false,
      streamId: STREAM_A,
      gridEpoch: baseline!.gridEpoch,
      seq: 2n,
      baseSeq: 1n,
    });
    expect(delta!.viewportRows.map((row) => row.index)).toContain(1);
    expect(frameRowText(delta!, 1)).toContain("EXACT-DELTA");
    expect(aggregateStreamDelivery(harness.manager, stream)).toMatchObject({
      baselineReady: true,
      snapshotPending: false,
    });
  });

  test("repairs a transport-rejected delta and answers a later gap request with full baselines", async () => {
    trackKeeper(installAutoKeeper({ cols: TEST_COLS, rows: TEST_ROWS }));
    const core = await createWtermCore(TEST_COLS, TEST_ROWS);
    core.writeString("BASE");
    let rejectNextDelta = true;
    const delivered: PbCellGridFrame[] = [];
    const harness = await makeHarness(core, {
      sendFrame: (frame) => {
        if (!frame.full && rejectNextDelta) {
          rejectNextDelta = false;
          return "dropped";
        }
        delivered.push(frame);
        return "sent";
      },
    });

    await enableStream(harness.manager, STREAM_A);
    core.writeString("\x1b[2;1HDROPPED-DELTA");
    harness.manager.emitCellFrame(CHANNEL_ID, false);

    expect(harness.frameAttempts).toHaveLength(3);
    const [, rejectedDelta, repair] = harness.frameAttempts;
    expect(rejectedDelta).toMatchObject({
      full: false,
      streamId: STREAM_A,
      seq: 2n,
      baseSeq: 1n,
    });
    expect(repair).toMatchObject({
      full: true,
      streamId: STREAM_A,
      gridEpoch: rejectedDelta!.gridEpoch,
      seq: 2n,
      baseSeq: 0n,
    });
    expect(delivered.map((frame) => [frame.full, frame.seq, frame.baseSeq])).toEqual([
      [true, 1n, 0n],
      [true, 2n, 0n],
    ]);

    harness.manager.requestTerminalSnapshot(SESSION_ID, STREAM_B);
    expect(harness.frameAttempts).toHaveLength(3);
    harness.manager.requestTerminalSnapshot(SESSION_ID, STREAM_A);
    expect(harness.frameAttempts.at(-1)).toMatchObject({
      full: true,
      streamId: STREAM_A,
      seq: 3n,
      baseSeq: 0n,
    });

    core.writeString("\x1b[3;1HAFTER-REPAIR");
    harness.manager.emitCellFrame(CHANNEL_ID, false);
    expect(harness.frameAttempts.at(-1)).toMatchObject({
      full: false,
      streamId: STREAM_A,
      seq: 4n,
      baseSeq: 3n,
    });
  });

  test("re-baselines only the coordinator sink when its link reopens", async () => {
    trackKeeper(installAutoKeeper({ cols: TEST_COLS, rows: TEST_ROWS }));
    const core = await createWtermCore(TEST_COLS, TEST_ROWS);
    core.writeString("STREAM-A");
    const harness = await makeHarness(core);
    const local = attachExtraCellSink(harness.manager, "local:tab-1");

    await enableStream(harness.manager, STREAM_A);
    const stream = harness.manager.terminalStreams.get(CHANNEL_ID)!;
    const epochA = harness.frameAttempts[0]!.gridEpoch;
    expect(local.frames.map((frame) => frame.full)).toEqual([true]);

    // The coordinator link drops. The local viewer keeps painting deltas.
    suspendCellSink(harness.manager, COORD_CELL_SINK_ID);
    core.writeString("\x1b[2;1HWHILE-COORD-DOWN");
    harness.manager.emitCellFrame(CHANNEL_ID, false);
    expect(harness.frameAttempts).toHaveLength(1);
    expect(local.frames.map((frame) => frame.full)).toEqual([true, false]);
    expect(frameRowText(local.frames[1]!, 1)).toContain("WHILE-COORD");

    resumeCellSink(harness.manager, COORD_CELL_SINK_ID);
    // The reopen disturbed neither the stream identity nor its live geometry.
    expect(harness.manager.terminalStreams.get(CHANNEL_ID)).toBe(stream);
    expect(stream).toMatchObject({
      streamId: STREAM_A,
      enabled: true,
      cols: TEST_COLS,
      rows: TEST_ROWS,
    });
    expect(harness.frameAttempts.slice(1).map((frame) => frame.full)).toEqual([true]);
    expect(harness.frameAttempts[1]!.gridEpoch).toBe(epochA);
    expect(frameRowText(harness.frameAttempts[1]!, 1)).toContain("WHILE-COORD");

    // Coordinator-only from here: a second bounce still costs exactly one full.
    unregisterCellSink(harness.manager, local.id);
    core.writeString("\x1b[3;1HCOORD-ONLY");
    harness.manager.emitCellFrame(CHANNEL_ID, false);
    suspendCellSink(harness.manager, COORD_CELL_SINK_ID);
    resumeCellSink(harness.manager, COORD_CELL_SINK_ID);
    expect(harness.frameAttempts.slice(2).map((frame) => frame.full)).toEqual([false, true]);
    expect(local.frames).toHaveLength(3);
  });

  test("keeps a compatible renewal epoch while emitting a viewport-only full", async () => {
    trackKeeper(installAutoKeeper({ cols: TEST_COLS, rows: TEST_ROWS }));
    const core = await createWtermCore(TEST_COLS, TEST_ROWS);
    core.writeString(Array.from({ length: TEST_ROWS * 3 }, (_, index) => `H${index}\r\n`).join(""));
    expect(core.getScrollbackCount()).toBeGreaterThan(0);
    const harness = await makeHarness(core);

    await enableStream(harness.manager, STREAM_A);
    const initial = harness.frameAttempts.at(-1);
    if (!initial) throw new Error("initial terminal baseline was not emitted");

    await enableStream(harness.manager, STREAM_B);
    const renewal = harness.frameAttempts.at(-1);
    if (!renewal) throw new Error("renewal terminal baseline was not emitted");

    expect(renewal).toMatchObject({
      full: true,
      streamId: STREAM_B,
      baseSeq: 0n,
      seq: 1n,
      scrollbackRows: [],
      scrollbackAppend: [],
    });
    expect(renewal.gridEpoch).toBe(initial.gridEpoch);
    expect(renewal.sbBase).toBe(renewal.scrollbackTotal);
  });
});
