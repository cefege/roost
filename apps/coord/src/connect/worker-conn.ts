// makeWorkerConn — the transport-agnostic per-connection worker link.
// Owns the upstream-frame reader logic + downstream 30s keepalive + the
// connectWorkers registry lifecycle, plus the respawn-if-missing dispatch
// fired 3s after hello. The raw-WebSocket handler (worker-ws-handler.ts)
// creates a WorkerConn and feeds it frames.
//
// TRANSPORT: this is NOT a Connect bidi. The worker dials a raw Bun
// WebSocket at /ws/coord-worker/:fp?token=<jwt> (worker-ws-handler.ts);
// CoordWorkerUp/Down proto frames ride it as binary. NEVER re-wire this
// as a Connect/gRPC bidi under Bun — it h2-tight-loops / h1.1-stalls /
// flaps. See CLAUDE.md L11 + project_worker_coord_raw_ws_not_connect_bidi.

import { create } from "@bufbuild/protobuf";
import { randomUUID } from "node:crypto";
import { CoordWorkerDownSchema, DHelloAckSchema, DBrowserCommandSchema, DEventAckSchema, DPingSchema } from "@roost/shared/proto/worker_transport_pb";
import type { CoordWorkerUp, CoordWorkerDown } from "@roost/shared/proto/worker_transport_pb";
import type { Database } from "bun:sqlite";
import type { CoordKey } from "../coord-key.ts";
import type { CoordConfig } from "@roost/shared/config";
import type { JwtCache } from "../jwt.ts";
import type { CoordinatorMoveService } from "../coord-move/orchestrator.ts";
import { verifyJwt } from "../jwt.ts";
import { appendEvent } from "../event-log.ts";
import { publishBytes, publishCellGrid, primeChannelMap } from "../byte-hub.ts";
import { resolvePendingRpc, rejectPendingRpc, rejectPendingRpcsForWorker } from "../router/pending-rpcs.ts";
import { globalAgentEntryBus } from "../buses.ts";
import { nextAgentSeq, upsertAgentEntries } from "../agent-transcript.ts";
import { protoToEvent } from "@roost/shared/wire/event-proto";
import { agentEntryFromProto } from "@roost/shared/wire/agent-proto";
import { asWorkerFp, asChannelId, asSessionId, SessionKind } from "@roost/shared/wire";
import type { ClientControlFrame } from "@roost/shared/wire";
import { safeJsonParse } from "@roost/shared/json";
import type { KyselyDB } from "../db/connection.ts";
import { log } from "@roost/shared/log";
import { signal, diag } from "@roost/shared/diag";
import { connectWorkers, _publishRoutable, type WorkerHandle } from "./worker-registry.ts";

export interface WorkerServiceDeps {
  db: KyselyDB;
  sqlite: Database;
  coordKey: CoordKey;
  jwtCache: JwtCache;
  cfg: CoordConfig;
  move?: CoordinatorMoveService;
}

// ─── makeWorkerConn — transport-agnostic per-connection worker link ──
// Owns the upstream-frame reader logic + downstream 30s keepalive + the
// connectWorkers registry lifecycle. The raw-WebSocket handler
// (worker-ws-handler.ts) creates a WorkerConn and feeds it frames. `send`
// is the transport's downstream writer (ws.send(toBinary)); `requestClose`
// asks the transport to tear the connection down (hard-close on fp
// mismatch). Callers MUST serialize handleUpstream() (await each) so event
// appends keep order.
export interface WorkerConn {
  handleUpstream(f: CoordWorkerUp): Promise<void>;
  close(): void;
}

