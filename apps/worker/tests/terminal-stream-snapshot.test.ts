import { afterEach, describe, expect, test } from "bun:test";
import type { TerminalCore } from "@wterm/core";
import {
  CELL_GRID_PART_MAX_BYTES,
  encodedCellGridChunkSize,
} from "@roost/shared/cell";
import type { PbCellGridChunk } from "@roost/shared/proto/cell_pb";
import { installAutoKeeper } from "./keeper-fake-pool.ts";
import {
  CHANNEL_ID,
  cleanupStreamHarnesses,
  DenseLinkedCore,
  enableStream,
  flushLeadingCellEmit,
  makeHarness,
  STREAM_A,
  trackKeeper,
} from "./terminal-stream-state-harness.ts";

afterEach(cleanupStreamHarnesses);

describe("worker terminal snapshot cursor", () => {
  test("resumes the exact chunk and promotes an oversized dirty delta to a second full", async () => {
    trackKeeper(installAutoKeeper({ cols: 256, rows: 256 }));
    const core = new DenseLinkedCore();
    let blockSecondPart = true;
    const delivered: PbCellGridChunk[] = [];
    const harness = await makeHarness(core as unknown as TerminalCore, {
      sendChunk: (chunk) => {
        if (blockSecondPart && chunk.chunkIndex === 1) return "dropped";
        delivered.push(chunk);
        return "sent";
      },
    });

    const result = await enableStream(harness.manager, STREAM_A, 256, 256);
    expect(result).toMatchObject({
      status: "committed",
      streamId: STREAM_A,
      resized: false,
    });
    expect(harness.frameAttempts).toHaveLength(0);
    expect(harness.chunkAttempts.length).toBe(2);
    expect(harness.chunkAttempts.map((chunk) => chunk.chunkIndex)).toEqual([0, 1]);
    expect(harness.chunkAttempts[0]!.chunkCount).toBeGreaterThan(1);
    expect(delivered.map((chunk) => chunk.chunkIndex)).toEqual([0]);
    expect(harness.manager.terminalStreams.get(CHANNEL_ID)).toMatchObject({
      baselineReady: false,
    });

    core.markAllDirty();
    harness.manager.emitCellFrame(CHANNEL_ID, false);
    expect(harness.chunkAttempts).toHaveLength(2);

    const firstSnapshotId = harness.chunkAttempts[0]!.snapshotId;
    blockSecondPart = false;
    harness.manager.resumeTerminalSnapshots();
    await flushLeadingCellEmit();

    expect(
      harness.chunkAttempts.filter(
        (chunk) => chunk.snapshotId === firstSnapshotId && chunk.chunkIndex === 1,
      ),
    ).toHaveLength(2);
    expect(harness.frameAttempts).toHaveLength(0);
    const bySnapshot = new Map<string, PbCellGridChunk[]>();
    for (const chunk of delivered) {
      const group = bySnapshot.get(chunk.snapshotId) ?? [];
      group.push(chunk);
      bySnapshot.set(chunk.snapshotId, group);
      expect(encodedCellGridChunkSize(chunk)).toBeLessThanOrEqual(CELL_GRID_PART_MAX_BYTES);
      expect(chunk.part).toMatchObject({
        full: true,
        streamId: STREAM_A,
        baseSeq: 0n,
      });
    }
    expect(bySnapshot.size).toBe(2);
    const groups = [...bySnapshot.values()];
    for (const group of groups) {
      expect(group.map((chunk) => chunk.chunkIndex)).toEqual(
        Array.from({ length: group[0]!.chunkCount }, (_, index) => index),
      );
      expect(group).toHaveLength(group[0]!.chunkCount);
    }
    expect(groups[0]![0]!.part!.seq).toBe(1n);
    expect(groups[1]![0]!.part!.seq).toBe(2n);
    expect(groups[1]![0]!.snapshotId).not.toBe(groups[0]![0]!.snapshotId);
    expect(harness.manager.terminalStreams.get(CHANNEL_ID)).toMatchObject({
      baselineReady: true,
      baselineDirty: false,
      snapshotCursor: null,
    });
  }, 30_000);
});
