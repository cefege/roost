// These tests cover renderer lifecycle, generation rollover, and the view
// commands one pane publishes over its transport.
// They share the terminal-stream fixture so each replica begins from the same reset state.
// The cases protect atomic chunk assembly, view fencing, and renewal liveness
// after a publication that never reached a socket.

import { describe, expect, setSystemTime, test, vi } from "bun:test";
import {
  CELL_GRID_CHUNK_STALL_MS,
  RecordingRenderer,
  SESSION_ID,
  SNAPSHOT_A,
  STREAM_A,
  STREAM_B,
  WORKER_FP,
  acceptView,
  cellFrameToProto,
  chunkCellGridFrame,
  delta,
  full,
  latestViewCommand,
  rejectView,
  renderer,
  resyncCommands,
  row,
  terminalStream,
  updateSyncState,
  viewCommands,
  type TerminalViewHandleStatus,
} from "./helpers/terminalStreamFixture.ts";
import { terminalSessions } from "../src/store/terminal-stream-state.ts";

describe("per-session browser terminal replica", () => {
  test("keeps subscriber row shells independent from each other and the canonical replica", () => {
    const view = terminalStream.createTerminalView(SESSION_ID, WORKER_FP);
    const mutating = new RecordingRenderer();
    mutating.mutateRows = true;
    const observing = new RecordingRenderer();
    view.subscribeRenderer(renderer(mutating));
    view.subscribeRenderer(renderer(observing));
    view.setViewport({ cols: 1, rows: 1 });
    acceptView(view.viewId, latestViewCommand().value.revision as bigint);

    terminalStream.dispatchTerminalCellFrame(cellFrameToProto(full(), SESSION_ID));
    expect(mutating.fullFrames[0]!.viewportRows[0]!.index).toBe(99);
    expect(observing.fullFrames[0]!.viewportRows[0]!.index).toBe(0);

    terminalStream.dispatchTerminalCellFrame(cellFrameToProto(delta(2, "B"), SESSION_ID));
    expect(mutating.deltaFrames[0]!.viewportRows[0]!.index).toBe(99);
    expect(observing.deltaFrames[0]!.viewportRows[0]!.index).toBe(0);

    const late = new RecordingRenderer();
    view.subscribeRenderer(renderer(late));
    expect(late.fullFrames).toHaveLength(1);
    expect(late.fullFrames[0]!.viewportRows[0]!.index).toBe(0);
    expect(late.fullFrames[0]!.viewportRows[0]!.spans[0]!.text).toBe("B");
    expect(late.fullFrames[0]!.full).toBe(true);
    expect(late.fullFrames[0]!.baseSeq).toBe(0);
  });
  test("survives renderer detach and evicts only after the final handle", () => {
    const firstView = terminalStream.createTerminalView(SESSION_ID, WORKER_FP);
    const firstRenderer = new RecordingRenderer();
    const detach = firstView.subscribeRenderer(renderer(firstRenderer));
    firstView.setViewport({ cols: 1, rows: 1 });
    acceptView(firstView.viewId, latestViewCommand().value.revision as bigint);
    terminalStream.dispatchTerminalCellFrame(cellFrameToProto(full(), SESSION_ID));
    detach();

    terminalStream.dispatchTerminalCellFrame(cellFrameToProto(delta(2, "D"), SESSION_ID));
    expect(firstRenderer.deltaFrames).toHaveLength(0);
    const reattached = new RecordingRenderer();
    firstView.subscribeRenderer(renderer(reattached));
    expect(reattached.fullFrames[0]!.viewportRows[0]!.spans[0]!.text).toBe("D");
    expect(reattached.fullFrames[0]!.seq).toBe(2);

    const secondView = terminalStream.createTerminalView(SESSION_ID, WORKER_FP);
    firstView.dispose();
    const stillRetained = new RecordingRenderer();
    secondView.subscribeRenderer(renderer(stillRetained));
    expect(stillRetained.fullFrames[0]!.seq).toBe(2);

    secondView.dispose();
    const replacement = terminalStream.createTerminalView(SESSION_ID, WORKER_FP);
    const empty = new RecordingRenderer();
    replacement.subscribeRenderer(renderer(empty));
    expect(empty.fullFrames).toHaveLength(0);
    replacement.dispose();
  });

  test("retires and recreates a same-session replica when its worker changes", () => {
    const first = terminalStream.createTerminalView(SESSION_ID, WORKER_FP);
    first.setViewport({ cols: 80, rows: 24 });
    const commandCount = viewCommands().length;

    const replacement = terminalStream.createTerminalView(SESSION_ID, "worker-replacement");
    replacement.setViewport({ cols: 100, rows: 30 });

    expect(terminalSessions.get(SESSION_ID)?.workerFp).toBe("worker-replacement");
    expect(viewCommands()).toHaveLength(commandCount + 1);
    first.setViewport({ cols: 120, rows: 40 });
    expect(viewCommands()).toHaveLength(commandCount + 1);
    replacement.dispose();
  });
  test("assembles chunks atomically and resyncs on order and idle violations", () => {
    const view = terminalStream.createTerminalView(SESSION_ID, WORKER_FP);
    const sink = new RecordingRenderer();
    view.subscribeRenderer(renderer(sink));
    view.setViewport({ cols: 256, rows: 256 });
    acceptView(view.viewId, latestViewCommand().value.revision as bigint, STREAM_A, 256, 256);

    const largeRows = Array.from({ length: 256 }, (_, rowIndex) => ({
      index: rowIndex,
      spans: Array.from({ length: 256 }, (_, colIndex) => {
        const mapping = (rowIndex * 256 + colIndex) % 1_024;
        return {
          text: "x",
          columns: 1,
          fg: 256,
          bg: 256,
          flags: 0,
          linkKey: `link-${mapping}`,
          linkUri: `https://example.invalid/${mapping}/${"u".repeat(64)}`,
        };
      }),
    }));
    const pb = cellFrameToProto(full(STREAM_A, largeRows), SESSION_ID);
    const chunks = chunkCellGridFrame(pb, SNAPSHOT_A);
    expect(chunks.length).toBeGreaterThan(1);

    terminalStream.dispatchTerminalCellChunk(chunks[0]!);
    expect(sink.fullFrames).toHaveLength(0);
    for (const chunk of chunks.slice(1)) terminalStream.dispatchTerminalCellChunk(chunk);
    expect(sink.fullFrames).toHaveLength(1);
    expect(sink.fullFrames[0]!.viewportRows.map((value) => value.index))
      .toEqual(Array.from({ length: 256 }, (_, index) => index));

    terminalStream.dispatchTerminalCellFrame(cellFrameToProto(full(STREAM_B), SESSION_ID));
    expect(sink.fullFrames).toHaveLength(1);

    terminalStream.dispatchTerminalCellChunk(chunks[1]!);
    expect(resyncCommands()).toHaveLength(1);

    const blankRows = largeRows.map(({ index }) => row(index, " ".repeat(256)));
    terminalStream.dispatchTerminalCellFrame(cellFrameToProto(full(STREAM_A, blankRows, 2), SESSION_ID));
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
  }, 15_000);
  test("keeps accepted status stable across exact renewals", () => {
    setSystemTime(0);
    try {
      const view = terminalStream.createTerminalView(SESSION_ID, WORKER_FP);
      const sink = new RecordingRenderer();
      view.subscribeRenderer(renderer(sink));
      const statuses: TerminalViewHandleStatus[] = [];
      view.subscribeStatus((status) => statuses.push(status));
      view.setViewport({ cols: 80, rows: 24 });
      const revision = latestViewCommand().value.revision as bigint;
      acceptView(view.viewId, revision, STREAM_A, 80, 24);
      terminalStream.dispatchTerminalCellFrame(cellFrameToProto(full(STREAM_A, Array.from(
        { length: 24 },
        (_, index) => row(index, "x".repeat(80)),
      )), SESSION_ID));
      const acceptedCount = statuses.length;
      const acceptedCommandCount = viewCommands().length;

      setSystemTime(10_001);
      terminalStream.dispatchTerminalCellFrame(cellFrameToProto({
        ...delta(2, "y".repeat(80)),
        cols: 80,
        rows: 24,
      }, SESSION_ID));
      expect(viewCommands()).toHaveLength(acceptedCommandCount + 1);
      expect(viewCommands().at(-1)!.value).toMatchObject({
        viewId: view.viewId,
        revision,
        active: true,
        cols: 80,
        rows: 24,
      });
      expect(statuses).toHaveLength(acceptedCount);
      expect(statuses.at(-1)).toMatchObject({
        status: "accepted",
        baselineReady: true,
      });

      view.setInactive();
      const inactiveCommandCount = viewCommands().length;
      const inactiveStatusCount = statuses.length;
      terminalStream.dispatchTerminalCellFrame(cellFrameToProto({
        ...delta(3, "z".repeat(80)),
        cols: 80,
        rows: 24,
      }, SESSION_ID));
      expect(viewCommands()).toHaveLength(inactiveCommandCount);
      expect(statuses).toHaveLength(inactiveStatusCount);

      view.dispose();
      terminalStream.dispatchTerminalCellFrame(cellFrameToProto({
        ...delta(4, "q".repeat(80)),
        cols: 80,
        rows: 24,
      }, SESSION_ID));
      expect(viewCommands()).toHaveLength(inactiveCommandCount);
      expect(statuses).toHaveLength(inactiveStatusCount);
    } finally {
      setSystemTime();
    }
  });

  test("replays one fresh baseline without carrying an old resync latch into the next generation", () => {
    const view = terminalStream.createTerminalView(SESSION_ID, WORKER_FP);
    const sink = new RecordingRenderer();
    const statuses: TerminalViewHandleStatus[] = [];
    view.subscribeRenderer(renderer(sink));
    view.subscribeStatus((status) => statuses.push(status));
    view.setViewport({ cols: 80, rows: 24 });
    const revision = latestViewCommand().value.revision as bigint;
    const initialRows = Array.from(
      { length: 24 },
      (_, index) => row(index, "x".repeat(80)),
    );
    acceptView(view.viewId, revision, STREAM_A, 80, 24);
    terminalStream.dispatchTerminalCellFrame(cellFrameToProto(
      full(STREAM_A, initialRows),
      SESSION_ID,
    ));
    expect(sink.fullFrames).toHaveLength(1);

    // This latch is valid only for socket 1. Socket 2's replayed view requests
    // its own baseline, so it must not also emit terminalResync.
    terminalStream.dispatchTerminalCellFrame(cellFrameToProto(
      delta(3, "gap", STREAM_A, 2),
      SESSION_ID,
    ));
    expect(resyncCommands()).toHaveLength(1);

    // The stale socket blocks the tab just short of its scheduled renewal.
    // Redial then replays once; that deferred renewal must not send a second
    // view plus resync before the first replayed baseline arrives.
    vi.advanceTimersByTime(4_999);

    updateSyncState(null);
    updateSyncState({
      socketGeneration: 2,
      socketId: "socket-2",
      processEpoch: "process-1",
      domainGeneration: 12n,
      ready: false,
    });
    expect(viewCommands()).toHaveLength(1);
    expect(resyncCommands()).toHaveLength(1);

    updateSyncState({
      socketGeneration: 2,
      socketId: "socket-2",
      processEpoch: "process-1",
      domainGeneration: 12n,
      ready: true,
    });
    expect(terminalStream.terminalStreamDiagnosticSnapshot(SESSION_ID).replica.baseline_ready).toBe(false);
    expect(viewCommands()).toHaveLength(2);
    expect(resyncCommands()).toHaveLength(1);
    vi.advanceTimersByTime(1);
    expect(viewCommands()).toHaveLength(2);
    expect(resyncCommands()).toHaveLength(1);

    expect(latestViewCommand().value).toMatchObject({
      viewId: view.viewId,
      revision,
      cols: 80,
      rows: 24,
      active: true,
      domainGeneration: 12n,
    });
    expect(statuses.at(-1)?.status).toBe("pending");

    acceptView(view.viewId, revision, STREAM_A, 80, 24);
    terminalStream.dispatchTerminalCellFrame(cellFrameToProto(
      full(STREAM_A, Array.from(
        { length: 24 },
        (_, index) => row(index, "fresh".padEnd(80, "x")),
      ), 2),
      SESSION_ID,
    ));
    expect(sink.fullFrames).toHaveLength(2);
    expect(statuses.at(-1)).toMatchObject({
      status: "accepted",
      baselineReady: true,
    });
  });

  test("rolls rejected intent above the fence and renews the rollback", () => {
    const view = terminalStream.createTerminalView(SESSION_ID, WORKER_FP);
    const statuses: TerminalViewHandleStatus[] = [];
    view.subscribeStatus((status) => statuses.push(status));
    view.setViewport({ cols: 100, rows: 40 });
    const acceptedRevision = latestViewCommand().value.revision as bigint;
    acceptView(view.viewId, acceptedRevision, STREAM_A, 100, 40);

    view.setViewport({ cols: 120, rows: 50 });
    const rejectedRevision = latestViewCommand().value.revision as bigint;
    rejectView(view.viewId, rejectedRevision);
    const rollback = latestViewCommand().value;
    const rollbackRevision = rollback.revision as bigint;
    expect(rollbackRevision).toBeGreaterThan(rejectedRevision);
    expect(rollback).toMatchObject({ cols: 100, rows: 40, active: true });
    expect(statuses.some((status) => status.status === "rejected")).toBe(true);

    acceptView(view.viewId, rollbackRevision, STREAM_A, 100, 40);
    const statusCount = statuses.length;
    vi.advanceTimersByTime(5_000);
    expect(latestViewCommand().value).toMatchObject({
      revision: rollbackRevision,
      cols: 100,
      rows: 40,
      active: true,
    });
    expect(statuses).toHaveLength(statusCount);
    expect(statuses.at(-1)?.status).toBe("accepted");
  });

});
