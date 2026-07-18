// Presentational bits for CommandPalette: the kind badge + keycap. Split out
// so CommandPalette.tsx stays under the 400-line cap. Called by the palette
// body only. (Folder kinds were removed when the picker moved to /browse.)

import { type Component } from "solid-js";

export type ItemKind = "session" | "workspace" | "action";

const KIND_COLOR: Record<ItemKind, string> = {
  session: "var(--status-info)",
  workspace: "var(--status-ok)",
  action: "var(--text-lo)",
};

const KIND_LABEL: Record<ItemKind, string> = {
  session: "session", workspace: "workspace", action: "action",
};

export const KindBadge: Component<{ kind: ItemKind }> = (props) => (
  <span style={{
    background: "transparent",
    border: `1px solid ${KIND_COLOR[props.kind]}`,
    color: KIND_COLOR[props.kind],
    "border-radius": "3px",
    padding: "0 4px",
    "font-size": "10px",
    "line-height": "14px",
    height: "16px",
    "flex-shrink": "0",
    "letter-spacing": "0.04em",
    "text-transform": "uppercase",
  }}>
    {KIND_LABEL[props.kind]}
  </span>
);

const Kbd: Component<{ children: string }> = (props) => (
  <kbd style={{
    background: "var(--bg-elev-3)",
    border: "1px solid var(--border-subtle)",
    "border-radius": "3px",
    padding: "1px 4px",
    "font-size": "10px",
    "font-family": "inherit",
    color: "var(--text-mid)",
  }}>
    {props.children}
  </kbd>
);

// Keyboard-legend footer. `hasResults` = the filtered list is non-empty.
export const CommandPaletteFooter: Component<{ hasResults: boolean }> = (props) => (
  <div data-testid="command-palette-footer"
    style={{ display: "flex", "align-items": "center", "justify-content": "space-between", gap: "12px", padding: "6px 16px", "border-top": "1px solid var(--md-sys-color-outline-variant)", background: "var(--md-sys-color-surface-container)", "font-size": "11px", color: "var(--md-sys-color-on-surface-variant)" }}
  >
    <span>{props.hasResults ? "Select to open" : "Type to search"}</span>
    <span style={{ display: "flex", "align-items": "center", gap: "12px" }}>
      <span style={{ display: "flex", "align-items": "center", gap: "4px" }}><Kbd>↑</Kbd><Kbd>↓</Kbd> navigate</span>
      <span style={{ display: "flex", "align-items": "center", gap: "4px" }}><Kbd>↵</Kbd> open</span>
      <span style={{ display: "flex", "align-items": "center", gap: "4px" }}><Kbd>esc</Kbd> close</span>
    </span>
  </div>
);
