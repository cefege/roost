// Badge text for the compact deck bar's terminal-count square
// (MobileDeckBar). With more than one terminal it reads "position/total" over
// the folder's flattened terminal order — the same order left/right swipe
// walks. `activeIndex` is a raw Array#findIndex result: 0-based, -1 when the
// painted terminal is not in the list. Displayed position is activeIndex + 1.

export interface DeckTabBadge {
  /** Glyphs inside the badge: "3/5", or the bare total. */
  text: string;
  /** Sentence for aria-label/title. */
  description: string;
  /** True when `text` is the fraction (the badge widens for it). */
  fraction: boolean;
}

export function deckTabBadge(tabCount: number, activeIndex: number): DeckTabBadge {
  const total = Math.max(0, Math.trunc(tabCount));
  const inRange = Number.isInteger(activeIndex) && activeIndex >= 0 && activeIndex < total;
  if (total > 1 && inRange) {
    const position = activeIndex + 1;
    return {
      text: `${position}/${total}`,
      description: `terminal ${position} of ${total} in this workspace`,
      fraction: true,
    };
  }
  return {
    text: `${total}`,
    description: `${total} terminal${total === 1 ? "" : "s"} in this workspace`,
    fraction: false,
  };
}
