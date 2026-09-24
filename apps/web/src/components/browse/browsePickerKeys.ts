// Keyboard cursor for the folder picker's entry grid: left/right walk entries,
// up/down jump a whole row (the column count is read from the live grid, since
// CSS decides it), Enter drills, Backspace goes to the parent, and Escape
// leaves on compact. WorkerBrowsePage owns every signal this reads and mounts
// the window listener; this module owns only the key-to-intent mapping.
//
// Callers: WorkerBrowsePage.tsx.

export function createBrowsePickerKeys(deps: {
  surface: () => HTMLElement | undefined;
  results: () => HTMLElement | undefined;
  dialogOpen: () => boolean;
  scoped: () => boolean;
  compact: () => boolean;
  folderCount: () => number;
  columns: () => number;
  folderNameAt: (index: number) => string | undefined;
  activeIdx: () => number;
  setActiveIdx: (next: (idx: number) => number) => void;
  onDrill: (name: string) => void;
  onBack: () => void;
  onParent: () => void;
  onOpenHere: () => void;
  onEscape: () => void;
}): (event: KeyboardEvent) => void {
  // The cursor rests at -1 (nothing selected); the first arrow key in any
  // direction must land on entry 0 rather than jump a row.
  function move(event: KeyboardEvent, delta: number): void {
    event.preventDefault();
    const last = deps.folderCount() - 1;
    deps.setActiveIdx((idx) => (idx < 0 ? 0 : Math.max(0, Math.min(last, idx + delta))));
  }

  return (event: KeyboardEvent) => {
    if (event.defaultPrevented) return;
    if (deps.dialogOpen()) return;
    const surface = deps.surface();
    const eventPath = event.composedPath();
    if (!surface || !eventPath.includes(surface)) return;
    if (event.key === "Escape") {
      if (deps.compact()) {
        event.preventDefault();
        deps.onEscape();
      }
      return;
    }
    const results = deps.results();
    if (!deps.scoped() || !results || !eventPath.includes(results)) return;
    if (event.key === "ArrowRight") {
      move(event, 1);
    } else if (event.key === "ArrowLeft") {
      if (event.altKey) {
        event.preventDefault();
        deps.onBack();
        return;
      }
      move(event, -1);
    } else if (event.key === "ArrowDown") {
      move(event, Math.max(1, deps.columns()));
    } else if (event.key === "ArrowUp") {
      move(event, -Math.max(1, deps.columns()));
    } else if (event.key === "Backspace") {
      event.preventDefault();
      deps.onParent();
    } else if (event.key === "Enter") {
      event.preventDefault();
      const name = deps.folderNameAt(deps.activeIdx());
      if (name !== undefined) deps.onDrill(name);
      else deps.onOpenHere();
    }
  };
}
