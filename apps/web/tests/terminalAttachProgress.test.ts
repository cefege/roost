// Attach-progress exposure on the view handle follows assembler transitions.
// The loading card receives an immediate value, each accepted chunk, and reset.
// No view-owned interval is allowed to poll the shared replica.

import { describe, expect, test, vi } from "bun:test";
import {
  SESSION_ID,
  SNAPSHOT_A,
  STREAM_A,
  acceptView,
  cellFrameToProto,
  chunkCellGridFrame,
  full,
  latestViewCommand,
  terminalStream,
} from "./helpers/terminalStreamFixture.ts";
import type {
  BaselineProgress,
  TerminalViewHandle,
} from "../src/store/terminal-stream-types.ts";
import type { PbCellGridChunk } from "@roost/shared/proto/cell_pb";

let cachedChunks: PbCellGridChunk[] | null = null;

function multiChunkBaseline(): PbCellGridChunk[] {
  if (cachedChunks) return cachedChunks;
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
  if (chunks.length < 3) throw new Error("fixture produced too few chunks");
  cachedChunks = chunks;
  return chunks;
}

function acceptedView(): TerminalViewHandle {
  const view = terminalStream.createTerminalView(SESSION_ID);
  view.setViewport({ cols: 256, rows: 256 });
  acceptView(view.viewId, latestViewCommand().value.revision as bigint, STREAM_A, 256, 256);
  return view;
}

describe("view handle subscribeProgress", () => {
  test("pushes accepted chunk progress and completion without polling", () => {
    const view = acceptedView();
    const chunks = multiChunkBaseline();
    const emissions: Array<BaselineProgress | null> = [];
    const release = view.subscribeProgress((progress) => emissions.push(progress));

    expect(emissions).toEqual([null]);
    vi.advanceTimersByTime(4_999);
    expect(emissions).toEqual([null]);

    terminalStream.dispatchTerminalCellChunk(chunks[0]!);
    expect(emissions.at(-1)).toEqual({
      snapshotId: SNAPSHOT_A, receivedChunks: 1, totalChunks: chunks.length,
    });
    for (const chunk of chunks.slice(1)) terminalStream.dispatchTerminalCellChunk(chunk);
    expect(emissions.at(-1)).toBeNull();

    release();
    const before = emissions.length;
    terminalStream.dispatchTerminalCellChunk(chunks[0]!);
    expect(emissions).toHaveLength(before);
    view.dispose();
  }, 30_000);

  test("pushes null immediately when an invalid chunk resets assembly", () => {
    const view = acceptedView();
    const chunks = multiChunkBaseline();
    const emissions: Array<BaselineProgress | null> = [];
    view.subscribeProgress((progress) => emissions.push(progress));

    terminalStream.dispatchTerminalCellChunk(chunks[0]!);
    expect(emissions.at(-1)?.receivedChunks).toBe(1);
    terminalStream.dispatchTerminalCellChunk(chunks[2]!);
    expect(emissions.at(-1)).toBeNull();
    view.dispose();
  }, 30_000);

  test("does not add polling work for multiple progress listeners", () => {
    const view = acceptedView();
    let firstCalls = 0;
    let secondCalls = 0;
    view.subscribeProgress(() => { firstCalls++; });
    view.subscribeProgress(() => { secondCalls++; });

    vi.advanceTimersByTime(4_999);
    expect(firstCalls).toBe(1);
    expect(secondCalls).toBe(1);
    view.dispose();
  });
});
