// Dead-route safety net (extracted from MainPane for unit testing).
//
// Never strand the user on a blank pane at a terminal route that resolves to no
// open session. But a LIVE session's URL-resolution can blip to null for a tick
// (a route-transition / reactive-settle window) — treating that transient as a
// death and navigating away synchronously is the "randomly bounced to Home"
// bug. So EVERY null goes through the same grace window + re-check: only a
// session still gone after `graceMs` navigates. A resolution that recovers
// within the window clears the pending timer → no bounce.
//
// The state machine (createDeadRouteSafetyNet) is split from the reactive
// wiring (installDeadRouteSafetyNet) so the decision logic is unit-testable
// under `bun test`, where Solid resolves to its SSR build and createEffect is a
// no-op. In the app, the effect calls `evaluate` on every dep change.

import { createEffect, onCleanup } from "solid-js";
import type { Session } from "@roost/shared/wire";

export interface DeadRouteSafetyNetDeps {
  onTerminalRoute: () => boolean; // are we on a terminal route?
  activeOpenSession: () => Session | null; // URL-resolved OPEN session, or null
  hydrated: () => boolean; // sessionsHydrated()
  bounceTarget: (lastOpen: Session | null) => string; // href to go to when durably gone
  navigate: (href: string) => void;
  onBounce?: (target: string, lastOpen: Session | null) => void; // diag hook, pre-navigate
  graceMs?: number; // default 2500
}

export interface DeadRouteSafetyNet {
  /** One evaluation of the state machine — the reactive effect body. Reads the
   *  dep accessors and schedules/cancels the grace-window bounce timer. */
  evaluate: () => void;
  /** Cancel any pending bounce timer (owner/effect teardown). */
  dispose: () => void;
}

const DEFAULT_GRACE_MS = 2500;

// State machine backing the safety net. `evaluate` is the effect body; it holds
// `timer`/`lastOpen` across runs. Call once per input change (the reactive
// effect does this in the app; a test drives it directly).
export function createDeadRouteSafetyNet(deps: DeadRouteSafetyNetDeps): DeadRouteSafetyNet {
  const graceMs = deps.graceMs ?? DEFAULT_GRACE_MS;
  let timer: Timer | undefined;
  let lastOpen: Session | null = null;
  function evaluate(): void {
    const open = deps.activeOpenSession();
    const onRoute = deps.onTerminalRoute();
    const hydrated = deps.hydrated();
    clearTimeout(timer);
    if (open) {
      lastOpen = open;
      return;
    } // live → remember it, never bounce
    if (!onRoute || !hydrated) return; // off-route / pre-hydration → wait
    timer = setTimeout(() => {
      if (deps.activeOpenSession()) return; // recovered within grace → cancel
      const target = deps.bounceTarget(lastOpen);
      deps.onBounce?.(target, lastOpen);
      deps.navigate(target);
    }, graceMs);
  }
  function dispose(): void {
    clearTimeout(timer);
  }
  return { evaluate, dispose };
}

// Install the safety net as a reactive effect in the current Solid owner. The
// effect re-runs `evaluate` whenever any accessor it reads changes.
export function installDeadRouteSafetyNet(deps: DeadRouteSafetyNetDeps): void {
  const net = createDeadRouteSafetyNet(deps);
  createEffect(() => {
    net.evaluate();
  });
  onCleanup(net.dispose);
}
