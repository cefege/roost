// Generation-aware terminal input transport for Sync, and the single routing
// point that hands a session's input to the local worker socket instead. View
// membership and screen continuity live in store/terminal-stream.ts; queue
// caps, correlations and timeouts live in ws/terminal-input-lanes.ts. This
// module owns only the Sync fence and the Sync wire send.

import { diag, signal } from "@roost/shared/diag";
import { getSessionTraceId } from "../lib/diag.ts";
import { localTerminalTransport } from "../store/terminal-stream-transport.ts";
import {
  currentSyncV2TerminalState,
  registerSyncV2ControlHandler,
  registerSyncV2GenerationHandler,
  sendSyncV2Command,
  type SyncV2Control,
  type SyncV2TerminalState,
} from "../store/sync.ts";
import { _resetSmokeOutboundForTest } from "./sync-outbound-smoke.ts";
import {
  createTerminalInputLanes,
  resetTerminalInputLanes,
  type InputAdmission,
  type InputOutcome,
  type PendingTerminalInput,
} from "./terminal-input-lanes.ts";

export { setSmokeTerminalInputObserver } from "./sync-outbound-smoke.ts";
export type { SmokeTerminalInputObserver } from "./sync-outbound-smoke.ts";

type TerminalState = SyncV2TerminalState;
type ResultControl = SyncV2Control;
type OutboundCommand = Parameters<typeof sendSyncV2Command>[0];

/** The Sync socket and terminal domain generation a batch was admitted under. */
interface SyncInputFence {
  socketId: string;
  domainGeneration: bigint;
}

type SyncPendingInput = PendingTerminalInput<SyncInputFence>;

const lanes = createTerminalInputLanes<SyncInputFence>();
let observedSocketId: string | null = null;
let observedDomainGeneration: bigint | null = null;

function command(value: unknown): OutboundCommand {
  return value as OutboundCommand;
}

function trySendInput(
  pending: SyncPendingInput,
  state = currentSyncV2TerminalState(),
): void {
  if (pending.started || !state?.ready) return;
  if (
    state.socketId !== pending.fence.socketId
    || state.domainGeneration !== pending.fence.domainGeneration
  ) {
    lanes.finish(pending, {
      status: "rejected",
      inputSeq: pending.inputSeq,
      writtenBytes: 0,
      reason: "Sync generation closed before input was sent",
    });
    return;
  }
  const sent = sendSyncV2Command(command({
    case: "input",
    value: {
      sessionId: pending.sessionId,
      inputSeq: pending.inputSeq,
      data: pending.bytes,
      domainGeneration: pending.fence.domainGeneration,
      ...(pending.viewId === undefined ? {} : { viewId: pending.viewId }),
    },
  }));
  if (!sent) {
    lanes.finish(pending, {
      status: "rejected",
      inputSeq: pending.inputSeq,
      writtenBytes: 0,
      reason: "Sync input command was not admitted",
    });
    return;
  }
  lanes.markStarted(pending);
  diag("bytes.up_send", {
    sid: pending.sessionId,
    session_trace_id: getSessionTraceId(pending.sessionId),
    dir: "up",
    len: pending.bytes.byteLength,
    input_seq: pending.inputSeq,
    view_id: pending.viewId,
  });
}

function handleControl(control: ResultControl, state: TerminalState): void {
  if (
    control.case !== "inputAccepted"
    && control.case !== "inputRejected"
    && control.case !== "inputAmbiguous"
  ) return;
  const value = control.value;
  const pending = lanes.find(value.sessionId, value.inputSeq);
  if (
    !pending
    || !pending.started
    || pending.fence.socketId !== state.socketId
    || pending.fence.domainGeneration !== value.domainGeneration
    || pending.fence.domainGeneration !== state.domainGeneration
  ) return;

  if (control.case === "inputAccepted") {
    const accepted = control.value;
    if (accepted.writtenBytes === pending.bytes.byteLength) {
      lanes.finish(pending, {
        status: "accepted",
        inputSeq: pending.inputSeq,
        writtenBytes: accepted.writtenBytes,
      });
    } else {
      lanes.finish(pending, {
        status: "ambiguous",
        inputSeq: pending.inputSeq,
        writtenBytes: accepted.writtenBytes,
        reason: "coordinator accepted an incomplete input batch",
      });
    }
    return;
  }
  if (control.case === "inputRejected") {
    lanes.finish(pending, {
      status: "rejected",
      inputSeq: pending.inputSeq,
      writtenBytes: 0,
      reason: control.value.reason,
    });
    return;
  }
  lanes.finish(pending, {
    status: "ambiguous",
    inputSeq: pending.inputSeq,
    writtenBytes: control.value.writtenBytes,
    reason: control.value.reason,
  });
}

