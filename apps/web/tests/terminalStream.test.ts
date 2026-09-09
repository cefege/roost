// These tests cover baseline admission, scoped repair, and ACK ownership.
// The shared fixture records renderer output and coordinator commands per session.
// Generation and gap handling must remain deterministic across renewal timing.

import { describe, expect, test, vi } from "bun:test";
import {
  EPOCH_A,
  RecordingRenderer,
  SESSION_ID,
  STREAM_A,
  acceptView,
  cellFrameToProto,
  delta,
  full,
  latestViewCommand,
  dispatchTerminalCellFrameFrom,
  renderer,
  generationRecoveries,
  resyncCommands,
  row,
  setPageVisible,
  terminalStream,
  updateSyncState,
} from "./helpers/terminalStreamFixture.ts";

describe("per-session browser terminal replica", () => {
  test("requires a full baseline, admits only an exact delta, and latches one resync", () => {
    const view = terminalStream.createTerminalView(SESSION_ID);
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
    const view = terminalStream.createTerminalView(SESSION_ID);
    view.setViewport({ cols: 1, rows: 1 });
    const revision = latestViewCommand().value.revision as bigint;
    expect(terminalStream.terminalStreamDiagnosticSnapshot(SESSION_ID).view).toMatchObject({
      pending_ack_age_ms: 0,
      pending_ack_generation: {
        socketGeneration: 1,
        socketId: "socket-1",
        processEpoch: "process-1",
        domainGeneration: "11",
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

  test("ignores hidden and replaced-generation view ACK deadlines", () => {
    const hidden = terminalStream.createTerminalView(SESSION_ID);
    hidden.setViewport({ cols: 1, rows: 1 });
    setPageVisible(false);
    vi.advanceTimersByTime(15_000);
    expect(generationRecoveries).toHaveLength(0);
    hidden.dispose();

    setPageVisible(true);
    const replaced = terminalStream.createTerminalView(SESSION_ID);
    replaced.setViewport({ cols: 1, rows: 1 });
    updateSyncState({
      socketGeneration: 1,
      socketId: "socket-1",
      processEpoch: "process-1",
      domainGeneration: 12n,
      ready: false,
    });
    vi.advanceTimersByTime(15_000);
    expect(generationRecoveries).toHaveLength(0);
    replaced.dispose();
  });

  test("challenges an idle baseline at its absolute deadline and accepts a full proof", () => {
    const view = terminalStream.createTerminalView(SESSION_ID);
    view.setViewport({ cols: 1, rows: 1 });
    const revision = latestViewCommand().value.revision as bigint;
    acceptView(view.viewId, revision);
    terminalStream.dispatchTerminalCellFrame(cellFrameToProto(full(), SESSION_ID));

    for (let elapsed = 5_000; elapsed < 20_000; elapsed += 5_000) {
      vi.advanceTimersByTime(5_000);
      acceptView(view.viewId, revision);
    }
    vi.advanceTimersByTime(4_999);
    expect(resyncCommands()).toHaveLength(0);
    vi.advanceTimersByTime(1);
    expect(resyncCommands()).toHaveLength(1);
    terminalStream.dispatchTerminalCellFrame(cellFrameToProto(
      full(STREAM_A, [row(0, "P")], 2),
      SESSION_ID,
    ));
    vi.advanceTimersByTime(10_000);
    expect(generationRecoveries).toHaveLength(0);
    view.dispose();
  });

  test("keeps a scoped gap repair local when a later delta proves the lane", () => {
    const view = terminalStream.createTerminalView(SESSION_ID);
    view.setViewport({ cols: 1, rows: 1 });
    const revision = latestViewCommand().value.revision as bigint;
    acceptView(view.viewId, revision);
    terminalStream.dispatchTerminalCellFrame(cellFrameToProto(full(), SESSION_ID));
    terminalStream.dispatchTerminalCellFrame(cellFrameToProto(
      delta(3, "gap", STREAM_A, 2),
      SESSION_ID,
    ));
    expect(resyncCommands()).toHaveLength(1);
    terminalStream.dispatchTerminalCellFrame(cellFrameToProto(
      delta(2, "L", STREAM_A, 1),
      SESSION_ID,
    ));
    vi.advanceTimersByTime(10_000);
    expect(generationRecoveries).toHaveLength(0);
    expect(terminalStream.terminalStreamDiagnosticSnapshot(SESSION_ID).replica).toMatchObject({
      resync_latched: true,
      challenge_age_ms: null,
      seq: 2,
    });
    view.dispose();
  });

  test("rejects a stale same-socket domain frame as terminal proof", () => {
    const view = terminalStream.createTerminalView(SESSION_ID);
    view.setViewport({ cols: 1, rows: 1 });
    const revision = latestViewCommand().value.revision as bigint;
    acceptView(view.viewId, revision);
    terminalStream.dispatchTerminalCellFrame(cellFrameToProto(full(), SESSION_ID));

    updateSyncState({
      socketGeneration: 1,
      socketId: "socket-1",
      processEpoch: "process-1",
      domainGeneration: 12n,
      ready: true,
    });
    acceptView(view.viewId, revision);
    dispatchTerminalCellFrameFrom({
      socketGeneration: 1,
      socketId: "socket-1",
      processEpoch: "process-1",
      domainGeneration: 11n,
    }, cellFrameToProto(full(STREAM_A, [row(0, "S")], 2), SESSION_ID));

    expect(terminalStream.terminalStreamDiagnosticSnapshot(SESSION_ID).replica).toMatchObject({
      baseline_ready: false,
      last_terminal_proof_age_ms: null,
    });
    view.dispose();
  });

  test("requests a missing initial full baseline at the first document renewal", () => {
    const view = terminalStream.createTerminalView(SESSION_ID);
    view.setViewport({ cols: 1, rows: 1 });
    acceptView(view.viewId, latestViewCommand().value.revision as bigint);
    expect(resyncCommands()).toHaveLength(0);
    vi.advanceTimersByTime(5_000);
    expect(resyncCommands()).toHaveLength(1);
    view.dispose();
  });

  test("repeats an unanswered scoped repair at its proof deadline", () => {
    const view = terminalStream.createTerminalView(SESSION_ID);
    view.setViewport({ cols: 1, rows: 1 });
    acceptView(view.viewId, latestViewCommand().value.revision as bigint);

    vi.advanceTimersByTime(5_000);
    expect(resyncCommands()).toHaveLength(1);
    vi.advanceTimersByTime(9_999);
    expect(resyncCommands()).toHaveLength(1);
    vi.advanceTimersByTime(1);
    expect(resyncCommands()).toHaveLength(2);
    expect(generationRecoveries).toHaveLength(0);
    view.dispose();
  });

  test("deduplicates renewal repairs across handles for one session", () => {
    const first = terminalStream.createTerminalView(SESSION_ID);
    first.setViewport({ cols: 1, rows: 1 });
    acceptView(first.viewId, latestViewCommand().value.revision as bigint);
    const second = terminalStream.createTerminalView(SESSION_ID);
    second.setViewport({ cols: 1, rows: 1 });
    acceptView(second.viewId, latestViewCommand().value.revision as bigint);

    vi.advanceTimersByTime(5_000);
    expect(resyncCommands()).toHaveLength(1);
    vi.advanceTimersByTime(10_000);
    expect(resyncCommands()).toHaveLength(2);
    first.dispose();
    second.dispose();
  });

  test("stops scoped repair retries after accepting a full baseline", () => {
    const view = terminalStream.createTerminalView(SESSION_ID);
    const sink = new RecordingRenderer();
    view.subscribeRenderer(renderer(sink));
    view.setViewport({ cols: 1, rows: 1 });
    acceptView(view.viewId, latestViewCommand().value.revision as bigint);

    vi.advanceTimersByTime(5_000);
    expect(resyncCommands()).toHaveLength(1);
    terminalStream.dispatchTerminalCellFrame(cellFrameToProto(full(), SESSION_ID));
    expect(sink.fullFrames).toHaveLength(1);
    vi.advanceTimersByTime(10_000);
    expect(resyncCommands()).toHaveLength(1);
    view.dispose();
  });
});
