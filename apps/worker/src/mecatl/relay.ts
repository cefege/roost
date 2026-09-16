// Runs coordinator-relayed HTTP exchanges against this machine's local Mecatl
// daemon and streams each reply back as head/body*/end frames. Built in
// coord-link-deps.ts, driven by the two relay arms in coord-link-downstream.ts,
// and dependent on daemon.ts, which holds the only copy of the daemon bearer.
// Method, path, headers and bytes pass through untouched: Roost transports the
// agent API here, it never models it.

import { log } from "@roost/shared/log";
import type { DMecatlRelayRequest } from "@roost/shared/proto/worker_transport_pb";
import type {
  MecatlRelayChunkFrame,
  TransportSendResult,
} from "../transport/coord-link-types.ts";
import {
  MecatlUnavailableError,
  unavailableReasonFor,
  type MecatlDaemon,
} from "./daemon.ts";

/** Worker-side ceilings for one machine's relay lane. They exist so a browser
 *  cannot turn the agent surface into an unbounded memory or socket consumer:
 *  each one ends the exchange with a named `error` instead. */
const RELAY_IN_FLIGHT_CAP = 8;
const REQUEST_BODY_CAP_BYTES = 1024 * 1024;
const RESPONSE_CAP_BYTES = 64 * 1024 * 1024;
const BODY_CHUNK_BYTES = 32 * 1024;
const UPSTREAM_IDLE_MS = 60_000;
const QUEUED_DRAIN_WAIT_MS = 5;
const RELAYABLE_METHODS: Record<string, true> = { GET: true, POST: true, DELETE: true, PATCH: true };
/** Log-only outcome: a coordinator cancellation is not a wire error, because
 *  the coordinator stopped reading before it asked. */
const CANCELLED = "cancelled";

export type MecatlRelayChunkSender = (chunk: MecatlRelayChunkFrame) => TransportSendResult;

export interface MecatlRelay {
  /** Fire-and-forget: every outcome, refusals included, reaches the
   *  coordinator as frames rather than a rejected promise. */
  handleRequest(frame: DMecatlRelayRequest): void;
  handleCancel(requestId: string): void;
  /** Aborts every in-flight exchange without answering: the transport that
   *  would carry the answer is the thing going away. */
  dispose(): void;
}

interface RelayExchange {
  controller: AbortController;
  /** Recorded before the abort so the catch can tell an idle daemon, a
   *  coordinator cancellation and a dead daemon apart — an AbortError alone
   *  says none of the three. */
  idle: boolean;
  cancelled: boolean;
}

/** Carries an already-decided `error` code out of the streaming path. */
class RelayRefusal extends Error {
  readonly code: string;
  constructor(code: string) {
    super(code);
    this.code = code;
  }
}

