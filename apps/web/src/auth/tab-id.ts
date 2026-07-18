// Per-tab UUID. Sent as `x-roost-tab-id` on every Connect request so
// coord can distinguish multiple tabs from the SAME browser (same
// EdDSA fingerprint, different windows). Without it the viewport
// claim Map keyed by viewer_fp collapses across tabs — last writer
// wins, withdraws cross-fire, PTY ping-pongs sizes between tabs.
//
// sessionStorage scope: survives reload, distinct per tab. localStorage
// would be SHARED across tabs of the same browser → wrong.

const KEY = "roost.tabId";

let _cached: string | null = null;

export function getTabId(): string {
  if (_cached) return _cached;
  if (typeof sessionStorage === "undefined") {
    _cached = _randomId();
    return _cached;
  }
  const stored = sessionStorage.getItem(KEY);
  if (stored) { _cached = stored; return stored; }
  const fresh = _randomId();
  try { sessionStorage.setItem(KEY, fresh); } catch { /* private mode / quota */ }
  _cached = fresh;
  return fresh;
}

function _randomId(): string {
  // crypto.randomUUID is universal in modern browsers; fall back to
  // a 16-byte hex if unavailable (older WebViews, jsdom test env).
  const c = globalThis.crypto;
  if (c?.randomUUID) return c.randomUUID();
  const buf = new Uint8Array(16);
  if (c?.getRandomValues) c.getRandomValues(buf);
  else for (let i = 0; i < 16; i++) buf[i] = Math.floor(Math.random() * 256);
  return Array.from(buf).map((b) => b.toString(16).padStart(2, "0")).join("");
}