export function makeWorkerConn(
  deps: WorkerServiceDeps,
  caller: { fingerprint: string },
  send: (frame: CoordWorkerDown) => number,
  requestClose: () => void,
  bufferedAmount?: () => number,
): WorkerConn {
  let workerFp: string | null = null;
  let done = false;
  let keepaliveTimer: ReturnType<typeof setInterval> | null = null;
  const ownedAgentSessions = new Set<string>();
  const agentSessionOwner = deps.sqlite.query<{ worker_fp: string }, [string]>(
    "SELECT worker_fp FROM sessions WHERE id = ?",
  );
  // 2026-06-15: identity-stamped handle so cleanup only deletes THIS
  // connection's own registry entry. When a worker reconnects rapidly,
  // connection A's delayed cleanup must not delete connection B's entry —
  // else getWorkerHubSocket(fp) returns null and every browser-command
  // silently no-ops (the pane-X-doing-nothing bug).
  const myHandle: WorkerHandle = { workerFp: "", send, close: requestClose, bufferedAmount };
  // `send` re-throws transport failures so the coordinator-move snapshot pump
  // can detect a lost chunk. Every other downstream frame is best-effort: a
  // dead socket is already closing, and letting the throw escape here would
  // either kill a timer tick or be misfiled as a DB durability fault by the
  // ws handler's `event.append_failed` path.
  const trySend = (what: string, frame: CoordWorkerDown): void => {
    try { send(frame); }
    catch (e) { log.warn("worker-service", "send_failed", { what, worker_fp: workerFp ?? caller.fingerprint, error: String(e) }); }
  };
  const _deleteIfStillMine = (fp: string): void => {
    if (connectWorkers.get(fp) === myHandle) {
      connectWorkers.delete(fp);
      _publishRoutable(); // worker went unroutable → live-update the SPA
    }
  };

  function close(): void {
    if (done) return;
    done = true;
    if (keepaliveTimer) { clearInterval(keepaliveTimer); keepaliveTimer = null; }
    agentSessionOwner.finalize();
    if (workerFp) {
      _deleteIfStillMine(workerFp);
      // A5: fast-fail this worker's in-flight RPCs (browser spawn/attach
      // spinner) instead of leaving them to hang until the 15-30s deadline
      // — UNLESS a fresh connection for the same fp already re-registered,
      // in which case it owns them (don't cancel its RPCs). Same identity
      // guard as _deleteIfStillMine.
      if (!connectWorkers.has(workerFp)) {
        rejectPendingRpcsForWorker(workerFp, "worker disconnected");
      }
    }
  }

  async function handleUpstream(f: CoordWorkerUp): Promise<void> {
    if (done) return;
    switch (f.frame.case) {
      case "hello": {
        const fp = f.frame.value.workerFp;
        // Worker must announce the same fp that authed the JWT.
        if (caller.fingerprint !== fp) {
          log.warn("worker-service", "hello_fp_mismatch", { expected: caller.fingerprint, got: fp });
          signal("worker.protocol_violation", { reason: "fp_mismatch", worker_fp: fp, cooldownKey: fp });
          requestClose();
          return;
        }
        workerFp = fp;
        myHandle.workerFp = fp;
        connectWorkers.set(fp, myHandle);
        _publishRoutable(); // worker became routable → live-update the SPA
        // Keepalive: coord pings the worker every 30s so the socket never
        // goes idle (worker pongs). Lets the transport survive the quiet
        // gaps between 270s JWT refreshes.
        keepaliveTimer = setInterval(() => {
          if (done) return;
          trySend("keepalive", create(CoordWorkerDownSchema, {
            frame: { case: "ping", value: create(DPingSchema, { ts: BigInt(Date.now()) }) },
          }));
        }, 30_000);
        // Prime byte-hub's channel→session map from DB on hello. When coord
        // restarts but the worker stays up, surviving sessions never re-emit
        // `opened`; without priming, every upstream echo byte drops as
        // drop_unmapped_chunk until the next snapshot — user types, sees
        // nothing until refresh.
        try {
          const rows = await deps.db.selectFrom("sessions")
            .select(["id", "worker_fp", "channel"])
            .where("worker_fp", "=", fp)
            .where("status", "=", "open")
            .execute();
          primeChannelMap(rows.map(r => ({ id: r.id, worker_fp: r.worker_fp, channel: r.channel })));
          log.info("worker-service", "channel_map_primed", { worker_fp: fp, count: rows.length });
        } catch (e) {
          log.warn("worker-service", "prime_channel_map_failed", { error: String(e) });
          signal("worker.protocol_violation", { reason: "prime_channel_map_failed", worker_fp: fp, cooldownKey: fp });
        }
        trySend("hello_ack", create(CoordWorkerDownSchema, {
          frame: { case: "helloAck", value: create(DHelloAckSchema, {
            coordPubkeyB64: deps.coordKey.verifyingKeyB64(),
            coordPubkeyKid: deps.coordKey.verifyingKeyKid(),
          }) },
        }));
        log.info("worker-service", "hello", { worker_fp: fp });
        // Respawn-if-missing 3s after hello (grace for the worker's own
        // snapshot events to land first); idempotent on the worker side.
        setTimeout(() => {
          _respawnMissingForWorker(deps.db, deps.sqlite, fp, myHandle).catch((e) => {
            log.warn("worker-service", "respawn_missing_failed", { error: String(e), worker_fp: fp });
          });
        }, 3000);
        return;
      }
      case "pong": return;
      case "event": {
        // D-4b: dedup via (worker_fp, client_seq) UNIQUE INDEX; ack on
        // insert-or-dedup. Pre-hello guard: without workerFp the dedup row
        // is worker_fp=NULL which the partial index doesn't cover.
        if (!workerFp) {
          log.warn("worker-service", "event_before_hello", {});
          diag("worker.frame_dropped", { reason: "event_before_hello", worker_fp: caller.fingerprint });
          signal("worker.protocol_violation", { reason: "event_before_hello", worker_fp: caller.fingerprint, cooldownKey: caller.fingerprint });
          requestClose();
          return;
        }
        const wevt = f.frame.value as { event?: unknown; clientSeq?: bigint };
        const clientSeq = wevt.clientSeq !== undefined ? Number(wevt.clientSeq) : 0;
        let ev;
        try {
          ev = protoToEvent(wevt.event as never);
        } catch (e) {
          log.warn("worker-service", "event_decode_failed", { error: String(e) });
          diag("worker.frame_dropped", { reason: "event_decode_failed", worker_fp: workerFp });
          signal("worker.protocol_violation", { reason: "event_decode_failed", worker_fp: workerFp, cooldownKey: workerFp });
          return;
        }
        if (!ev) {
          log.warn("worker-service", "event_decode_returned_null", {});
          diag("worker.frame_dropped", { reason: "event_decode_returned_null", worker_fp: workerFp });
          signal("worker.protocol_violation", { reason: "event_decode_returned_null", worker_fp: workerFp, cooldownKey: workerFp });
          return;
        }
        if (deps.move?.gate.mode !== undefined && deps.move.gate.mode !== "active") {
          // Pending targets and draining/retired sources keep the worker link
          // alive but deliberately withhold the event ack; CoordLink replays
          // its preserved unacked entry once the committed coordinator accepts it.
          return;
        }
        const lease = deps.move?.gate.acquire();
        try {
          await appendEvent(deps.db, ev, {
            worker_fp: workerFp ?? null,
            client_seq: clientSeq > 0 ? clientSeq : null,
          });
        } catch (e) {
          log.error("worker-service", "event_append_failed", {
            worker_fp: workerFp, kind: ev.kind, client_seq: clientSeq, error: String(e),
          });
          signal("event.append_failed", { error: String(e), worker_fp: workerFp, cooldownKey: "events" });
          throw e; // surface DB durability faults; caller tears down + worker retries.
        } finally {
          lease?.release();
        }
        if (clientSeq > 0) {
          trySend("event_ack", create(CoordWorkerDownSchema, {
            frame: { case: "eventAck", value: create(DEventAckSchema, {
              clientSeq: BigInt(clientSeq),
            })},
          }));
        }
        return;
      }
      case "binary": {
        const b = f.frame.value;
        if (workerFp) {
          publishBytes(asWorkerFp(workerFp), asChannelId(b.channelId), b.data);
        }
        return;
      }
      case "cellGrid": {
        const cg = f.frame.value;
        if (workerFp && cg.frame) {
          publishCellGrid(asWorkerFp(workerFp), asChannelId(cg.channelId), cg.frame);
        }
        return;
      }
      case "agentEntries": {
        // Write through before publishing. Re-emitting a growing entry under
        // the same seq replaces its durable row; a DB hiccup must not interrupt
        // the live transcript, so persistence failure degrades to relay-only.
        const ae = f.frame.value;
        if (workerFp && ae.sessionId) {
          if (!ownedAgentSessions.has(ae.sessionId)) {
            const owner = agentSessionOwner.get(ae.sessionId);
            if (owner?.worker_fp !== workerFp) {
              log.warn("worker-service", "agent_entries_owner_mismatch", {
                session_id: ae.sessionId,
                worker_fp: workerFp,
                owner_fp: owner?.worker_fp ?? null,
              });
              signal("worker.protocol_violation", {
                reason: "agent_entries_owner_mismatch",
                worker_fp: workerFp,
                cooldownKey: workerFp,
              });
              return;
            }
            ownedAgentSessions.add(ae.sessionId);
          }
          diag("agent.entries_relay", { sid: ae.sessionId, count: ae.entries.length });
          try {
            upsertAgentEntries(
              deps.sqlite,
              ae.sessionId,
              ae.entries.map(agentEntryFromProto),
            );
          } catch (e) {
            log.warn("worker-service", "agent_entries_persist_failed", {
              session_id: ae.sessionId,
              error: String(e),
            });
          }
          globalAgentEntryBus.publish(ae);
        }
        return;
      }
      case "rpcOk": {
        try { resolvePendingRpc(f.frame.value.requestId, JSON.parse(f.frame.value.dataJson)); }
        catch { /* ignore malformed */ }
        return;
      }
      case "rpcError": {
        rejectPendingRpc(f.frame.value.requestId, f.frame.value.message);
        return;
      }
      case "refreshJwt": {
        // T2.2 — in-band JWT rotation. Verify the new token; fingerprint
        // must not change mid-connection.
        try {
          const c = await verifyJwt(f.frame.value.jwt, {
            db: deps.db, cache: deps.jwtCache,
            jwtMaxAgeSecs: deps.cfg.jwtMaxAgeSecs,
          });
          if (c.fingerprint !== caller.fingerprint) {
            log.warn("worker-service", "refresh_jwt_fp_mismatch", {
              expected: caller.fingerprint, got: c.fingerprint,
            });
            signal("worker.protocol_violation", { reason: "fp_mismatch", worker_fp: caller.fingerprint, cooldownKey: caller.fingerprint });
            requestClose(); // fingerprint changed mid-connection is suspect.
          } else {
            log.debug("worker-service", "jwt_refreshed", { worker_fp: caller.fingerprint });
          }
        } catch (e) {
          // B4: a worker presenting a token that no longer verifies (expired
          // / revoked key / clock skew past max-age) must be torn down, not
          // left open on the stale credential. Mirrors the fp-mismatch
          // branch — defense-in-depth (fp is cryptographically pinned, so
          // this isn't a hijack vector, but a dead credential shouldn't keep
          // a live transport). The worker re-dials with a fresh JWT.
          log.warn("worker-service", "refresh_jwt_failed", { error: String(e) });
          requestClose();
        }
        return;
      }
      default: return; // agent-patch / presence / transfer-* handled elsewhere
    }
  }

  return { handleUpstream, close };
}

