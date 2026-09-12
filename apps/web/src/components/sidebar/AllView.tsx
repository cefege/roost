// Primary sessions-sidebar body.
// This is the one route-stable navigation tree: it owns search debounce and
// delegates workspace grouping, rows, and empty states to focused siblings.
// The workbench title bar owns product branding and application destinations.


import { createMemo, createSignal, For, Show, onCleanup, onMount } from "solid-js";
import { rootStore } from "../../store/root.ts";
import { terminalOwnsKeyboard } from "../../lib/keyboardShortcuts.ts";
import { matchesPlatformShortcut } from "../../lib/browserPlatform.ts";
import {
  filterNavigationSearchDocuments,
  navigationSearchDocuments,
} from "../../store/navigation-search.ts";
import { SidebarSearch } from "./SidebarSearch.tsx";
import { SidebarEmptyState } from "./SidebarEmptyState.tsx";
import { SessionRow } from "./SessionRow.tsx";
import { FolderList } from "./FolderList.tsx";
// Debounce interval for the search query → filtered-sessions recompute.
// Keystrokes update `query()` immediately (controlled input stays snappy);
// `debouncedQuery()` lags by SEARCH_DEBOUNCE_MS so the O(n) filter over
// allSessions() doesn't run on every character at scale (>1000 sessions).
const SEARCH_DEBOUNCE_MS = 120;

interface AllViewProps {
  active: boolean;
  onActivate: () => void;
}

export function AllView(props: AllViewProps) {
  const [query, setQuery] = createSignal("");
  const [debouncedQuery, setDebouncedQuery] = createSignal("");
  let debounceTimer: number | undefined;
  let focusFrame: number | undefined;
  let searchRef: HTMLInputElement | undefined;

  function onQueryChange(next: string) {
    setQuery(next);
    clearTimeout(debounceTimer);
    // Empty query: flush immediately so the list re-appears without lag.
    if (next.trim().length === 0) { setDebouncedQuery(""); return; }
    debounceTimer = window.setTimeout(() => setDebouncedQuery(next), SEARCH_DEBOUNCE_MS);
  }

  function activateSpacesSearch(): void {
    props.onActivate();
    if (focusFrame !== undefined) cancelAnimationFrame(focusFrame);
    focusFrame = requestAnimationFrame(() => {
      focusFrame = undefined;
      searchRef?.focus();
    });
  }

  function onGlobalKeyDown(event: KeyboardEvent): void {
    if (event.defaultPrevented || terminalOwnsKeyboard()) return;
    if (!matchesPlatformShortcut(event, "sidebarSearch")) return;
    event.preventDefault();
    activateSpacesSearch();
  }

  onMount(() => document.addEventListener("keydown", onGlobalKeyDown));
  onCleanup(() => {
    clearTimeout(debounceTimer);
    if (focusFrame !== undefined) cancelAnimationFrame(focusFrame);
    document.removeEventListener("keydown", onGlobalKeyDown);
  });

  const noMachines = createMemo(() => Object.keys(rootStore.workers).length === 0);
  const searchActive = createMemo(() => debouncedQuery().trim().length > 0);
  const folderListActive = () => props.active && !searchActive() && !noMachines();
  const showEmptyState = createMemo(() => !searchActive() && noMachines());


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
      <SidebarSearch
        query={query()}
        onChange={onQueryChange}
        inputRef={(element) => { searchRef = element; }}
        placeholder="Filter spaces…"
      />

      <div
        class="workbench-sidebar-folder-list"
        data-active={folderListActive() ? "true" : "false"}
        inert={!folderListActive() ? true : undefined}
        aria-hidden={folderListActive() ? undefined : "true"}
      >
        <FolderList active={folderListActive()} />
      </div>

      <Show when={searchActive()}>
        <Show
          when={(filteredSessions()?.length ?? 0) > 0}
          fallback={<SidebarEmptyState kind="search-empty" query={query()} />}
        >
          <For each={filteredSessions() ?? []}>
            {(session) => <SessionRow session={session} />}
          </For>
        </Show>
      </Show>

      <Show when={showEmptyState()}>
        <SidebarEmptyState
          kind={rootStore.browser_unauthorized ? "browser-unpaired" : "no-machines"}
        />
      </Show>
    </div>
  );
}
