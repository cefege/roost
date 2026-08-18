// Pure browser-style navigation history for /browse file manager.
// Entries are canonical directory paths WITHOUT trailing slash:
// "~" (home), "/Users/dev/Code", "/". Never "".
// No Solid imports — pure + immutable, mirroring folderPalette.ts pattern.
//
// Called by: BrowsePage.tsx
// Tested by: apps/web/tests/browseHistory.test.ts

export interface HistoryState { entries: string[]; cursor: number }

export function initHistory(start: string): HistoryState {
  return { entries: [start], cursor: 0 };
}

export function pushHistory(state: HistoryState, path: string): HistoryState {
  const past = state.entries.slice(0, state.cursor + 1);
  if (past.length > 0 && past[past.length - 1] === path) return state;
  return { entries: [...past, path], cursor: past.length };
}

export function goBack(state: HistoryState): HistoryState {
  return state.cursor <= 0 ? state : { entries: state.entries, cursor: state.cursor - 1 };
}

export function goForward(state: HistoryState): HistoryState {
  return state.cursor >= state.entries.length - 1
    ? state
    : { entries: state.entries, cursor: state.cursor + 1 };
}

export function canGoBack(state: HistoryState): boolean { return state.cursor > 0; }
export function canGoForward(state: HistoryState): boolean {
  return state.cursor < state.entries.length - 1;
}