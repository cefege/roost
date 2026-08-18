import { createSignal } from "solid-js";

// True while the user drags a pane divider (PaneDivider) or the desktop sidebar
// resizer (AppShell). CellTerminal gates its ResizeObserver-driven PTY resize
// claims on this: a drag would otherwise round-trip a full grid rebuild per
// paused frame (the "laggy resize" bug). Instead it suppresses claims during the
// drag and fires ONE claim after the final live owner releases.
const [isResizeDragging, setResizeDragging] = createSignal(false);
export { isResizeDragging };

const MAX_RESIZE_DRAG_OWNERS = 32;
const liveOwners = new Set<number>();
let generation = 0;
let nextOwner = 0;

// The returned release belongs only to this drag. A stale or repeated release
// cannot end another gesture. The hard cap defends the page-lifetime singleton
// from abandoned owners: an impossible overflow invalidates the old generation,
// then admits the new gesture without briefly dropping resize suppression.
export function beginResizeDrag(): () => void {
  if (liveOwners.size >= MAX_RESIZE_DRAG_OWNERS) {
    liveOwners.clear();
    generation++;
    nextOwner = 0;
  }

  while (liveOwners.has(nextOwner)) {
    nextOwner = (nextOwner + 1) % MAX_RESIZE_DRAG_OWNERS;
  }
  const owner = nextOwner;
  nextOwner = (nextOwner + 1) % MAX_RESIZE_DRAG_OWNERS;
  const ownerGeneration = generation;
  let released = false;
  liveOwners.add(owner);
  setResizeDragging(true);

  return () => {
    if (released) return;
    released = true;
    if (ownerGeneration !== generation || !liveOwners.delete(owner)) return;
    if (liveOwners.size === 0) setResizeDragging(false);
  };
}

// Page-lifecycle teardown invalidates outstanding closures as well as clearing
// the current owners. A release retained by the old page is therefore inert.
export function resetResizeDrags(): void {
  generation++;
  liveOwners.clear();
  nextOwner = 0;
  setResizeDragging(false);
}

type ResizePointerTarget = EventTarget & {
  setPointerCapture?: (pointerId: number) => void;
  hasPointerCapture?: (pointerId: number) => boolean;
  releasePointerCapture?: (pointerId: number) => void;
};

type ResizeDragHost = EventTarget & {
  requestAnimationFrame: (callback: FrameRequestCallback) => number;
  cancelAnimationFrame: (handle: number) => void;
};

// Owns the complete lifetime of one pointer resize: token, capture, listeners,
// and its single coalesced frame. Pointer termination settles the latest sampled
// geometry synchronously; explicit disposal aborts without committing it.
export function beginPointerResizeDrag<T>(options: {
  target: ResizePointerTarget;
  pointerId: number;
  initialGeometry: T;
  geometryFor: (event: PointerEvent) => T;
  onMove: (geometry: T) => void;
  onCommit: (geometry: T) => void;
  onRelease?: () => void;
  capturePointer?: boolean;
  host?: ResizeDragHost;
}): () => void {
  const host = options.host ?? window;
  const releaseOwner = beginResizeDrag();
  let latestGeometry = options.initialGeometry;
  let frameId: number | undefined;
  let active = true;

  const flushMove = () => {
    frameId = undefined;
    if (active) options.onMove(latestGeometry);
  };
  const onPointerMove: EventListener = (event) => {
    const pointerEvent = event as PointerEvent;
    if (pointerEvent.pointerId !== options.pointerId) return;
    latestGeometry = options.geometryFor(pointerEvent);
    if (frameId === undefined) {
      frameId = host.requestAnimationFrame(flushMove);
    }
  };
  const onPointerFinish: EventListener = (event) => {
    if ((event as PointerEvent).pointerId === options.pointerId) finish(true);
  };
  const onBlur: EventListener = () => finish(true);
  const dispose = () => finish(false);
  function finish(commit: boolean): void {
    if (!active) return;
    active = false;
    if (frameId !== undefined) {
      host.cancelAnimationFrame(frameId);
      frameId = undefined;
    }
    host.removeEventListener("pointermove", onPointerMove);
    host.removeEventListener("pointerup", onPointerFinish);
    host.removeEventListener("pointercancel", onPointerFinish);
    host.removeEventListener("blur", onBlur);
    options.target.removeEventListener("lostpointercapture", onPointerFinish);
    if (options.capturePointer !== false) {
      try {
        if (!options.target.hasPointerCapture ||
            options.target.hasPointerCapture(options.pointerId)) {
          options.target.releasePointerCapture?.(options.pointerId);
        }
      } catch {
        // Synthetic pointers and detached elements can reject capture release.
      }
    }
    try {
      if (commit) options.onCommit(latestGeometry);
    } finally {
      try {
        options.onRelease?.();
      } finally {
        releaseOwner();
      }
    }
  }

  host.addEventListener("pointermove", onPointerMove);
  host.addEventListener("pointerup", onPointerFinish);
  host.addEventListener("pointercancel", onPointerFinish);
  host.addEventListener("blur", onBlur);
  options.target.addEventListener("lostpointercapture", onPointerFinish);
  if (options.capturePointer !== false) {
    try {
      options.target.setPointerCapture?.(options.pointerId);
    } catch {
      // Synthetic pointers can have an id that is not active in the UA.
    }
  }
  return dispose;
}

// Bumped once per arrange-preset commit (TerminalDeck doArrange). CellTerminal
// watches it and fires ONE band-bypassing settle claim per visible pane —
// arrange rect deltas are often within the ±CLAIM_BAND hold-anchor, so the
// passive ResizeObserver claim gets suppressed and the PTY keeps the stale
// grid. Deliberately NOT isResizeDragging: that flag means "user is dragging"
// and toggles the deck's data-resizing CSS (glow drop).
const [arrangeEpoch, setArrangeEpoch] = createSignal(0);
export { arrangeEpoch };
export function pulseArrange(): void { setArrangeEpoch((n) => n + 1); }
