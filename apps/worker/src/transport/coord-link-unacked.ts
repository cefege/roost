// D-4b at-least-once SessionEvent ledger for coord-link-outbox.ts: the durable
// client_seq counter, the in-memory unacked map, and the replay bookkeeping
// that survives a socket swap. Extracted from coord-link.ts as pure code
// motion; it sits behind the byte lanes rather than beside them, so the only
// coupling to the encoded outbox is "encode this" / "try to write this" /
// "kick the drain", all injected below.

import { create } from "@bufbuild/protobuf";
import {
  CoordWorkerUpSchema, WSessionEventSchema,
} from "@roost/shared/proto/worker_transport_pb";
import type { CoordWorkerUp } from "@roost/shared/proto/worker_transport_pb";
import type { SessionEvent } from "@roost/shared/wire";
import { eventToProto } from "@roost/shared/wire/event-proto";
import { signal } from "@roost/shared/diag";
import { log } from "@roost/shared/log";
import { ClientSeq } from "./client-seq.ts";
import { UNACKED_CAP } from "./coord-link-constants.ts";

export interface CoordLinkUnackedHooks {
  isDisposed(): boolean;
  encodeUpstream(frame: CoordWorkerUp): Uint8Array | null;
  tryWriteEncoded(bytes: Uint8Array): boolean;
  /** True once a native socket is attached and accepting encoded bytes. */
  isAttached(): boolean;
  /** Re-runs the encoded outbox's drain ladder. */
  kick(): void;
}

export interface CoordLinkUnacked {
  /** Assigns the next client_seq, records the event, and returns true only
   * when it reached the native socket synchronously. */
  send(event: SessionEvent): boolean;
  /** Admits not-yet-sent events oldest-first, stopping at the first the
   * socket refuses. Callers MUST re-check unsentCount() afterwards. */
  drainUnsent(): void;
  /** Events recorded but not yet admitted to the CURRENT native socket. */
  unsentCount(): number;
  /** Re-admits every unacked entry — the socket that carried them is gone. */
  requeueAll(): void;
  /** requeueAll() plus the reconnect log line; used on a fresh open. */
  replay(): void;
  ack(seq: number): void;
  count(): number;
  clear(): void;
}

export function createCoordLinkUnacked(hooks: CoordLinkUnackedHooks): CoordLinkUnacked {
  // D-4b at-least-once WITHIN A WORKER PROCESS.
  // clientSeq is fsynced (client-seq.ts) and survives restart so coord's
  // dedup key stays stable across reboots. `unacked` is in-memory only:
  // on reconnect inside the same process, every still-unacked entry
  // replays at the head of the new outbox and coord dedups via UNIQUE
  // INDEX (worker_fp, client_seq). On worker process crash, in-flight
  // unacked events are LOST — clientSeq still advances on the next emit
  // so there's no seq collision, but the worker has no record to replay.
  // Persisting `unacked` would mean an fsync per emit; not worth it
  // until we see real loss in practice.
  const clientSeq = new ClientSeq();
  const unacked = new Map<number, SessionEvent>();
  // Events stay in `unacked` until the coordinator ACKs them. This separate
  // set tracks which entries have not yet entered the current native socket;
  // it prevents a bufferedAmount stall from replaying every already-sent event.
  const unsentEventSeqs = new Set<number>();

  /** Wrap a SessionEvent + client_seq into a WSessionEvent frame.
   * Caller is responsible for adding to unacked beforehand. */
  function encodeEventFrame(event: SessionEvent, seq: number): CoordWorkerUp | null {
    const proto = eventToProto(event, 0);
    if (!proto) {
      log.warn("coord-link", "event_proto_encode_returned_null", { kind: event.kind });
      return null;
    }
    return create(CoordWorkerUpSchema, {
      frame: { case: "event", value: create(WSessionEventSchema, {
        event: proto,
        clientSeq: BigInt(seq),
      })},
    });
  }

  function drainUnsent(): void {
    for (const seq of unsentEventSeqs) {
      const event = unacked.get(seq);
      if (!event) {
        unsentEventSeqs.delete(seq);
        continue;
      }
      const proto = encodeEventFrame(event, seq);
      const bytes = proto ? hooks.encodeUpstream(proto) : null;
      if (!bytes) {
        // An unencodable event can never become sendable on reconnect.
        unsentEventSeqs.delete(seq);
        unacked.delete(seq);
        signal("transport.event_drop", { dropped_seq: seq, reason: "encode", cooldownKey: "outbox" });
        continue;
      }
      if (!hooks.tryWriteEncoded(bytes)) return;
      unsentEventSeqs.delete(seq);
    }
  }

  /** Owns D-4b bookkeeping for SessionEvents. Events live in `unacked`
   * until acked; unsentEventSeqs records native-socket admission. */
  function send(event: SessionEvent): boolean {
    if (hooks.isDisposed()) return false;
    if (unacked.size >= UNACKED_CAP) {
      const oldest = unacked.keys().next().value;
      if (oldest !== undefined) {
        unacked.delete(oldest);
        unsentEventSeqs.delete(oldest);
      }
      log.error("coord-link", "unacked_overflow_drop", { cap: UNACKED_CAP, dropped_seq: oldest });
      signal("transport.event_drop", { dropped_seq: oldest, unacked_size: unacked.size, cooldownKey: "outbox" });
    }
    const seq = clientSeq.next();
    unacked.set(seq, event);
    unsentEventSeqs.add(seq);
    if (hooks.isAttached()) hooks.kick();
    return !unsentEventSeqs.has(seq);
  }

  function requeueAll(): void {
    for (const seq of unacked.keys()) unsentEventSeqs.add(seq);
  }

  function replay(): void {
    if (unacked.size === 0) return;
    requeueAll();
    log.info("coord-link", "replaying_unacked", { count: unsentEventSeqs.size });
  }

  return {
    send, drainUnsent, requeueAll, replay,
    unsentCount: () => unsentEventSeqs.size,
    ack: (seq) => { unacked.delete(seq); unsentEventSeqs.delete(seq); },
    count: () => unacked.size,
    clear: () => { unsentEventSeqs.clear(); unacked.clear(); },
  };
}
