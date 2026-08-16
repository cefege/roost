// Sidebar body: FolderList (folder rows) — the ONE
// sidebar layout; the Status/Folder/Folders view modes were deleted
// 2026-07-04. Brand row + ⌘F search on top; SidebarEmptyState when no
// machines are registered.
// Reads rootStore.workers (empty-state gate) + allSessions (search).

import { createMemo, createSignal, For, Show, onMount, onCleanup } from "solid-js";
import { useNavigate } from "@solidjs/router";
import { rootStore } from "../../store/root.ts";
import { uiStore, toggleSidebarCollapsed, closeSidebar } from "../../store/uiStore.ts";
import { isCompact } from "../../lib/windowSizeClass.ts";
import { allSessions } from "../../store/selectors.ts";
import { terminalOwnsKeyboard } from "../../lib/keyboardShortcuts.ts";
import { matchesPlatformShortcut, platformShortcutLabel } from "../../lib/browserPlatform.ts";
import { SidebarSearch } from "./SidebarSearch.tsx";
import { SidebarEmptyState } from "./SidebarEmptyState.tsx";
import { SessionRow } from "./SessionRow.tsx";
import { FolderList } from "./FolderList.tsx";
import { BrandMark } from "../BrandMark.tsx";
import "@material/web/iconbutton/icon-button.js";

// Debounce interval for the search query → filtered-sessions recompute.
// Keystrokes update `query()` immediately (controlled input stays snappy);
// `debouncedQuery()` lags by SEARCH_DEBOUNCE_MS so the O(n) filter over
// allSessions() doesn't run on every character at scale (>1000 sessions).
const SEARCH_DEBOUNCE_MS = 120;

export function AllView() {
  const navigate = useNavigate();
  const [query, setQuery] = createSignal("");
  const [debouncedQuery, setDebouncedQuery] = createSignal("");
  // Search is collapsed by default — opened via the brand-row 🔍 button or ⌘F —
  // so the resting sidebar isn't dominated by a search box.
  const [searchOpen, setSearchOpen] = createSignal(false);
  let debounceTimer: ReturnType<typeof setTimeout> | null = null;
  function onQueryChange(next: string) {
    setQuery(next);
    if (debounceTimer) clearTimeout(debounceTimer);
    // Empty query: flush immediately so the list re-appears without lag.
    if (next.trim().length === 0) { setDebouncedQuery(""); return; }
    debounceTimer = setTimeout(() => setDebouncedQuery(next), SEARCH_DEBOUNCE_MS);
  }
  onCleanup(() => { if (debounceTimer) clearTimeout(debounceTimer); });
  let searchRef: HTMLInputElement | undefined;

  function openSearch() {
    setSearchOpen(true);
    queueMicrotask(() => searchRef?.focus());
  }
  function toggleSearch() {
    if (searchOpen()) { setSearchOpen(false); onQueryChange(""); }
    else openSearch();
  }
  function onGlobalKeyDown(e: KeyboardEvent) {
    if (e.defaultPrevented || terminalOwnsKeyboard()) return;
    if (!matchesPlatformShortcut(e, "sidebarSearch")) return;
    e.preventDefault();
    openSearch();
  }
  onMount(() => document.addEventListener("keydown", onGlobalKeyDown));
  onCleanup(() => document.removeEventListener("keydown", onGlobalKeyDown));

  const noMachines = createMemo(() => Object.keys(rootStore.workers).length === 0);

  // When query active: flat filtered terminal-session list.
  // Uses debouncedQuery so the filter only re-runs after typing settles.
  const filteredSessions = createMemo(() => {
    const q = debouncedQuery().toLowerCase().trim();
    if (!q) return null;
    return allSessions().filter((s) => {
      if (s.kind !== "shell") return false;
      if (s.cwd.toLowerCase().includes(q)) return true;
      const ws = s.workspace_id ? rootStore.workspaces[s.workspace_id] : null;
      return ws?.name.toLowerCase().includes(q) ?? false;
    });
  });

  return (
    <div class="df-all-view" data-testid="all-view" data-m3flat="1">
      <div class="df-brand-row">
        <BrandMark size={26} class="brand-mark" />
        <span class="df-brand-title">Roost</span>
        <span style={{ "margin-left": "auto", display: "inline-flex", "align-items": "center", gap: "2px", position: "relative" }}>
          <md-icon-button
            aria-label="Search"
            title={`Search sessions & workspaces (${platformShortcutLabel("sidebarSearch", "⌘F")})`}
            onClick={toggleSearch}
            data-testid="brand-row-search"
          >
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" style={{ width: "20px", height: "20px" }}>
              <circle cx="11" cy="11" r="7" />
              <path d="M21 21l-4.3-4.3" />
            </svg>
          </md-icon-button>

          <md-icon-button
            aria-label="Settings"
            title="Settings"
            onClick={() => navigate("/settings/keys")}
            data-testid="brand-row-settings"
          >
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" style={{ width: "20px", height: "20px" }}>
              <circle cx="12" cy="12" r="3"/>
              <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1 0 2.83 2 2 0 0 1-2.83 0l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09a1.65 1.65 0 0 0-1-1.51 1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83 0 2 2 0 0 1 0-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09a1.65 1.65 0 0 0 1.51-1 1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 0-2.83 2 2 0 0 1 2.83 0l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 0 2 2 0 0 1 0 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z"/>
            </svg>
          </md-icon-button>
          <md-icon-button
            aria-label="Collapse sidebar"
            title={`Collapse sidebar (${platformShortcutLabel("toggleSidebar", "⌘B")})`}
            data-testid="brand-row-collapse"
            // Mobile: the drawer is driven by sidebarOpen, NOT sidebarCollapsed
            // (a desktop-only icon-rail concept) — so toggleSidebarCollapsed did
            // nothing on a phone. Close the drawer instead. Desktop keeps the
            // collapse-to-rail behavior.
            onClick={() => (isCompact() ? closeSidebar() : toggleSidebarCollapsed())}
          >
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" style={{ width: "20px", height: "20px" }}>
              <path d="M15 6l-6 6 6 6" />
            </svg>
          </md-icon-button>
        </span>
      </div>

      <Show when={searchOpen()}>
        <SidebarSearch
          query={query()}
          onChange={onQueryChange}
          inputRef={(el) => { searchRef = el; }}
          placeholder="Search sessions, workspaces…"
        />
      </Show>

      {/* Search active → flat filtered list. Gated on debouncedQuery (same
          signal the rows use) — gating on raw query() unmounted FolderList
          into an empty pane for the 120 ms debounce on the first keystroke. */}
      <Show when={debouncedQuery().trim().length > 0}>
        <Show
          when={(filteredSessions()?.length ?? 0) > 0}
          fallback={<SidebarEmptyState kind="search-empty" query={query()} />}
        >
          <For each={filteredSessions() ?? []}>
            {(session) => <SessionRow session={session} />}
          </For>
        </Show>
      </Show>

      {/* Normal view — the folder list, or the empty-state when no
          machines are registered. Distinct empty-state kinds:
          - browser-unpaired: coord 401'd the authed list calls → this
            BROWSER isn't trusted yet → CTA to /pair (Onboarding).
          - no-machines: zero workers registered → CTA to /settings/machines. */}
      <Show when={!debouncedQuery().trim()}>
        <Show when={noMachines()} fallback={<FolderList />}>
          <SidebarEmptyState
            kind={rootStore.browser_unauthorized ? "browser-unpaired" : "no-machines"}
          />
        </Show>
      </Show>
    </div>
  );
}