// ─── respawn-if-missing ──────────────────────────────────────────────
// Called 3s after worker.hello. Reads coord DB rows status='open' for
// this worker and sends the revival frame for each kind — worker no-ops if it
// already has the sid live (survivor keeper resumed it, or the omp child is
// still attached), else recreates it at the saved cwd with the SAME session_id
// so SPA URLs keep working. Shell rows get `respawn-if-missing`; agent rows get
// `spawn-agent` with the resume path (see the frame comment below).

// AgentState.session_file out of the opaque agent_json column: the absolute omp
// session .jsonl a revived agent resumes from. Read narrowly rather than
// parsing the whole AgentState — unrelated schema drift in a sibling field must
// not cost us the resume path — and absent/malformed JSON degrades to undefined
// (start a fresh omp session), mirroring sessionRowToProto's agent=null.
function _agentSessionFile(agentJson: string | null): string | undefined {
  // A non-object payload ("null", a bare number) must not throw here: this runs
  // inside the hello sweep, and one bad row would abort every OTHER session's
  // respawn with it.
  const state = safeJsonParse<Record<string, unknown> | null>(agentJson, null, "respawn.agent_json");
  const file = state?.session_file;
  return typeof file === "string" && file.length > 0 ? file : undefined;
}
export function _agentRespawnFrame(
  sqlite: Database,
  row: { id: string; cwd: string; agent_json: string | null },
): Extract<ClientControlFrame, { kind: "spawn-agent" }> {
  return {
    kind: "spawn-agent",
    folder: row.cwd,
    session_id: asSessionId(row.id),
    resume_file: _agentSessionFile(row.agent_json),
    next_seq: nextAgentSeq(sqlite, row.id),
  };
}


