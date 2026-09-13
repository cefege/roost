// Covers reentrant terminal-screen sink retirement at the hub boundary.
// Callbacks may register sockets, so both watcher directions must be detached first.
// The deterministic harness makes those lifecycle edges directly observable.
import { describe, expect, test } from "bun:test";
import {
  SESSION,
  STREAM,
  TestSink,
  makeHarness,
} from "./terminal-screen-hub-harness.ts";

const OTHER_SESSION = "40000000-0000-4000-8000-000000000002";

interface TerminalScreenHubInternals {
  readonly sockets: Map<string, unknown>;
  readonly watchersBySession: Map<string, Set<string>>;
}

describe("TerminalScreenHub watcher retirement", () => {
  test("detaches every old socket watch before callback and preserves a reentrant replacement", () => {
    const { hub } = makeHarness();
    const stale = new TestSink();
    const replacement = new TestSink();
    const internals = hub as unknown as TerminalScreenHubInternals;
    hub.registerSocket("socket", stale);
    hub.setWatching("socket", SESSION, true);
    hub.setWatching("socket", OTHER_SESSION, true);

    stale.dropTerminalSession = (sessionId) => {
      stale.drops.push(sessionId);
      if (sessionId !== SESSION) return;
      expect(internals.watchersBySession.get(OTHER_SESSION)).toBeUndefined();
      hub.registerSocket("socket", replacement);
      hub.setWatching("socket", OTHER_SESSION, true);
    };

    hub.unregisterSocket("socket");

    expect(stale.drops).toEqual([SESSION, OTHER_SESSION]);
    expect(internals.watchersBySession.get(SESSION)).toBeUndefined();
    expect([...internals.watchersBySession.get(OTHER_SESSION) ?? []]).toEqual(["socket"]);
  });

  test("clears watches reinstalled by stale session-drop callbacks", () => {
    const { hub } = makeHarness();
    const first = new TestSink();
    const second = new TestSink();
    const reentrant = new TestSink();
    const internals = hub as unknown as TerminalScreenHubInternals;
    hub.expectStream(SESSION, STREAM, 8, 2);
    hub.registerSocket("first", first);
    hub.registerSocket("second", second);
    hub.setWatching("first", SESSION, true);
    hub.setWatching("second", SESSION, true);

    first.dropTerminalSession = (sessionId) => {
      first.drops.push(sessionId);
      expect(internals.watchersBySession.get(sessionId)).toBeUndefined();
      hub.registerSocket("reentrant", reentrant);
      hub.setWatching("reentrant", sessionId, true);
    };

    hub.dropSession(SESSION);

    expect(first.drops).toEqual([SESSION]);
    expect(second.drops).toEqual([SESSION]);
    expect(reentrant.begins).toHaveLength(0);
    expect(internals.watchersBySession.get(SESSION)).toBeUndefined();
  });

  test("detaches every registration before disposing reentrant callbacks", () => {
    const { hub } = makeHarness();
    const retiring = new TestSink();
    const other = new TestSink();
    const reentrant = new TestSink();
    const internals = hub as unknown as TerminalScreenHubInternals;
    hub.expectStream(SESSION, STREAM, 8, 2);
    hub.registerSocket("retiring", retiring);
    hub.registerSocket("other", other);
    hub.setWatching("retiring", SESSION, true);
    hub.setWatching("other", OTHER_SESSION, true);

    retiring.dropTerminalSession = (sessionId) => {
      retiring.drops.push(sessionId);
      expect(internals.watchersBySession.get(OTHER_SESSION)).toBeUndefined();
      hub.registerSocket("reentrant", reentrant);
      hub.setWatching("reentrant", sessionId, true);
    };

    hub.dispose();

    expect(retiring.drops).toEqual([SESSION]);
    expect(other.drops).toEqual([OTHER_SESSION]);
    expect(reentrant.begins).toHaveLength(0);
    expect(internals.sockets.size).toBe(0);
    expect(internals.watchersBySession.size).toBe(0);
  });
});
