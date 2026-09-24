// Owns admission and lifecycle for one authenticated worker connection.
// A hello claims an unready registry generation; only its committed exact
// snapshot activates routing. Generation fences wrap every delegated frame so
// a superseded socket cannot publish after an awaited database append.
import { create } from "@bufbuild/protobuf";
import { randomUUID } from "node:crypto";
import { CoordWorkerDownSchema, DHelloAckSchema } from "@roost/protocol/proto/worker_transport_pb";
import type { CoordWorkerDown, CoordWorkerUp } from "@roost/protocol/proto/worker_transport_pb";
import { TERMINAL_INPUT_ROUTE_CAPABILITY, TERMINAL_PEER_WEBRTC_CAPABILITY } from "@roost/protocol/terminal-peer";
import { TERMINAL_METADATA_CAPABILITY } from "@roost/protocol/terminal-metadata";
import {
  jwtKeyGeneration,
  verifyJwt,
  type Caller as VerifiedJwtCaller,
} from "../auth/jwt.ts";
import { resolveCallerPrincipal } from "../auth/auth-interceptor.ts";
import { replaceWorkerChannelIndex } from "../terminal/screen/byte-hub.ts";
import { rejectPendingRpcsForWorker } from "../router/pending-rpcs.ts";
import { asWorkerFp } from "@roost/protocol/wire";
import { log } from "@roost/observability/log";
import { signal, diag } from "@roost/observability/diag";
import { connectWorkers, _publishRoutable, type WorkerHandle } from "./worker-registry.ts";
import { rejectPendingSpawnsForWorker } from "../sessions/pending-spawns.ts";
import {
  clearTerminalViewOwner,
  registerTerminalViewOwner,
  TERMINAL_VIEW_OWNER_CAPABILITY,
  type TerminalViewOwnerRegistration,
} from "../terminal/view/terminal-view-projection.ts";
import { respawnMissingForWorker } from "./worker-respawn.ts";
import { makeWorkerConnKeepalive } from "./worker-conn-keepalive.ts";
import { makeWorkerFrameDispatcher } from "./worker-frame-dispatch.ts";
import { acknowledgeAttachmentPeerCapability, cancelAttachmentDirectWorkerResults } from "../attachments/worker-conn-attachment.ts";
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
  /** Raw WS owner uses this to atomically replace the credential deadline. */
  onAuthRefreshed?: (caller: VerifiedJwtCaller) => void,
): WorkerConn {
  let workerFp: string | null = null;
  let terminalMetadataNegotiated = false;
  let terminalViewOwner: TerminalViewOwnerRegistration | null = null;
  let done = false;
  let revokedCleanupDone = false;
  let respawnTimer: ReturnType<typeof setTimeout> | null = null;
  // Cleanup is identity-stamped so a reconnecting worker's delayed old socket
  // cannot delete the replacement handle and silently disable browser commands.
  const myHandle: WorkerHandle = {
    workerFp: "",
    processEpoch: null,
    connectionGeneration: randomUUID(),
    capabilities: new Set(),
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
  // Maintenance sends must still observe transport throws, while ACK and
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
    requestClose,
    getWorkerFp: () => workerFp,
    isSnapshotReady: () => myHandle.ready,
    getWorkerHandle: () => myHandle,
    isCurrentGeneration: _isCurrentGeneration,
    fenced: _fenced,
    terminalMetadataNegotiated: () => terminalMetadataNegotiated,
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
        respawnMissingForWorker(deps.db, fp, myHandle, deps.writeGate).catch((error) => {
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
    deps.terminalInputRouteResults?.cancelForWorkerHandle(myHandle, "worker_revoked");
    deps.terminalPeerNegotiations?.cancelForWorkerHandle(myHandle, "worker_revoked");
    cancelAttachmentDirectWorkerResults(deps, myHandle, "worker_revoked");
    if (revokedCleanupDone) return;
    revokedCleanupDone = true;
    myHandle.revoked = true;
    stopLocalWork();
    terminalViewOwner?.release();
    if (workerFp) {
      rejectPendingRpcsForWorker(workerFp, "worker credential revoked");
      rejectPendingSpawnsForWorker(workerFp);
    }
  }
  function close(): void {
    if (done) return;
    deps.terminalInputRouteResults?.cancelForWorkerHandle(myHandle, "worker_disconnected");
    deps.terminalPeerNegotiations?.cancelForWorkerHandle(myHandle, "worker_disconnected");
    cancelAttachmentDirectWorkerResults(deps, myHandle, "worker_disconnected");
    revokedCleanupDone = true;
    done = true;
    myHandle.revoked = true;
    stopLocalWork();
    terminalViewOwner?.release();
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
        if (superseded && superseded !== myHandle) {
          deps.terminalInputRouteResults?.cancelForWorkerHandle(superseded, "connection_superseded");
          deps.terminalPeerNegotiations?.cancelForWorkerHandle(superseded, "connection_superseded");
          cancelAttachmentDirectWorkerResults(deps, superseded, "connection_superseded");
          rejectPendingRpcsForWorker(fp, "worker connection superseded");
        }
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
        const advertised = f.frame.value.capabilities;
        const acknowledgedCapabilities: string[] = [];
        myHandle.processEpoch = f.frame.value.processEpoch || null;
        terminalMetadataNegotiated = advertised.includes(TERMINAL_METADATA_CAPABILITY);
        if (terminalMetadataNegotiated) acknowledgedCapabilities.push(TERMINAL_METADATA_CAPABILITY);
        // This hello is the authoritative generation for the fingerprint in
        // BOTH directions. `superseded.close()` above only asks the transport
        // to close, so the prior connection's identity-stamped release runs
        // strictly later; a worker that dropped the capability has to clear
        // the claim here or its sessions keep being relayed to a build that
        // no longer speaks the relay.
        const terminalViewOwnerNegotiated = advertised.includes(TERMINAL_VIEW_OWNER_CAPABILITY);
        if (terminalViewOwnerNegotiated) {
          terminalViewOwner = registerTerminalViewOwner(fp);
          acknowledgedCapabilities.push(TERMINAL_VIEW_OWNER_CAPABILITY);
        } else {
          clearTerminalViewOwner(fp);
        }
        const terminalInputRouteNegotiated = deps.terminalInputRouteResults !== undefined
          && advertised.includes(TERMINAL_INPUT_ROUTE_CAPABILITY);
        if (terminalInputRouteNegotiated) {
          acknowledgedCapabilities.push(TERMINAL_INPUT_ROUTE_CAPABILITY);
        }
        const terminalPeerNegotiated = deps.cfg?.terminalPeerEnabled === true
          && deps.terminalPeerNegotiations !== undefined
          && advertised.includes(TERMINAL_PEER_WEBRTC_CAPABILITY);
        if (terminalPeerNegotiated) {
          acknowledgedCapabilities.push(TERMINAL_PEER_WEBRTC_CAPABILITY);
        }
        const attachmentPeerNegotiated = acknowledgeAttachmentPeerCapability(deps, advertised, acknowledgedCapabilities);
        myHandle.capabilities = new Set(acknowledgedCapabilities);
        trySend("hello_ack", create(CoordWorkerDownSchema, {
          frame: { case: "helloAck", value: create(DHelloAckSchema, {
            capabilities: acknowledgedCapabilities,
          }) },
        }));
        log.info("worker-service", "hello", {
          worker_fp: fp,
          terminal_metadata_v1: terminalMetadataNegotiated,
          terminal_view_owner_v1: terminalViewOwnerNegotiated,
          terminal_input_route_v1: terminalInputRouteNegotiated,
          terminal_peer_webrtc_v1: terminalPeerNegotiated,
          attachment_transfer_peer_webrtc_v1: attachmentPeerNegotiated,
        });
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
        // deletion and closes the verification→lookup race.
        try {
          const refreshed = await verifyJwt(f.frame.value.jwt, {
            db: deps.db,
            cache: deps.jwtCache,
            jwtMaxAgeSecs: deps.cfg.jwtMaxAgeSecs,
          });
          const principal = await resolveCallerPrincipal(deps.db, refreshed);
          if (
            refreshed.fingerprint !== caller.fingerprint
            || principal?.kind !== "worker"
            || jwtKeyGeneration(deps.jwtCache, refreshed.fingerprint) !== refreshed.keyGeneration
          ) {
            log.warn("worker-service", "refresh_jwt_principal_mismatch", {
              expected_fp: caller.fingerprint,
              got_fp: refreshed.fingerprint,
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
