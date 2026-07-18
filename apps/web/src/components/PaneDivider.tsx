import type { Accessor } from "solid-js";
import { createSignal } from "solid-js";
import type { DividerRect } from "../store/paneLayout.ts";
import { beginResizeDrag, endResizeDrag } from "../lib/resizeDrag.ts";

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
  function onPointerDown(e: PointerEvent) {
    if (e.button !== 0) return;
    e.preventDefault();
    e.stopPropagation(); // don't let the deck treat this as a focus-pane click
    beginResizeDrag();
    setDragging(true);
    // Snapshot: regionStart/regionLen/dir/splitId are fixed for the whole drag.
    const d = props.divider();
    const onDrag = props.onDrag;
    const onCommit = props.onCommit;
    const rect = props.deckEl()?.getBoundingClientRect();
    const originX = rect?.left ?? 0;
    const originY = rect?.top ?? 0;
    let last = d.ratio;
    let rafId = 0;
    const ratioFor = (ev: PointerEvent): number => {
      const pos = d.dir === "row" ? ev.clientX - originX : ev.clientY - originY;
      const raw = d.regionLen > 0 ? (pos - d.regionStart) / d.regionLen : 0.5;
      return Math.max(0.1, Math.min(0.9, raw));
    };
    // Coalesce pointermove → one layout update per frame; latest ratio wins.
    const flush = () => { rafId = 0; onDrag(d.splitId, last); };
    const onMove = (ev: PointerEvent) => {
      last = ratioFor(ev);
      if (!rafId) rafId = requestAnimationFrame(flush);
    };
    const onUp = () => {
      if (rafId) { cancelAnimationFrame(rafId); rafId = 0; }
      setDragging(false);
      endResizeDrag();
      onCommit(d.splitId, last);
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
      window.removeEventListener("pointercancel", onUp);
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
    window.addEventListener("pointercancel", onUp);
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
