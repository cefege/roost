// Help overlay. Keybindings doc + reference.
// Route: /help. Navigates back on Escape.

import { useNavigate } from "@solidjs/router";
import { onMount, onCleanup } from "solid-js";
import { platformShortcutLabel } from "../lib/browserPlatform.ts";

const KEYBINDINGS: Array<{ key: string; action: string }> = [
  { key: platformShortcutLabel("commandPalette", "⌘K"), action: "Command palette / open terminal" },
  { key: platformShortcutLabel("sidebarSearch", "⌘F"), action: "Search" },
  { key: platformShortcutLabel("toggleSidebar", "⌘B"), action: "Toggle sidebar" },
  { key: "↑ ↓ ↵", action: "Move / open in sidebar" },
  { key: platformShortcutLabel("settings", "⌘,"), action: "Settings" },
  { key: "Shift+?", action: "Help" },
  { key: "Esc", action: "Close overlay" },
];

export function Help() {
  const navigate = useNavigate();

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") navigate(-1);
  }

  onMount(() => window.addEventListener("keydown", onKey));
  onCleanup(() => window.removeEventListener("keydown", onKey));

  return (
    <div style={{ padding: "40px", color: "var(--text-hi)", "max-width": "480px" }}>
      <h2 style={{ "font-size": "18px", "margin-bottom": "20px" }}>Help / Keybindings</h2>
      <table style={{ width: "100%", "border-collapse": "collapse", "font-size": "13px" }}>
        <tbody>
          {KEYBINDINGS.map((kb) => (
            <tr style={{ "border-bottom": "1px solid var(--bg-elev-2)" }}>
              <td style={{ padding: "6px 0", color: "var(--color-ok)", "font-family": "monospace", width: "140px" }}>
                {kb.key}
              </td>
              <td style={{ padding: "6px 0", color: "var(--text-hi)" }}>{kb.action}</td>
            </tr>
          ))}
        </tbody>
      </table>
      <p style={{ "margin-top": "24px", "font-size": "12px", color: "var(--text-lo)" }}>
        Press Esc to close.
      </p>
    </div>
  );
}
