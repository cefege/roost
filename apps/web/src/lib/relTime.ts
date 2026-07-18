// Compact relative-age formatter shared by sidebar surfaces.
// Used by: SwarmCard (age from last-message ts), QueueView (enqueued_at_ms).
// Both call sites pre-date this; were duplicated until phase-sb1.

export function relTimeSince(ms: number): string {
  const d = Math.max(0, Math.floor((Date.now() - ms) / 1000));
  if (d < 60) return `${d}s`;
  if (d < 3600) return `${Math.floor(d / 60)}m`;
  if (d < 86400) return `${Math.floor(d / 3600)}h`;
  return `${Math.floor(d / 86400)}d`;
}
