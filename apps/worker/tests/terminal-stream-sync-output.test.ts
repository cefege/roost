import { afterEach, describe, expect, test, vi } from "bun:test";
import { createWtermCore } from "@roost/shared/wterm-core-factory";
import {
  SYNC_OUTPUT_MAX_MS,
  SYNC_OUTPUT_MAX_PENDING_ROWS,
} from "../src/session-constants.ts";
import { installAutoKeeper } from "./keeper-fake-pool.ts";
import {
  CHANNEL_ID,
  cleanupStreamHarnesses,
  enableStream,
  flushLeadingCellEmit,
  frameRowText,
  makeHarness,
  STREAM_A,
  TEST_COLS,
  TEST_ROWS,
  trackKeeper,
} from "./terminal-stream-state-harness.ts";

afterEach(() => {
  cleanupStreamHarnesses();
  vi.useRealTimers();
});

describe("worker DEC 2026 synchronized-output ceilings", () => {
  test("wall cap installs an initial full while the synchronized generation stays open", async () => {
    trackKeeper(installAutoKeeper({ cols: TEST_COLS, rows: TEST_ROWS }));
    const core = await createWtermCore(TEST_COLS, TEST_ROWS);
    const harness = await makeHarness(core);
    vi.useFakeTimers();

    core.writeString("\x1b[?2026h\x1b[1;1HHELD");
    expect(core.synchronizedOutput?.()).toBe(true);
    const resultPromise = enableStream(harness.manager, STREAM_A);
    await flushLeadingCellEmit();

    expect(harness.frameAttempts).toHaveLength(0);
    const hold = harness.manager.syncOutputHolds.get(CHANNEL_ID);
    expect(hold).toMatchObject({ tripped: false });
    const timer = hold?.timer;
    expect(timer).toBeDefined();

    vi.advanceTimersByTime(SYNC_OUTPUT_MAX_MS - 1);
    await Promise.resolve();
    expect(harness.frameAttempts).toHaveLength(0);
    expect(hold?.timer).toBe(timer);

    vi.advanceTimersByTime(1);
    await flushLeadingCellEmit();
    expect(core.synchronizedOutput?.()).toBe(true);
    expect(harness.frameAttempts).toHaveLength(1);
    expect(harness.frameAttempts[0]).toMatchObject({
      full: true,
      streamId: STREAM_A,
      seq: 1n,
      baseSeq: 0n,
    });
    expect(frameRowText(harness.frameAttempts[0]!, 0)).toContain("HELD");
    expect(hold).toMatchObject({ tripped: true, timer: undefined });
    expect(harness.manager.pendingSyncCellSnapshots.has(CHANNEL_ID)).toBe(false);
    await expect(resultPromise).resolves.toMatchObject({ status: "committed" });

    core.writeString("\x1b[2;1HAFTER-CAP");
    harness.manager.emitCellFrame(CHANNEL_ID, false);
    expect(harness.frameAttempts).toHaveLength(2);
    expect(harness.frameAttempts[1]).toMatchObject({
      full: false,
      streamId: STREAM_A,
      seq: 2n,
      baseSeq: 1n,
    });
    expect(frameRowText(harness.frameAttempts[1]!, 1)).toContain("AFTER-CAP");
    expect(harness.manager.syncOutputHolds.get(CHANNEL_ID)).toBe(hold);
    expect(hold).toMatchObject({ tripped: true, timer: undefined });
  });

  test("pending-row cap releases an owed full and leaves the generation pass-through", async () => {
    trackKeeper(installAutoKeeper({ cols: TEST_COLS, rows: TEST_ROWS }));
    const core = await createWtermCore(TEST_COLS, TEST_ROWS);
    const harness = await makeHarness(core);
    await enableStream(harness.manager, STREAM_A);
    expect(harness.frameAttempts).toHaveLength(1);
    vi.useFakeTimers();

    core.writeString("\x1b[?2026h");
    harness.manager.emitCellFrame(CHANNEL_ID, true);
    expect(harness.frameAttempts).toHaveLength(1);
    const hold = harness.manager.syncOutputHolds.get(CHANNEL_ID);
    expect(hold).toMatchObject({ tripped: false });

    core.writeString(`X\r\n`.repeat(SYNC_OUTPUT_MAX_PENDING_ROWS + TEST_ROWS));
    harness.manager.emitCellFrame(CHANNEL_ID, false);

    expect(core.synchronizedOutput?.()).toBe(true);
    expect(harness.frameAttempts).toHaveLength(2);
    expect(harness.frameAttempts[1]).toMatchObject({
      full: true,
      streamId: STREAM_A,
      seq: 2n,
      baseSeq: 0n,
    });
    expect(hold).toMatchObject({ tripped: true, timer: undefined });
    expect(harness.manager.pendingSyncCellSnapshots.has(CHANNEL_ID)).toBe(false);

    vi.advanceTimersByTime(SYNC_OUTPUT_MAX_MS);
    await Promise.resolve();
    expect(harness.frameAttempts).toHaveLength(2);

    core.writeString("\x1b[1;1HPASS");
    harness.manager.emitCellFrame(CHANNEL_ID, false);
    expect(harness.frameAttempts).toHaveLength(3);
    expect(harness.frameAttempts[2]).toMatchObject({
      full: false,
      streamId: STREAM_A,
      seq: 3n,
      baseSeq: 2n,
    });
  });

  test("a normal close before the wall cap flushes exactly once", async () => {
    trackKeeper(installAutoKeeper({ cols: TEST_COLS, rows: TEST_ROWS }));
    const core = await createWtermCore(TEST_COLS, TEST_ROWS);
    const harness = await makeHarness(core);
    await enableStream(harness.manager, STREAM_A);
    expect(harness.frameAttempts).toHaveLength(1);
    vi.useFakeTimers();

    core.writeString("\x1b[?2026h\x1b[3;1HCLOSE-FLUSH");
    harness.manager.emitCellFrame(CHANNEL_ID, false);
    expect(harness.frameAttempts).toHaveLength(1);

    vi.advanceTimersByTime(SYNC_OUTPUT_MAX_MS - 1);
    await Promise.resolve();
    expect(harness.frameAttempts).toHaveLength(1);

    core.writeString("\x1b[?2026l");
    harness.manager.emitCellFrame(CHANNEL_ID, false);
    expect(core.synchronizedOutput?.()).toBe(false);
    expect(harness.frameAttempts).toHaveLength(2);
    expect(harness.frameAttempts[1]).toMatchObject({
      full: false,
      streamId: STREAM_A,
      seq: 2n,
      baseSeq: 1n,
    });
    expect(frameRowText(harness.frameAttempts[1]!, 2)).toContain("CLOSE-FLUSH");
    expect(harness.manager.syncOutputHolds.has(CHANNEL_ID)).toBe(false);

    vi.advanceTimersByTime(SYNC_OUTPUT_MAX_MS);
    await Promise.resolve();
    expect(harness.frameAttempts).toHaveLength(2);
  });
});
