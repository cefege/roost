// Chunk-mismatch recovery predicate.
//
// After a redeploy, `dist/assets/*` is rebuilt with fresh content hashes and the
// previous files are gone. A tab that is still running (or that Safari restored
// from its own cache) then asks for a hash that 404s, and the app dies with a
// module-load error instead of picking up the new build.
//
// Vite dispatches `vite:preloadError` for failures inside its dynamic-import
// wrapper, and main.tsx reloads on that. It does NOT fire when the failure is a
// TOP-LEVEL module script — which is the case Safari reports as "Importing a
// module script failed", observed in production on 2026-07-29 after a coord
// redeploy. So the window `error` handler needs the same recovery, and that
// means matching on the message text: the engines disagree on wording and none
// of them expose a structured code.
//
// Kept as a pure function so the matching and the loop-guard are unit-testable;
// main.tsx owns the listener, the sessionStorage counter, and the reload.

/** Per-engine wording for "a module I asked for did not load". */
const CHUNK_ERROR_PATTERNS: readonly RegExp[] = [
  // Safari
  /importing a module script failed/i,
  // Chrome / Edge
  /failed to fetch dynamically imported module/i,
  /error loading dynamically imported module/i,
  // Firefox
  /error loading a module script/i,
  // Generic chunk-name shapes other bundlers surface
  /loading chunk \S+ failed/i,
  /loading css chunk \S+ failed/i,
];

export function isChunkLoadError(message: string | undefined | null): boolean {
  if (!message) return false;
  return CHUNK_ERROR_PATTERNS.some((re) => re.test(message));
}

/** Cooldown between recovery reloads, matching the vite:preloadError path. */
export const CHUNK_RELOAD_COOLDOWN_MS = 10_000;
/** A reload that doesn't fix it must not loop forever: if the NEW index.html is
 *  itself broken (bad deploy) or the box is offline, spinning would hide the
 *  real error. After this many attempts in one tab session, stop and let the
 *  error surface. Two allows for a first reload that raced a finishing deploy. */
export const CHUNK_RELOAD_MAX_ATTEMPTS = 2;
/** How long a recovery attempt stays "recent". Past this, the app has clearly
 *  been running fine since the last reload, so the counter resets — otherwise a
 *  tab left open across two separate deploys would exhaust its attempts for the
 *  rest of its life and stop self-healing. */
export const CHUNK_ATTEMPT_RESET_MS = 60_000;

/** Drop a stale attempt count; see CHUNK_ATTEMPT_RESET_MS. */
export function effectiveAttempts(attempts: number, lastReloadAt: number, now: number): number {
  if (lastReloadAt <= 0) return 0;
  return now - lastReloadAt > CHUNK_ATTEMPT_RESET_MS ? 0 : attempts;
}

export interface ChunkReloadInput {
  message: string | undefined | null;
  /** Date.now() at the error. */
  now: number;
  /** When this tab last performed a recovery reload; 0 = never. */
  lastReloadAt: number;
  /** navigator.onLine — offline means the network is missing, not the chunk. */
  online: boolean;
  /** Recovery reloads already attempted in THIS tab session. */
  attempts: number;
}

/** Should this error trigger a recovery reload? */
export function shouldReloadForChunkError(input: ChunkReloadInput): boolean {
  if (!isChunkLoadError(input.message)) return false;
  // Offline: the chunk is missing because the network is, and reloading a
  // no-cache index.html would just blank the app.
  if (!input.online) return false;
  if (input.attempts >= CHUNK_RELOAD_MAX_ATTEMPTS) return false;
  if (input.lastReloadAt > 0 && input.now - input.lastReloadAt < CHUNK_RELOAD_COOLDOWN_MS) return false;
  return true;
}
