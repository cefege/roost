import { afterEach, describe, expect, test, vi } from "bun:test";
import { createWtermCore } from "@roost/wterm/wterm-core-factory";
import {
  CELL_EMIT_COALESCE_MS,
  SYNC_OUTPUT_MAX_MS,
} from "../src/session-constants.ts";
import {
  cancelCellEmission,
  consumeInputEchoPromotion,
} from "../src/session-cell-scheduler.ts";
import { installLiveResizeCapture } from "../src/session-resize-capture.ts";
import { installAutoKeeper } from "./keeper-fake-pool.ts";
import {
  CHANNEL_ID,
  cleanupStreamHarnesses,
  enableStream,
  flushLeadingCellEmit,
  frameRowText,
  makeHarness,
  STREAM_A,
  STREAM_B,
  TEST_COLS,
  TEST_ROWS,
  trackKeeper,
} from "./terminal-stream-state-harness.ts";
import {
  COORD_CELL_SINK_ID,
  resumeCellSink,
  suspendCellSink,
} from "../src/session-cell-sinks.ts";

afterEach(() => {
  cleanupStreamHarnesses();
  vi.useRealTimers();
});

describe("worker cell emission scheduler", () => {
  test("identity-fences a cancelled leading callback before same-stream rescheduling", async () => {
    trackKeeper(installAutoKeeper({ cols: TEST_COLS, rows: TEST_ROWS }));
    const core = await createWtermCore(TEST_COLS, TEST_ROWS);
    const harness = await makeHarness(core);
    await enableStream(harness.manager, STREAM_A);

    core.writeString("\x1b[2;1HRESCHEDULED");
    harness.manager._scheduleCellEmit(CHANNEL_ID);
    const staleSchedule = harness.manager.cellEmitSchedules.get(CHANNEL_ID);
    expect(staleSchedule).toBeDefined();

    cancelCellEmission(harness.manager, CHANNEL_ID);
    harness.manager._scheduleCellEmit(CHANNEL_ID);
    const currentSchedule = harness.manager.cellEmitSchedules.get(CHANNEL_ID);
    expect(currentSchedule).toBeDefined();
    expect(currentSchedule).not.toBe(staleSchedule);

    await flushLeadingCellEmit();
    expect(harness.frameAttempts).toHaveLength(2);
    expect(harness.frameAttempts[1]).toMatchObject({
      full: false,
      streamId: STREAM_A,
    });
    expect(harness.manager.cellEmitSchedules.get(CHANNEL_ID)).toBe(currentSchedule);
  });

  test("does not let queued old work touch a replacement or a suspended sink", async () => {
    trackKeeper(installAutoKeeper({ cols: TEST_COLS, rows: TEST_ROWS }));
    const core = await createWtermCore(TEST_COLS, TEST_ROWS);
    const harness = await makeHarness(core);
    await enableStream(harness.manager, STREAM_A);

    core.writeString("\x1b[2;1HREPLACED");
    harness.manager._scheduleCellEmit(CHANNEL_ID);
    expect(harness.manager.cellEmitSchedules.get(CHANNEL_ID)).toBeDefined();

    await enableStream(harness.manager, STREAM_B);
    await flushLeadingCellEmit();
    expect(harness.frameAttempts.map((frame) => frame.streamId)).toEqual([
      STREAM_A,
      STREAM_B,
    ]);

    core.writeString("\x1b[3;1HCOORD-DOWN");
    harness.manager._scheduleCellEmit(CHANNEL_ID);
    expect(harness.manager.cellEmitSchedules.get(CHANNEL_ID)).toBeDefined();
    suspendCellSink(harness.manager, COORD_CELL_SINK_ID);
    await flushLeadingCellEmit();
    // The queued leading emit found no active sink, so it built no frame.
    expect(harness.manager.cellEmitSchedules.has(CHANNEL_ID)).toBe(false);
    expect(harness.frameAttempts).toHaveLength(2);

    // Resuming owes one full on the SAME stream generation, carrying the work
    // that landed while the coordinator was gone.
    resumeCellSink(harness.manager, COORD_CELL_SINK_ID);
    expect(harness.frameAttempts).toHaveLength(3);
    expect(harness.frameAttempts[2]).toMatchObject({
      full: true,
      streamId: STREAM_B,
    });
    expect(frameRowText(harness.frameAttempts[2]!, 2)).toContain("COORD-DOWN");
  });

  test("cancels a gated trailing cooldown until the post-boundary full wakes it", async () => {
    trackKeeper(installAutoKeeper({ cols: TEST_COLS, rows: TEST_ROWS }));
    const core = await createWtermCore(TEST_COLS, TEST_ROWS);
    const harness = await makeHarness(core);
    await enableStream(harness.manager, STREAM_A);
    vi.useFakeTimers();

    core.writeString("\x1b[2;1HLEADING");
    harness.manager._scheduleCellEmit(CHANNEL_ID);
    await flushLeadingCellEmit();
    expect(harness.frameAttempts).toHaveLength(2);

    core.writeString("\x1b[3;1HGATED");
    harness.manager._scheduleCellEmit(CHANNEL_ID);
    const trailingSchedule = harness.manager.cellEmitSchedules.get(CHANNEL_ID);
    expect(trailingSchedule).toBeDefined();
    expect(trailingSchedule?.timer).not.toBeNull();

    const state = harness.manager.terminalStreams.get(CHANNEL_ID)!;
    installLiveResizeCapture(
      harness.manager,
      CHANNEL_ID,
      state,
      1,
      TEST_COLS,
      TEST_ROWS,
      TEST_COLS,
      TEST_ROWS,
    );
    vi.advanceTimersByTime(CELL_EMIT_COALESCE_MS * 3);
    await Promise.resolve();
    expect(harness.frameAttempts).toHaveLength(2);
    expect(harness.manager.cellEmitSchedules.has(CHANNEL_ID)).toBe(false);
    expect(harness.manager.cellDirty.has(CHANNEL_ID)).toBe(true);

    state.resizeCapture = null;
    harness.manager.cellEmissionGates.delete(CHANNEL_ID);
    harness.manager.installTerminalBaseline(CHANNEL_ID);
    expect(harness.frameAttempts).toHaveLength(3);
    expect(harness.frameAttempts[2]).toMatchObject({
      full: true,
      streamId: STREAM_A,
    });
    expect(frameRowText(harness.frameAttempts[2]!, 2)).toContain("GATED");
  });

  test("schedules normally after an unclosed synchronized-output hold trips", async () => {
    trackKeeper(installAutoKeeper({ cols: TEST_COLS, rows: TEST_ROWS }));
    const core = await createWtermCore(TEST_COLS, TEST_ROWS);
    const harness = await makeHarness(core);
    await enableStream(harness.manager, STREAM_A);
    vi.useFakeTimers();

    core.writeString("\x1b[?2026h\x1b[2;1HHELD");
    harness.manager._scheduleCellEmit(CHANNEL_ID);
    await flushLeadingCellEmit();
    expect(harness.frameAttempts).toHaveLength(1);

    vi.advanceTimersByTime(SYNC_OUTPUT_MAX_MS);
    await flushLeadingCellEmit();
    expect(harness.manager.syncOutputHolds.get(CHANNEL_ID)).toMatchObject({ tripped: true });
    expect(harness.frameAttempts).toHaveLength(2);

    core.writeString("\x1b[3;1HAFTER-CAP");
    harness.manager._scheduleCellEmit(CHANNEL_ID);
    await flushLeadingCellEmit();
    expect(harness.frameAttempts).toHaveLength(3);
    expect(harness.frameAttempts[2]).toMatchObject({
      full: false,
      streamId: STREAM_A,
    });
    expect(frameRowText(harness.frameAttempts[2]!, 2)).toContain("AFTER-CAP");
  });

  test("a second keystroke in one coalesce window keeps its echo promotion", async () => {
    trackKeeper(installAutoKeeper({ cols: TEST_COLS, rows: TEST_ROWS }));
    const core = await createWtermCore(TEST_COLS, TEST_ROWS);
    const harness = await makeHarness(core);
    await enableStream(harness.manager, STREAM_A);
    vi.useFakeTimers();
    harness.manager.markInputSensitive(CHANNEL_ID);
    harness.manager.markInputSensitive(CHANNEL_ID);

    core.writeString("\x1b[2;1HE1");
    harness.manager._scheduleCellEmit(
      CHANNEL_ID,
      consumeInputEchoPromotion(harness.manager, CHANNEL_ID),
    );
    await flushLeadingCellEmit();
    const afterFirstEcho = harness.frameAttempts.length;
    expect(harness.manager.cellEmitSchedules.get(CHANNEL_ID)?.timer).not.toBeNull();

    core.writeString("\x1b[3;1HE2");
    harness.manager._scheduleCellEmit(
      CHANNEL_ID,
      consumeInputEchoPromotion(harness.manager, CHANNEL_ID),
    );
    await flushLeadingCellEmit();
    // The promoted second echo re-leads instead of waiting out the cooldown.
    expect(harness.frameAttempts.length).toBe(afterFirstEcho + 1);
    expect(frameRowText(harness.frameAttempts.at(-1)!, 2)).toContain("E2");
    expect(harness.manager.inputSensitiveChannels.has(CHANNEL_ID)).toBe(false);
  });
});
