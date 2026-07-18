// Defensive JSON.parse helpers for code paths where a partial-write /
// hand-edited row would otherwise throw AFTER the surrounding mutation
// committed, producing a 5xx response while state is persisted —
// the split-brain pattern that surfaced in mcpCreate + taskRowToWire +
// host_metrics_json reads across the rewrite.
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
