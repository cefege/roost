import { afterEach, expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { SessionEvent } from "@roost/shared/wire";
import {
  classifySessionEvent,
  coordLinkSink,
  isFatalSessionEventError,
  SessionEventSinkProgrammerError,
} from "../src/event-sink.ts";
import type { CoordLink } from "../src/transport/coord-link.ts";
import type { UpstreamFrame } from "../src/transport/coord-link-types.ts";
import { createCoordLinkUnacked } from "../src/transport/coord-link-unacked.ts";
import { SessionEventStore } from "../src/transport/session-event-store.ts";

const roots: string[] = [];
afterEach(() => {
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true });
});
function store(): SessionEventStore {
  const root = mkdtempSync(join(tmpdir(), "roost-event-sink-"));
  roots.push(root);
  return new SessionEventStore({
    dbPath: join(root, "outbox.sqlite"),
    legacySequencePath: join(root, "client-seq.txt"),
  });
}
const sessionId = "00000000-0000-4000-8000-000000000001" as never;
const workerFp = "a".repeat(64) as never;
function opened(): SessionEvent {
  return { kind: "opened", ts: 1, session_id: sessionId, worker_fp: workerFp, channel: 1 as never, session_kind: "shell", cwd: "/tmp" };
}
function cwd(value: string): SessionEvent {
  return { kind: "cwd", ts: 2, session_id: sessionId, cwd: value };
}

test("classification is exhaustive across worker event policy", () => {
  expect(classifySessionEvent(opened())).toEqual({ kind: "lifecycle", lifecycleKind: "opened" });
  expect(classifySessionEvent({ kind: "closed", ts: 1, session_id: sessionId, exit_code: 0 })).toEqual({ kind: "lifecycle", lifecycleKind: "closed" });
  expect(classifySessionEvent({ kind: "respawned", ts: 1, session_id: sessionId, new_channel: 2 as never })).toEqual({ kind: "lifecycle", lifecycleKind: "respawned" });
  expect(classifySessionEvent({ kind: "snapshot", ts: 1, worker_fp: workerFp, sessions: [] })).toEqual({ kind: "programmer-error" });
  for (const event of [
    cwd("/a"),
    { kind: "git", ts: 1, session_id: sessionId, branch: null },
    { kind: "pr", ts: 1, session_id: sessionId, number: null, state: null, checks: null, url: null },
    { kind: "ports", ts: 1, session_id: sessionId, ports: [] },
  ] as SessionEvent[]) {
    expect(classifySessionEvent(event)).toEqual({ kind: "metadata", key: `${sessionId}\0${event.kind}` });
  }
  for (const event of [
    { kind: "attached", ts: 1, session_id: sessionId },
    { kind: "detached", ts: 1, session_id: sessionId },
    { kind: "workspace_assigned", ts: 1, session_id: sessionId, workspace_id: null },
    { kind: "renamed", ts: 1, session_id: sessionId, custom_title: "x" },
  ] as SessionEvent[]) expect(classifySessionEvent(event)).toEqual({ kind: "programmer-error" });
});

test("one store assigns sequences to durable and metadata events", () => {
  const lifecycleStore = store();
  const frames: UpstreamFrame[] = [];
  const link = { send(frame: UpstreamFrame) { frames.push(frame); return false; } } as CoordLink;
  const sink = coordLinkSink(link, lifecycleStore);
  const reservation = sink.reserveLifecycleEvent("opened");
  sink.emit(opened(), reservation);
  sink.emit(cwd("/next"));
  const eventFrames = frames.filter((frame): frame is Extract<UpstreamFrame, { kind: "event" }> => frame.kind === "event");
  expect(eventFrames.map((frame) => frame.clientSeq)).toEqual([1, 2]);
  expect(eventFrames.map((frame) => frame.eventClass)).toEqual(["lifecycle", "metadata"]);
  expect(lifecycleStore.pendingEvents()).toHaveLength(1);

  const forbidden = { kind: "attached", ts: 4, session_id: sessionId } as SessionEvent;
  expect(() => sink.emit(forbidden)).toThrow(SessionEventSinkProgrammerError);
  try { sink.emit(forbidden); } catch (error) { expect(isFatalSessionEventError(error)).toBe(true); }
  lifecycleStore.close();
});

test("direct snapshot emission is rejected without allocating a sequence", () => {
  const lifecycleStore = store();
  const frames: UpstreamFrame[] = [];
  const link = { send(frame: UpstreamFrame) { frames.push(frame); return false; } } as CoordLink;
  const sink = coordLinkSink(link, lifecycleStore);
  const snapshot = { kind: "snapshot", ts: 3, worker_fp: workerFp, sessions: [] } as SessionEvent;

  expect(() => sink.emit(snapshot)).toThrow("snapshot events are owned by the coord-link barrier");
  try { sink.emit(snapshot); } catch (error) { expect(isFatalSessionEventError(error)).toBe(true); }
  sink.emit(cwd("/after-rejection"));
  const eventFrames = frames.filter((frame): frame is Extract<UpstreamFrame, { kind: "event" }> => frame.kind === "event");
  expect(eventFrames.map((frame) => [frame.clientSeq, frame.eventClass])).toEqual([[1, "metadata"]]);
  lifecycleStore.close();
});

test("metadata replacement never removes a durable lifecycle row", () => {
  const lifecycleStore = store();
  const reservation = lifecycleStore.reserveLifecycleEvent("opened");
  const durable = lifecycleStore.appendLifecycleEvent(reservation, opened());
  const ledger = createCoordLinkUnacked(lifecycleStore, {
    isDisposed: () => false,
    encodeUpstream: () => new Uint8Array([1, 2, 3]),
    tryWriteEncoded: () => true,
    isAttached: () => true,
    kick: () => {},
    onLive: () => {},
  });
  ledger.acceptHelloAck(false);
  const firstMetadata = lifecycleStore.nextClientSeq();
  const secondMetadata = lifecycleStore.nextClientSeq();
  ledger.send(cwd("/first"), firstMetadata, "metadata", `${sessionId}\0cwd`);
  ledger.send(cwd("/second"), secondMetadata, "metadata", `${sessionId}\0cwd`);
  expect(ledger.count()).toBe(2);
  expect(lifecycleStore.pendingEvents().map((row) => row.clientSeq)).toEqual([durable.clientSeq]);
  ledger.ack(firstMetadata);
  expect(ledger.count()).toBe(2);
  ledger.ack(durable.clientSeq);
  expect(ledger.count()).toBe(1);
  expect(lifecycleStore.pendingEvents()).toEqual([]);
  lifecycleStore.close();
});
