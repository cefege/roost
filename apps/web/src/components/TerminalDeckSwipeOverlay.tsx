// Mobile forward-swipe affordance for TerminalDeck: the new-terminal surface
// that peeks out from behind the shrinking current terminal, plus the + FAB that
// grows under the finger and container-transforms into the full new terminal on
// commit. Split out of TerminalDeck.tsx so the deck keeps only layout + gesture
// ownership; the geometry/progress math is pure in lib/deckSwipe.ts and colour +
// elevation live in styles/sidebar.css (.deck-new-peek / .deck-new-fab).
//
// Callers: TerminalDeck.tsx (one instance, fed the live swipe state).

import { Show } from "solid-js";
import { isCompact } from "../lib/windowSizeClass.ts";
import { newFabProgress, newFabStyle, newPeekStyle, type Swipe } from "../lib/deckSwipe.ts";
import type { Rect } from "../store/paneLayout.ts";

export function TerminalDeckSwipeOverlay(props: {
  swipe: Swipe | null;
  /** The single compact pane's rect (view().panes[0]). */
  paneRect: Rect | undefined;
  deckWidth: number;
  stripH: number;
  /** Short cwd the pull would spawn into. */
  folderLabel: string;
}) {
  return (
    <Show when={isCompact() && props.swipe?.mode === "new-terminal"}>
      <div class="deck-new-peek" data-testid="deck-new-peek" style={newPeekStyle(props.swipe, props.paneRect, props.deckWidth, props.stripH)} aria-hidden="true">
        <div class="deck-new-peek__label">
          <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" aria-hidden="true"><path d="M12 5v14M5 12h14" /></svg>
          <span>New terminal · {props.folderLabel}</span>
        </div>
      </div>
      <div
        class="deck-new-fab"
        data-testid="deck-new-fab"
        data-armed={newFabProgress(props.swipe!.offset, props.deckWidth) >= 1 ? "true" : undefined}
        style={newFabStyle(props.swipe, props.paneRect, props.deckWidth, props.stripH)}
        aria-hidden="true"
      >
        <svg width="30" height="30" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" aria-hidden="true"><path d="M12 5v14M5 12h14" /></svg>
      </div>
    </Show>
  );
}