async function _respawnMissingForWorker(
  db: KyselyDB,
  sqlite: Database,
  workerFp: string,
  handle: WorkerHandle,
): Promise<void> {
  const rows = await db.selectFrom("sessions")
    .select(["id", "kind", "cwd", "agent_json"])
    .where("worker_fp", "=", workerFp)
    .where("status", "=", "open")
    .execute();
  if (rows.length === 0) return;
  log.info("worker-service", "respawn_missing_dispatch", { worker_fp: workerFp, count: rows.length });
  for (const row of rows) {
    const requestId = randomUUID();
    // Kinds SessionKind knows about are recreated; a historical row carrying
    // any other value stays visible but is not respawned.
    const kind = SessionKind.safeParse(row.kind);
    if (!kind.success) {
      log.warn("worker-service", "respawn_unknown_kind", {
        worker_fp: workerFp, session_id: row.id, kind: row.kind,
      });
      continue;
    }
    // `respawn-if-missing` carries neither a kind nor a resume path and the
    // worker's handler hardcodes spawnShell, so an agent row is revived through
    // `spawn-agent` — the only frame able to carry resume_file. Coord is the
    // sole holder of that path: the worker's omp child and its transcript ring
    // are in-memory and gone after a restart, while the omp session .jsonl
    // survives here in AgentState.session_file (the agent_json column). Firing
    // it on every hello is safe because the worker keys its idempotency on
    // session_id alone: a sid it still holds gets the existing
    // {session_id, channel_id} back, never a second omp child. The flip side,
    // deliberate on the worker's side: a session whose child wedged but whose
    // record is still in the worker's map is ALSO a no-op, so a stale agent does
    // not self-heal on reconnect — clearing that is a user-driven kill +
    // respawn, not something this sweep can force.
    const frame: ClientControlFrame = kind.data === "agent"
      ? _agentRespawnFrame(sqlite, row)
      : {
          kind: "respawn-if-missing",
          request_id: requestId,
          session_id: asSessionId(row.id),
          cwd: row.cwd,
          cols: 80,
          rows: 24,
        };
    try {
      // sendBrowserCmd helper expects a viewer_id; respawn-if-missing has
      // no human caller — use a synthetic "coord:respawn" tag.
      const bc = create(DBrowserCommandSchema, {
        browserId: "coord", viewerId: "coord:respawn",
        requestId, frameJson: JSON.stringify(frame),
      });
      handle.send(create(CoordWorkerDownSchema, {
        frame: { case: "browserCommand", value: bc },
      }));
    } catch (e) {
      log.warn("worker-service", "respawn_send_failed", {
        worker_fp: workerFp, session_id: row.id, error: String(e),
      });
    }
  }
}
