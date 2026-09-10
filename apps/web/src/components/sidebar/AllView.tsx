// Primary sessions-sidebar body.
// This is the one route-stable navigation tree: it owns search debounce and
// delegates workspace grouping, rows, and empty states to focused siblings.
// The workbench title bar owns product branding and application destinations.


import { createMemo, createSignal, For, Show, onMount, onCleanup } from "solid-js";
import { useNavigate } from "@solidjs/router";
import { rootStore } from "../../store/root.ts";
import { uiStore, toggleSidebarCollapsed, closeSidebar } from "../../store/uiStore.ts";
import { isCompact } from "../../lib/windowSizeClass.ts";
import { terminalOwnsKeyboard } from "../../lib/keyboardShortcuts.ts";
import { matchesPlatformShortcut, platformShortcutLabel } from "../../lib/browserPlatform.ts";
import {
  filterNavigationSearchDocuments,
  navigationSearchDocuments,
} from "../../store/navigation-search.ts";
import { SidebarSearch } from "./SidebarSearch.tsx";
import { SidebarEmptyState } from "./SidebarEmptyState.tsx";
import { SessionRow } from "./SessionRow.tsx";
import { FolderList } from "./FolderList.tsx";
import { IconButton } from "../Settings/md/IconButton.tsx";
import { settingsPaneHref } from "../../routes.ts";

// Debounce interval for the search query → filtered-sessions recompute.
// Keystrokes update `query()` immediately (controlled input stays snappy);
// `debouncedQuery()` lags by SEARCH_DEBOUNCE_MS so the O(n) filter over
// allSessions() doesn't run on every character at scale (>1000 sessions).
const SEARCH_DEBOUNCE_MS = 120;

export function AllView() {
  const navigate = useNavigate();
  const [query, setQuery] = createSignal("");
  const [debouncedQuery, setDebouncedQuery] = createSignal("");
  // Search is collapsed by default and opens from the sidebar title or ⌘F.
  // The resting tree remains dense and immediately scannable.
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
  // Uses the same metadata index as /search and the palette, while retaining
  // the sidebar's 120 ms trailing debounce and established SessionRow surface.
  const filteredSessions = createMemo(() => {
    const q = debouncedQuery();
    if (!q.trim()) return null;
    return filterNavigationSearchDocuments(navigationSearchDocuments(), q)
      .map((document) => rootStore.sessions[document.sessionId])
      .filter((session) => session?.kind === "shell");
  });

  return (
    <div class="df-all-view workbench-sidebar-content" data-testid="all-view">
      <header class="workbench-sidebar-title">
        <h2 class="workbench-sidebar-title__label">Sessions</h2>
        <div class="workbench-sidebar-title__actions">
          <IconButton
            icon="search"
            label="Search sessions and workspaces"
            class="workbench-sidebar-title__action"
            title={`Search sessions & workspaces (${platformShortcutLabel("sidebarSearch", "⌘F")})`}
            onClick={toggleSearch}
            data-testid="brand-row-search"
          />
          <IconButton
            icon="settings"
            label="Settings"
            class="workbench-sidebar-title__action"
            title="Settings"
            onClick={() => navigate(settingsPaneHref("devices"))}
            data-testid="brand-row-settings"
          />
          <IconButton
            icon="chevron_left"
            label="Collapse sidebar"
            class="workbench-sidebar-title__action"
            title={`Collapse sidebar (${platformShortcutLabel("toggleSidebar", "⌘B")})`}
            data-testid="brand-row-collapse"
            onClick={() => (isCompact() ? closeSidebar() : toggleSidebarCollapsed())}
          />
        </div>
      </header>

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
