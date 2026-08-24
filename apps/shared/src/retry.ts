// Exponential-backoff delay computation shared by every redial/retry site
// (web sync bootstrap+watchdog+store, worker coord-link, cli deploy). Five
// hand-rolled `Math.min(base * 2 ** attempt, cap)` copies had drifted on
// base/cap and none jittered — deterministic ladders synchronize a fleet of
// workers into retry waves against a restarting coordinator. Callers keep
// their own state machines; only the delay math lives here.

export type BackoffOptions = {
  baseMs: number;
  maxMs: number;
  /** "none" preserves the legacy deterministic ladder; default is equal jitter. */
  jitter?: "equal" | "full" | "none";
  /** Injectable for tests; defaults to Math.random. */
  rng?: () => number;
};

export function backoffDelayMs(attempt: number, opts: BackoffOptions): number {
  const capped = Math.min(opts.baseMs * 2 ** Math.max(0, attempt), opts.maxMs);
  const jitter = opts.jitter ?? "equal";
  if (jitter === "none") return capped;
  const rng = opts.rng ?? Math.random;
  if (jitter === "full") return Math.floor(rng() * capped);
  return Math.floor(capped / 2 + rng() * (capped / 2));
}
