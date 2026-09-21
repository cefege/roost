// The core-trap half of the worker's terminal stream contract: an unprovable
// keeper boundary latches the stream fail-closed, no later generation inherits
// the dead capture, and the next stream desire re-proves that core in place
// from the keeper's ordered history (src/session-core-reprove.ts). Split from
// terminal-stream-state.test.ts, which owns the baseline/sequence half; both
// drive the same harness and fake keeper socket.

import { afterEach, describe, expect, test } from "bun:test";
import { createWtermCore } from "@roost/shared/wterm-core-factory";
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
  type StreamHarness,
} from "./terminal-stream-state-harness.ts";
import { aggregateStreamDelivery } from "../src/session-cell-sinks.ts";
import {
  applyResizeResultAtBoundary,
  installLiveResizeCapture,
} from "../src/session-resize-capture.ts";
import type { LiveResizeCapture } from "../src/session-terminal-state.ts";
import {
  getMultiplexedPool,
  type KeeperHistoryRecord,
  type KeeperHistoryRecords,
  type KeeperTerminalState,
} from "../src/keeper/multiplexed-client.ts";

afterEach(cleanupStreamHarnesses);

const BEFORE_TRAP = "BEFORE-TRAP";
const AFTER_TRAP = "\r\nAFTER-TRAP";
const REPROVED_COLS = TEST_COLS + 6;
const REPROVED_ROWS = TEST_ROWS + 2;

