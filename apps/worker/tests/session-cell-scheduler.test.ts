import { afterEach, describe, expect, test, vi } from "bun:test";
import { createWtermCore } from "@roost/shared/wterm-core-factory";
import {
  CELL_EMIT_COALESCE_MS,
  SYNC_OUTPUT_MAX_MS,
} from "../src/session-constants.ts";
import { cancelCellEmission } from "../src/session-cell-scheduler.ts";
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
  STREAM_C,
  TEST_COLS,
  TEST_ROWS,
  trackKeeper,
} from "./terminal-stream-state-harness.ts";

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

  test("does not let queued old work touch a replacement or reconnect stream", async () => {
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

    core.writeString("\x1b[3;1HRECONNECT");
    harness.manager._scheduleCellEmit(CHANNEL_ID);
    expect(harness.manager.cellEmitSchedules.get(CHANNEL_ID)).toBeDefined();
    harness.manager.invalidateTerminalStreamsForReconnect();
    await flushLeadingCellEmit();
    expect(harness.manager.cellEmitSchedules.has(CHANNEL_ID)).toBe(false);
    expect(harness.frameAttempts).toHaveLength(2);

    await enableStream(harness.manager, STREAM_C);
    expect(harness.frameAttempts).toHaveLength(3);
    expect(harness.frameAttempts[2]).toMatchObject({
      full: true,
      streamId: STREAM_C,
    });
    expect(frameRowText(harness.frameAttempts[2]!, 2)).toContain("RECONNECT");
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
});
