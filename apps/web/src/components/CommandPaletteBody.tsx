// CommandPaletteBody — the ⌘K palette's reactive body (session/workspace/
// action search). Split out of CommandPalette.tsx (perf sweep C1.1): the host
// stays always mounted but renders this ONLY while the palette is open, so the
// memo chain (defaultItems full-store build + filtered) exists only while open
// — zero work per WS tick while closed. Loaded lazily by the host (first ⌘K
// fetches this chunk).
//
// Folder browsing moved to /browse (BrowsePage.tsx) — this palette is now a
// pure session/action jumper. Callers: CommandPalette.tsx (host).

import { createMemo, createSignal, createEffect, on, For, Show, onMount, onCleanup } from "solid-js";
import { useNavigate } from "@solidjs/router";
import { cmdPaletteOpen, closeCmdPalette } from "../lib/keyboardShortcuts.ts";
import { isCompact } from "../lib/windowSizeClass.ts";
import { KindBadge, CommandPaletteFooter } from "./CommandPalettePieces.tsx";
import { type PaletteItem, matchesQuery, buildDefaultItems } from "./CommandPalette.data.ts";

export function PaletteBody(props: { setPanelRef: (el: HTMLElement) => void }) {
  const navigate = useNavigate();
  const [query, setQuery] = createSignal("");
  const [activeIdx, setActiveIdx] = createSignal(0);
  let inputRef: HTMLInputElement | undefined;
  let resultsRef: HTMLDivElement | undefined;

  const defaultItems = createMemo<PaletteItem[]>(() => buildDefaultItems(navigate));

  const filtered = createMemo<PaletteItem[]>(() => {
    // Lowercase the query ONCE per filter run (C1.3) — matchesQuery takes the
    // pre-lowered needle instead of re-lowering per item.
    const qLower = query().toLowerCase();
    return defaultItems().filter((it) => matchesQuery(`${it.label} ${it.hint ?? ""} ${it.search ?? ""}`, qLower));
  });

  // Reset the cursor on QUERY change only (C1.2) — not whenever the list
  // rebuilds, so a background WS tick can't yank the selection mid-arrowing.
  createEffect(on(query, () => setActiveIdx(0), { defer: true }));

  // Keep the keyboard-highlighted row in view — arrow nav must scroll the
  // results container to follow the selection. block:"nearest" only scrolls
  // when the row is off-screen (no-op when already visible, so hover doesn't
  // jump) and stays scoped to the scroll container (never yanks the page).
  createEffect(() => {
    const idx = activeIdx();
    const el = resultsRef?.querySelectorAll<HTMLElement>('[data-testid="command-palette-item"]')[idx];
    el?.scrollIntoView({ block: "nearest" });
  });

  // Clear + focus on open. The body mounts fresh per open.
  createEffect(() => {
    if (!cmdPaletteOpen()) return;
    setQuery("");
    requestAnimationFrame(() => inputRef?.focus());
  });

  function selectItem(it: PaletteItem) {
    closeCmdPalette();
    setQuery("");
    if (it.href) navigate(it.href);
    else if (it.action) void it.action();
  }

  function onKeydown(e: KeyboardEvent) {
    if (!cmdPaletteOpen()) return;
    if (e.key === "ArrowDown") { e.preventDefault(); setActiveIdx((i) => Math.min(filtered().length - 1, i + 1)); }
    else if (e.key === "ArrowUp") { e.preventDefault(); setActiveIdx((i) => Math.max(0, i - 1)); }
    else if (e.key === "Enter") {
      e.preventDefault();
      const it = filtered()[activeIdx()];
      if (it) selectItem(it);
    }
  }

  // The list-nav keydown listener lives here (not the host): the body is only
  // mounted while the palette is open/exiting, so mount/cleanup bounds it.
  onMount(() => window.addEventListener("keydown", onKeydown));
  onCleanup(() => window.removeEventListener("keydown", onKeydown));

  return (
    <div data-testid="command-palette"
      style={{ position: "fixed", inset: "0", background: "color-mix(in srgb, var(--md-scrim) 55%, transparent)", display: "grid", "place-items": isCompact() ? "end center" : "start center", "padding-top": isCompact() ? "0" : "15vh", "z-index": "60" }}
      onClick={closeCmdPalette}
    >
      <div ref={props.setPanelRef} onClick={(e) => e.stopPropagation()}
        style={{ width: isCompact() ? "100%" : "560px", "max-height": isCompact() ? "90vh" : "60vh", display: "flex", "flex-direction": "column", background: "var(--md-sys-color-surface-container-high)", border: isCompact() ? "none" : "1px solid var(--md-sys-color-outline-variant)", "border-radius": isCompact() ? "var(--md-shape-xl) var(--md-shape-xl) 0 0" : "var(--md-shape-lg)", "box-shadow": "var(--md-elev-3)", overflow: "hidden", "padding-bottom": isCompact() ? "env(safe-area-inset-bottom, 0px)" : "0" }}
      >
        <div style={{ display: "flex", "align-items": "center", gap: "8px", padding: "10px 16px", "border-bottom": "1px solid var(--md-sys-color-outline-variant)" }}>
          <span style={{ color: "var(--text-lo)", "font-size": "13px" }}>⌘</span>
          <input ref={inputRef} type="text" value={query()} onInput={(e) => setQuery(e.currentTarget.value)}
            placeholder="Jump to session, workspace, or action…" data-testid="command-palette-input"
            style={{ flex: "1", background: "transparent", border: "none", outline: "none", color: "var(--text-hi)", "font-size": "var(--md-body-m-size)" }}
          />
        </div>

        <div ref={resultsRef} data-testid="command-palette-results" style={{ flex: "1", "overflow-y": "auto" }}>
          <Show when={filtered().length === 0}>
            <div style={{ padding: "24px 16px", "text-align": "center", color: "var(--text-lo)", "font-size": "var(--md-body-s-size)" }}>
              No matches
            </div>
          </Show>
          <For each={filtered()}>
            {(it, i) => (
              <button type="button" onMouseEnter={() => setActiveIdx(i())} onClick={() => selectItem(it)}
                data-testid="command-palette-item" data-kind={it.kind}
                style={{ width: "100%", display: "flex", "align-items": "center", "justify-content": "space-between", gap: "12px", padding: "8px 16px", background: activeIdx() === i() ? "var(--md-state-hover)" : "transparent", border: "none", cursor: "pointer", color: "var(--md-sys-color-on-surface)", "font-size": "13px", "font-weight": "400", "text-align": "left" }}
              >
                <span style={{ display: "flex", "align-items": "center", gap: "8px", "min-width": "0" }}>
                  <KindBadge kind={it.kind} />
                  <span style={{ overflow: "hidden", "text-overflow": "ellipsis", "white-space": "nowrap" }}>{it.label}</span>
                </span>
                <Show when={it.hint}>
                  <span style={{ color: "var(--text-lo)", "font-size": "11px", "line-height": "1", "flex-shrink": "0", overflow: "hidden", "text-overflow": "ellipsis", "white-space": "nowrap" }}>{it.hint}</span>
                </Show>
              </button>
            )}
          </For>
        </div>

        <CommandPaletteFooter hasResults={filtered().length > 0} />
      </div>
    </div>
  );
}
