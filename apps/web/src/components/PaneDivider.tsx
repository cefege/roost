import type { Accessor } from "solid-js";
import { createSignal, onCleanup } from "solid-js";
import type { DividerRect } from "../store/paneLayout.ts";
import { beginPointerResizeDrag } from "../lib/resizeDrag.ts";

// Draggable split divider on the seam between two panes. Under <Index> the
// node is STABLE across drag frames (only its position updates reactively),
// so a per-divider `dragging` signal drives a steady highlight and pointer
// state survives. Ratio updates are coalesced to one per animation frame so
// a 120Hz+ trackpad can't run the deck layout more than once per paint.
// Listens on WINDOW (not the element) so the gesture is robust even if the
// node is ever replaced. Callers: TerminalDeck.tsx (one per split).
export function PaneDivider(props: {
  divider: Accessor<DividerRect>;
  deckEl: () => HTMLElement | undefined;
  onDrag: (splitId: string, ratio: number) => void;
  onCommit: (splitId: string, ratio: number) => void;
}) {
  const [dragging, setDragging] = createSignal(false);
  let disposeGesture: (() => void) | undefined;

  onCleanup(() => disposeGesture?.());

  function onPointerDown(e: PointerEvent) {
    if (e.button !== 0) return;
    e.preventDefault();
    e.stopPropagation(); // don't let the deck treat this as a focus-pane click
    if (disposeGesture) return;

    const target = e.currentTarget as HTMLElement;
    // Snapshot: regionStart/regionLen/dir/splitId are fixed for the whole drag.
    const d = props.divider();
    const onDrag = props.onDrag;
    const onCommit = props.onCommit;
    const rect = props.deckEl()?.getBoundingClientRect();
    const originX = rect?.left ?? 0;
    const originY = rect?.top ?? 0;

    setDragging(true);
    disposeGesture = beginPointerResizeDrag({
      target,
      pointerId: e.pointerId,
      initialGeometry: d.ratio,
      geometryFor: (ev) => {
        const pos = d.dir === "row" ? ev.clientX - originX : ev.clientY - originY;
        const raw = d.regionLen > 0 ? (pos - d.regionStart) / d.regionLen : 0.5;
        return Math.max(0.1, Math.min(0.9, raw));
      },
      onMove: (ratio) => onDrag(d.splitId, ratio),
      onCommit: (ratio) => onCommit(d.splitId, ratio),
      onRelease: () => {
        disposeGesture = undefined;
        setDragging(false);
      },
    });
  }

  return (
    <div
      data-testid={`pane-divider-${props.divider().splitId}`}
      class="pane-divider"
      data-dir={props.divider().dir}
      data-dragging={dragging() ? "true" : undefined}
      onPointerDown={onPointerDown}
      style={{
        position: "absolute",
        left: `${props.divider().x}px`,
        top: `${props.divider().y}px`,
        width: `${props.divider().w}px`,
        height: `${props.divider().h}px`,
        cursor: props.divider().dir === "row" ? "col-resize" : "row-resize",
        "z-index": "4",
        "touch-action": "none",
      }}
    />
  );
}
