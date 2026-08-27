import { afterEach, describe, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import type { TerminalCore } from "@wterm/core";
import {
  PbCellGridFrameSchema,
  PbCellRowSchema,
  type PbCellGridChunk,
  type PbCellGridFrame,
} from "@roost/shared/proto/cell_pb";
import { installSnapshotCursor } from "../src/session-snapshot-cursor.ts";
import {
  CELL_GRID_PART_MAX_BYTES,
  encodedCellGridChunkSize,
} from "@roost/shared/cell";
import type { TerminalCellSendResult } from "../src/transport/coord-link-types.ts";
import type { SessionManager } from "../src/session-manager.ts";
import type { TerminalStreamState } from "../src/session-terminal-state.ts";
import { applyResizeResultAtBoundary, installLiveResizeCapture } from "../src/session-resize-capture.ts";
import { installAutoKeeper } from "./keeper-fake-pool.ts";
import {
  CHANNEL_ID,
  cleanupStreamHarnesses,
  DenseLinkedCore,
  enableStream,
  STREAM_B,
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

    const resultPromise = enableStream(harness.manager, STREAM_A, 256, 256);
    await flushLeadingCellEmit();
    expect(harness.frameAttempts).toHaveLength(0);
    expect(harness.chunkAttempts).toHaveLength(2);
    expect(harness.chunkAttempts.map((chunk) => chunk.chunkIndex)).toEqual([0, 1]);
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
    const result = await resultPromise;

    expect(result).toMatchObject({
      status: "committed",
      streamId: STREAM_A,
      resized: false,
    });
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

  test("retires a blocked baseline before stream replacement and sends no retired part", async () => {
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

    const firstOperation = enableStream(harness.manager, STREAM_A, 256, 256);
    await flushLeadingCellEmit();
    const oldState = harness.manager.terminalStreams.get(CHANNEL_ID)!;
    const oldBaseline = oldState.baselineInstalled;
    expect(oldState.snapshotCursor?.nextPart).toBe(1);

    const replacement = enableStream(harness.manager, STREAM_B, 256, 256);
    blockSecondPart = false;
    await expect(oldBaseline).resolves.toBe(false);
    await expect(firstOperation).resolves.toMatchObject({
      status: "ambiguous",
      failure: "core_failed",
    });
    expect(oldState.snapshotCursor).toBeNull();
    await expect(replacement).resolves.toMatchObject({
      status: "committed",
      streamId: STREAM_B,
    });
    expect(delivered.filter((chunk) => chunk.part?.streamId === STREAM_A)
      .map((chunk) => chunk.chunkIndex)).toEqual([0]);
  });

  test("settles the blocked baseline false when reconnect retires its cursor", async () => {
    trackKeeper(installAutoKeeper({ cols: 256, rows: 256 }));
    const core = new DenseLinkedCore();
    const delivered: PbCellGridChunk[] = [];
    const harness = await makeHarness(core as unknown as TerminalCore, {
      sendChunk: (chunk) => {
        if (chunk.chunkIndex === 1) return "dropped";
        delivered.push(chunk);
        return "sent";
      },
    });

    const operation = enableStream(harness.manager, STREAM_A, 256, 256);
    await flushLeadingCellEmit();
    const oldState = harness.manager.terminalStreams.get(CHANNEL_ID)!;
    const oldBaseline = oldState.baselineInstalled;
    harness.manager.invalidateTerminalStreamsForReconnect();
    await expect(oldBaseline).resolves.toBe(false);
    await expect(operation).resolves.toMatchObject({
      status: "ambiguous",
      failure: "core_failed",
    });
    expect(oldState.snapshotCursor).toBeNull();
    await flushLeadingCellEmit();
    expect(delivered.filter((chunk) => chunk.part?.streamId === STREAM_A)
      .map((chunk) => chunk.chunkIndex)).toEqual([0]);
    expect(harness.manager.terminalStreams.get(CHANNEL_ID)).toMatchObject({
      baselineReady: true,
      snapshotCursor: null,
    });
  });

  test("settles the blocked baseline false on core failure without sending its tail", async () => {
    trackKeeper(installAutoKeeper({ cols: 256, rows: 256 }));
    const core = new DenseLinkedCore();
    const delivered: PbCellGridChunk[] = [];
    const harness = await makeHarness(core as unknown as TerminalCore, {
      sendChunk: (chunk) => {
        if (chunk.chunkIndex === 1) return "dropped";
        delivered.push(chunk);
        return "sent";
      },
    });

    const operation = enableStream(harness.manager, STREAM_A, 256, 256);
    await flushLeadingCellEmit();
    const state = harness.manager.terminalStreams.get(CHANNEL_ID)!;
    const baseline = state.baselineInstalled;
    const capture = installLiveResizeCapture(
      harness.manager,
      CHANNEL_ID,
      state,
      1,
      256,
      256,
      256,
      256,
    );
    Object.defineProperty(capture, "installSeq", { value: 1 });
    applyResizeResultAtBoundary(harness.manager, CHANNEL_ID, capture, {
      kind: "ack",
      seq: 1,
      cols: 256,
      rows: 256,
    });
    await expect(baseline).resolves.toBe(false);
    await expect(operation).resolves.toMatchObject({
      status: "ambiguous",
      failure: "core_failed",
    });
    expect(state.snapshotCursor).toBeNull();
    expect(state.coreValid).toBe(false);
    await flushLeadingCellEmit();
    expect(delivered.filter((chunk) => chunk.part?.streamId === STREAM_A)
      .map((chunk) => chunk.chunkIndex)).toEqual([0]);
  });

  test("settles the blocked baseline false during channel teardown", async () => {
    trackKeeper(installAutoKeeper({ cols: 256, rows: 256 }));
    const core = new DenseLinkedCore();
    const delivered: PbCellGridChunk[] = [];
    const harness = await makeHarness(core as unknown as TerminalCore, {
      sendChunk: (chunk) => {
        if (chunk.chunkIndex === 1) return "dropped";
        delivered.push(chunk);
        return "sent";
      },
    });

    const operation = enableStream(harness.manager, STREAM_A, 256, 256);
    await flushLeadingCellEmit();
    const oldState = harness.manager.terminalStreams.get(CHANNEL_ID)!;
    const oldBaseline = oldState.baselineInstalled;
    harness.manager._dropChannelState(CHANNEL_ID);
    await expect(oldBaseline).resolves.toBe(false);
    await expect(operation).resolves.toMatchObject({
      status: "ambiguous",
      failure: "core_failed",
    });
    expect(oldState.snapshotCursor).toBeNull();
    await flushLeadingCellEmit();
    expect(delivered.filter((chunk) => chunk.part?.streamId === STREAM_A)
      .map((chunk) => chunk.chunkIndex)).toEqual([0]);
    expect(harness.manager.terminalStreams.has(CHANNEL_ID)).toBe(false);
  });
  test("does not advance a snapshot cursor on queued cell admission", () => {
    const baseline = Promise.withResolvers<boolean>();
    const state: TerminalStreamState = {
      streamId: STREAM_A,
      enabled: true,
      cols: 2,
      rows: 2,
      version: 1,
      baselineReady: false,
      baselinePromisePending: true,
      coreValid: true,
      baselineDirty: false,
      snapshotCursor: null,
      resizeCapture: null,
      baselineInstalled: baseline.promise,
      resolveBaselineInstalled: baseline.resolve,
    };
    const queuedCellResult = "queued" as unknown as TerminalCellSendResult;
    const manager = {
      terminalStreams: new Map([[CHANNEL_ID, state]]),
      sendCellGridUpstream: () => queuedCellResult,
      sendCellGridChunkUpstream: () => queuedCellResult,
    } as unknown as SessionManager;
    const frame = create(PbCellGridFrameSchema, {
      sessionId: "worker-test",
      streamId: STREAM_A,
      gridEpoch: "grid:queued",
      cols: 2,
      rows: 2,
      full: true,
      viewportRows: [
        create(PbCellRowSchema, { index: 0 }),
        create(PbCellRowSchema, { index: 1 }),
      ],
      seq: 1n,
      baseSeq: 0n,
      sbBase: 0n,
      scrollbackTotal: 0n,
    });
    expect(installSnapshotCursor(manager, CHANNEL_ID, state, frame)).toBe(true);
    expect(state.snapshotCursor).toMatchObject({ nextPart: 0 });
    expect(state.baselineReady).toBe(false);
    expect(state.baselinePromisePending).toBe(true);
  });
});
