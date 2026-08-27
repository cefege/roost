import { afterEach, describe, expect, test } from "bun:test";
import type { PbCellGridFrame } from "@roost/shared/proto/cell_pb";
import { createWtermCore } from "@roost/shared/wterm-core-factory";
import { installAutoKeeper } from "./keeper-fake-pool.ts";
import {
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

afterEach(cleanupStreamHarnesses);

describe("worker terminal stream baseline and sequence contract", () => {
  test("withholds every delta until the complete baseline sends, then emits the exact successor", async () => {
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
    expect(harness.manager.terminalStreams.get(CHANNEL_ID)).toMatchObject({
      streamId: STREAM_A,
      baselineReady: false,
    });

    core.writeString("\x1b[2;1HEXACT-DELTA");
    harness.manager.emitCellFrame(CHANNEL_ID, false);
    expect(harness.frameAttempts).toHaveLength(1);

    writable = true;
    harness.manager.resumeTerminalSnapshots();
    await flushLeadingCellEmit();
    const result = await resultPromise;

    expect(result).toMatchObject({
      status: "committed",
      streamId: STREAM_A,
      resized: false,
      cols: TEST_COLS,
      rows: TEST_ROWS,
    });
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
    expect(harness.manager.terminalStreams.get(CHANNEL_ID)).toMatchObject({
      baselineReady: true,
      snapshotCursor: null,
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

  test("invalidates a disconnected generation and requires a new stream identity", async () => {
    trackKeeper(installAutoKeeper({ cols: TEST_COLS, rows: TEST_ROWS }));
    const core = await createWtermCore(TEST_COLS, TEST_ROWS);
    core.writeString("STREAM-A");
    const harness = await makeHarness(core);

    await enableStream(harness.manager, STREAM_A);
    const epochA = harness.frameAttempts[0]!.gridEpoch;
    harness.manager.invalidateTerminalStreamsForReconnect();
    expect(harness.manager.terminalStreams.get(CHANNEL_ID)).toMatchObject({
      streamId: STREAM_A,
      enabled: false,
      baselineReady: true,
    });

    core.writeString("\x1b[2;1HWHILE-DISCONNECTED");
    harness.manager.emitCellFrame(CHANNEL_ID, false);
    expect(harness.frameAttempts).toHaveLength(1);

    const stale = await enableStream(harness.manager, STREAM_A);
    expect(stale).toMatchObject({
      status: "rejected",
      streamId: STREAM_A,
      failure: "invalid_request",
      phase: "pre_write",
    });
    expect(harness.frameAttempts).toHaveLength(1);

    const replacement = await enableStream(harness.manager, STREAM_B);
    expect(replacement).toMatchObject({
      status: "committed",
      streamId: STREAM_B,
      resized: false,
    });
    expect(harness.frameAttempts).toHaveLength(2);
    expect(harness.frameAttempts[1]).toMatchObject({
      full: true,
      streamId: STREAM_B,
      seq: 1n,
      baseSeq: 0n,
    });
    expect(harness.frameAttempts[1]!.gridEpoch).toBe(epochA);
    expect(frameRowText(harness.frameAttempts[1]!, 1)).toContain("WHILE-DISCON");

    harness.manager.requestTerminalSnapshot(SESSION_ID, STREAM_A);
    expect(harness.frameAttempts).toHaveLength(2);
  });
});
