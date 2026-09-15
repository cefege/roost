// Request/response correlation over the local terminal socket: the per-session
// input lane and the scrollback page fetches. The socket owner (local-terminal.ts)
// passes itself in as the link, so this module never reaches back for socket
// state. Queue caps and the admission vocabulary come from terminal-input-lanes.ts;
// a socket generation boundary settles every outstanding request exactly once.

import { create } from "@bufbuild/protobuf";
import { diag, signal } from "@roost/shared/diag";
import {
  LocalScrollbackRequestSchema,
  type LocalScrollbackResponse,
  type LocalTerminalClientFrame,
} from "@roost/shared/proto/local_terminal_pb";
import { InputCommandSchema } from "@roost/shared/proto/sync_pb";
import { getSessionTraceId } from "../lib/diag.ts";
import type { LocalScrollbackQuery } from "../store/terminal-stream-transport.ts";
import {
  createTerminalInputLanes,
  type InputAdmission,
  type PendingTerminalInput,
} from "./terminal-input-lanes.ts";

const SCROLLBACK_TIMEOUT_MS = 15_000;

/** What a request needs from the socket that carries it. */
export interface LocalTerminalLink {
  ownsSession(sessionId: string): boolean;
  /** The ready socket's generation, or null while no socket is ready. */
  generation(): bigint | null;
  send(frame: LocalTerminalClientFrame["frame"]): boolean;
}

export type LocalInputResultKind = "inputAccepted" | "inputRejected" | "inputAmbiguous";

export interface LocalInputResult {
  sessionId: string;
  inputSeq: bigint;
  writtenBytes?: number;
  reason?: string;
}

interface LocalInputFence {
  generation: bigint;
}

interface ScrollbackWaiter {
  resolve(response: LocalScrollbackResponse): void;
  reject(error: Error): void;
  timer: ReturnType<typeof setTimeout>;
}

const lanes = createTerminalInputLanes<LocalInputFence>();
const scrollbackWaiters = new Map<string, ScrollbackWaiter>();
let nextRequestId = 0;

export function sendLocalInput(
  link: LocalTerminalLink,
  sessionId: string,
  bytes: Uint8Array,
  viewId?: string,
): InputAdmission {
  const generation = link.generation();
  if (generation === null || !link.ownsSession(sessionId)) {
    return { accepted: false, reason: "local terminal socket is not connected" };
  }
  const refusal = lanes.refuse(sessionId, bytes.byteLength);
  if (refusal) return { accepted: false, reason: refusal };
  const admitted = lanes.enqueue(sessionId, bytes, viewId, { generation });
  const sent = link.send({
    case: "input",
    value: create(InputCommandSchema, {
      sessionId,
      inputSeq: admitted.inputSeq,
      data: admitted.pending.bytes,
      // The socket itself is the fence; the worker never reads this field.
      domainGeneration: 0n,
      ...(viewId === undefined ? {} : { viewId }),
    }),
  });
  if (!sent) {
    lanes.finish(admitted.pending, {
      status: "rejected",
      inputSeq: admitted.inputSeq,
      writtenBytes: 0,
      reason: "local terminal socket did not accept input",
    });
    return { accepted: true, inputSeq: admitted.inputSeq, result: admitted.result };
  }
  lanes.markStarted(admitted.pending);
  diag("bytes.up_send", {
    sid: sessionId,
    session_trace_id: getSessionTraceId(sessionId),
    dir: "up",
    transport: "local",
    len: admitted.pending.bytes.byteLength,
    input_seq: admitted.inputSeq,
    view_id: viewId,
  });
  return { accepted: true, inputSeq: admitted.inputSeq, result: admitted.result };
}

export function settleLocalInput(
  kind: LocalInputResultKind,
  result: LocalInputResult,
  generation: bigint,
): void {
  const pending = lanes.find(result.sessionId, result.inputSeq);
  if (!pending || !pending.started || pending.fence.generation !== generation) return;
  if (kind === "inputAccepted") {
    const writtenBytes = result.writtenBytes ?? 0;
    lanes.finish(pending, writtenBytes === pending.bytes.byteLength
      ? { status: "accepted", inputSeq: pending.inputSeq, writtenBytes }
      : {
          status: "ambiguous",
          inputSeq: pending.inputSeq,
          writtenBytes,
          reason: "the worker accepted an incomplete input batch",
        });
    return;
  }
  if (kind === "inputRejected") {
    lanes.finish(pending, {
      status: "rejected",
      inputSeq: pending.inputSeq,
      writtenBytes: 0,
      reason: result.reason ?? "the worker rejected the input batch",
    });
    return;
  }
  lanes.finish(pending, {
    status: "ambiguous",
    inputSeq: pending.inputSeq,
    writtenBytes: result.writtenBytes ?? 0,
    reason: result.reason ?? "the worker could not confirm the input batch",
  });
}

/** The local mirror of SessionsGetScrollbackCells. Rejects when the socket
 * cannot answer, so the caller's existing retry path applies unchanged. */
export function requestLocalScrollbackCells(
  link: LocalTerminalLink,
  query: LocalScrollbackQuery,
): Promise<LocalScrollbackResponse> {
  if (link.generation() === null || !link.ownsSession(query.sessionId)) {
    return Promise.reject(new Error("local terminal socket is not connected"));
  }
  const requestId = `sb-${++nextRequestId}`;
  const { promise, resolve, reject } = Promise.withResolvers<LocalScrollbackResponse>();
  const sent = link.send({
    case: "scrollback",
    value: create(LocalScrollbackRequestSchema, {
      requestId,
      sessionId: query.sessionId,
      endRow: query.endRow,
      maxRows: query.maxRows,
      gridEpoch: query.gridEpoch,
    }),
  });
  if (!sent) {
    return Promise.reject(new Error("local terminal socket did not accept the request"));
  }
  scrollbackWaiters.set(requestId, {
    resolve,
    reject,
    timer: setTimeout(() => {
      scrollbackWaiters.delete(requestId);
      reject(new Error("local scrollback request timed out"));
    }, SCROLLBACK_TIMEOUT_MS),
  });
  return promise;
}

export function settleLocalScrollback(response: LocalScrollbackResponse): void {
  const waiter = scrollbackWaiters.get(response.requestId);
  if (!waiter) return;
  scrollbackWaiters.delete(response.requestId);
  clearTimeout(waiter.timer);
  if (response.error) waiter.reject(new Error(response.error));
  else waiter.resolve(response);
}

/** A generation boundary fails every outstanding request exactly once and never
 * replays it: the bytes may or may not have reached the PTY. */
export function failLocalRequests(reason: string): void {
  for (const pending of lanes.pending()) failPendingInput(pending, reason);
  lanes.resetSequence();
  for (const [requestId, waiter] of scrollbackWaiters) {
    scrollbackWaiters.delete(requestId);
    clearTimeout(waiter.timer);
    waiter.reject(new Error(reason));
  }
}

function failPendingInput(
  pending: PendingTerminalInput<LocalInputFence>,
  reason: string,
): void {
  const started = pending.started;
  lanes.finish(pending, started
    ? {
        status: "ambiguous",
        inputSeq: pending.inputSeq,
        writtenBytes: 0,
        reason: `${reason} after input was sent; the batch will not be retried`,
      }
    : {
        status: "rejected",
        inputSeq: pending.inputSeq,
        writtenBytes: 0,
        reason: `${reason} before input was sent`,
      });
  signal("input.drop_burst", {
    sid: pending.sessionId,
    reason: started ? "local_generation_ambiguous" : "local_generation_closed",
    cooldownKey: pending.sessionId,
  });
}