describe("worker terminal core traps and re-proof", () => {
  test("a trapped resize releases the emission gate it can never lift", async () => {
    trackKeeper(installAutoKeeper({ cols: TEST_COLS, rows: TEST_ROWS }));
    const core = await createWtermCore(TEST_COLS, TEST_ROWS);
    core.writeString("BEFORE-TRAP");
    const harness = await makeHarness(core);

    await enableStream(harness.manager, STREAM_A);
    const stream = harness.manager.terminalStreams.get(CHANNEL_ID)!;
    const capture = trapResizeCapture(harness);

    expect(capture.boundaryApplied).toBe(false);
    expect(capture.failedReason).not.toBeNull();
    expect(stream.coreValid).toBe(false);
    // Nothing can ever finish this capture, so neither it nor its gate may
    // outlive the failure.
    expect(stream.resizeCapture).toBeNull();
    expect(harness.manager.cellEmissionGates.has(CHANNEL_ID)).toBe(false);
    expect(harness.manager.cellGateSuppression.has(CHANNEL_ID)).toBe(false);
  });

  test("a generation minted after a core trap inherits no dead capture", async () => {
    trackKeeper(installAutoKeeper({ cols: TEST_COLS, rows: TEST_ROWS }));
    const core = await createWtermCore(TEST_COLS, TEST_ROWS);
    core.writeString("BEFORE-TRAP");
    const harness = await makeHarness(core);

    await enableStream(harness.manager, STREAM_A);
    const capture = trapResizeCapture(harness);
    // A generation boundary is the last place that can refuse a capture nothing
    // will ever finish, so it must hold even when that capture is still attached
    // and still holding the channel's emission gate.
    harness.manager.terminalStreams.get(CHANNEL_ID)!.resizeCapture = capture;
    harness.manager.cellEmissionGates.add(CHANNEL_ID);
    const framesBeforeRenewal = harness.frameAttempts.length;

    // Route reconciliation mints the next generation over the same channel.
    // The mint's own re-proof attempt is refused here so this case stays about
    // the dead capture: a keeper that cannot serve history keeps the verdict
    // fail-closed, which is what the assertions below read.
    const restoreHistory = stubHistoryRefusal();
    try {
      await expect(enableStream(harness.manager, STREAM_B)).resolves.toMatchObject({
        status: "rejected",
        streamId: STREAM_B,
        failure: "core_failed",
      });
    } finally {
      restoreHistory();
    }
    await flushLeadingCellEmit();
    const minted = harness.manager.terminalStreams.get(CHANNEL_ID)!;
    expect(minted.streamId).toBe(STREAM_B);
    expect(minted.resizeCapture).toBeNull();
    expect(harness.manager.cellEmissionGates.has(CHANNEL_ID)).toBe(false);
    expect(harness.frameAttempts).toHaveLength(framesBeforeRenewal);

    // Re-proof or adoption is what re-proves a frozen core; once one has, no
    // stranded gate may keep this generation silent.
    minted.coreValid = true;
    harness.manager.installTerminalBaseline(CHANNEL_ID);
    expect(harness.frameAttempts).toHaveLength(framesBeforeRenewal + 1);
    expect(harness.frameAttempts.at(-1)).toMatchObject({
      full: true,
      streamId: STREAM_B,
    });
  });

  test("a fail-closed core is re-proved from keeper history on the next stream desire", async () => {
    trackKeeper(installAutoKeeper({ cols: TEST_COLS, rows: TEST_ROWS }));
    const harness = await makeHarness();

    await enableStream(harness.manager, STREAM_A);
    harness.manager.emitUpstreamChunk(CHANNEL_ID, Buffer.from(BEFORE_TRAP));
    trapResizeCapture(harness);
    expect(harness.manager.terminalStreams.get(CHANNEL_ID)!.coreValid).toBe(false);
    // The retain lane keeps the stream moving while the core stays frozen, so
    // these bytes live ONLY in the ring until something replays them.
    harness.manager.emitUpstreamChunk(CHANNEL_ID, Buffer.from(AFTER_TRAP));
    await flushLeadingCellEmit();
    const frozenHeadSeq = harness.record.head_seq;
    const frozenEpoch = harness.record.cell_emit.gridEpochBase;
    const framesBeforeRepair = harness.frameAttempts.length;

    const restoreHistory = stubOrderedHistory(frozenHeadSeq, [BEFORE_TRAP, AFTER_TRAP]);
    try {
      await expect(enableStream(harness.manager, STREAM_B, REPROVED_COLS, REPROVED_ROWS))
        .resolves.toMatchObject({ status: "committed", resized: true });
      await flushLeadingCellEmit();
    } finally {
      restoreHistory();
    }

    expect(harness.manager.terminalStreams.get(CHANNEL_ID)!.coreValid).toBe(true);
    // Re-derived history is a NEW grid identity, never a revision of the old
    // one: a warm renderer must renumber instead of merging into retained rows.
    expect(harness.record.cell_emit.gridEpochBase).not.toBe(frozenEpoch);
    // retainedStart is head_seq minus the ring's length, so a window rebuilt
    // over the same bytes must not renumber its own head.
    expect(harness.record.head_seq).toBe(frozenHeadSeq);
    expect(harness.frameAttempts.length).toBeGreaterThan(framesBeforeRepair);
    const painted = harness.frameAttempts.at(-1)!;
    expect(painted).toMatchObject({
      full: true,
      streamId: STREAM_B,
      cols: REPROVED_COLS,
      rows: REPROVED_ROWS,
    });
    // Both the pre-trap bytes and the ones only the ring held are on the grid.
    expect(frameRowText(painted, 0)).toContain("BEFORE-TRAP");
    expect(frameRowText(painted, 1)).toContain("AFTER-TRAP");
  });

  test("a normal acknowledged resize keeps its live core without rebuilding retained history", async () => {
    trackKeeper(installAutoKeeper({ cols: TEST_COLS, rows: TEST_ROWS }));
    const core = await createWtermCore(TEST_COLS, TEST_ROWS);
    const harness = await makeHarness(core);
    await enableStream(harness.manager, STREAM_A);
    harness.manager.emitUpstreamChunk(CHANNEL_ID, Buffer.from("\x1b[1;1HLIVE-BEFORE-RESIZE"));

    const stream = harness.manager.terminalStreams.get(CHANNEL_ID)!;
    const capture = installLiveResizeCapture(
      harness.manager,
      CHANNEL_ID,
      stream,
      1,
      TEST_COLS,
      TEST_ROWS,
      TEST_COLS + 3,
      TEST_ROWS + 2,
    );
    const pool = getMultiplexedPool();
    const priorHistory = pool.getHistoryRecords.bind(pool);
    let historyCalls = 0;
    pool.getHistoryRecords = async () => {
      historyCalls += 1;
      throw new Error("normal resize must not rebuild from history");
    };
    try {
      applyResizeResultAtBoundary(harness.manager, CHANNEL_ID, capture, {
        kind: "ack",
        seq: 1,
        cols: TEST_COLS + 3,
        rows: TEST_ROWS + 2,
      });
    } finally {
      pool.getHistoryRecords = priorHistory;
    }

    expect(historyCalls).toBe(0);
    expect(harness.record.wtermCore).toBe(core);
    expect([core.getCols(), core.getRows()]).toEqual([TEST_COLS + 3, TEST_ROWS + 2]);
    expect(stream.coreValid).toBe(true);
    expect(stream.resizeCapture).toBeNull();
    expect(harness.manager.cellEmissionGates.has(CHANNEL_ID)).toBe(false);
  });

  test("core re-proof replays ordered resize history before its retained live tail exactly once", async () => {
    trackKeeper(installAutoKeeper({ cols: TEST_COLS, rows: TEST_ROWS }));
    const harness = await makeHarness();
    await enableStream(harness.manager, STREAM_A);
    const beforeResize = "\x1b[1;1HORD-BEFORE";
    const afterResize = "\x1b[2;1HORD-AFTER";
    const liveAfterHistory = "\x1b[3;1HORD-LIVE";
    const beforeBytes = Buffer.from(beforeResize);
    const afterBytes = Buffer.from(afterResize);
    const liveBytes = Buffer.from(liveAfterHistory);

    harness.manager.emitUpstreamChunk(CHANNEL_ID, beforeBytes);
    trapResizeCapture(harness);
    harness.manager.emitUpstreamChunk(CHANNEL_ID, afterBytes);
    harness.manager.emitUpstreamChunk(CHANNEL_ID, liveBytes);
    const historyHeadSeq = beforeBytes.byteLength + afterBytes.byteLength;
    const restoreHistory = stubOrderedHistoryRecords({
      headSeq: historyHeadSeq,
      baseCols: TEST_COLS,
      baseRows: TEST_ROWS,
      records: [
        { kind: "output", bytes: beforeBytes },
        { kind: "resize", seq: 1, cols: REPROVED_COLS, rows: REPROVED_ROWS },
        { kind: "output", bytes: afterBytes },
      ],
    }, {
      headSeq: historyHeadSeq,
      cols: REPROVED_COLS,
      rows: REPROVED_ROWS,
      highestResizeSeq: 1,
      appliedResizeSeq: 1,
    });
    try {
      await expect(enableStream(harness.manager, STREAM_B, REPROVED_COLS, REPROVED_ROWS))
        .resolves.toMatchObject({ status: "committed" });
      await flushLeadingCellEmit();
    } finally {
      restoreHistory();
    }

    const painted = harness.frameAttempts.at(-1)!;
    expect([harness.record.wtermCore.getCols(), harness.record.wtermCore.getRows()])
      .toEqual([REPROVED_COLS, REPROVED_ROWS]);
    expect(frameRowText(painted, 0)).toContain("ORD-BEFORE");
    expect(frameRowText(painted, 1)).toContain("ORD-AFTER");
    expect(frameRowText(painted, 2)).toContain("ORD-LIVE");
    const text = [0, 1, 2].map((row) => frameRowText(painted, row)).join("\n");
    for (const marker of ["ORD-BEFORE", "ORD-AFTER", "ORD-LIVE"]) {
      expect(text.split(marker).length - 1).toBe(1);
    }
    expect(harness.record.head_seq).toBe(historyHeadSeq + liveBytes.byteLength);
  });
  test("a core the keeper cannot re-prove stays fail-closed", async () => {
    trackKeeper(installAutoKeeper({ cols: TEST_COLS, rows: TEST_ROWS }));
    const harness = await makeHarness();

    await enableStream(harness.manager, STREAM_A);
    trapResizeCapture(harness);

    const restoreHistory = stubHistoryRefusal();
    try {
      await expect(enableStream(harness.manager, STREAM_B, REPROVED_COLS, REPROVED_ROWS))
        .resolves.toMatchObject({ status: "rejected", failure: "core_failed" });
    } finally {
      restoreHistory();
    }
    expect(harness.manager.terminalStreams.get(CHANNEL_ID)!.coreValid).toBe(false);
  });

  test("a lost ACK whose recovery cannot prove the boundary reports a re-provable core", async () => {
    trackKeeper(installAutoKeeper(
      { cols: TEST_COLS, rows: TEST_ROWS },
      { unknownResizeSeqs: [1] },
    ));
    const harness = await makeHarness();

    // Ordered history with no resize record: the lost-ACK recovery cannot prove
    // the boundary and fails closed by trapping the core. That verdict must
    // report as core_failed — the one the view owner can re-prove — and not as
    // an unknown boundary, which stays un-retried forever.
    const restoreHistory = stubOrderedHistory(harness.record.head_seq, []);
    try {
      await expect(enableStream(harness.manager, STREAM_A, REPROVED_COLS, REPROVED_ROWS))
        .resolves.toMatchObject({ status: "ambiguous", failure: "core_failed" });
    } finally {
      restoreHistory();
    }
    expect(harness.manager.terminalStreams.get(CHANNEL_ID)!.coreValid).toBe(false);
  });

  test("a trapped core still refuses frames for the generation it trapped", async () => {
    trackKeeper(installAutoKeeper({ cols: TEST_COLS, rows: TEST_ROWS }));
    const core = await createWtermCore(TEST_COLS, TEST_ROWS);
    core.writeString("BEFORE-TRAP");
    const harness = await makeHarness(core);

    await enableStream(harness.manager, STREAM_A);
    const stream = harness.manager.terminalStreams.get(CHANNEL_ID)!;
    trapResizeCapture(harness);
    const framesBeforeOutput = harness.frameAttempts.length;

    harness.manager.emitUpstreamChunk(CHANNEL_ID, Buffer.from("\x1b[3;1HAFTER-TRAP"));
    harness.manager.emitCellFrame(CHANNEL_ID, false);
    harness.manager.installTerminalBaseline(CHANNEL_ID);
    await flushLeadingCellEmit();

    expect(harness.frameAttempts).toHaveLength(framesBeforeOutput);
    expect(aggregateStreamDelivery(harness.manager, stream)).toMatchObject({
      baselineReady: false,
    });
  });
});


