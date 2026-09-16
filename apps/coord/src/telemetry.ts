// Module-level telemetry counters. Tracks per-route request counts and error
// counts (4xx/5xx), with label cardinality capped so a caller cannot grow the
// maps by inventing request paths.
// Callers: middleware/security.ts (recordRequest / recordError through
//          recordAuditTelemetry), connect/handlers-system.ts
//          (getMetricsSnapshot for the metrics RPC).

const MAX_TELEMETRY_KEYS = 256;
// Counters past the cap collapse here: an unmatched request path is chosen by
// the caller, so key cardinality is the one dimension it could grow.
const TELEMETRY_OVERFLOW_KEY = "<other>";

const requestCounts = new Map<string, number>();
const errorCounts = new Map<string, number>();

let startMs = Date.now();

function inc(map: Map<string, number>, key: string): void {
  const current = map.get(key);
  if (current === undefined && map.size >= MAX_TELEMETRY_KEYS) {
    map.set(TELEMETRY_OVERFLOW_KEY, (map.get(TELEMETRY_OVERFLOW_KEY) ?? 0) + 1);
    return;
  }
  map.set(key, (current ?? 0) + 1);
}

// Record a completed request. path = URL pathname or Connect
// /<service>/<method>, already collapsed by the caller when it is unbounded.
export function recordRequest(path: string): void {
  inc(requestCounts, path);
}

// Record a 4xx or 5xx response. path = URL pathname.
export function recordError(path: string): void {
  inc(errorCounts, path);
}

export interface MetricsSnapshot {
  uptime_ms: number;
  requests: Record<string, number>;
  errors: Record<string, number>;
  total_requests: number;
  total_errors: number;
}

export function getMetricsSnapshot(): MetricsSnapshot {
  const requests: Record<string, number> = {};
  const errors: Record<string, number> = {};

  for (const [k, v] of requestCounts) requests[k] = v;
  for (const [k, v] of errorCounts) errors[k] = v;

  const total_requests = [...requestCounts.values()].reduce((s, v) => s + v, 0);
  const total_errors = [...errorCounts.values()].reduce((s, v) => s + v, 0);

  return {
    uptime_ms: Date.now() - startMs,
    requests,
    errors,
    total_requests,
    total_errors,
  };
}
