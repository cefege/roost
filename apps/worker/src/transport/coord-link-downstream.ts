// Coordinator -> worker frame dispatch for coord-link.ts: every CoordWorkerDown
// variant, the per-kind terminal-control admission slots, and the monotonic
// request budget derived from the coordinator's RELATIVE budget_ms. Extracted
// from coord-link.ts as pure code motion to keep both files under the 400-line
// cap. Reply frames go back out through the same outbox, so ordering relative
// to cells and raw metadata is unchanged.

import {
  TerminalInputStatus, TerminalViewportStatus, TerminalWritePhase,
} from "@roost/shared/proto/worker_transport_pb";
import type {
  CoordWorkerDown,
  DInputRequest,
  DViewportRequest,
} from "@roost/shared/proto/worker_transport_pb";
import type { ClientControlFrame } from "@roost/shared/wire";
import { diag } from "@roost/shared/diag";
import { log } from "@roost/shared/log";
import {
  INPUT_REQUEST_INFLIGHT_CAP, VIEWPORT_REQUEST_INFLIGHT_CAP,
  TERMINAL_REQUEST_BUDGET_CAP_MS,
} from "./coord-link-constants.ts";
import type {
  CoordLinkDeps, CoordLinkDownstream, CoordLinkOutbox, TerminalRequestBudget,
} from "./coord-link-types.ts";

