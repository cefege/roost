// Terminal offline watch — decides when a VIEWED terminal pane has received no
// screen frame for long enough to be treated as "not responding".
//
// The case: a session the coordinator still marks `status:"open"` but whose PTY
// the worker lost. The worker deliberately keeps the row as an offline
// "breadcrumb" (apps/worker/src/boot-reconcile.ts) so the sidebar keeps your
// place — but no cell frame ever arrives, so the pane paints blank with no
// explanation. This watch turns that silent blank into an explicit state.
//
// Signal: a live pane, once viewed (claimed), always gets a snapshot frame
// quickly; a breadcrumb never does. So the accusation is "viewed + no frame
// past graceMs, and `retries` silent re-claims — each followed by a further
// grace window — all failed to produce a frame" = dead.
// Self-correcting: the instant a frame arrives (hasFrame), offline clears.
//
// Pure state machine (no Solid, no DOM) so it is unit-testable under `bun test`
// where Solid's SSR build makes createEffect a no-op. CellTerminal drives
// `update` from a reactive effect over (viewed, hasFrame).

export interface OfflineWatch {
  /** Feed the current pane state. Arms a one-shot grace timer while the pane is
   *  viewed but frameless; clears offline the moment a frame lands or the pane
   *  stops being viewed. Idempotent under repeated identical input. */
  update: (viewed: boolean, hasFrame: boolean) => void;
  /** Cancel any pending grace timer (owner teardown). */
  dispose: () => void;
}

export function createOfflineWatch(
  graceMs: number,
  onChange: (offline: boolean) => void,
  onRetry?: () => void,
  retries = 2,
): OfflineWatch {
  let timer: Timer | undefined;
  let offline = false;
  let armed = false;
  let attempts = 0;

  const set = (v: boolean): void => {
    if (v === offline) return;
    offline = v;
    onChange(v);
  };

  // Grace expired with still no frame. Spend a retry budget entry on a silent
  // re-claim and wait another grace window; only declare offline once the
  // budget is exhausted. The pane stays `armed` (a timer is always pending)
  // until offline is set.
  function fire(): void {
    if (onRetry && attempts < retries) {
      attempts += 1;
      onRetry();
      timer = setTimeout(fire, graceMs);
      return;
    }
    armed = false;
    set(true);
  }

  function update(viewed: boolean, hasFrame: boolean): void {
    if (hasFrame || !viewed) {
      // A frame proves the PTY is live; not-viewed panes never accuse. Either
      // way: cancel any pending accusation, refresh the retry budget, and clear.
      clearTimeout(timer);
      armed = false;
      attempts = 0;
      set(false);
      return;
    }
    // viewed && !hasFrame → arm the grace ONCE. Don't restart it on repeat
    // updates and don't re-arm once already offline.
    if (offline || armed) return;
    armed = true;
    timer = setTimeout(fire, graceMs);
  }

  function dispose(): void {
    clearTimeout(timer);
    armed = false;
  }

  return { update, dispose };
}
