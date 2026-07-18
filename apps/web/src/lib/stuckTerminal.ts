// Stuck-terminal escape (companion to deadRouteSafetyNet).
//
// A terminal route (/s, /t, /w) whose URL resolves to no open session paints a
// blank pane — over the near-black app background that reads as a "black screen"
// the user can't escape. The dead-route safety net auto-bounces such a route
// Home, but ONLY once the session list has hydrated. When bootstrap can't
// complete — the coordinator is unreachable (a fresh load that lands mid coord
// restart) or this browser isn't paired with the coord — hydration never
// arrives, the safety net waits forever, and the user is stranded on a black
// void with no way out.
//
// This watcher fills that gap: while a terminal route is blank AND bootstrap is
// genuinely stuck, it flips a flag so MainPane can render an actionable card
// (Reconnecting… / Pair this browser, plus a Go-home escape) instead of
// nothing. Debounced so a healthy load's sub-second hydration never flashes it.
// The hydrated-but-gone case is intentionally left to deadRouteSafetyNet (it
// auto-bounces there within its grace window).
//
// Same split as deadRouteSafetyNet: a pure state machine
// (createStuckTerminalWatcher, unit-testable under `bun test` where createEffect
// is inert) plus a thin reactive installer (installStuckTerminalWatcher).

import { createEffect, createSignal, onCleanup, type Accessor } from "solid-js";

export type StuckKind = "connecting" | "unpaired";

export interface StuckTerminalDeps {
  onTerminalRoute: () => boolean; // on a terminal route (/s /t /w)
  hasOpenSession: () => boolean; // URL resolved to an OPEN session
  hydrated: () => boolean; // sessionsHydrated()
  unauthorized: () => boolean; // rootStore.browser_unauthorized
  onChange: (kind: StuckKind | null) => void; // emit the display state
  graceMs?: number; // debounce before showing (default 600)
}

export interface StuckTerminalWatcher {
  /** One evaluation of the state machine — the reactive effect body. */
  evaluate: () => void;
  /** Cancel any pending show timer (owner/effect teardown). */
  dispose: () => void;
}

const DEFAULT_GRACE_MS = 600;

// Which stuck-card to show, or null when the route is fine / handled elsewhere.
// Unauthorized wins over unhydrated: an untrusted browser's lists reject as
// Unauthenticated (so hydration also never lands) but the actionable fix is
// pairing, not waiting. hydrated + authorized + no session → null: that's a
// durably-gone route, which deadRouteSafetyNet bounces.
function classify(deps: StuckTerminalDeps): StuckKind | null {
  if (!deps.onTerminalRoute() || deps.hasOpenSession()) return null;
  if (deps.unauthorized()) return "unpaired";
  if (!deps.hydrated()) return "connecting";
  return null;
}

// State machine backing the watcher. `evaluate` is the effect body; it holds the
// debounce `timer` + last-emitted `shown` across runs. Call once per input
// change (the reactive effect does this in the app; a test drives it directly).
export function createStuckTerminalWatcher(deps: StuckTerminalDeps): StuckTerminalWatcher {
  const graceMs = deps.graceMs ?? DEFAULT_GRACE_MS;
  let timer: Timer | undefined;
  let shown: StuckKind | null = null;
  function emit(kind: StuckKind | null): void {
    if (kind === shown) return;
    shown = kind;
    deps.onChange(kind);
  }
  function evaluate(): void {
    clearTimeout(timer);
    const kind = classify(deps);
    if (!kind) {
      emit(null);
      return;
    } // not stuck → hide now (a recovery within grace clears the timer here)
    if (shown) {
      emit(kind);
      return;
    } // already showing → update the kind live, no re-debounce
    timer = setTimeout(() => emit(classify(deps)), graceMs); // re-check at fire time
  }
  function dispose(): void {
    clearTimeout(timer);
  }
  return { evaluate, dispose };
}

// Install the watcher as a reactive effect in the current Solid owner and return
// an accessor for the current stuck-card kind (null when none).
export function installStuckTerminalWatcher(
  deps: Omit<StuckTerminalDeps, "onChange">,
): Accessor<StuckKind | null> {
  const [kind, setKind] = createSignal<StuckKind | null>(null);
  const watcher = createStuckTerminalWatcher({ ...deps, onChange: setKind });
  createEffect(() => {
    watcher.evaluate();
  });
  onCleanup(watcher.dispose);
  return kind;
}
