// Covers TerminalScreenHub's canonical replica and per-socket snapshot sources.
// Exercises full/delta folding, lazy cache encoding, and source-pinned predecessor cursors.
// The deterministic harness supplies terminal frames and sink boundaries.
import { describe, expect, test } from "bun:test";
import type { TerminalScreenHub } from "../src/connect/terminal-screen-hub.ts";
import {
  EPOCH,
  OTHER_STREAM,
  SESSION,
  SNAPSHOT_B,
  STREAM,
  TestSink,
  chunks,
  deltaFrame,
  fullFrame,
  makeHarness,
  row,
  seededFrame,
  texts,
  watch,
} from "./terminal-screen-hub-harness.ts";

const OTHER_SESSION = "40000000-0000-4000-8000-000000000002";

interface TerminalScreenHubInternals {
  readonly sockets: Map<string, unknown>;
  readonly watchersBySession: Map<string, Set<string>>;
}


describe("TerminalScreenHub canonical cache", () => {
  test("folds deltas once and falls back to the folded baseline when a socket cursor rejects", () => {
    const { hub } = makeHarness();
    const incremental = new TestSink(true);
    const needsBaseline = new TestSink(false);
    const handled = new TestSink("handled");
    watch(hub, incremental, "incremental");
    watch(hub, needsBaseline, "needs-baseline");
    watch(hub, handled, "handled");
    hub.expectStream(SESSION, STREAM, 8, 2);

    hub.publishFrame(SESSION, fullFrame({ texts: ["old-a", "old-b"] }));
    expect(incremental.snapshots).toHaveLength(1);
    expect(needsBaseline.snapshots).toHaveLength(1);
    expect(handled.snapshots).toHaveLength(1);

    hub.publishFrame(SESSION, deltaFrame({ text: "new-b" }));
    expect(incremental.deltas).toHaveLength(1);
    expect(incremental.snapshots).toHaveLength(1);
    expect(needsBaseline.deltas).toHaveLength(1);
    expect(needsBaseline.snapshots).toHaveLength(2);
    expect(handled.deltas).toHaveLength(1);
    expect(handled.snapshots).toHaveLength(1);

    const folded = seededFrame(needsBaseline);
    expect(folded).toMatchObject({
      sessionId: SESSION,
      streamId: STREAM,
      gridEpoch: EPOCH,
      full: true,
      seq: 2n,
      baseSeq: 0n,
      cursorRow: 1,
      cursorCol: 2,
      cursorVisible: false,
      cursorKeysApp: true,
      bracketedPaste: true,
      mouseTracking: 1000,
      mouseSgr: true,
      focusEvents: true,
    });
    expect(texts(folded)).toEqual(["old-a", "new-b"]);
    expect(hub.snapshot(SESSION)).toMatchObject({
      streamId: STREAM,
      gridEpoch: EPOCH,
      seq: 2,
      cols: 8,
      rows: 2,
      valid: true,
    });

    const late = new TestSink();
    watch(hub, late, "late");
    expect(late.begins).toEqual([[SESSION, STREAM]]);
    expect(late.snapshots).toHaveLength(0);
    hub.seedSocket("late", SESSION);
    expect(texts(seededFrame(late))).toEqual(["old-a", "new-b"]);
  });


  test("validates legacy full history before storing a viewport-only cache", () => {
    const { hub, requests } = makeHarness();
    const sink = new TestSink();
    watch(hub, sink);
    hub.expectStream(SESSION, STREAM, 8, 2);

    const malformed = fullFrame();
    malformed.scrollbackTotal = 1n;
    malformed.sbBase = 0n;
    hub.publishFrame(SESSION, malformed);
    expect(hub.snapshot(SESSION)).toBeNull();
    expect(requests).toEqual([[SESSION, STREAM]]);

    const legacy = fullFrame();
    legacy.scrollbackRows = [row(0, "old")];
    legacy.scrollbackTotal = 1n;
    legacy.sbBase = 0n;
    hub.publishFrame(SESSION, legacy);

    const canonical = seededFrame(sink);
    expect(canonical).toMatchObject({
      full: true,
      baseSeq: 0n,
      scrollbackTotal: 1n,
      sbBase: 1n,
      scrollbackRows: [],
      scrollbackAppend: [],
    });
  });

  test("keeps history on live deltas while recovery snapshots stay viewport-only", () => {
    const { hub } = makeHarness();
    const live = new TestSink();
    watch(hub, live);
    hub.expectStream(SESSION, STREAM, 8, 2);
    hub.publishFrame(SESSION, fullFrame());

    const delta = deltaFrame();
    delta.scrollbackAppend = [row(0, "scrolled")];
    delta.scrollbackTotal = 1n;
    hub.publishFrame(SESSION, delta);

    const delivered = live.deltas[0]?.frame;
    if (!delivered || delivered.frame.case !== "cellGrid") {
      throw new Error("expected terminal delta");
    }
    expect(delivered.frame.value.scrollbackAppend.map((entry) =>
      entry.spans.map((span) => span.text).join(""),
    )).toEqual(["scrolled"]);

    const late = new TestSink();
    watch(hub, late, "late");
    expect(hub.seedSocket("late", SESSION)).toBe(true);
    expect(seededFrame(late)).toMatchObject({
      scrollbackTotal: 1n,
      sbBase: 1n,
      scrollbackRows: [],
      scrollbackAppend: [],
    });
  });


  test("publishes activation before cells and drops hidden socket state", () => {
    const { hub } = makeHarness();
    const sink = new TestSink();
    hub.registerSocket("socket-a", sink);
    hub.setWatching("socket-a", SESSION, true);
    expect(sink.begins).toHaveLength(0);

    hub.expectStream(SESSION, STREAM, 8, 2);
    hub.expectStream(SESSION, STREAM, 8, 2);
    hub.publishFrame(SESSION, fullFrame());
    expect(sink.events).toEqual([
      `begin:${STREAM}`,
      `snapshot:${STREAM}`,
    ]);

    hub.setWatching("socket-a", SESSION, false);
    expect(sink.drops).toEqual([SESSION]);
    hub.expectStream(SESSION, OTHER_STREAM, 8, 2);
    hub.publishFrame(SESSION, fullFrame({ streamId: OTHER_STREAM, seq: 3n }));
    expect(sink.begins).toHaveLength(1);
    expect(sink.snapshots).toHaveLength(1);

    hub.setWatching("socket-a", SESSION, true);
    expect(sink.begins.at(-1)).toEqual([SESSION, OTHER_STREAM]);
    hub.seedSocket("socket-a", SESSION);
    expect(seededFrame(sink).streamId).toBe(OTHER_STREAM);
  });

  test("ignores stale streams and latches one repair across wrong base and epoch", () => {
    const { hub, requests } = makeHarness();
    const sink = new TestSink();
    watch(hub, sink);
    hub.expectStream(SESSION, STREAM, 8, 2);
    hub.publishFrame(SESSION, fullFrame());

    hub.publishFrame(SESSION, deltaFrame({ streamId: OTHER_STREAM }));
    expect(requests).toHaveLength(0);
    expect(hub.snapshot(SESSION)).toMatchObject({ seq: 1, valid: true });

    hub.publishFrame(SESSION, deltaFrame({ baseSeq: 9n, seq: 10n }));
    hub.publishFrame(SESSION, deltaFrame({ epoch: "wrong-epoch" }));
    expect(requests).toEqual([[SESSION, STREAM]]);
    expect(hub.snapshot(SESSION)).toMatchObject({ seq: 1, valid: false });

    hub.publishFrame(SESSION, fullFrame({ seq: 10n }));
    expect(hub.snapshot(SESSION)).toMatchObject({ seq: 10, valid: true });
    hub.publishFrame(SESSION, deltaFrame({ baseSeq: 10n, seq: 11n, epoch: "wrong-again" }));
    expect(requests).toEqual([
      [SESSION, STREAM],
      [SESSION, STREAM],
    ]);
  });

  test("keeps the old baseline visible until a complete replacement assembles", () => {
    const { hub, requests } = makeHarness();
    const sink = new TestSink();
    watch(hub, sink);
    hub.expectStream(SESSION, STREAM, 8, 2);
    hub.publishFrame(SESSION, fullFrame({ seq: 1n, texts: ["old-0", "old-1"] }));

    const replacement = fullFrame({ seq: 2n, texts: ["new-0", "new-1"] });
    const firstAttempt = chunks(replacement, [
      [replacement.viewportRows[0]!],
      [replacement.viewportRows[1]!],
    ]);
    hub.publishChunk(SESSION, firstAttempt[0]!);
    expect(hub.snapshot(SESSION)).toMatchObject({ seq: 1, valid: true });
    expect(sink.snapshots).toHaveLength(1);

    hub.publishChunk(SESSION, firstAttempt[0]!);
    expect(requests).toEqual([[SESSION, STREAM]]);
    expect(hub.snapshot(SESSION)).toMatchObject({ seq: 1, valid: true });
    expect(texts(seededFrame(sink))).toEqual(["old-0", "old-1"]);

    const complete = chunks(replacement, [
      [replacement.viewportRows[0]!],
      [replacement.viewportRows[1]!],
    ], SNAPSHOT_B);
    hub.publishChunk(SESSION, complete[0]!);
    expect(hub.snapshot(SESSION)).toMatchObject({ seq: 1, valid: true });
    hub.publishChunk(SESSION, complete[1]!);
    expect(hub.snapshot(SESSION)).toMatchObject({ seq: 2, valid: true });
    expect(sink.snapshots).toHaveLength(2);
    expect(texts(seededFrame(sink))).toEqual(["new-0", "new-1"]);
  });

  test("indexes watcher lifecycle through unwatch, replacement, retirement, and session drop without scanning unrelated sockets", () => {
    const { hub } = makeHarness();
    const view = new TestSink();
    const retired = new TestSink();
    hub.registerSocket("view", view);
    hub.registerSocket("retired", retired);
    hub.setWatching("view", SESSION, true);
    hub.setWatching("view", OTHER_SESSION, true);
    hub.setWatching("retired", SESSION, true);

    const internals = hub as unknown as TerminalScreenHubInternals;
    expect([...(internals.watchersBySession.get(SESSION) ?? [])].sort()).toEqual(["retired", "view"]);
    const socketValues = internals.sockets.values;
    Object.defineProperty(internals.sockets, "values", {
      configurable: true,
      value: () => { throw new Error("terminal fanout must use the session watcher index"); },
    });
    try {
      hub.expectStream(SESSION, STREAM, 8, 2);
      hub.publishFrame(SESSION, fullFrame());
      hub.publishFrame(SESSION, deltaFrame());
      hub.setWatching("view", SESSION, false);
      expect([...internals.watchersBySession.get(SESSION) ?? []]).toEqual(["retired"]);

      const reentered = new TestSink();
      view.dropTerminalSession = (sessionId) => {
        view.drops.push(sessionId);
        hub.registerSocket("view", reentered);
        hub.setWatching("view", SESSION, true);
      };

      const replacement = new TestSink();
      hub.registerSocket("view", replacement);
      expect(view.drops).toEqual([SESSION, OTHER_SESSION]);
      expect(reentered.drops).toHaveLength(0);
      expect([...internals.watchersBySession.get(SESSION) ?? []].sort()).toEqual(["retired", "view"]);
      expect(internals.watchersBySession.get(OTHER_SESSION)).toBeUndefined();

      hub.unregisterSocket("retired");
      expect(retired.drops).toEqual([SESSION]);
      expect([...internals.watchersBySession.get(SESSION) ?? []]).toEqual(["view"]);
      hub.unregisterSocket("view");
      expect(reentered.drops).toEqual([SESSION]);
      expect(internals.watchersBySession.get(SESSION)).toBeUndefined();

      const closing = new TestSink();
      hub.registerSocket("closing", closing);
      hub.setWatching("closing", SESSION, true);
      hub.dropSession(SESSION);
      expect(closing.drops).toEqual([SESSION]);
    } finally {
      Object.defineProperty(internals.sockets, "values", {
        configurable: true,
        value: socketValues,
      });
    }
    expect(internals.watchersBySession.get(SESSION)).toBeUndefined();
  });

  test("copies watcher IDs before reentrant delta callbacks", () => {
    const { hub } = makeHarness();
    const first = new TestSink();
    const second = new TestSink();
    const late = new TestSink();
    let delivered = 0;
    first.enqueueTerminalDelta = () => {
      delivered++;
      hub.unregisterSocket("second");
      hub.registerSocket("late", late);
      hub.setWatching("late", SESSION, true);
      return "queued";
    };
    hub.registerSocket("first", first);
    hub.registerSocket("second", second);
    hub.setWatching("first", SESSION, true);
    hub.setWatching("second", SESSION, true);
    hub.expectStream(SESSION, STREAM, 8, 2);
    hub.publishFrame(SESSION, fullFrame());
    hub.publishFrame(SESSION, deltaFrame());

    expect(delivered).toBe(1);
    expect(second.deltas).toHaveLength(0);
    expect(second.drops).toEqual([SESSION]);
    expect(late.deltas).toHaveLength(0);
  });

});
