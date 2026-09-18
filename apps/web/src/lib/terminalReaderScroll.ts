// The one reader-gesture scroll write for a terminal box, for input devices that
// produce no trusted scroll event (a controller stick or D-pad). It writes and
// nothing else: the pane's own scroll listener runs handleScroll(), which
// classifies and parks the reader byte-for-byte as it does for a wheel notch.
// Callers: lib/padActions.ts. Depends on: nothing.

/** One controller scroll tick, in CSS pixels — roughly a wheel notch. */
export const PAD_SCROLL_STEP_PX = 96;

/** Scroll `container` by `deltaPx`, clamped. Returns false when the box is
 *  already at that edge — the caller's signal to hand the direction to focus
 *  navigation instead of dead-ending in the pane. */
export function scrollTerminalReaderBox(container: HTMLElement, deltaPx: number): boolean {
	const max = Math.max(0, container.scrollHeight - container.clientHeight);
	if (max <= 0) return false;
	const next = Math.max(0, Math.min(container.scrollTop + deltaPx, max));
	if (next === container.scrollTop) return false;
	container.scrollTop = next;
	return true;
}
