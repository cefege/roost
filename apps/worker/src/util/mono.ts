// Monotonic milliseconds for deadlines and ages.
//
// Every budget in the terminal-control path (keeper command results, viewport
// transaction phases, cell-emission gates) is a DURATION, and durations must not
// be measured with Date.now(): a host clock step (NTP correction, VM resume,
// operator change) silently extends or expires them, and two machines never
// agree on an absolute instant anyway. Bun.nanoseconds() is process-monotonic
// since process start, so a difference between two readings is always truthful.
//
// Wall-clock stays where an operator reads it (log/diag timestamps); it is never
// the source of a deadline.
export function monoNowMs(): number {
  return Bun.nanoseconds() / 1e6;
}
