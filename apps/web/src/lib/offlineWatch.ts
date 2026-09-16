// Terminal offline watch — decides when a VIEWED terminal pane whose view
// cannot deliver output has stayed that way long enough to be treated as "not
// responding", and spends silent re-claims before it says so.
//
// The case: a session the coordinator still marks `status:"open"` but whose PTY
// the worker lost. The worker deliberately keeps the row as an offline
// "breadcrumb" (apps/worker/src/boot-reconcile.ts) so the sidebar keeps your
// place — but no cell frame ever arrives, so the pane paints blank with no
// explanation. A pane that painted and THEN lost its view is the same silence
// with a stale screen still on display; both need a mechanism that re-claims
// the view, and nothing else in the pane will.
//
// Output silence is NOT evidence. A shell with nothing to print is quiet
// indefinitely and must never be accused, so the accusation is driven by the
// VIEW being undeliverable while the operator looks at it: the `detached`
// presentation state (`deriveTerminalPresentationState`) — no accepted, active,
// baseline-ready view, sustained past its own grace. The full accusation is
// "viewed + detached past graceMs, and `retries` silent re-claims — each
// followed by a further grace window — all failed to produce a frame" = dead.
//
// Self-correcting: a frame painted recently proves the view delivers whatever
// its status claims, and a deliverable view explains any quiet, so either fact
// disarms the watch and clears offline.
//
// Pure state machine (no Solid, no DOM) so it is unit-testable under `bun test`
// where Solid's SSR build makes createEffect a no-op. CellTerminal drives
// `update` from a reactive effect over (viewed, detached, frame freshness).

export interface OfflineWatch {
  /** Feed the current pane facts. Arms a one-shot grace timer while a viewed
   *  pane's view is undeliverable; clears offline the moment a frame paints,
   *  the view becomes deliverable, or the pane stops being viewed. Idempotent
   *  under repeated identical input. */
  update: (
    viewed: boolean,
    viewDetached: boolean,
    framePaintedRecently: boolean,
  ) => void;
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

  // Grace expired with the view still undeliverable. Spend a retry budget entry
  // on a silent re-claim and wait another grace window; only declare offline
  // once the budget is exhausted. The pane stays `armed` (a timer is always
  // pending) until offline is set.
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

  function update(
    viewed: boolean,
    viewDetached: boolean,
    framePaintedRecently: boolean,
  ): void {
    if (framePaintedRecently || !viewed || !viewDetached) {
      // A painted frame proves the PTY and the view are live; a deliverable
      // view accounts for any silence on its own; a pane nobody is looking at
      // never accuses. Either way: cancel any pending accusation, refresh the
      // retry budget, and clear.
      clearTimeout(timer);
      armed = false;
      attempts = 0;
      set(false);
      return;
    }
    // viewed && detached && quiet → arm the grace ONCE. Don't restart it on
    // repeat updates and don't re-arm once already offline.
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
