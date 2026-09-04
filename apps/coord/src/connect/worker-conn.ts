// Owns admission and lifecycle for one authenticated worker connection.
// A hello claims an unready registry generation; only its committed exact
// snapshot activates routing. Generation fences wrap every delegated frame so
// a superseded socket cannot publish after an awaited database append.

import { create } from "@bufbuild/protobuf";
import { CoordWorkerDownSchema, DHelloAckSchema } from "@roost/shared/proto/worker_transport_pb";
import type { CoordWorkerUp, CoordWorkerDown } from "@roost/shared/proto/worker_transport_pb";
import {
  jwtKeyGeneration,
  verifyJwt,
  type Caller as VerifiedJwtCaller,
} from "../jwt.ts";
import { resolveCallerPrincipal } from "./auth-interceptor.ts";
import { replaceWorkerChannelIndex } from "../byte-hub.ts";
import { rejectPendingRpcsForWorker } from "../router/pending-rpcs.ts";
import { asWorkerFp } from "@roost/shared/wire";
import { log } from "@roost/shared/log";
import { signal, diag } from "@roost/shared/diag";
import { connectWorkers, _publishRoutable, type WorkerHandle } from "./worker-registry.ts";
import { rejectPendingSpawnsForWorker } from "./pending-spawns.ts";
import { respawnMissingForWorker } from "./worker-respawn.ts";
import {
  makeWorkerConnKeepalive,
} from "./worker-conn-keepalive.ts";
import { makeWorkerFrameDispatcher } from "./worker-frame-dispatch.ts";
import type { WorkerConn, WorkerServiceDeps } from "./worker-conn-types.ts";
export {
  WORKER_PING_DELAY_MS,
  WORKER_PONG_TIMEOUT_MS,
} from "./worker-conn-keepalive.ts";
export type { WorkerConn, WorkerServiceDeps } from "./worker-conn-types.ts";

