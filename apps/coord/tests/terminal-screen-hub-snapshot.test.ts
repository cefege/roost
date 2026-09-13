// Covers lazy terminal snapshot source construction and immutable predecessor cursors.
// The canonical-cache suite owns folding and watcher behavior; this suite owns cursor demand.
// Deferred sinks avoid materializing a source before the test explicitly asks for it.

import { describe, expect, test } from "bun:test";
import type { FirehoseFrame } from "@roost/shared/proto/sync_pb";
import type {
  TerminalSnapshotCursor,
  TerminalSnapshotSource,
} from "../src/connect/terminal-screen-frames.ts";
import type {
  TerminalDeltaEnqueueResult,
  TerminalScreenSocketSink,
} from "../src/connect/terminal-screen-hub.ts";
import {
  SESSION,
  STREAM,
  deltaFrame,
  fullFrame,
  makeHarness,
  texts,
} from "./terminal-screen-hub-harness.ts";

class DeferredSnapshotSink implements TerminalScreenSocketSink {
  readonly begins: Array<[sessionId: string, streamId: string]> = [];
  readonly states: Array<{ frame: FirehoseFrame; sessionId: string }> = [];
  readonly deltas: Array<{
    sessionId: string;
    streamId: string;
    frame: FirehoseFrame;
  }> = [];
  readonly drops: string[] = [];
  readonly sources: Array<{
    sessionId: string;
    streamId: string;
    source: TerminalSnapshotSource;
  }> = [];

  beginTerminalStream(sessionId: string, streamId: string): boolean {
    this.begins.push([sessionId, streamId]);
    return true;
  }

  enqueueTerminalState(frame: FirehoseFrame, sessionId: string): void {
    this.states.push({ frame, sessionId });
  }

  replaceTerminalSnapshot(
    sessionId: string,
    streamId: string,
    source: TerminalSnapshotSource,
  ): boolean {
    this.sources.push({ sessionId, streamId, source });
    return true;
  }

  enqueueTerminalDelta(
    sessionId: string,
    streamId: string,
    frame: FirehoseFrame,
  ): TerminalDeltaEnqueueResult {
    this.deltas.push({ sessionId, streamId, frame });
    return "queued";
  }

  dropTerminalSession(sessionId: string): void {
    this.drops.push(sessionId);
  }
}

function frameFromSnapshotCursor(cursor: TerminalSnapshotCursor) {
  expect(cursor.partCount).toBe(1);
  const frame = cursor.materialize(0);
  if (frame.frame.case !== "cellGrid") throw new Error("expected unchunked terminal snapshot");
  return frame.frame.value;
}

describe("TerminalScreenHub snapshot sources", () => {
  test("defers accepted full and delta snapshots until cursor demand", () => {
    const { hub } = makeHarness();
    hub.expectStream(SESSION, STREAM, 8, 2);
    hub.publishFrame(SESSION, fullFrame({ texts: ["first", "second"] }));
    hub.publishFrame(SESSION, deltaFrame({ text: "updated" }));

    const sink = new DeferredSnapshotSink();
    hub.registerSocket("demand", sink);
    hub.setWatching("demand", SESSION, true);
    expect(sink.sources).toHaveLength(0);

    expect(hub.seedSocket("demand", SESSION)).toBe(true);
    expect(sink.sources).toHaveLength(1);
    const firstSource = sink.sources[0];
    if (!firstSource) throw new Error("expected first snapshot source");

    expect(hub.seedSocket("demand", SESSION)).toBe(true);
    expect(sink.sources).toHaveLength(2);
    const repeatedSource = sink.sources[1];
    if (!repeatedSource) throw new Error("expected memoized snapshot source");
    expect(repeatedSource.source).toBe(firstSource.source);

    const cursor = firstSource.source.createCursor();
    try {
      const encoded = frameFromSnapshotCursor(cursor);
      expect(encoded.sessionId).toBe(SESSION);
      expect(encoded.seq).toBe(2n);
      expect(texts(encoded)).toEqual(["first", "updated"]);
    } finally {
      cursor.release();
    }
  });

  test("keeps a leased predecessor cursor immutable after the canonical cache advances", () => {
    const { hub } = makeHarness();
    const predecessor = new DeferredSnapshotSink();
    hub.registerSocket("predecessor", predecessor);
    hub.setWatching("predecessor", SESSION, true);
    hub.expectStream(SESSION, STREAM, 8, 2);
    hub.publishFrame(SESSION, fullFrame({ texts: ["old-a", "old-b"] }));

    const predecessorSource = predecessor.sources[0];
    if (!predecessorSource) throw new Error("expected predecessor snapshot source");
    const predecessorCursor = predecessorSource.source.createCursor();
    try {
      hub.publishFrame(SESSION, deltaFrame({ text: "new-b" }));
      expect(hub.snapshot(SESSION)).toMatchObject({ seq: 2, valid: true });

      const successor = new DeferredSnapshotSink();
      hub.registerSocket("successor", successor);
      hub.setWatching("successor", SESSION, true);
      expect(successor.sources).toHaveLength(0);
      expect(hub.seedSocket("successor", SESSION)).toBe(true);

      const successorSource = successor.sources[0];
      if (!successorSource) throw new Error("expected successor snapshot source");
      expect(successorSource.source).not.toBe(predecessorSource.source);
      const successorCursor = successorSource.source.createCursor();
      try {
        const predecessorFrame = frameFromSnapshotCursor(predecessorCursor);
        expect(predecessorFrame.seq).toBe(1n);
        expect(texts(predecessorFrame)).toEqual(["old-a", "old-b"]);

        const successorFrame = frameFromSnapshotCursor(successorCursor);
        expect(successorFrame.seq).toBe(2n);
        expect(texts(successorFrame)).toEqual(["old-a", "new-b"]);
      } finally {
        successorCursor.release();
      }
    } finally {
      predecessorCursor.release();
    }
  });
});
