// trace_id correlation. R0.15.
// Every HTTP request / WS frame / log line carries a TraceId so a
// `grep trace_id=abc123` reconstructs the request across coord+worker+
// browser logs.

import { asTraceId, type TraceId } from "./wire/brand.ts";

export const TRACE_HEADER = "x-roost-trace-id";

export function newTraceId(): TraceId {
  // 16 hex chars = 8 bytes of randomness, urlsafe, plenty unique for
  // a single-operator fleet's logs.
  const buf = new Uint8Array(8);
  crypto.getRandomValues(buf);
  const hex = Array.from(buf, (b) => b.toString(16).padStart(2, "0")).join("");
  return asTraceId(hex);
}
