// Module-level telemetry counters. Tracks per-route request counts and error
// counts (4xx/5xx). Updated by recordRequest / recordError. Snapshot exposed
// via misc.metrics tRPC query.
// Callers: security middleware (recordRequest), trpc error handler (recordError),
//          misc.ts (getMetricsSnapshot).

const requestCounts = new Map<string, number>();
const errorCounts = new Map<string, number>();

let startMs = Date.now();

function inc(map: Map<string, number>, key: string): void {
  map.set(key, (map.get(key) ?? 0) + 1);
}

// Record a completed request. path = URL pathname (e.g. /api/trpc/workers.list).
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
