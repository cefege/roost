// Human-readable byte size. Canonical for the web app — consolidates the
// former per-component copies (AttachmentChip / AttachmentsPane / MachinesPane).
export function formatBytes(n: number): string {
  if (n >= 1_073_741_824) return `${(n / 1_073_741_824).toFixed(1)} GB`;
  if (n >= 1_048_576) return `${(n / 1_048_576).toFixed(1)} MB`;
  if (n >= 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${n} B`;
}

// Transfer speed, e.g. "4.0 MB/s". "—" when unknown (no samples yet / zero).
export function formatSpeed(bytesPerSec: number): string {
  if (!isFinite(bytesPerSec) || bytesPerSec <= 0) return "—";
  return `${formatBytes(bytesPerSec)}/s`;
}

// Human-readable time-remaining, e.g. "45s", "2m 10s", "1h 5m". "" when unknown
// (negative) so callers can omit the "N left" suffix entirely.
export function formatEta(seconds: number): string {
  if (!isFinite(seconds) || seconds < 0) return "";
  if (seconds < 1) return "<1s";
  const s = Math.round(seconds);
  const m = Math.floor(s / 60);
  const h = Math.floor(m / 60);
  if (s < 60) return `${s}s`;
  if (m < 60) return s % 60 ? `${m}m ${s % 60}s` : `${m}m`;
  return m % 60 ? `${h}h ${m % 60}m` : `${h}h`;
}