export function handleGeneration(state: TerminalState | null): void {
  const changed = !state
    || state.socketId !== observedSocketId
    || state.domainGeneration !== observedDomainGeneration;
  if (changed) {
    const closingSocket = observedSocketId;
    const closingDomain = observedDomainGeneration;
    for (const pending of lanes.pending()) {
      if (
        closingSocket !== null
        && (pending.fence.socketId !== closingSocket
          || pending.fence.domainGeneration !== closingDomain)
      ) continue;
      const outcome: InputOutcome = pending.started
        ? {
            status: "ambiguous",
            inputSeq: pending.inputSeq,
            writtenBytes: 0,
            reason: "Sync closed after input was sent; the batch will not be retried",
          }
        : {
            status: "rejected",
            inputSeq: pending.inputSeq,
            writtenBytes: 0,
            reason: "Sync closed before input was sent",
          };
      lanes.finish(pending, outcome);
      signal("input.drop_burst", {
        sid: pending.sessionId,
        reason: outcome.status === "ambiguous"
          ? "generation_ambiguous"
          : "generation_closed",
        cooldownKey: pending.sessionId,
      });
    }
    observedSocketId = state?.socketId ?? null;
    observedDomainGeneration = state?.domainGeneration ?? null;
    lanes.resetSequence();
  }

  if (!state?.ready) return;
  for (const pending of lanes.pending()) trySendInput(pending, state);
}

queueMicrotask(() => {
  registerSyncV2ControlHandler(handleControl);
  registerSyncV2GenerationHandler(handleGeneration);
});

/** Admit one complete PTY input batch on the transport that owns the session.
 * `viewId` is attribution only; callers without a mounted browser view
 * intentionally omit it. */
export function sendTerminalInput(
  sessionId: string,
  bytes: Uint8Array,
  viewId?: string,
): InputAdmission {
  const local = localTerminalTransport();
  if (local?.ownsSession(sessionId)) return local.sendInput(sessionId, bytes, viewId);
  const state = currentSyncV2TerminalState();
  if (!state) return { accepted: false, reason: "terminal Sync is not connected" };
  const refusal = lanes.refuse(sessionId, bytes.byteLength);
  if (refusal) return { accepted: false, reason: refusal };
  if (
    observedSocketId !== state.socketId
    || observedDomainGeneration !== state.domainGeneration
  ) handleGeneration(state);
  const admitted = lanes.enqueue(sessionId, bytes, viewId, {
    socketId: state.socketId,
    domainGeneration: state.domainGeneration,
  });
  trySendInput(admitted.pending, state);
  return { accepted: true, inputSeq: admitted.inputSeq, result: admitted.result };
}

/** Reject every credential-bound input lane and drop the local fast path. A
 * credential boundary is a hard transport boundary: queued bytes must never be
 * replayed onto a newly accepted socket, and a grant minted for the retired
 * credential must not survive it. */
export function resetTerminalOutboundState(reason = "credential boundary"): void {
  resetTerminalInputLanes(reason);
  observedSocketId = null;
  observedDomainGeneration = null;
  localTerminalTransport()?.reset(reason);
}

export function _resetTerminalOutboundForTest(): void {
  resetTerminalOutboundState("test reset");
  _resetSmokeOutboundForTest();
}
