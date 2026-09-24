// Desktop pane composer geometry: the dock holds only the pill's one-line
// resting height in the pane's flex flow, and a longer draft or a submission
// status overflows upward over the terminal instead of taking rows from it.
// TerminalComposeButton attaches it to the pane dock; CellTerminal receives the
// upward overflow and translates its display so the newest rows stay visible.

/** Pixels the pill currently extends above the dock's reserved resting row. */
export type PaneComposerGrowthListener = (growthPx: number) => void;

export interface TerminalComposePaneGeometry {
  attach(dock: HTMLDivElement): void;
  detach(): void;
}

interface TerminalComposePaneGeometryOptions {
  box: () => HTMLElement | undefined;
  input: () => HTMLTextAreaElement | undefined;
  onGrowth?: PaneComposerGrowthListener;
}

export function createTerminalComposePaneGeometry(
  options: TerminalComposePaneGeometryOptions,
): TerminalComposePaneGeometry {
  let dockEl: HTMLDivElement | undefined;
  let resizeObserver: ResizeObserver | undefined;
  let mutationObserver: MutationObserver | undefined;
  let observedBox: HTMLElement | undefined;
  let observedPane: HTMLElement | undefined;
  let publishedRest = -1;
  let publishedGrowth = 0;

  const publishGrowth = (growth: number) => {
    if (growth === publishedGrowth) return;
    publishedGrowth = growth;
    options.onGrowth?.(growth);
  };

  const update = () => {
    const dock = dockEl;
    const box = options.box();
    const input = options.input();
    if (!dock || !box || !input) return;
    const pane = dock.parentElement ?? undefined;
    // The box resizes as the draft grows; the pane resizes independently of the
    // fixed-height dock, and either can change whether the content still fits.
    if (observedBox !== box) {
      if (observedBox) resizeObserver?.unobserve(observedBox);
      resizeObserver?.observe(box);
      observedBox = box;
    }
    if (observedPane !== pane) {
      if (observedPane) resizeObserver?.unobserve(observedPane);
      if (pane) resizeObserver?.observe(pane);
      observedPane = pane;
    }
    if (dock.clientWidth === 0 || box.offsetHeight === 0) return;
    // Every PTY row change makes an inline TUI repaint, and one that repaints in
    // place leaves the rows a shrink pushed into history duplicated there. The
    // reserve is therefore the pill with a one-line field: typing never moves it.
    const minInputHeight = Number.parseFloat(getComputedStyle(input).minHeight) || 0;
    const fieldGrowth = Math.max(0, input.offsetHeight - minInputHeight);
    const rest = Math.round(box.offsetHeight - fieldGrowth);
    if (rest !== publishedRest) {
      publishedRest = rest;
      dock.style.setProperty("--term-chat-pane-rest", `${rest}px`);
    }
    // The pill grows up from the dock's bottom edge and the pane clips its top,
    // so the scroll fallback takes over once the WHOLE dock content (grown
    // field, submission status) no longer fits between the pane top and that
    // edge. Both measures are independent of which mode is active: no flapping.
    const dockRect = dock.getBoundingClientRect();
    const room = pane ? dockRect.bottom - pane.getBoundingClientRect().top : Number.POSITIVE_INFINITY;
    const constrained = dock.scrollWidth > dock.clientWidth + 1
      || rest > dock.clientHeight + 1
      || dockContentHeight(dock) > room + 1;
    dock.toggleAttribute("data-size-constrained", constrained);
    const overflowAbove = dockRect.top - box.getBoundingClientRect().top;
    publishGrowth(constrained ? 0 : Math.max(0, Math.round(overflowAbove)));
  };

  const detach = () => {
    resizeObserver?.disconnect();
    mutationObserver?.disconnect();
    resizeObserver = undefined;
    mutationObserver = undefined;
    observedBox = undefined;
    observedPane = undefined;
    dockEl = undefined;
    publishedRest = -1;
    publishGrowth(0);
  };

  return {
    attach(dock) {
      detach();
      dockEl = dock;
      resizeObserver = new ResizeObserver(update);
      resizeObserver.observe(dock);
      mutationObserver = new MutationObserver(update);
      mutationObserver.observe(dock, { childList: true, subtree: true, characterData: true });
      // Solid runs the dock ref before its children's refs.
      queueMicrotask(update);
    },
    detach,
  };
}

/** Stacked height of the dock's flex children plus their gaps. */
function dockContentHeight(dock: HTMLElement): number {
  const gap = Number.parseFloat(getComputedStyle(dock).rowGap) || 0;
  let height = 0;
  for (const child of dock.children) {
    if (child instanceof HTMLElement) height += child.offsetHeight;
  }
  return height + gap * Math.max(0, dock.children.length - 1);
}