export function createCoordLinkDownstream(
  deps: CoordLinkDeps,
  outbox: CoordLinkOutbox,
): CoordLinkDownstream {
  const { send } = outbox;
  const snapshotRequestIds = new Map<string, string>();
  // Downstream terminal-control admission, counted per kind. A viewport RPC
  // parked on a keeper resize therefore cannot spend the slots PTY input
  // needs, and neither lane can starve the other.
  let inputRequestsInFlight = 0;
  let viewportRequestsInFlight = 0;

  /** Bound one downstream terminal request to a monotonic budget derived from
   * the coordinator's RELATIVE `budget_ms`. Frame receipt is the origin, so
   * time spent queueing inside the worker is charged against the same budget
   * the coordinator is still waiting on, and neither host's wall clock — nor
   * any skew between them — participates in the decision. */
  function terminalBudget(socket: WebSocket, budgetMs: number): TerminalRequestBudget {
    const receivedAtMono = performance.now();
    const allowedMs = budgetMs > 0
      ? Math.min(budgetMs, TERMINAL_REQUEST_BUDGET_CAP_MS)
      : TERMINAL_REQUEST_BUDGET_CAP_MS;
    return {
      remainingMs: () => allowedMs - (performance.now() - receivedAtMono),
      isCurrentConnection: () => outbox.activeSocket() === socket,
    };
  }

  function handleDownstream(frame: CoordWorkerDown, reconnected: boolean, socket: WebSocket): void {
    const k = frame.frame?.case;
    if (!k) return;
    const v = frame.frame.value;
    switch (k) {
      case "helloAck": {
        const ha = v as { coordPubkeyB64: string; coordPubkeyKid: string };
        outbox.markLinkReady();
        // SessionManager emits reconnect/full repairs synchronously from this
        // callback. drainQueues() then releases raw metadata, preserving the
        // repair-before-backlog boundary.
        deps.onHelloAck?.({
          coord_pubkey_b64: ha.coordPubkeyB64,
          coord_pubkey_kid: ha.coordPubkeyKid,
          reconnected,
        });
        outbox.drainQueues();
        log.info("coord-link", "hello_ack", { coord_kid: ha.coordPubkeyKid, reconnected });
        return;
      }
      case "ping": {
        const ts = Number((v as { ts: bigint }).ts);
        send({ kind: "pong", ts });
        return;
      }
      case "browserCommand": {
        const bc = v as { browserId: string; viewerId: string; requestId: string; frameJson: string };
        try {
          deps.onBrowserCommand?.({
            browser_id: bc.browserId, viewer_id: bc.viewerId,
            request_id: bc.requestId, frame: JSON.parse(bc.frameJson) as ClientControlFrame,
          });
        } catch (e) { log.warn("coord-link", "browser_command_parse", { error: String(e) }); diag("transport.cmd_parse_failed", {}); }
        return;
      }
      case "binary": {
        const b = v as { channelId: number; direction: number; data: Uint8Array };
        deps.onBinary?.(b.channelId, b.direction, b.data);
        return;
      }
      case "inputRequest": {
        const request = v as DInputRequest;
        if (inputRequestsInFlight >= INPUT_REQUEST_INFLIGHT_CAP) {
          // Fail closed with proof rather than dropping: nothing was handed to
          // the session manager, so the coordinator may reject definitely and
          // the browser may retry without risking a duplicate write.
          diag("transport.terminal_admission_full", {
            kind: "input",
            in_flight: inputRequestsInFlight,
          });
          send({
            kind: "input-result",
            request_id: request.requestId,
            session_id: request.sessionId,
            input_seq: request.inputSeq,
            status: TerminalInputStatus.REJECTED,
            written_bytes: 0,
            phase: TerminalWritePhase.PRE_WRITE,
            reason: "worker input admission is full",
          });
          return;
        }
        inputRequestsInFlight += 1;
        // The async IIFE keeps the handler call synchronous — receive order
        // into the keeper-admission lane is preserved — while turning a
        // synchronous throw into a rejection this catch can answer, rather
        // than letting it escape through ws.onmessage and strand the request.
        void (async () => deps.onInputRequest?.(request, terminalBudget(socket, request.budgetMs)))()
          .catch((error: unknown) => {
            // A thrown handler cannot say which side of the keeper write it
            // died on, so the batch is reported unknown, never "unsent" —
            // presenting it as unsent is what would license a duplicate.
            const message = error instanceof Error ? error.message : String(error);
            log.warn("coord-link", "input_request_failed", {
              request_id: request.requestId,
              error: message,
            });
            send({
              kind: "input-result",
              request_id: request.requestId,
              session_id: request.sessionId,
              input_seq: request.inputSeq,
              status: TerminalInputStatus.AMBIGUOUS,
              written_bytes: 0,
              phase: TerminalWritePhase.UNKNOWN,
              reason: message,
            });
          })
          .finally(() => { inputRequestsInFlight -= 1; });
        return;
      }
      case "viewportRequest": {
        const request = v as DViewportRequest;
        if (viewportRequestsInFlight >= VIEWPORT_REQUEST_INFLIGHT_CAP) {
          diag("transport.terminal_admission_full", {
            kind: "viewport",
            in_flight: viewportRequestsInFlight,
          });
          send({
            kind: "viewport-result",
            request_id: request.requestId,
            session_id: request.sessionId,
            client_seq: request.clientSeq,
            status: TerminalViewportStatus.REJECTED,
            channel_resize_seq: 0n,
            cols: 0,
            rows: 0,
            resized: false,
            phase: TerminalWritePhase.PRE_WRITE,
            reason: "worker viewport admission is full",
          });
          return;
        }
        viewportRequestsInFlight += 1;
        void (async () => deps.onViewportRequest?.(request, terminalBudget(socket, request.budgetMs)))()
          .catch((error: unknown) => {
            const message = error instanceof Error ? error.message : String(error);
            log.warn("coord-link", "viewport_request_failed", {
              request_id: request.requestId,
              error: message,
            });
            send({
              kind: "viewport-result",
              request_id: request.requestId,
              session_id: request.sessionId,
              client_seq: request.clientSeq,
              status: TerminalViewportStatus.AMBIGUOUS,
              channel_resize_seq: 0n,
              cols: 0,
              rows: 0,
              resized: false,
              phase: TerminalWritePhase.UNKNOWN,
              reason: message,
            });
          })
          .finally(() => { viewportRequestsInFlight -= 1; });
        return;
      }
      case "attachmentChunk": {
        const a = v as { requestId: string; sessionId: string; filename: string; shortPath: boolean; data: Uint8Array; last: boolean; seq: number };
        deps.onAttachmentChunk?.({
          request_id: a.requestId, session_id: a.sessionId, filename: a.filename,
          short_path: a.shortPath, data: a.data, last: a.last, seq: a.seq,
        });
        return;
      }
      case "coordMovePrepare": {
        const move = v as { requestId: string; handoffId: string; sourceUrl: string; targetUrl: string; expectedCoordKid: string; expectedGitSha: string; estimatedDbSize: bigint; action: "CHECK" | "PREPARE" };
        if (!deps.onCoordMovePrepare) {
          send({ kind: "rpc-error", request_id: move.requestId, message: "coordinator move unsupported by this worker" });
          return;
        }
        void Promise.resolve(deps.onCoordMovePrepare({
          request_id: move.requestId, handoff_id: move.handoffId, source_url: move.sourceUrl, target_url: move.targetUrl,
          expected_coord_kid: move.expectedCoordKid, expected_git_sha: move.expectedGitSha, estimated_db_size: move.estimatedDbSize, action: move.action,
        })).then(() => send({ kind: "rpc-ok", request_id: move.requestId, data: {} }))
          .catch((error) => send({ kind: "rpc-error", request_id: move.requestId, message: (error as Error).message }));
        return;
      }
      case "coordMoveSnapshotStart": {
        const snapshot = v as { requestId: string; handoffId: string; totalSize: bigint; sha256: string; coordKeyPem: Uint8Array; authorizedKeys: Uint8Array; secretSha256: string; expectedWorkerFps: string[] };
        if (!deps.onCoordMoveSnapshotStart) {
          send({ kind: "rpc-error", request_id: snapshot.requestId, message: "coordinator move unsupported by this worker" });
          return;
        }
        void Promise.resolve(deps.onCoordMoveSnapshotStart({
          request_id: snapshot.requestId, handoff_id: snapshot.handoffId, total_size: snapshot.totalSize, sha256: snapshot.sha256,
          coord_key_pem: snapshot.coordKeyPem, authorized_keys: snapshot.authorizedKeys, secret_sha256: snapshot.secretSha256,
          expected_worker_fps: snapshot.expectedWorkerFps,
        })).then(() => snapshotRequestIds.set(snapshot.handoffId, snapshot.requestId))
          .catch((error) => send({ kind: "rpc-error", request_id: snapshot.requestId, message: (error as Error).message }));
        return;
      }
      case "coordMoveSnapshotChunk": {
        const chunk = v as { handoffId: string; seq: number; data: Uint8Array; last: boolean };
        if (!deps.onCoordMoveSnapshotChunk) return;
        void Promise.resolve(deps.onCoordMoveSnapshotChunk({
          handoff_id: chunk.handoffId, seq: chunk.seq, data: chunk.data, last: chunk.last,
        })).then(() => {
          if (!chunk.last) return;
          const requestId = snapshotRequestIds.get(chunk.handoffId);
          if (!requestId) return;
          snapshotRequestIds.delete(chunk.handoffId);
          send({ kind: "rpc-ok", request_id: requestId, data: {} });
        }).catch((error) => {
          const requestId = snapshotRequestIds.get(chunk.handoffId);
          if (!requestId) return;
          snapshotRequestIds.delete(chunk.handoffId);
          send({ kind: "rpc-error", request_id: requestId, message: (error as Error).message });
        });
        return;
      }
      case "coordRelocate": {
        const relocate = v as { requestId: string; handoffId: string; sourceUrl: string; targetUrl: string; action: "STAGE" | "ACTIVATE" | "COMMIT" | "ABORT" };
        if (!deps.onCoordRelocate) {
          send({ kind: "rpc-error", request_id: relocate.requestId, message: "coordinator move unsupported by this worker" });
          return;
        }
        void Promise.resolve(deps.onCoordRelocate({
          request_id: relocate.requestId, handoff_id: relocate.handoffId, source_url: relocate.sourceUrl, target_url: relocate.targetUrl, action: relocate.action,
        })).then(() => send({ kind: "rpc-ok", request_id: relocate.requestId, data: {} }))
          .catch((error) => send({ kind: "rpc-error", request_id: relocate.requestId, message: (error as Error).message }));
        return;
      }
      case "updateBroker": {
        const update = v as {
          requestId: string;
          jobId: string;
          action: string;
          manifestUrl: string;
          signatureUrl: string;
          manifestSha256: string;
          publisherSha256: string;
        };
        if (update.action !== "START" && update.action !== "STATUS") {
          send({ kind: "rpc-error", request_id: update.requestId, message: `unsupported updater action: ${update.action}` });
          return;
        }
        if (!deps.onUpdateBroker) {
          send({ kind: "rpc-error", request_id: update.requestId, message: "Windows update broker unsupported by this worker" });
          return;
        }
        void Promise.resolve(deps.onUpdateBroker({
          request_id: update.requestId,
          job_id: update.jobId,
          action: update.action,
          manifest_url: update.manifestUrl,
          signature_url: update.signatureUrl,
          manifest_sha256: update.manifestSha256,
          publisher_sha256: update.publisherSha256,
        })).then((progress) => {
          for (const frame of progress) send({ kind: "update-progress", ...frame });
          const lastSequence = progress.length > 0 ? progress[progress.length - 1]!.sequence : 0;
          send({ kind: "rpc-ok", request_id: update.requestId, data: { last_sequence: lastSequence } });
        }).catch((error) => {
          send({ kind: "rpc-error", request_id: update.requestId, message: (error as Error).message });
        });
        return;
      }
      case "eventAck": {
        // D-4b: drop the acked entry from the unacked outbox so it
        // doesn't replay on the next reconnect.
        const seq = Number((v as { clientSeq: bigint }).clientSeq);
        outbox.ackEvent(seq);
        return;
      }
    }
  }

  return { handleDownstream };
}