export function createMecatlRelay(deps: {
  daemon: MecatlDaemon;
  send: MecatlRelayChunkSender;
}): MecatlRelay {
  const inFlight = new Map<string, RelayExchange>();

  function handleRequest(frame: DMecatlRelayRequest): void {
    const requestId = frame.requestId;
    const method = frame.method.toUpperCase();
    log.info("worker", "mecatl_relay_start", {
      request_id: requestId,
      method,
      path: frame.path,
    });
    const refusal = refusalFor(frame, method);
    if (refusal) {
      deps.send({ request_id: requestId, end: true, error: refusal });
      log.info("worker", "mecatl_relay_end", {
        request_id: requestId,
        method,
        path: frame.path,
        status: 0,
        bytes: 0,
        error: refusal,
      });
      return;
    }
    // Registered synchronously: the in-flight cap has to hold against a burst
    // that arrives inside one turn of the event loop.
    const exchange: RelayExchange = {
      controller: new AbortController(),
      idle: false,
      cancelled: false,
    };
    inFlight.set(requestId, exchange);
    void relay(frame, method, exchange);
  }

  function refusalFor(frame: DMecatlRelayRequest, method: string): string | undefined {
    const state = deps.daemon.state();
    if (state.kind !== "ready") return unavailableReasonFor(state);
    if (!frame.path.startsWith("/v1/")) return "bad_path";
    if (!RELAYABLE_METHODS[method]) return "bad_method";
    if (frame.body.byteLength > REQUEST_BODY_CAP_BYTES) return "body_too_large";
    if (inFlight.size >= RELAY_IN_FLIGHT_CAP) return "relay_busy";
    return undefined;
  }

  async function relay(
    frame: DMecatlRelayRequest,
    method: string,
    exchange: RelayExchange,
  ): Promise<void> {
    const requestId = frame.requestId;
    let status = 0;
    let bytes = 0;
    let error: string | undefined;
    try {
      const response = await bounded(exchange, () => deps.daemon.request({
        method,
        path: frame.path,
        headers: forwardedRequestHeaders(frame.headersJson),
        body: frame.body.byteLength > 0 ? frame.body : undefined,
        signal: exchange.controller.signal,
      }));
      status = response.status;
      const responseHeaders: Record<string, string> = {};
      response.headers.forEach((value, name) => { responseHeaders[name] = value; });
      await emit({
        request_id: requestId,
        head: true,
        status,
        headers_json: JSON.stringify(responseHeaders),
      });
      bytes = await streamResponseBody(exchange, requestId, response);
      await emit({ request_id: requestId, end: true });
    } catch (cause) {
      error = errorCodeFor(exchange, cause);
      exchange.controller.abort();
      // A dropped transport cannot carry the refusal either, and a cancelled
      // exchange was already ended by the coordinator.
      if (error !== "transport_closed" && error !== CANCELLED) {
        deps.send({ request_id: requestId, end: true, error });
      }
    } finally {
      inFlight.delete(requestId);
      log.info("worker", "mecatl_relay_end", {
        request_id: requestId,
        method,
        path: frame.path,
        status,
        bytes,
        error: error ?? "",
      });
    }
  }

  async function streamResponseBody(
    exchange: RelayExchange,
    requestId: string,
    response: Response,
  ): Promise<number> {
    const body = response.body;
    if (!body) return 0;
    const reader = body.getReader();
    let bytes = 0;
    for (;;) {
      const { done, value } = await bounded(exchange, () => reader.read());
      if (done || !value) return bytes;
      bytes += value.byteLength;
      if (bytes > RESPONSE_CAP_BYTES) throw new RelayRefusal("response_too_large");
      for (let offset = 0; offset < value.byteLength; offset += BODY_CHUNK_BYTES) {
        await emit({
          request_id: requestId,
          body: value.subarray(offset, offset + BODY_CHUNK_BYTES),
        });
      }
    }
  }

  async function emit(chunk: MecatlRelayChunkFrame): Promise<void> {
    const result = deps.send(chunk);
    if (result === "dropped") throw new RelayRefusal("transport_closed");
    // "queued" means the link's bounded outbox took the frame instead of the
    // socket. Reading the daemon faster than that outbox drains would turn
    // upstream backpressure into a dropped frame, truncating the response.
    if (result === "queued") await Bun.sleep(QUEUED_DRAIN_WAIT_MS);
  }

  /** A daemon that produces nothing for a full minute ends the exchange. SSE
   *  keepalives keep a live agent run well inside the bound. */
  async function bounded<T>(exchange: RelayExchange, work: () => Promise<T>): Promise<T> {
    const idleTimer = setTimeout(() => {
      exchange.idle = true;
      exchange.controller.abort();
    }, UPSTREAM_IDLE_MS);
    try {
      return await work();
    } finally {
      clearTimeout(idleTimer);
    }
  }

  function errorCodeFor(exchange: RelayExchange, cause: unknown): string {
    if (exchange.cancelled) return CANCELLED;
    if (exchange.idle) return "upstream_idle";
    if (cause instanceof RelayRefusal) return cause.code;
    if (cause instanceof MecatlUnavailableError) return cause.reason;
    // The daemon was ready when this exchange started, so a transport failure
    // here means it died underneath the request.
    return unavailableReasonFor(deps.daemon.state());
  }

  function handleCancel(requestId: string): void {
    const exchange = inFlight.get(requestId);
    if (!exchange) return;
    exchange.cancelled = true;
    exchange.controller.abort();
    inFlight.delete(requestId);
  }

  function dispose(): void {
    for (const exchange of inFlight.values()) {
      exchange.cancelled = true;
      exchange.controller.abort();
    }
    inFlight.clear();
  }

  return { handleRequest, handleCancel, dispose };
}

/** The daemon bearer is set inside `MecatlDaemon.request`, after these, so a
 *  forwarded `authorization` header can never replace it. A frame whose header
 *  map did not survive the wire relays no headers rather than failing the
 *  exchange: the daemon then answers for itself. */
function forwardedRequestHeaders(headersJson: string): Record<string, string> {
  if (!headersJson) return {};
  let parsed: unknown;
  try {
    parsed = JSON.parse(headersJson);
  } catch {
    return {};
  }
  if (!parsed || typeof parsed !== "object") return {};
  const headers: Record<string, string> = {};
  for (const [name, value] of Object.entries(parsed as Record<string, unknown>)) {
    if (typeof value === "string") headers[name] = value;
  }
  return headers;
}
