// These tests cover which cell frames may become the canonical baseline.
// The shared fixture records renderer output and coordinator commands per session.
// terminalStream.test.ts owns the repair, proof-deadline and liveness cases.

import { describe, expect, test } from "bun:test";
import {
  EPOCH_A,
  RecordingRenderer,
  SESSION_ID,
  STREAM_A,
  STREAM_B,
  acceptView,
  cellFrameToProto,
  delta,
  full,
  latestViewCommand,
  renderer,
  resyncCommands,
  row,
  terminalStream,
} from "./helpers/terminalStreamFixture.ts";

describe("browser terminal baseline admission", () => {
  test("rejects stale or conflicting fulls but admits a fresh stream baseline", () => {
    const view = terminalStream.createTerminalView(SESSION_ID);
    const sink = new RecordingRenderer();
    view.subscribeRenderer(renderer(sink));
    view.setViewport({ cols: 1, rows: 1 });
    const revision = latestViewCommand().value.revision as bigint;
    acceptView(view.viewId, revision);

    terminalStream.dispatchTerminalCellFrame(cellFrameToProto(full(), SESSION_ID));
    terminalStream.dispatchTerminalCellFrame(cellFrameToProto(delta(2, "B"), SESSION_ID));
    terminalStream.dispatchTerminalCellFrame(cellFrameToProto(delta(3, "C"), SESSION_ID));
    const conflicting = {
      ...full(STREAM_A, [row(0, "X")], 3),
      gridEpoch: "grid-epoch-conflict",
    };
    terminalStream.dispatchTerminalCellFrame(cellFrameToProto(conflicting, SESSION_ID));
    expect(terminalStream.terminalStreamDiagnosticSnapshot(SESSION_ID).replica).toMatchObject({
      grid_epoch: EPOCH_A,
      seq: 3,
      resync_latched: true,
    });
    expect(resyncCommands()).toHaveLength(1);
    terminalStream.dispatchTerminalCellFrame(
      cellFrameToProto(full(STREAM_A, [row(0, "S")], 1), SESSION_ID),
    );

    expect(sink.fullFrames).toHaveLength(1);
    expect(sink.fullFrames[0]!.viewportRows[0]!.spans[0]!.text).toBe("A");
    expect(terminalStream.terminalStreamDiagnosticSnapshot(SESSION_ID).replica).toMatchObject({
      expected_stream_id: STREAM_A,
      grid_epoch: EPOCH_A,
      seq: 3,
      baseline_ready: true,
      resync_latched: true,
    });
    expect(resyncCommands()).toHaveLength(1);

    acceptView(view.viewId, revision, STREAM_B);
    terminalStream.dispatchTerminalCellFrame(
      cellFrameToProto(full(STREAM_B, [row(0, "R")], 1), SESSION_ID),
    );

    const freshBaseline = terminalStream.terminalStreamDiagnosticSnapshot(SESSION_ID);
    expect(freshBaseline.wire_received).toEqual({
      stream_id: STREAM_B,
      grid_epoch: EPOCH_A,
      seq: 1,
    });
    expect(freshBaseline.replica.seq).toBe(1);
    expect(freshBaseline.replica).toMatchObject({
      expected_stream_id: STREAM_B,
      grid_epoch: EPOCH_A,
      seq: 1,
      baseline_ready: true,
      resync_latched: false,
    });
    const replayed = new RecordingRenderer();
    const unsubscribeReplay = view.subscribeRenderer(renderer(replayed));
    expect(replayed.fullFrames).toHaveLength(1);
    expect(replayed.fullFrames[0]!.viewportRows[0]!.spans[0]!.text).toBe("R");
    unsubscribeReplay();
  });

  test("validates legacy full history before normalizing its canonical checkpoint", () => {
    const view = terminalStream.createTerminalView(SESSION_ID);
    const sink = new RecordingRenderer();
    view.subscribeRenderer(renderer(sink));
    view.setViewport({ cols: 1, rows: 1 });
    acceptView(view.viewId, latestViewCommand().value.revision as bigint);

    const malformed = cellFrameToProto(full(), SESSION_ID);
    malformed.scrollbackTotal = 1n;
    malformed.sbBase = 0n;
    terminalStream.dispatchTerminalCellFrame(malformed);
    expect(sink.fullFrames).toHaveLength(0);
    expect(resyncCommands()).toHaveLength(1);

    const legacy = full();
    legacy.scrollbackRows = [row(0, "old")];
    legacy.scrollbackTotal = 1;
    legacy.sbBase = 0;
    terminalStream.dispatchTerminalCellFrame(cellFrameToProto(legacy, SESSION_ID));

    const canonical = sink.fullFrames.at(-1);
    expect(canonical).toMatchObject({
      full: true,
      baseSeq: 0,
      scrollbackTotal: 1,
      sbBase: 1,
      scrollbackRows: [],
      scrollbackAppend: [],
    });
    view.dispose();
  });
});
