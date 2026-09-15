import { afterEach, describe, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import type { TerminalCore } from "@wterm/core";
import {
  PbCellGridFrameSchema,
  PbCellRowSchema,
  type PbCellGridChunk,
  type PbCellGridFrame,
} from "@roost/shared/proto/cell_pb";
import {
  installSnapshotCursor,
  prepareCellRenewalEpoch,
} from "../src/session-snapshot-cursor.ts";
import {
  CELL_GRID_PART_MAX_BYTES,
  encodedCellGridChunkSize,
  initCellEmitState,
} from "@roost/shared/cell";
import type { SessionManager } from "../src/session-manager.ts";
import type { TerminalStreamState } from "../src/session-terminal-state.ts";
import type { TerminalCellSendResult } from "../src/transport/coord-link-types.ts";
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

function renewalCore(options: {
  cols?: number;
  rows?: number;
  alt?: boolean;
  scrollbackCount?: number;
  discarded?: number;
} = {}): TerminalCore {
  return {
    getCols: () => options.cols ?? 80,
    getRows: () => options.rows ?? 24,
    usingAltScreen: () => options.alt ?? false,
    getScrollbackCount: () => options.scrollbackCount ?? 15,
    getScrollbackDiscardedCount: () => options.discarded ?? 2,
  } as unknown as TerminalCore;
}

function renewalEmitState() {
  return {
    ...initCellEmitState("renewal-grid", STREAM_A),
    gridEpochRevision: 7,
    sentFull: true,
    cols: 80,
    rows: 24,
    alt: false,
    lastSbTotal: 20,
    sbOrigin: 3,
    sbDropped: 5,
  };
}

describe("worker terminal snapshot cursor", () => {
  test("preserves only compatible renewal epochs", () => {
    const compatible = renewalEmitState();
    prepareCellRenewalEpoch(renewalCore(), compatible);
    expect(compatible.gridEpochRevision).toBe(7);

    for (const core of [
      renewalCore({ cols: 81 }),
      renewalCore({ alt: true }),
      renewalCore({ discarded: 18 }),
      renewalCore({ scrollbackCount: 14 }),
    ]) {
      const incompatible = renewalEmitState();
      prepareCellRenewalEpoch(core, incompatible);
      expect(incompatible.gridEpochRevision).toBe(8);
    }
  });

  test("commits while a blocked full later resumes and extends the stream", async () => {
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
    const result = await resultPromise;
    expect(result).toMatchObject({
      status: "committed",
      streamId: STREAM_A,
      resized: false,
    });
    expect(harness.frameAttempts).toHaveLength(0);
    expect(harness.chunkAttempts).toHaveLength(2);
    expect(harness.chunkAttempts.map((chunk) => chunk.chunkIndex)).toEqual([0, 1]);
    expect(delivered.map((chunk) => chunk.chunkIndex)).toEqual([0]);
    expect(harness.manager.terminalStreams.get(CHANNEL_ID)).toMatchObject({
      baselineReady: false,
      coreValid: true,
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
    expect(oldState.snapshotCursor?.nextPart).toBe(1);
    await expect(firstOperation).resolves.toMatchObject({
      status: "committed",
      streamId: STREAM_A,
    });

    const replacement = enableStream(harness.manager, STREAM_B, 256, 256);
    blockSecondPart = false;
    expect(oldState.snapshotCursor).toBeNull();
    await expect(replacement).resolves.toMatchObject({
      status: "committed",
      streamId: STREAM_B,
    });
    expect(delivered.filter((chunk) => chunk.part?.streamId === STREAM_A)
      .map((chunk) => chunk.chunkIndex)).toEqual([0]);
  });

  test("retires a blocked baseline on reconnect without sending its tail", async () => {
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
    await expect(operation).resolves.toMatchObject({
      status: "committed",
      streamId: STREAM_A,
    });
    harness.manager.invalidateTerminalStreamsForReconnect();
    expect(oldState.snapshotCursor).toBeNull();
    await flushLeadingCellEmit();
    expect(delivered.filter((chunk) => chunk.part?.streamId === STREAM_A)
      .map((chunk) => chunk.chunkIndex)).toEqual([0]);
    expect(harness.manager.terminalStreams.get(CHANNEL_ID)).toMatchObject({
      baselineReady: true,
      snapshotCursor: null,
    });
  });

  test("retires a blocked baseline on core failure without sending its tail", async () => {
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
    await expect(operation).resolves.toMatchObject({
      status: "committed",
      streamId: STREAM_A,
    });
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
    expect(state.snapshotCursor).toBeNull();
    expect(state.coreValid).toBe(false);
    await flushLeadingCellEmit();
    expect(delivered.filter((chunk) => chunk.part?.streamId === STREAM_A)
      .map((chunk) => chunk.chunkIndex)).toEqual([0]);
  });

  test("retires a blocked baseline during channel teardown", async () => {
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
    await expect(operation).resolves.toMatchObject({
      status: "committed",
      streamId: STREAM_A,
    });
    harness.manager._dropChannelState(CHANNEL_ID);
    expect(oldState.snapshotCursor).toBeNull();
    await flushLeadingCellEmit();
    expect(delivered.filter((chunk) => chunk.part?.streamId === STREAM_A)
      .map((chunk) => chunk.chunkIndex)).toEqual([0]);
    expect(harness.manager.terminalStreams.has(CHANNEL_ID)).toBe(false);
  });
  test("does not advance a snapshot cursor on queued cell admission", () => {
    const state: TerminalStreamState = {
      streamId: STREAM_A,
      enabled: true,
      cols: 2,
      rows: 2,
      version: 1,
      baselineReady: false,
      coreValid: true,
      baselineDirty: false,
      snapshotCursor: null,
      resizeCapture: null,
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
  });
});
