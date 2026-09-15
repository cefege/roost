// One row of the terminal context menu's mobile action sheet. Owns the row's
// focus, pointer, and keyboard activation contract so a remote or a screen
// reader can reach and fire it, not just a finger.
//
// Split out of TerminalContextMenu.tsx, which sat at the 400-line cap.
// Callers: TerminalContextMenu.tsx. Depends on: solid-js JSX types only.

import type { JSX } from "solid-js";

export function TerminalSheetItem(props: {
  testid: string;
  onClick: () => void;
  danger?: boolean;
  disabled?: boolean;
  children: JSX.Element;
}) {
  return (
    <div
      data-testid={props.testid}
      role="menuitem"
      aria-disabled={props.disabled ? "true" : undefined}
      tabIndex={props.disabled ? -1 : 0}
      onClick={() => { if (!props.disabled) props.onClick(); }}
      // A <div> fires no click for Enter/Space, so a focusable row without this
      // is reachable but dead — the only activation a D-pad's OK button has.
      onKeyDown={(e) => {
        if (props.disabled) return;
        if (e.key !== "Enter" && e.key !== " ") return;
        e.preventDefault();
        props.onClick();
      }}
      style={{
        padding: "14px 20px",
        cursor: props.disabled ? "default" : "pointer",
        // Disabled is opacity, not a colour swap: the role colour must survive so
        // a destructive row stays destructive and --text-lo keeps meaning idle.
        opacity: props.disabled ? "0.4" : undefined,
        color: props.danger ? "var(--md-error)" : "var(--text-hi)",
        "min-height": "44px",
        display: "flex",
        "align-items": "center",
        gap: "var(--md-space-2)",
      }}
      onTouchStart={(e) => { if (!props.disabled) (e.currentTarget as HTMLElement).style.background = "var(--border-strong)"; }}
      onTouchEnd={(e) => { if (!props.disabled) (e.currentTarget as HTMLElement).style.background = "transparent"; }}
    >
      {props.children}
    </div>
  );
}
