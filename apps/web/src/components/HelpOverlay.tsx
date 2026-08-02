// HelpOverlay — Shift+? modal. Lists all keyboard shortcuts grouped by category.
// Text filter + "Copy diagnostic" button (copies URL/UA/time to clipboard).
// Open/close state from lib/keyboardShortcuts.ts.
//
// Callers: App.tsx (always rendered; internally gated on helpOpen).
// Depends on: keyboardShortcuts signals only — no store reads.

import { createMemo, createSignal, For, Show, onMount, onCleanup } from "solid-js";
import type { JSX } from "solid-js";
import { helpOpen, closeHelp } from "../lib/keyboardShortcuts.ts";
import { Button } from "./Settings/md/primitives.tsx";
import { createOverlayPresence } from "../lib/overlayMotion.ts";

// ── Static shortcut catalogue ─────────────────────────────────────────────────

interface ShortcutEntry {
  category: string;
  label: string;
  binding: string;
  description?: string;
}

const SHORTCUTS: ShortcutEntry[] = [
  // Navigation
  { category: "Navigation", label: "Command palette / open terminal", binding: "⌘K" },
  { category: "Navigation", label: "Filter the sidebar", binding: "⌘F (no terminal on screen) / Ctrl+F" },
  { category: "Navigation", label: "Toggle sidebar", binding: "⌘B" },
  { category: "Navigation", label: "Move / open in sidebar", binding: "↑ ↓ ↵" },
  { category: "Navigation", label: "Move focus to the adjacent pane", binding: "⌘⌥← ↑ → ↓ / Ctrl+Alt+← ↑ → ↓" },
  { category: "Navigation", label: "Help", binding: "Shift+?" },
  { category: "Navigation", label: "Close modal / Escape", binding: "Esc" },
  // Terminal
  { category: "Terminal", label: "Context menu", binding: "Right-click" },
  { category: "Terminal", label: "New terminal in the focused pane (same folder & server)", binding: "⌘⌥T / Ctrl+Alt+T" },
  { category: "Terminal", label: "Focus tab 1–8 / last tab in the focused pane", binding: "⌘1–⌘8 / ⌘9 · Ctrl+1–8 / Ctrl+9" },
  { category: "Terminal", label: "Kill session", binding: "context menu" },
  { category: "Terminal", label: "Bring pane to front / push back", binding: "⌘↵ / middle-click / right-click" },
  { category: "Terminal", label: "Split right / split down", binding: "⌘D / ⌘⇧D" },
  { category: "Terminal", label: "Arrange — equalize pane sizes", binding: "Cmd+Opt+B" },
  { category: "Terminal", label: "Arrange — grid / columns / rows / main+stack", binding: "Cmd+Opt+G / E / R / V" },
  { category: "Terminal", label: "Copy selection / paste", binding: "⌘⇧C / ⌘⇧V" },
  { category: "Terminal", label: "Text size — bigger / smaller / reset", binding: "⌘+ / ⌘− / ⌘0" },
  { category: "Terminal", label: "Find in scrollback", binding: "⌘F / Ctrl+⇧F" },
  { category: "Terminal", label: "Find next / previous match", binding: "↵ / ⇧↵ · ⌘G / ⌘⇧G" },
  { category: "Terminal", label: "Close find", binding: "Esc" },
  // Settings
  { category: "Settings", label: "Open Settings", binding: "⌘," },
];

// ── Component ─────────────────────────────────────────────────────────────────

