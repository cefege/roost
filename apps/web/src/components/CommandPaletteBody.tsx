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
import { useLocation, useNavigate } from "@solidjs/router";
import { folderKeyOf } from "../lib/folderKey.ts";
import { closeCmdPalette, cmdPaletteOpen } from "../lib/keyboardShortcuts.ts";
import { activeSessionForPath } from "../store/selectors.ts";
import { rootStore } from "../store/root.ts";
import { workerOnline } from "../store/sync.ts";
import { normalizeNavigationSearchQuery } from "../store/navigation-search.ts";
import { KindBadge, CommandPaletteFooter } from "./CommandPalettePieces.tsx";
import {
  buildDefaultItems,
  matchesQuery,
  type CommandPaletteContext,
  type PaletteItem,
} from "./CommandPalette.data.ts";
import { platformShortcutLabel } from "../lib/browserPlatform.ts";

export function PaletteBody() {
  const navigate = useNavigate();
  const location = useLocation();
  const [query, setQuery] = createSignal("");
  const [activeIdx, setActiveIdx] = createSignal(0);
  let inputRef: HTMLInputElement | undefined;
  let resultsRef: HTMLDivElement | undefined;

  const paletteContext = createMemo<CommandPaletteContext>(() => {
    const pathname = location.pathname;
    const routeSession = activeSessionForPath(pathname);
    const activeSession = routeSession?.status === "open" ? routeSession : null;
    const worker = activeSession
      ? rootStore.workers[activeSession.worker_fp]
      : undefined;
    const sessionTarget = activeSession
      ? {
          id: activeSession.id,
          workerFp: activeSession.worker_fp,
          cwd: activeSession.cwd,
        }
      : null;
    return {
      pathname,
      authGeneration: rootStore.auth_generation,
      activeSession: sessionTarget,
      activeFolder: activeSession
        ? {
            id: folderKeyOf(activeSession),
            workerFp: activeSession.worker_fp,
            cwd: activeSession.cwd,
          }
        : null,
      workerRoutable: worker ? workerOnline(worker) : false,
    };
  });

  const defaultItems = createMemo<PaletteItem[]>(() =>
    buildDefaultItems(navigate, paletteContext())
  );

  const filtered = createMemo<PaletteItem[]>(() => {
    const normalizedQuery = normalizeNavigationSearchQuery(query());
    const queryTerms = normalizedQuery ? normalizedQuery.split(" ") : [];
    return defaultItems().filter((item) =>
      matchesQuery(
        `${item.label} ${item.hint ?? ""} ${item.search ?? ""}`,
        queryTerms,
      )
    );
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

  // The list-nav listener exists only while the lazily loaded body is mounted.
  onMount(() => window.addEventListener("keydown", onKeydown));
  onCleanup(() => window.removeEventListener("keydown", onKeydown));

  return (
    <div data-testid="command-palette" class="roost-command-palette">
      <div class="roost-command-palette__search">
        <span style={{ color: "var(--text-lo)", "font-size": "13px" }}>{platformShortcutLabel("commandPalette", "⌘")}</span>
        <input
          ref={inputRef}
          class="roost-command-palette__input"
          type="text"
          value={query()}
          onInput={(event) => setQuery(event.currentTarget.value)}
          placeholder="Jump to session, workspace, or action…"
          data-testid="command-palette-input"
        />
      </div>

      <div ref={resultsRef} data-testid="command-palette-results" class="roost-command-palette__results">
        <Show when={filtered().length === 0}>
          <div style={{ padding: "24px 16px", "text-align": "center", color: "var(--text-lo)", "font-size": "var(--md-body-s-size)" }}>
            No matches
          </div>
        </Show>
        <For each={filtered()}>
          {(it, i) => (
            <button type="button" onMouseEnter={() => setActiveIdx(i())} onClick={() => selectItem(it)}
              data-testid="command-palette-item" data-kind={it.kind}
              style={{ width: "100%", display: "flex", "align-items": "center", "justify-content": "space-between", gap: "12px", padding: "8px 16px", background: activeIdx() === i() ? "var(--md-secondary-container)" : "transparent", border: "none", cursor: "pointer", color: activeIdx() === i() ? "var(--md-on-secondary-container)" : "var(--md-on-surface)", "font-size": "13px", "font-weight": "400", "text-align": "left" }}
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
  );
}
