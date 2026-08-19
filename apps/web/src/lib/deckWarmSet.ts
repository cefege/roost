// Bounded warm-pane policy for the deck (wired in TerminalDeck.tsx). Pure — no
// Solid, no JSX — so the cap and its recency order are unit-tested and a
// Playwright spec can import DECK_WARM_LIMIT as the single definition.
//
// Why a cap: a parked pane keeps its renderer and scrollback DOM, so switching
// back to it is a visibility flip instead of a remount plus a worker snapshot.
// But a parked pane also stays fully laid out on purpose (park geometry must be
// truthful — docs/FAILURE-INDEX.md "A parked pane paints at a lying box size"),
// so every mounted pane adds forced layout to every FUTURE switch: ~11 ms with
// one pane laid out against ~50 ms across 15+. Unbounded warmth therefore makes
// switch cost O(every session visited this page-load) — instant on a fresh page,
// permanently slower as terminals accumulate. Capping trades one cold remount
// on a pane the user left long ago for a fixed per-switch ceiling.

/** Extra warm panes beyond the ones the deck currently shows. Sized to the
 *  keyboard-reachable tab range (⌘1–⌘8, HelpOverlay "Focus tab 1–8 / last tab")
 *  so no shortcut ever lands on a cold mount; past that a pane is remounted
 *  from the worker's viewport-only snapshot and refills history on demand. */
export const DECK_WARM_LIMIT = 8;

/** Next warm set. Iteration order is least → most recently slotted; eviction
 *  takes from the front and never touches a currently-slotted id. Returns
 *  `previous` unchanged when both membership AND recency order are identical,
 *  so an unrelated layout commit re-styles nothing. */
export function nextWarmSessionIds(
  previous: ReadonlySet<string>,
  openIds: ReadonlySet<string>,
  slottedIds: readonly string[],
  limit = DECK_WARM_LIMIT,
): ReadonlySet<string> {
  // A slot naming a session the store no longer reports open is stale: holding
  // it warm would keep a mount for a session that can never stream again.
  const slotted = new Set<string>();
  for (const id of slottedIds) if (openIds.has(id)) slotted.add(id);
  // Dropping the slotted ids here and appending them below is what makes Set
  // insertion order the RECENCY order: whatever the deck currently shows moves
  // to the newest end, so the front of `next` is the true least-recently-shown
  // pane. Closed sessions leave entirely and cost the survivors nothing.
  const next = new Set<string>();
  for (const id of previous) if (openIds.has(id) && !slotted.has(id)) next.add(id);
  for (const id of slotted) next.add(id);
  // Only the non-slotted tail is capped, so a layout showing more panes than
  // the limit still keeps all of them warm — the deck may never unmount a pane
  // it is painting. Skipping slotted ids keeps that guarantee local to this
  // loop rather than resting on the insertion order above. Deleting the entry
  // the loop is standing on is well-defined and skips no later entry.
  let evictable = next.size - slotted.size - limit;
  for (const id of next) {
    if (evictable <= 0) break;
    if (slotted.has(id)) continue;
    next.delete(id);
    evictable--;
  }
  // Ref-stability: the caller feeds this straight to a signal setter, so an
  // equivalent result MUST be the same reference or every mounted pane
  // re-styles. Compare positions, not membership: same membership in a new
  // order means the user moved, and returning `previous` there would freeze
  // recency and let a later eviction drop the pane just left.
  if (next.size !== previous.size) return next;
  const prior = previous[Symbol.iterator]();
  for (const id of next) if (prior.next().value !== id) return next;
  return previous;
}