export function HelpOverlay() {
  const { present, setPanelRef } = createOverlayPresence(helpOpen, "panel");
  const [filter, setFilter] = createSignal("");
  let inputRef: HTMLInputElement | undefined;

  // Auto-focus filter input when overlay opens.
  createMemo(() => {
    if (helpOpen()) {
      requestAnimationFrame(() => inputRef?.focus());
    }
  });

  const filteredShortcuts = createMemo<ShortcutEntry[]>(() => {
    const q = filter().toLowerCase().trim();
    if (!q) return SHORTCUTS;
    return SHORTCUTS.filter(
      (s) =>
        s.label.toLowerCase().includes(q) ||
        s.category.toLowerCase().includes(q) ||
        (s.description?.toLowerCase().includes(q) ?? false),
    );
  });

  const grouped = createMemo(() => {
    const out = new Map<string, ShortcutEntry[]>();
    for (const s of filteredShortcuts()) {
      const list = out.get(s.category) ?? [];
      list.push(s);
      out.set(s.category, list);
    }
    return out;
  });

  function onEsc(e: KeyboardEvent) {
    if (e.key === "Escape" && helpOpen()) {
      closeHelp();
    }
  }
  onMount(() => window.addEventListener("keydown", onEsc));
  onCleanup(() => window.removeEventListener("keydown", onEsc));

  return (
    <Show when={present()}>
      <div
        style={{
          position: "fixed",
          inset: "0",
          display: "grid",
          "place-items": "center",
          background: "rgba(0,0,0,0.55)",
          "z-index": "60",
        }}
        data-testid="help-overlay"
        onClick={closeHelp}
      >
        <div
          ref={setPanelRef}
          onClick={(e) => e.stopPropagation()}
          style={{
            width: "640px",
            "max-height": "80vh",
            display: "flex",
            "flex-direction": "column",
            background: "var(--surface-2)",
            border: "1px solid var(--border-subtle)",
            "border-radius": "var(--md-shape-sm)",
            overflow: "hidden",
          }}
        >
          {/* Header */}
          <div
            style={{
              display: "flex",
              "align-items": "center",
              "justify-content": "space-between",
              padding: "14px 20px 10px",
              "border-bottom": "1px solid var(--border-subtle)",
            }}
          >
            <span style={{ "font-size": "15px", "font-weight": "600", color: "var(--text-hi)" }}>
              Roost help
            </span>
            <div style={{ display: "flex", gap: "8px" }}>
              <Button
                variant="tonal"
                data-testid="help-overlay-copy-diagnostic"
                onClick={() => void copyDiagnostic()}
              >
                Copy diagnostic
              </Button>
              <Button variant="text" onClick={closeHelp}>
                esc
              </Button>
            </div>
          </div>

          {/* Filter */}
          <div style={{ padding: "10px 20px 6px" }}>
            <input
              ref={inputRef}
              type="text"
              value={filter()}
              onInput={(e) => setFilter(e.currentTarget.value)}
              placeholder="Filter shortcuts…"
              data-testid="help-overlay-filter"
              style={{
                width: "100%",
                padding: "6px 10px",
                "border-radius": "var(--md-shape-xs)",
                background: "var(--surface-1)",
                border: "1px solid var(--border-subtle)",
                color: "var(--text-hi)",
                "font-size": "13px",
                outline: "none",
                "box-sizing": "border-box",
              }}
            />
          </div>

          {/* Shortcut list */}
          <div style={{ flex: "1", "overflow-y": "auto", padding: "6px 20px 16px" }}>
            <Show when={filteredShortcuts().length === 0}>
              <p style={{ "font-size": "var(--md-body-s-size)", color: "var(--text-lo)", "margin-top": "8px" }}>
                No matches.
              </p>
            </Show>
            <For each={Array.from(grouped().entries())}>
              {([cat, list]) => (
                <section style={{ "margin-bottom": "14px" }}>
                  <h2
                    style={{
                      "font-size": "10px",
                      "font-weight": "600",
                      "text-transform": "uppercase",
                      "letter-spacing": "0.06em",
                      color: "var(--text-lo)",
                      "margin-bottom": "4px",
                    }}
                  >
                    {cat}
                  </h2>
                  <ul style={{ "list-style": "none", margin: "0", padding: "0" }}>
                    <For each={list}>
                      {(s) => (
                        <li
                          data-testid="help-overlay-action"
                          data-action-id={`${s.category}:${s.label}`}
                          style={{
                            display: "flex",
                            "justify-content": "space-between",
                            "align-items": "center",
                            gap: "8px",
                            padding: "3px 0",
                            "font-size": "13px",
                          }}
                        >
                          <span style={{ color: "var(--text-hi)" }}>{s.label}</span>
                          <Show when={s.binding}>
                            <BindingChip>{s.binding}</BindingChip>
                          </Show>
                        </li>
                      )}
                    </For>
                  </ul>
                </section>
              )}
            </For>
          </div>
        </div>
      </div>
    </Show>
  );
}

// ── Sub-components ─────────────────────────────────────────────────────────────

function BindingChip(props: { children: JSX.Element }) {
  return (
    <kbd style={{
      "font-size": "11px",
      padding: "1px 6px",
      "border-radius": "var(--md-shape-xs)",
      background: "var(--surface-1)",
      border: "1px solid var(--border-subtle)",
      color: "var(--text-mid)",
      "font-family": "monospace",
      "white-space": "nowrap",
    }}>
      {props.children}
    </kbd>
  );
}

// ── Diagnostic copy ────────────────────────────────────────────────────────────

async function copyDiagnostic(): Promise<void> {
  const payload = {
    url: typeof window !== "undefined" ? window.location.href : "",
    ua: typeof navigator !== "undefined" ? navigator.userAgent : "",
    time: new Date().toISOString(),
  };
  try {
    await navigator.clipboard.writeText(JSON.stringify(payload, null, 2));
  } catch {
    // clipboard write may fail in insecure context; ignore silently
  }
}
