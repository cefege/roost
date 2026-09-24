// These tests cover scoped repair, liveness watchdogs, and ACK ownership.
// The shared fixture records renderer output and coordinator commands per session.
// Generation and gap handling must remain deterministic across renewal timing.

import { describe, expect, test, vi } from "bun:test";
import {
  TERMINAL_FOREGROUND_IDLE_PROBE_MS,
  TERMINAL_FOREGROUND_PROBE_DEADLINE_MS,
  TERMINAL_VIEW_HEARTBEAT_MS,
} from "@roost/protocol/viewport";
import {
  CELL_GRID_CHUNK_STALL_MS,
  CURRENT_SYNC_OWNER,
  EPOCH_A,
  RecordingRenderer,
  SESSION_ID,
  SNAPSHOT_A,
  STREAM_A,
  WORKER_FP,
  acceptView,
  cellFrameToProto,
  chunkCellGridFrame,
  delta,
  full,
  generationRecoveries,
  latestViewCommand,
  renderer,
  resyncCommands,
  row,
  setPageVisible,
  terminalStream,
  updateSyncState,
} from "./helpers/terminalStreamFixture.ts";

function chunkedProofBaseline() {
  const linkUri = `https://example.invalid/${"u".repeat(2_000)}`;
  const rows = Array.from({ length: 256 }, (_, rowIndex) => ({
    index: rowIndex,
    spans: [85, 85, 86].map((columns) => ({
      text: "x".repeat(columns), columns, fg: 256, bg: 256, flags: 0, linkKey: "proof-link", linkUri,
    })),
  }));
  const chunks = chunkCellGridFrame(
    cellFrameToProto(full(STREAM_A, rows, 2), SESSION_ID),
    SNAPSHOT_A,
  );
  if (chunks.length < 2) throw new Error("proof fixture must split into chunks");
  return chunks;
}

