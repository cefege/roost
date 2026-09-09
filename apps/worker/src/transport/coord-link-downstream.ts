// Coordinator -> worker frame dispatch for coord-link.ts: the handled
// CoordWorkerDown variants, the per-kind terminal-control admission slots, and
// the monotonic request budget derived from the coordinator's RELATIVE
// budget_ms. A variant with no case here is ignored, which is how retired
// schema tags stay inert. Reply frames go back out through the same outbox, so
// ordering relative to cells and raw metadata is unchanged.

import {
  TerminalInputStatus,
  TerminalStreamFailureKind,
  TerminalStreamStatus,
  TerminalWritePhase,
} from "@roost/shared/proto/worker_transport_pb";
import type {
  DAgentPrompt,
  CoordWorkerDown,
  DInputRequest,
  DTerminalPipelineSnapshotRequest,
  DTerminalSnapshotRequest,
  DTerminalStreamState,
  DKeeperUpdatePrepare,
} from "@roost/shared/proto/worker_transport_pb";
import { ClientControlFrame } from "@roost/shared/wire";
import { diag } from "@roost/shared/diag";
import { log } from "@roost/shared/log";
import {
  INPUT_REQUEST_INFLIGHT_CAP, TERMINAL_STREAM_REQUEST_INFLIGHT_CAP,
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
  // Stream-state and input requests have independent bounded admissions so a
  // keeper resize cannot consume the input lane.
  let inputRequestsInFlight = 0;
  let terminalStreamRequestsInFlight = 0;

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

  function sendImmediateInputResult(
    request: { requestId: string; sessionId: string; inputSeq: bigint },
    status: TerminalInputStatus,
    phase: TerminalWritePhase,
    reason: string,
  ): void {
    send({
      kind: "input-result",
      request_id: request.requestId,
      session_id: request.sessionId,
      input_seq: request.inputSeq,
      status,
      written_bytes: 0,
      phase,
      reason,
    });
  }

  function handleDownstream(frame: CoordWorkerDown, reconnected: boolean, socket: WebSocket): void {
    const k = frame.frame?.case;
    if (!k) return;
    const v = frame.frame.value;
    switch (k) {
      case "helloAck": {
        deps.onHelloAck?.({ reconnected });
        outbox.acceptHelloAck(reconnected);
        log.info("coord-link", "hello_ack", { reconnected });
        return;
      }
      case "ping": {
        const ts = Number((v as { ts: bigint }).ts);
        send({ kind: "pong", ts });
        return;
      }
      case "browserCommand": {
        const bc = v as { browserId: string; viewerId: string; requestId: string; frameJson: string };
        let command: ClientControlFrame;
        try {
          command = ClientControlFrame.parse(JSON.parse(bc.frameJson));
        } catch {
          log.warn("coord-link", "browser_command_parse", { request_id: bc.requestId });
          diag("transport.cmd_parse_failed", { request_id: bc.requestId });
          send({
            kind: "rpc-error",
            request_id: bc.requestId,
            message: "invalid browser command",
          });
          return;
        }
        // Producer/durable-store failures are intentionally outside the parse
        // catch: swallowing one would acknowledge a command whose lifecycle
        // mutation was not durably journaled.
        deps.onBrowserCommand?.({
          browser_id: bc.browserId,
          viewer_id: bc.viewerId,
          request_id: bc.requestId,
          frame: command,
        });
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
          // Nothing reached the session manager, so retry remains safe.
          diag("transport.terminal_admission_full", {
            kind: "input",
            in_flight: inputRequestsInFlight,
          });
          sendImmediateInputResult(
            request,
            TerminalInputStatus.REJECTED,
            TerminalWritePhase.PRE_WRITE,
            "worker input admission is full",
          );
          return;
        }
        inputRequestsInFlight += 1;
        // Invoke synchronously to preserve receive order into keeper admission;
        // the IIFE turns a synchronous throw into a correlated result.
        void (async () => deps.onInputRequest?.(request, terminalBudget(socket, request.budgetMs)))()
          .catch((error: unknown) => {
            // A thrown handler cannot prove which side of the write it reached.
            const message = error instanceof Error ? error.message : String(error);
            log.warn("coord-link", "input_request_failed", {
              request_id: request.requestId,
              error: message,
            });
            sendImmediateInputResult(
              request,
              TerminalInputStatus.AMBIGUOUS,
              TerminalWritePhase.UNKNOWN,
              message,
            );
          })
          .finally(() => { inputRequestsInFlight -= 1; });
        return;
      }
      case "agentPrompt": {
        const request = v as DAgentPrompt;
        if (!deps.onAgentPrompt) {
          sendImmediateInputResult(
            request,
            TerminalInputStatus.REJECTED,
            TerminalWritePhase.PRE_WRITE,
            "worker agent prompt handler is unavailable",
          );
          return;
        }
        if (inputRequestsInFlight >= INPUT_REQUEST_INFLIGHT_CAP) {
          diag("transport.terminal_admission_full", {
            kind: "agent_prompt",
            in_flight: inputRequestsInFlight,
          });
          sendImmediateInputResult(
            request,
            TerminalInputStatus.REJECTED,
            TerminalWritePhase.PRE_WRITE,
            "worker agent prompt admission is full",
          );
          return;
        }
        inputRequestsInFlight += 1;
        void (async () => deps.onAgentPrompt!(
          request,
          terminalBudget(socket, request.budgetMs),
        ))().catch(() => {
          log.warn("coord-link", "agent_prompt_failed", {
            request_id: request.requestId,
            session_id: request.sessionId,
            occupant_id: request.expectedOccupantId,
            outcome: "ambiguous",
          });
          sendImmediateInputResult(
            request,
            TerminalInputStatus.AMBIGUOUS,
            TerminalWritePhase.UNKNOWN,
            "worker agent prompt handler failed",
          );
        }).finally(() => { inputRequestsInFlight -= 1; });
        return;
      }
      case "terminalStreamState": {
        const request = v as DTerminalStreamState;
        if (terminalStreamRequestsInFlight >= TERMINAL_STREAM_REQUEST_INFLIGHT_CAP) {
          diag("transport.terminal_admission_full", {
            kind: "terminal_stream",
            in_flight: terminalStreamRequestsInFlight,
          });
          send({
            kind: "terminal-stream-result",
            request_id: request.requestId,
            session_id: request.sessionId,
            stream_id: request.streamId,
            enabled: request.enabled,
            status: TerminalStreamStatus.REJECTED,
            channel_resize_seq: 0n,
            effective_cols: 0,
            effective_rows: 0,
            resized: false,
            phase: TerminalWritePhase.PRE_WRITE,
            failure_kind: TerminalStreamFailureKind.RETRYABLE_PRE_WRITE,
            reason: "worker terminal-stream admission is full",
          });
          return;
        }
        terminalStreamRequestsInFlight += 1;
        void (async () => deps.onTerminalStreamState?.(
          request,
          terminalBudget(socket, request.budgetMs),
        ))().catch((error: unknown) => {
          const message = error instanceof Error ? error.message : String(error);
          log.warn("coord-link", "terminal_stream_request_failed", {
            request_id: request.requestId,
            error: message,
          });
          send({
            kind: "terminal-stream-result",
            request_id: request.requestId,
            session_id: request.sessionId,
            stream_id: request.streamId,
            enabled: request.enabled,
            status: TerminalStreamStatus.AMBIGUOUS,
            channel_resize_seq: 0n,
            effective_cols: 0,
            effective_rows: 0,
            resized: false,
            phase: TerminalWritePhase.UNKNOWN,
            failure_kind: TerminalStreamFailureKind.AMBIGUOUS_BOUNDARY,
            reason: message,
          });
        }).finally(() => { terminalStreamRequestsInFlight -= 1; });
        return;
      }
      case "terminalPipelineSnapshot": {
        deps.onTerminalPipelineSnapshot?.(v as DTerminalPipelineSnapshotRequest);
        return;
      }
      case "terminalSnapshotRequest": {
        deps.onTerminalSnapshotRequest?.(v as DTerminalSnapshotRequest);
        return;
      }
      case "keeperUpdatePrepare": {
        const request = v as DKeeperUpdatePrepare;
        if (!deps.onKeeperUpdatePrepare) {
          send({
            kind: "rpc-error",
            request_id: request.requestId,
            message: "keeper update preparation unsupported by this worker",
          });
          return;
        }
        void Promise.resolve(deps.onKeeperUpdatePrepare(request))
          .then((result) => send({
            kind: "rpc-ok",
            request_id: request.requestId,
            data: result,
          }))
          .catch((error) => send({
            kind: "rpc-error",
            request_id: request.requestId,
            message: error instanceof Error ? error.message : String(error),
          }));
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
        const clientSeq = (v as { clientSeq: bigint }).clientSeq;
        if (clientSeq > 0n && clientSeq <= BigInt(Number.MAX_SAFE_INTEGER)) {
          // Exact durable deletion commits before the volatile replay entry is
          // forgotten. Stale, future and unsafe acknowledgements are no-ops.
          outbox.ackEvent(Number(clientSeq));
        }
        return;
      }
    }
  }

  return { handleDownstream };
}
