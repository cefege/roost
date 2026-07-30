// Reusable wrapper around the View Transitions API. Feature-detected and
// reduced-motion-guarded: callers pass the SYNCHRONOUS DOM mutation and get a
// smooth cross-fade where supported, an instant apply otherwise. The UA default
// `root` transition is already a cross-fade, so most callers need no extra CSS.
//
// No framework dependency so plain lib modules (lib/theme.ts) can use it too.
// Callers today: theme switching (Material "You" recolor cross-fade) and the
// Settings mobile push/pop (direction-aware slide via --settings-nav-dir).
//
// NOTE: only wrap SYNCHRONOUS mutations — the new snapshot is captured right
// after `mutate` returns, so async content (e.g. a dir fetch) would snapshot the
// stale/loading DOM. Live-terminal swaps are intentionally NOT wrapped: they add
// per-switch snapshot latency for no real gain (the deck already tweens rects).

type StartViewTransition = (cb: () => void) => unknown;


export function withViewTransition(mutate: () => void): void {
  const sv = (document as Document & { startViewTransition?: StartViewTransition })
    .startViewTransition;
  const reduced =
    typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;
  if (!sv || reduced) {
    mutate();
    return;
  }
  sv.call(document, mutate);
}
