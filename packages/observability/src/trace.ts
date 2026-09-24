// trace_id correlation. R0.15.
// Every HTTP request / WS frame / log line carries a TraceId so a
// `grep trace_id=abc123` reconstructs the request across coord+worker+
// browser logs.

import { z } from "zod";

export const TraceId = z.string().regex(/^[0-9a-f]{8,}$/i).brand<"TraceId">();
export type TraceId = z.infer<typeof TraceId>;
export const asTraceId = (s: string): TraceId => TraceId.parse(s);

export const TRACE_HEADER = "x-roost-trace-id";

export function newTraceId(): TraceId {
  // 16 hex chars = 8 bytes of randomness, urlsafe, plenty unique for
  // a single-operator fleet's logs.
  const buf = new Uint8Array(8);
  crypto.getRandomValues(buf);
  const hex = Array.from(buf, (b) => b.toString(16).padStart(2, "0")).join("");
  return asTraceId(hex);
}