export function makeWorkerConn(
  deps: WorkerServiceDeps,
  caller: { fingerprint: string },
  send: (frame: CoordWorkerDown) => number,
  requestClose: () => void,
  bufferedAmount?: () => number,
  /** Present only after raw WS admission selected workers.dashboard_id. */
  dashboardId?: string,
  /** Raw WS owner uses this to atomically replace the credential deadline. */
  onAuthRefreshed?: (caller: VerifiedJwtCaller) => void,
): WorkerConn {
  let workerFp: string | null = null;
  let done = false;
  let respawnTimer: ReturnType<typeof setTimeout> | null = null;
  // Cleanup is identity-stamped so a reconnecting worker's delayed old socket
  // cannot delete the replacement handle and silently disable browser commands.
  const myHandle: WorkerHandle = {
    workerFp: "",
    dashboardId,
    revoked: false,
    ready: false,
    send(frame): number {
      if (
        !myHandle.ready
        || myHandle.revoked
        || workerFp === null
        || connectWorkers.get(workerFp) !== myHandle
      ) return 0;
      return sendProtocolFrame(frame.frame.case ?? "unknown", frame);
    },
    fence: revoke,
    close: requestClose,
    bufferedAmount,
  };
  // Coordinator-move sends must still observe transport throws, while ACK and
  // keepalive callers contain them. Both paths close this exact socket on a
  // rejected write so a reconnect can replay anything left unacknowledged.
  const sendProtocolFrame = (what: string, frame: CoordWorkerDown): number => {
    try {
      const result = send(frame);
      if (result !== 0) return result;
      log.warn("worker-service", "send_failed", {
        what,
        worker_fp: workerFp ?? caller.fingerprint,
        result,
      });
      requestClose();
      return result;
    } catch (error) {
      log.warn("worker-service", "send_failed", {
        what,
        worker_fp: workerFp ?? caller.fingerprint,
        error: String(error),
      });
      requestClose();
      throw error;
    }
  };
  const trySend = (what: string, frame: CoordWorkerDown): boolean => {
    if (
      myHandle.revoked
      || (workerFp !== null && connectWorkers.get(workerFp) !== myHandle)
    ) return false;
    try {
      return sendProtocolFrame(what, frame) !== 0;
    } catch {
      return false;
    }
  };
  // A worker's inbound frames are authoritative only while this connection is
  // the registry's current handle for the fingerprint. Pre-hello frames pass:
  // the hello itself has to get through to claim the identity.
  const _isCurrentGeneration = (): boolean =>
    !myHandle.revoked
    && (workerFp === null || connectWorkers.get(workerFp) === myHandle);
  const _fenced = (what: string): boolean => {
    if (_isCurrentGeneration()) return false;
    diag("worker.frame_dropped", { reason: "superseded_generation", what, worker_fp: workerFp });
    return true;
  };
  const _isReady = (): boolean => _isCurrentGeneration() && myHandle.ready;
  const _deleteIfStillMine = (fp: string): void => {
    if (connectWorkers.get(fp) === myHandle) {
      connectWorkers.delete(fp);
      _publishRoutable(); // worker went unroutable → live-update the SPA
    }
  };
  const keepalive = makeWorkerConnKeepalive({
    isDone: () => done,
    isCurrent: (fp) => connectWorkers.get(fp) === myHandle,
    sendBestEffort: trySend,
    onPongTimeout: () => {
      close();
      requestClose();
    },
  });
  const workerFrames = makeWorkerFrameDispatcher({
    deps,
    callerFingerprint: caller.fingerprint,
    dashboardId,
    requestClose,
    getWorkerFp: () => workerFp,
    isSnapshotReady: () => myHandle.ready,
    isCurrentGeneration: _isCurrentGeneration,
    fenced: _fenced,
    sendBestEffort: trySend,
    markSnapshotReady: () => {
      const becameReady = !myHandle.ready;
      myHandle.ready = true;
      _publishRoutable();
      return becameReady;
    },
    scheduleRespawn: (fp) => {
      respawnTimer = setTimeout(() => {
        respawnTimer = null;
        if (done || !_isReady()) return;
        respawnMissingForWorker(deps.db, fp, myHandle).catch((error) => {
          log.warn("worker-service", "respawn_missing_failed", {
            error: String(error),
            worker_fp: fp,
          });
        });
      }, 3000);
    },
  });
  function stopLocalWork(): void {
    keepalive.stop();
    if (respawnTimer) {
      clearTimeout(respawnTimer);
      respawnTimer = null;
    }
  }

  function revoke(): void {
    myHandle.revoked = true;
    stopLocalWork();
    if (workerFp) {
      rejectPendingRpcsForWorker(workerFp, "worker credential revoked");
      rejectPendingSpawnsForWorker(workerFp);
    }
  }

  function close(): void {
    if (done) return;
    done = true;
    myHandle.revoked = true;
    stopLocalWork();
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
    if (myHandle.revoked) {
      diag("worker.frame_dropped", {
        reason: "revoked",
        what: f.frame.case,
        worker_fp: workerFp ?? caller.fingerprint,
      });
      return;
    }
    if (workerFp !== null && f.frame.case !== "hello" && _fenced(f.frame.case ?? "unknown")) {
      return;
    }
    if (
      workerFp !== null
      && !myHandle.ready
      && f.frame.case !== "hello"
      && f.frame.case !== "event"
      && f.frame.case !== "pong"
      && f.frame.case !== "refreshJwt"
    ) {
      diag("worker.frame_dropped", {
        reason: "before_snapshot_ready",
        what: f.frame.case,
        worker_fp: workerFp,
      });
      return;
    }
    if (workerFrames.handleLiveFrame(f)) return;
    switch (f.frame.case) {
      case "hello": {
        if (workerFp !== null) {
          diag("worker.frame_dropped", {
            reason: "duplicate_hello",
            worker_fp: workerFp,
          });
          requestClose();
          return;
        }
        const fp = f.frame.value.workerFp;
        // Worker must announce the same fp that authed the JWT.
        if (caller.fingerprint !== fp) {
          log.warn("worker-service", "hello_fp_mismatch", { expected: caller.fingerprint, got: fp });
          signal("worker.protocol_violation", { reason: "fp_mismatch", worker_fp: fp, cooldownKey: fp });
          requestClose();
          return;
        }
        // Generation change: the fingerprint's prior connection is superseded
        // the moment this authenticated hello lands. Close its socket instead of
        // leaving two live links whose events, snapshots, and cells interleave —
        // a late exact snapshot from the old socket would otherwise replace the
        // channel index this generation is about to install.
        const superseded = connectWorkers.get(fp);
        workerFp = fp;
        myHandle.workerFp = fp;
        myHandle.ready = false;
        connectWorkers.set(fp, myHandle);
        if (superseded && superseded !== myHandle) {
          log.info("worker-service", "superseded_prior_connection", { worker_fp: fp });
          diag("worker.superseded_connection", { worker_fp: fp });
          try { superseded.close?.(); } catch { /* already gone */ }
        }
        // Replacing a ready generation with an unready hello removes it from
        // every public route immediately. The exact snapshot will atomically
        // replace this empty volatile index after its durable commit.
        replaceWorkerChannelIndex(asWorkerFp(fp), []);
        _publishRoutable();
        // One ping generation remains outstanding until its exact application
        // pong is processed on the ordered frame lane. No repeated ping can
        // disguise queue backpressure as healthy transport progress.
        keepalive.scheduleNextPing(fp);
        // Hello claims only the socket generation. It deliberately does not
        // prime DB breadcrumbs into the live channel index: no route, control
        // dispatch, callback, or respawn is admitted before the exact snapshot.
        trySend("hello_ack", create(CoordWorkerDownSchema, {
          frame: { case: "helloAck", value: create(DHelloAckSchema, {}) },
        }));
        log.info("worker-service", "hello", { worker_fp: fp });
        return;
      }
      case "pong": {
        if (workerFp) keepalive.acceptPong(workerFp, f.frame.value.ts);
        return;
      }
      case "event": {
        await workerFrames.handleEvent(f.frame.value);
        return;
      }
      case "refreshJwt": {
        // Rotation is valid only for the exact persisted worker principal bound
        // at upgrade. Re-resolving after signature verification catches worker
        // deletion/dashboard movement and closes the verification→lookup race.
        try {
          const refreshed = await verifyJwt(f.frame.value.jwt, {
            db: deps.db,
            cache: deps.jwtCache,
            jwtMaxAgeSecs: deps.cfg.jwtMaxAgeSecs,
          });
          const principal = await resolveCallerPrincipal(deps.db, deps.cfg, refreshed);
          if (
            refreshed.fingerprint !== caller.fingerprint
            || principal?.kind !== "worker"
            || (dashboardId !== undefined && principal.dashboardId !== dashboardId)
            || jwtKeyGeneration(deps.jwtCache, refreshed.fingerprint) !== refreshed.keyGeneration
          ) {
            log.warn("worker-service", "refresh_jwt_principal_mismatch", {
              expected_fp: caller.fingerprint,
              got_fp: refreshed.fingerprint,
              expected_dashboard_id: dashboardId,
              got_dashboard_id: principal?.kind === "worker" ? principal.dashboardId : undefined,
            });
            signal("worker.protocol_violation", {
              reason: "auth_principal_mismatch",
              worker_fp: caller.fingerprint,
              cooldownKey: caller.fingerprint,
            });
            requestClose();
            return;
          }
          onAuthRefreshed?.(refreshed);
          log.debug("worker-service", "jwt_refreshed", {
            worker_fp: caller.fingerprint,
            valid_until_ms: refreshed.validUntilMs,
          });
        } catch (e) {
          // Missing, expired, revoked, malformed, or otherwise invalid refresh
          // credentials tear down the socket; the worker reconnects freshly.
          log.warn("worker-service", "refresh_jwt_failed", { error: String(e) });
          requestClose();
        }
        return;
      }
      default: return;
    }
  }

  return {
    handleUpstream,
    close,
    revoke,
    isCurrentGeneration: _isCurrentGeneration,
    isReady: _isReady,
  };
}
