import { createSignal } from "solid-js";

// The session id of the terminal currently floated (its pane is the one lifted).
const [_sid, _setSid] = createSignal<string | null>(null);
export const spotlightSessionId = _sid;
export const setSpotlightSessionId = (id: string | null) => _setSid(id);
export const clearSpotlight = () => _setSid(null);
export const isSpotlit = (id: string) => _sid() === id;

// Published by TerminalDeck each frame so the context menu can gate its item
// (spotlight only makes sense with 2+ panes).
const [_count, _setCount] = createSignal(0);
export const visiblePaneCount = _count;
export const setVisiblePaneCount = (n: number) => _setCount(n);