describe("per-session browser terminal replica", () => {
  test("requires a full baseline, admits only an exact delta, and latches one resync", () => {
    const view = terminalStream.createTerminalView(SESSION_ID, WORKER_FP);
    const sink = new RecordingRenderer();
    view.subscribeRenderer(renderer(sink));
    view.setViewport({ cols: 1, rows: 1 });
    const revision = latestViewCommand().value.revision as bigint;
    acceptView(view.viewId, revision);
    terminalStream.dispatchTerminalCellFrame(cellFrameToProto(delta(2, "gap"), SESSION_ID));
    terminalStream.dispatchTerminalCellFrame(cellFrameToProto(delta(3, "gap-2", STREAM_A, 2), SESSION_ID));
    expect(sink.deltaFrames).toHaveLength(0);
    expect(resyncCommands()).toHaveLength(1);

    terminalStream.dispatchTerminalCellFrame(cellFrameToProto(full(), SESSION_ID));
    expect(sink.fullFrames).toHaveLength(1);
    expect(sink.fullFrames[0]!.viewportRows[0]!.spans[0]!.text).toBe("A");
    expect(terminalStream.terminalStreamDiagnosticSnapshot(SESSION_ID).replica).toMatchObject({
      expected_stream_id: STREAM_A,
      grid_epoch: EPOCH_A,
      seq: 1,
      baseline_ready: true,
      resync_latched: false,
    });

    terminalStream.dispatchTerminalCellFrame(cellFrameToProto(delta(2, "B"), SESSION_ID));
    expect(sink.deltaFrames).toHaveLength(1);
    expect(sink.deltaFrames[0]!.baseSeq).toBe(1);
    expect(terminalStream.terminalStreamDiagnosticSnapshot(SESSION_ID).replica.seq).toBe(2);

    terminalStream.dispatchTerminalCellFrame(cellFrameToProto(delta(4, "later-gap", STREAM_A, 3), SESSION_ID));
    terminalStream.dispatchTerminalCellFrame(cellFrameToProto(delta(5, "same-latch", STREAM_A, 4), SESSION_ID));
    expect(resyncCommands()).toHaveLength(2);

    terminalStream.dispatchTerminalCellFrame(cellFrameToProto(full(STREAM_A, [row(0, "R")], 9), SESSION_ID));
    expect(sink.fullFrames.at(-1)!.viewportRows[0]!.spans[0]!.text).toBe("R");
    expect(terminalStream.terminalStreamDiagnosticSnapshot(SESSION_ID).replica).toMatchObject({
      seq: 9,
      baseline_ready: true,
      resync_latched: false,
    });
  });

  test("owns ACK and terminal progress diagnostics by the complete generation", () => {
    const view = terminalStream.createTerminalView(SESSION_ID, WORKER_FP);
    view.setViewport({ cols: 1, rows: 1 });
    const revision = latestViewCommand().value.revision as bigint;
    expect(terminalStream.terminalStreamDiagnosticSnapshot(SESSION_ID).view).toMatchObject({
      pending_ack_age_ms: 0,
      pending_ack_generation: {
        socketGeneration: 1,
        socketId: "socket-1",
        processEpoch: "process-1",
        domainGeneration: "11",
        transportKind: "sync",
        workerFp: null,
      },
    });
    vi.advanceTimersByTime(5_000);
    expect(terminalStream.terminalStreamDiagnosticSnapshot(SESSION_ID).view.pending_ack_age_ms).toBe(5_000);

    acceptView(view.viewId, revision);
    expect(terminalStream.terminalStreamDiagnosticSnapshot(SESSION_ID).view.pending_ack_age_ms).toBeNull();
    terminalStream.dispatchTerminalCellFrame(cellFrameToProto(delta(2, "gap"), SESSION_ID));
    expect(terminalStream.terminalStreamDiagnosticSnapshot(SESSION_ID).replica).toMatchObject({
      resync_latched: true,
      resync_latch_age_ms: 0,
      repair_attempts: 1,
      repair_outcome: "requested",
    });
    terminalStream.dispatchTerminalCellFrame(cellFrameToProto(full(), SESSION_ID));
    vi.advanceTimersByTime(25);
    expect(terminalStream.terminalStreamDiagnosticSnapshot(SESSION_ID).replica).toMatchObject({
      last_terminal_proof_age_ms: 25,
      last_terminal_proof_generation: {
        socketGeneration: 1,
        socketId: "socket-1",
        processEpoch: "process-1",
        domainGeneration: "11",
        transportKind: "sync",
        workerFp: null,
      },
      challenge_age_ms: null,
      resync_latch_age_ms: null,
      repair_outcome: "proved",
    });

    updateSyncState({
      socketGeneration: 2,
      socketId: "socket-1",
      processEpoch: "process-1",
      domainGeneration: 11n,
      ready: true,
    });
    expect(terminalStream.terminalStreamDiagnosticSnapshot(SESSION_ID).replica).toMatchObject({
      last_terminal_proof_age_ms: null,
      resync_latch_age_ms: null,
      repair_attempts: 0,
      repair_outcome: "generation_reset",
    });
  });

  test("redials DOM reconciliation only for a ready current foreground view", () => {
    const view = terminalStream.createTerminalView(SESSION_ID, WORKER_FP);
    view.setViewport({ cols: 1, rows: 1 });
    setPageVisible(false);
    view.recoverUnreconciledDom();
    expect(generationRecoveries).toHaveLength(0);

    setPageVisible(true);
    updateSyncState({
      socketGeneration: 1,
      socketId: "socket-1",
      processEpoch: "process-1",
      domainGeneration: 11n,
      ready: false,
    });
    view.recoverUnreconciledDom();
    expect(generationRecoveries).toHaveLength(0);

    updateSyncState({
      socketGeneration: 1,
      socketId: "socket-1",
      processEpoch: "process-1",
      domainGeneration: 11n,
      ready: true,
    });
    view.recoverUnreconciledDom();
    expect(generationRecoveries).toHaveLength(1);
    expect(generationRecoveries.at(-1)?.reason).toBe("terminal-dom-reconcile-timeout");
    view.dispose();
  });

  test("accepts only a newer same-stream canonical checkpoint as source proof", () => {
    const view = terminalStream.createTerminalView(SESSION_ID, WORKER_FP);
    view.setViewport({ cols: 1, rows: 1 });
    acceptView(view.viewId, latestViewCommand().value.revision as bigint);
    terminalStream.dispatchTerminalCellFrame(cellFrameToProto(full(), SESSION_ID));

    vi.advanceTimersByTime(TERMINAL_FOREGROUND_IDLE_PROBE_MS);
    expect(resyncCommands()).toHaveLength(1);
    terminalStream.dispatchTerminalCellFrame(cellFrameToProto(delta(2, "P"), SESSION_ID));

    expect(terminalStream.terminalStreamDiagnosticSnapshot(SESSION_ID).replica).toMatchObject({
      challenge_stream_id: null,
      challenge_seq: null,
      repair_outcome: "proved",
    });
    vi.advanceTimersByTime(TERMINAL_FOREGROUND_PROBE_DEADLINE_MS);
    expect(generationRecoveries).toHaveLength(0);
    view.dispose();
  });

  test("rejects equal and stale same-stream checkpoints as source proof", () => {
    const view = terminalStream.createTerminalView(SESSION_ID, WORKER_FP);
    view.setViewport({ cols: 1, rows: 1 });
    acceptView(view.viewId, latestViewCommand().value.revision as bigint);
    terminalStream.dispatchTerminalCellFrame(cellFrameToProto(full(STREAM_A, [row(0, "A")], 2), SESSION_ID));

    vi.advanceTimersByTime(TERMINAL_FOREGROUND_IDLE_PROBE_MS);
    vi.advanceTimersByTime(2_000);
    const equalChunk = chunkCellGridFrame(
      cellFrameToProto(full(STREAM_A, [row(0, "A")], 2), SESSION_ID),
      SNAPSHOT_A,
    )[0]!;
    terminalStream.dispatchTerminalCellChunk(equalChunk);
    terminalStream.dispatchTerminalCellFrame(cellFrameToProto(
      full(STREAM_A, [row(0, "S")], 1),
      SESSION_ID,
    ));
    expect(terminalStream.terminalStreamDiagnosticSnapshot(SESSION_ID).replica).toMatchObject({
      challenge_stream_id: STREAM_A,
      challenge_seq: 2,
    });
    vi.advanceTimersByTime(TERMINAL_FOREGROUND_PROBE_DEADLINE_MS - 2_000);
    expect(generationRecoveries.at(-1)?.reason).toBe("terminal-proof-timeout");
    view.dispose();
  });

  test("suspends source proof deadline while a newer chunked baseline progresses", () => {
    const view = terminalStream.createTerminalView(SESSION_ID, WORKER_FP);
    const progress: number[] = [];
    view.subscribeProgress((value) => {
      if (value) progress.push(value.receivedChunks);
    });
    view.setViewport({ cols: 256, rows: 256 });
    acceptView(view.viewId, latestViewCommand().value.revision as bigint, STREAM_A, 256, 256);
    const baselineRows = Array.from(
      { length: 256 },
      (_, rowIndex) => row(rowIndex, "x".repeat(256)),
    );
    terminalStream.dispatchTerminalCellFrame(cellFrameToProto(
      full(STREAM_A, baselineRows, 1),
      SESSION_ID,
    ));

    vi.advanceTimersByTime(TERMINAL_FOREGROUND_IDLE_PROBE_MS);
    const chunks = chunkedProofBaseline();
    terminalStream.dispatchTerminalCellChunk(chunks[0]!);
    expect(progress).toEqual([1]);
    vi.advanceTimersByTime(TERMINAL_FOREGROUND_PROBE_DEADLINE_MS);
    expect(generationRecoveries).toHaveLength(0);
    for (const chunk of chunks.slice(1)) terminalStream.dispatchTerminalCellChunk(chunk);
    vi.advanceTimersByTime(TERMINAL_FOREGROUND_PROBE_DEADLINE_MS);
    expect(generationRecoveries).toHaveLength(0);
    view.dispose();
  });

  test("rearms a fresh proof deadline after a stalled chunk transfer", () => {
    const view = terminalStream.createTerminalView(SESSION_ID, WORKER_FP);
    view.setViewport({ cols: 256, rows: 256 });
    acceptView(view.viewId, latestViewCommand().value.revision as bigint, STREAM_A, 256, 256);
    const baselineRows = Array.from(
      { length: 256 },
      (_, rowIndex) => row(rowIndex, "x".repeat(256)),
    );
    terminalStream.dispatchTerminalCellFrame(cellFrameToProto(
      full(STREAM_A, baselineRows, 1),
      SESSION_ID,
    ));

    vi.advanceTimersByTime(TERMINAL_FOREGROUND_IDLE_PROBE_MS);
    const chunks = chunkedProofBaseline();
    let nowMs = 1_000_000;
    const nowSpy = vi.spyOn(Date, "now").mockImplementation(() => nowMs);
    try {
      terminalStream.dispatchTerminalCellChunk(chunks[0]!);
      nowMs += CELL_GRID_CHUNK_STALL_MS;
      vi.advanceTimersByTime(CELL_GRID_CHUNK_STALL_MS);
    } finally {
      nowSpy.mockRestore();
    }
    expect(resyncCommands()).toHaveLength(2);
    vi.advanceTimersByTime(TERMINAL_FOREGROUND_PROBE_DEADLINE_MS - 1);
    expect(generationRecoveries).toHaveLength(0);
    vi.advanceTimersByTime(1);
    expect(generationRecoveries.at(-1)?.reason).toBe("terminal-proof-timeout");
    view.dispose();
  });

  test("coalesces same-generation repairs across concurrent view renewals", () => {
    const first = terminalStream.createTerminalView(SESSION_ID, WORKER_FP);
    first.setViewport({ cols: 1, rows: 1 });
    acceptView(first.viewId, latestViewCommand().value.revision as bigint);
    const second = terminalStream.createTerminalView(SESSION_ID, WORKER_FP);
    second.setViewport({ cols: 1, rows: 1 });
    acceptView(second.viewId, latestViewCommand().value.revision as bigint);

    vi.advanceTimersByTime(TERMINAL_FOREGROUND_IDLE_PROBE_MS);
    expect(resyncCommands()).toHaveLength(1);
    vi.advanceTimersByTime(TERMINAL_FOREGROUND_PROBE_DEADLINE_MS - 1);
    expect(generationRecoveries).toHaveLength(0);
    vi.advanceTimersByTime(1);
    expect(generationRecoveries).toHaveLength(1);
    first.dispose();
    second.dispose();
  });

  test("re-arms the idle probe when a liveness challenge cannot be published", () => {
    const view = terminalStream.createTerminalView(SESSION_ID, WORKER_FP);
    view.setViewport({ cols: 1, rows: 1 });
    acceptView(view.viewId, latestViewCommand().value.revision as bigint);
    terminalStream.dispatchTerminalCellFrame(cellFrameToProto(full(), SESSION_ID));
    updateSyncState({ ...CURRENT_SYNC_OWNER, ready: false });

    vi.advanceTimersByTime(TERMINAL_FOREGROUND_IDLE_PROBE_MS);
    expect(resyncCommands()).toHaveLength(0);
    expect(terminalStream.terminalStreamDiagnosticSnapshot(SESSION_ID).replica).toMatchObject({
      challenge_stream_id: null,
      repair_attempts: 0,
    });
    vi.advanceTimersByTime(TERMINAL_FOREGROUND_IDLE_PROBE_MS);
    expect(resyncCommands()).toHaveLength(0);

    updateSyncState({ ...CURRENT_SYNC_OWNER, ready: true });
    vi.advanceTimersByTime(TERMINAL_FOREGROUND_IDLE_PROBE_MS - 1);
    expect(resyncCommands()).toHaveLength(0);
    vi.advanceTimersByTime(1);
    expect(resyncCommands()).toHaveLength(1);
    expect(terminalStream.terminalStreamDiagnosticSnapshot(SESSION_ID).replica).toMatchObject({
      challenge_stream_id: STREAM_A,
      repair_outcome: "requested",
    });
    vi.advanceTimersByTime(TERMINAL_FOREGROUND_PROBE_DEADLINE_MS);
    expect(generationRecoveries.at(-1)?.reason).toBe("terminal-proof-timeout");
    view.dispose();
  });

  test("arms a proof deadline for a latch whose delta cleared its latch timestamp", () => {
    const view = terminalStream.createTerminalView(SESSION_ID, WORKER_FP);
    view.setViewport({ cols: 1, rows: 1 });
    const revision = latestViewCommand().value.revision as bigint;
    acceptView(view.viewId, revision);
    let nowMs = 1_000_000;
    const nowSpy = vi.spyOn(Date, "now").mockImplementation(() => nowMs);
    try {
      terminalStream.dispatchTerminalCellFrame(cellFrameToProto(full(), SESSION_ID));
      terminalStream.dispatchTerminalCellFrame(
        cellFrameToProto(delta(3, "gap", STREAM_A, 2), SESSION_ID),
      );
      terminalStream.dispatchTerminalCellFrame(cellFrameToProto(delta(2, "B"), SESSION_ID));
      expect(terminalStream.terminalStreamDiagnosticSnapshot(SESSION_ID).replica).toMatchObject({
        resync_latched: true,
        resync_latch_age_ms: null,
        challenge_stream_id: null,
        repair_outcome: "proved",
      });

      nowMs += TERMINAL_VIEW_HEARTBEAT_MS;
      acceptView(view.viewId, revision);
    } finally {
      nowSpy.mockRestore();
    }
    expect(resyncCommands()).toHaveLength(2);
    expect(terminalStream.terminalStreamDiagnosticSnapshot(SESSION_ID).replica).toMatchObject({
      challenge_stream_id: STREAM_A,
      challenge_seq: 2,
      repair_outcome: "requested",
    });
    vi.advanceTimersByTime(TERMINAL_FOREGROUND_PROBE_DEADLINE_MS);
    expect(generationRecoveries.at(-1)?.reason).toBe("terminal-proof-timeout");
    view.dispose();
  });
});
