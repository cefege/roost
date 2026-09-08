// Verifies the worker's single event durability classifier and sink boundary.
// Durable references share exact persistence and sequencing with lifecycle
// events while snapshots and coordinator-owned events remain prohibited.
import { afterEach, expect, test } from "bun:test";
import { fromBinary, toBinary } from "@bufbuild/protobuf";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { CoordWorkerUpSchema } from "@roost/shared/proto/worker_transport_pb";
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
  expect(classifySessionEvent(opened())).toEqual({ kind: "durable", durableKind: "opened" });
  expect(classifySessionEvent({ kind: "closed", ts: 1, session_id: sessionId, exit_code: 0 })).toEqual({ kind: "durable", durableKind: "closed" });
  expect(classifySessionEvent({ kind: "respawned", ts: 1, session_id: sessionId, new_channel: 2 as never })).toEqual({ kind: "durable", durableKind: "respawned" });
  expect(classifySessionEvent({
    kind: "agent_reference",
    ts: 1,
    session_id: sessionId,
    reference: null,
  })).toEqual({ kind: "durable", durableKind: "agent_reference" });
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
  const sessionEventStore = store();
  const frames: UpstreamFrame[] = [];
  const link = {
    send(frame: UpstreamFrame) { frames.push(frame); return false; },
    snapshotStateChanged() {},
  } as CoordLink;
  const sink = coordLinkSink(link, sessionEventStore);
  const reservation = sink.reserveSessionEvent("opened");
  sink.emit(opened(), reservation);
  sink.emit(cwd("/next"));
  const eventFrames = frames.filter((frame): frame is Extract<UpstreamFrame, { kind: "event" }> => frame.kind === "event");
  expect(eventFrames.map((frame) => frame.clientSeq)).toEqual([1, 2]);
  expect(eventFrames.map((frame) => frame.eventClass)).toEqual(["durable", "metadata"]);
  expect(sessionEventStore.pendingEvents()).toHaveLength(1);

  const forbidden = { kind: "attached", ts: 4, session_id: sessionId } as SessionEvent;
  expect(() => sink.emit(forbidden)).toThrow(SessionEventSinkProgrammerError);
  try { sink.emit(forbidden); } catch (error) { expect(isFatalSessionEventError(error)).toBe(true); }
  sessionEventStore.close();
});

test("direct snapshot emission is rejected without allocating a sequence", () => {
  const sessionEventStore = store();
  const frames: UpstreamFrame[] = [];
  const link = {
    send(frame: UpstreamFrame) { frames.push(frame); return false; },
    snapshotStateChanged() {},
  } as CoordLink;
  const sink = coordLinkSink(link, sessionEventStore);
  const snapshot = { kind: "snapshot", ts: 3, worker_fp: workerFp, sessions: [] } as SessionEvent;

  expect(() => sink.emit(snapshot)).toThrow("snapshot events are owned by the coord-link barrier");
  try { sink.emit(snapshot); } catch (error) { expect(isFatalSessionEventError(error)).toBe(true); }
  sink.emit(cwd("/after-rejection"));
  const eventFrames = frames.filter((frame): frame is Extract<UpstreamFrame, { kind: "event" }> => frame.kind === "event");
  expect(eventFrames.map((frame) => [frame.clientSeq, frame.eventClass])).toEqual([[1, "metadata"]]);
  sessionEventStore.close();
});

test("metadata replacement never removes a durable session-event row", () => {
  const sessionEventStore = store();
  const reservation = sessionEventStore.reserveSessionEvent("opened");
  const durable = sessionEventStore.appendSessionEvent(reservation, opened());
  const written: number[] = [];
  const ledger = createCoordLinkUnacked(sessionEventStore, {
    isDisposed: () => false,
    encodeUpstream: (frame) => toBinary(CoordWorkerUpSchema, frame),
    tryWriteEncoded: (bytes) => {
      const frame = fromBinary(CoordWorkerUpSchema, bytes);
      if (frame.frame.case === "event") written.push(Number(frame.frame.value.clientSeq));
      return true;
    },
    isAttached: () => true,
    kick: () => {},
    onLive: () => {},
  });
  ledger.activateSnapshotProvider(() => ({
    kind: "snapshot", ts: 5, worker_fp: workerFp, sessions: [],
  }));
  ledger.acceptHelloAck(false);
  const firstMetadata = sessionEventStore.nextClientSeq();
  const secondMetadata = sessionEventStore.nextClientSeq();
  ledger.send(cwd("/first"), firstMetadata, "metadata", `${sessionId}\0cwd`);
  ledger.send(cwd("/second"), secondMetadata, "metadata", `${sessionId}\0cwd`);
  expect(written).toEqual([durable.clientSeq]);
  expect(sessionEventStore.pendingEvents().map((row) => row.clientSeq)).toEqual([durable.clientSeq]);

  // Acknowledging the replaced metadata sequence must not release the durable
  // row that is still in flight ahead of it.
  ledger.ack(firstMetadata);
  expect(written).toEqual([durable.clientSeq]);
  expect(sessionEventStore.pendingEvents().map((row) => row.clientSeq)).toEqual([durable.clientSeq]);

  ledger.ack(durable.clientSeq);
  expect(sessionEventStore.pendingEvents()).toEqual([]);
  const snapshotSeq = written.at(-1)!;
  expect(snapshotSeq).toBeGreaterThan(secondMetadata);
  ledger.ack(snapshotSeq);
  expect(written).toEqual([durable.clientSeq, snapshotSeq, secondMetadata]);
  sessionEventStore.close();
});
