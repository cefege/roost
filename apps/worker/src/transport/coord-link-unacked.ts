// Drives the hello → lifecycle replay → snapshot → live protocol barrier.
// Exactly one SessionEvent is in flight so ACKs cannot skip durable rows or
// release post-snapshot traffic before the authoritative snapshot commits.
import { create } from "@bufbuild/protobuf";
import { CoordWorkerUpSchema, WSessionEventSchema } from "@roost/shared/proto/worker_transport_pb";
import type { CoordWorkerUp } from "@roost/shared/proto/worker_transport_pb";
import type { SessionEvent } from "@roost/shared/wire";
import { eventToProto } from "@roost/shared/wire/event-proto";
import { diag, signal } from "@roost/shared/diag";
import { log } from "@roost/shared/log";
import {
  PENDING_BYTES_CAP,
  UNACKED_CAP,
  WORKER_SNAPSHOT_MAX_BYTES,
  WORKER_SNAPSHOT_MAX_SESSIONS,
} from "./coord-link-constants.ts";
import { SessionEventStoreFatalError, type SessionEventStore } from "./session-event-store.ts";
import type { CoordLinkProtocolPhase, WorkerSnapshotProvider } from "./coord-link-types.ts";

export type UnackedEventClass = "lifecycle" | "metadata";
type InFlightEventClass = UnackedEventClass | "snapshot";
export interface CoordLinkUnackedHooks {
  isDisposed(): boolean;
  encodeUpstream(frame: CoordWorkerUp): Uint8Array | null;
  tryWriteEncoded(bytes: Uint8Array): boolean;
  isAttached(): boolean;
  kick(): void;
  onLive(reconnected: boolean): void;
}
export interface CoordLinkUnacked {
  send(event: SessionEvent, clientSeq: number, eventClass: UnackedEventClass, metadataKey?: string): boolean;
  drainUnsent(): void;
  unsentCount(): number;
  acceptHelloAck(reconnected: boolean): void;
  disconnect(): void;
  activateSnapshotProvider(provider: WorkerSnapshotProvider): void;
  snapshotStateChanged(): void;
  phase(): CoordLinkProtocolPhase;
  ready(): boolean;
  ack(seq: number): void;
  count(): number;
  clear(): void;
}
interface EventEntry {
  clientSeq: number;
  eventClass: InFlightEventClass;
  metadataKey?: string;
  bytes: Uint8Array;
  sent: boolean;
}
type SnapshotFailureReason = "provider" | "session_limit" | "encode" | "byte_limit";

