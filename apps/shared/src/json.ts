// Defensive JSON boundary. Both directions exist here so no caller forks a
// second replacer or a second try/catch:
//   parse  — a partial-write / hand-edited row must not throw AFTER the
//            surrounding mutation committed (the 5xx-with-persisted-state
//            split brain seen in mcpCreate + taskRowToWire + host_metrics_json)
//   encode — a value JSON cannot express must not throw out of the observer
//            that was only describing it (diagnostics, audit fields).
//
// Callers should pass `fallback` matching the consumer's wire schema:
// `{}` for non-nullable record fields, `null` for nullable ones.

export function safeJsonParse<T = unknown>(
  s: string | null | undefined,
  fallback: T,
  target: string,
): T {
  if (!s) return fallback;
  try { return JSON.parse(s) as T; }
  catch {
    return fallback;
  }
}

/** Encode `value` for a field whose producer must survive a hostile payload.
 *  bigint becomes its decimal string: proto uint64 fields (input_seq, frame
 *  seq) carry real information that JSON.stringify refuses outright, and
 *  Number() would silently round past 2^53. Anything JSON cannot express at
 *  all — a cycle, a throwing getter — yields `fallback`, because a field the
 *  consumer only reads must never break the path that produced it. */
export function safeJsonStringify(value: unknown, fallback: string): string {
  try {
    return JSON.stringify(
      value,
      (_key, item: unknown) => (typeof item === "bigint" ? item.toString() : item),
    ) ?? fallback;
  } catch {
    return fallback;
  }
}
