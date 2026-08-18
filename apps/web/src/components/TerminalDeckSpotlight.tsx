// Pane spotlight ("Bring to front") overlay for TerminalDeck: the scrim that
// dims the tiled stack plus the card frame that rings/elevates the floated
// pane. Split out of TerminalDeck.tsx so the deck keeps only layout + gesture
// ownership. The floated TERMINAL itself is a normal deck slot (termStyle z9) —
// this file paints only the two chrome layers behind/around it.
//
// Callers: TerminalDeck.tsx (one instance, fed the centered card rect).

import { Show } from "solid-js";
import type { Rect } from "../store/paneLayout.ts";
import { clearSpotlight } from "../store/spotlight.ts";
import { prefersReducedMotion } from "../lib/prefersReducedMotion.ts";

export function TerminalDeckSpotlight(props: { rect: Rect | null }) {
  return (
    <Show when={props.rect}>
      {(r) => (
        <>
          {/* Reduced motion: skip the 120ms scrim fade and paint the END
              state (fully opaque) on the first frame. Read here, at the
              moment the spotlight opens — i.e. at animation start. */}
          <div
            class="pane-spotlight-backdrop"
            data-testid="pane-spotlight-backdrop"
            style={{ position: "absolute", inset: "0", "z-index": "7", ...(prefersReducedMotion() ? { animation: "none" } : {}) }}
            onPointerDown={(e) => { e.stopPropagation(); clearSpotlight(); }}
            onContextMenu={(e) => { e.preventDefault(); clearSpotlight(); }}
            aria-hidden="true"
          />
          <div
            class="pane-spotlight-card"
            style={{ position: "absolute", left: `${r().x}px`, top: `${r().y}px`, width: `${r().w}px`, height: `${r().h}px`, "z-index": "8", "pointer-events": "none" }}
            aria-hidden="true"
          />
        </>
      )}
    </Show>
  );
}