/** Drive one live resize capture into the trap an ACK for geometry the worker
 *  never asked for produces: the boundary can never be applied, so the capture
 *  is exactly the one no later event can finish. */
function trapResizeCapture(harness: StreamHarness): LiveResizeCapture {
  const stream = harness.manager.terminalStreams.get(CHANNEL_ID)!;
  const capture = installLiveResizeCapture(
    harness.manager,
    CHANNEL_ID,
    stream,
    1,
    TEST_COLS,
    TEST_ROWS,
    TEST_COLS + 4,
    TEST_ROWS,
  );
  expect(harness.manager.cellEmissionGates.has(CHANNEL_ID)).toBe(true);
  applyResizeResultAtBoundary(harness.manager, CHANNEL_ID, capture, {
    kind: "ack",
    seq: 1,
    cols: TEST_COLS + 9,
    rows: TEST_ROWS,
  });
  return capture;
}

/** The fake keeper socket answers resizes and terminal state but never history
 *  frames, so an ordered-history read is stubbed at the pool — the same seam
 *  terminal-capture-resize.test.ts uses. */
function stubOrderedHistory(headSeq: number, outputs: readonly string[]): () => void {
  const encoder = new TextEncoder();
  const records: KeeperHistoryRecord[] = outputs.map((text) => ({
    kind: "output",
    bytes: encoder.encode(text),
  }));
  const history: KeeperHistoryRecords = {
    headSeq,
    baseCols: TEST_COLS,
    baseRows: TEST_ROWS,
    records,
  };
  const pool = getMultiplexedPool();
  const prior = pool.getHistoryRecords.bind(pool);
  pool.getHistoryRecords = async () => history;
  return () => { pool.getHistoryRecords = prior; };
}

function stubOrderedHistoryRecords(
  history: KeeperHistoryRecords,
  terminalState: KeeperTerminalState,
): () => void {
  const pool = getMultiplexedPool();
  const priorHistory = pool.getHistoryRecords.bind(pool);
  const priorTerminalState = pool.getTerminalState.bind(pool);
  pool.getHistoryRecords = async () => history;
  pool.getTerminalState = async () => terminalState;
  return () => {
    pool.getHistoryRecords = priorHistory;
    pool.getTerminalState = priorTerminalState;
  };
}

/** A keeper that cannot serve history at all: the re-proof refuses immediately
 *  instead of waiting out the pool's own command timeout. */
function stubHistoryRefusal(): () => void {
  const pool = getMultiplexedPool();
  const prior = pool.getHistoryRecords.bind(pool);
  pool.getHistoryRecords = async () => {
    throw new Error("ordered history is unavailable");
  };
  return () => { pool.getHistoryRecords = prior; };
}
