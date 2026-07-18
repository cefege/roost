import { type JSX, type Component, Show, onMount, onCleanup } from "solid-js";

// ─── Sheet — token-driven overlay panel (design-system phase 1) ─────────────
// Scrim + panel for side/bottom/center modals (the FileViewerSheet pattern),
// all token-driven. Closes on scrim-click + Esc. For simple form modals prefer
// Dialog (real md-dialog); Sheet is for larger content surfaces.
export const Sheet: Component<{
  open: boolean;
  onClose: () => void;
  side?: "right" | "bottom" | "center";
  class?: string;
  children: JSX.Element;
}> = (props) => {
  onMount(() => {
    const onKey = (e: KeyboardEvent) => { if (e.key === "Escape" && props.open) props.onClose(); };
    document.addEventListener("keydown", onKey);
    onCleanup(() => document.removeEventListener("keydown", onKey));
  });
  const justify = () => props.side === "right" ? "flex-end" : props.side === "bottom" ? "center" : "center";
  const align = () => props.side === "bottom" ? "flex-end" : "center";
  return (
    <Show when={props.open}>
      <div
        onClick={props.onClose}
        style={{
          position: "fixed", inset: "0", "z-index": "1000",
          display: "flex", "justify-content": justify(), "align-items": align(),
          background: "color-mix(in srgb, var(--md-scrim) 55%, transparent)",
        }}
      >
        <div
          class={props.class}
          onClick={(e) => e.stopPropagation()}
          style={{
            background: "var(--surface-1)",
            "box-shadow": "var(--md-elev-3)",
            "border-radius": props.side === "center" ? "var(--md-shape-xl)" : "var(--md-shape-lg)",
            "max-width": props.side === "right" ? "min(560px, 92vw)" : "min(720px, 94vw)",
            "max-height": "92vh", overflow: "auto",
            ...(props.side === "right" ? { height: "100vh", "border-radius": "0" } : {}),
          }}
        >
          {props.children}
        </div>
      </div>
    </Show>
  );
};