export function createCoordLinkUnacked(store: SessionEventStore, hooks: CoordLinkUnackedHooks): CoordLinkUnacked {
  const metadata = new Map<number, EventEntry>();
  const metadataSeqByKey = new Map<string, number>();
  let metadataBytes = 0;
  let inFlight: EventEntry | null = null;
  let protocolPhase: CoordLinkProtocolPhase = "hello";
  let snapshotProvider: WorkerSnapshotProvider | null = null;
  let currentReconnected = false;
  let lastSnapshotFailure = "";

  function encodeEvent(event: SessionEvent, clientSeq: number): Uint8Array | null {
    const proto = eventToProto(event, 0);
    if (!proto) return null;
    return hooks.encodeUpstream(create(CoordWorkerUpSchema, {
      frame: { case: "event", value: create(WSessionEventSchema, { event: proto, clientSeq: BigInt(clientSeq) }) },
    }));
  }
  function snapshotFailure(reason: SnapshotFailureReason, sessionCount: number, encodedBytes?: number): void {
    const signature = `${reason}:${sessionCount}:${encodedBytes ?? 0}`;
    if (signature === lastSnapshotFailure) return;
    lastSnapshotFailure = signature;
    const fields = {
      reason,
      session_count: sessionCount,
      encoded_bytes: encodedBytes ?? null,
      max_sessions: WORKER_SNAPSHOT_MAX_SESSIONS,
      max_bytes: WORKER_SNAPSHOT_MAX_BYTES,
    };
    diag("transport.snapshot_unready", fields);
    signal("transport.snapshot_unready", { ...fields, cooldownKey: signature });
    log.warn("coord-link", "snapshot_unready", fields);
  }
  function removeMetadata(seq: number): EventEntry | undefined {
    const entry = metadata.get(seq);
    if (!entry) return;
    metadata.delete(seq);
    metadataBytes -= entry.bytes.byteLength;
    if (entry.metadataKey && metadataSeqByKey.get(entry.metadataKey) === seq) metadataSeqByKey.delete(entry.metadataKey);
    return entry;
  }
  function metadataDiagnostic(reason: "replaced" | "frame_limit" | "byte_limit"): void {
    signal("transport.metadata_coalesced", { reason, cooldownKey: "event-outbox" });
  }
  function evictMetadata(reason: "frame_limit" | "byte_limit"): boolean {
    for (const [seq] of metadata) {
      if (inFlight?.clientSeq === seq) continue;
      removeMetadata(seq);
      metadataDiagnostic(reason);
      return true;
    }
    return false;
  }
  function admitMetadata(entry: EventEntry): boolean {
    while (metadata.size >= UNACKED_CAP) {
      if (evictMetadata("frame_limit")) continue;
      metadataDiagnostic("frame_limit");
      return false;
    }
    while (metadataBytes + entry.bytes.byteLength > PENDING_BYTES_CAP) {
      if (evictMetadata("byte_limit")) continue;
      metadataDiagnostic("byte_limit");
      return false;
    }
    metadata.set(entry.clientSeq, entry);
    metadataBytes += entry.bytes.byteLength;
    metadataSeqByKey.set(entry.metadataKey!, entry.clientSeq);
    return true;
  }
  function oldestDurable() {
    return store.pendingEvents()[0];
  }
  function hasReservedLifecycle(): boolean {
    return store.stats().blockingReservedRows > 0;
  }
  function startSnapshotBarrier(): void {
    protocolPhase = "snapshot";
    if (!snapshotProvider || inFlight || !hooks.isAttached()) return;
    if (hasReservedLifecycle()) {
      protocolPhase = "replay";
      return;
    }
    let snapshot: ReturnType<WorkerSnapshotProvider>;
    try {
      // Switching phase before the one-copy provider call is the synchronous
      // lifecycle-admission gate. A re-entrant append forces a fresh replay.
      snapshot = snapshotProvider();
    } catch (error) {
      snapshotFailure("provider", 0);
      log.warn("coord-link", "snapshot_provider_failed", { error: error instanceof Error ? error.message : String(error) });
      return;
    }
    if (oldestDurable() || hasReservedLifecycle()) {
      protocolPhase = "replay";
      pump();
      return;
    }
    if (snapshot.sessions.length > WORKER_SNAPSHOT_MAX_SESSIONS) {
      snapshotFailure("session_limit", snapshot.sessions.length);
      return;
    }
    const clientSeq = store.nextClientSeq();
    const bytes = encodeEvent(snapshot, clientSeq);
    if (!bytes) {
      snapshotFailure("encode", snapshot.sessions.length);
      return;
    }
    if (bytes.byteLength > WORKER_SNAPSHOT_MAX_BYTES) {
      snapshotFailure("byte_limit", snapshot.sessions.length, bytes.byteLength);
      return;
    }
    lastSnapshotFailure = "";
    inFlight = { clientSeq, eventClass: "snapshot", bytes, sent: false };
    drainUnsent();
  }
  function pump(): void {
    if (hooks.isDisposed() || !hooks.isAttached() || inFlight || protocolPhase === "hello") return;
    const durable = oldestDurable();
    if (protocolPhase === "replay") {
      if (!durable) {
        if (hasReservedLifecycle()) return;
        return startSnapshotBarrier();
      }
      const bytes = encodeEvent(durable.event, durable.clientSeq);
      if (!bytes) throw new SessionEventStoreFatalError("session event store contains an unencodable event");
      inFlight = { clientSeq: durable.clientSeq, eventClass: "lifecycle", bytes, sent: false };
      return drainUnsent();
    }
    if (protocolPhase === "snapshot") return startSnapshotBarrier();
    if (durable) {
      const bytes = encodeEvent(durable.event, durable.clientSeq);
      if (!bytes) throw new SessionEventStoreFatalError("session event store contains an unencodable event");
      inFlight = { clientSeq: durable.clientSeq, eventClass: "lifecycle", bytes, sent: false };
      return drainUnsent();
    }
    const nextMetadata = metadata.values().next().value as EventEntry | undefined;
    if (nextMetadata) {
      inFlight = nextMetadata;
      drainUnsent();
    }
  }
  function drainUnsent(): void {
    if (!inFlight) return pump();
    if (inFlight.sent || !hooks.isAttached()) return;
    if (hooks.tryWriteEncoded(inFlight.bytes)) inFlight.sent = true;
  }
  function send(event: SessionEvent, clientSeq: number, eventClass: InFlightEventClass, metadataKey?: string): boolean {
    if (hooks.isDisposed()) return false;
    if (!Number.isSafeInteger(clientSeq) || clientSeq <= 0) throw new SessionEventStoreFatalError("session event sequence is unsafe or duplicated");
    if (eventClass === "snapshot") throw new SessionEventStoreFatalError("snapshot events are owned by the coord-link barrier");
    if (eventClass === "metadata" ? !metadataKey : metadataKey !== undefined) throw new SessionEventStoreFatalError("session event metadata key mismatch");
    if (inFlight?.clientSeq === clientSeq || metadata.has(clientSeq)) throw new SessionEventStoreFatalError("session event sequence is unsafe or duplicated");
    if (eventClass === "metadata") {
      const bytes = encodeEvent(event, clientSeq);
      if (!bytes) { metadataDiagnostic("byte_limit"); return false; }
      const old = metadataSeqByKey.get(metadataKey!);
      if (old !== undefined && old !== inFlight?.clientSeq) { removeMetadata(old); metadataDiagnostic("replaced"); }
      if (!admitMetadata({ clientSeq, eventClass, metadataKey, bytes, sent: false })) return false;
    } else if (protocolPhase === "snapshot" && !inFlight) {
      protocolPhase = "replay";
    }
    pump();
    if (hooks.isAttached()) hooks.kick();
    return inFlight?.clientSeq === clientSeq && inFlight.sent;
  }
  function acceptHelloAck(reconnected: boolean): void {
    if (protocolPhase !== "hello" || !hooks.isAttached()) return;
    currentReconnected = reconnected;
    protocolPhase = "replay";
    pump();
  }
  function ack(seq: number): void {
    if (!Number.isSafeInteger(seq) || seq <= 0) return;
    const entry = inFlight;
    if (!entry || !entry.sent || entry.clientSeq !== seq) return;
    if (entry.eventClass === "lifecycle" && !store.acknowledge(seq)) throw new SessionEventStoreFatalError("in-flight lifecycle event disappeared before ACK");
    if (entry.eventClass === "metadata") removeMetadata(seq);
    inFlight = null;
    if (entry.eventClass === "snapshot") {
      protocolPhase = "live";
      hooks.onLive(currentReconnected);
    }
    pump();
  }
  function disconnect(): void {
    protocolPhase = "hello";
    inFlight = null;
    metadata.clear();
    metadataSeqByKey.clear();
    metadataBytes = 0;
    lastSnapshotFailure = "";
  }
  function activateSnapshotProvider(provider: WorkerSnapshotProvider): void {
    snapshotProvider = provider;
    if (protocolPhase === "snapshot" && !inFlight) pump();
  }
  function snapshotStateChanged(): void {
    if (inFlight || protocolPhase === "hello" || protocolPhase === "live") return;
    if (oldestDurable() || hasReservedLifecycle()) protocolPhase = "replay";
    pump();
  }
  function clear(): void { disconnect(); snapshotProvider = null; }
  return {
    send,
    drainUnsent,
    unsentCount: () => inFlight && !inFlight.sent ? 1 : 0,
    acceptHelloAck,
    disconnect,
    activateSnapshotProvider,
    snapshotStateChanged,
    phase: () => protocolPhase,
    ready: () => protocolPhase === "live",
    ack,
    count: () => store.pendingEvents().length + metadata.size + (inFlight?.eventClass === "snapshot" ? 1 : 0),
    clear,
  };
}
