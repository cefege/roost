// This page owns one worker's directory browsing, history, and terminal-launch flow.
// Keeping those route-scoped behaviors together resets their lifecycle when the worker
// changes while extracted view controllers handle isolated presentation concerns.

import { createMemo, createSignal, createEffect, For, Show, onMount, onCleanup } from "solid-js";
import { useNavigate } from "@solidjs/router";
import { diag } from "@roost/observability/diag";
import { rootStore } from "../../store/root.ts";
import { allSessions } from "../../store/selectors.ts";
import { workerOnline } from "../../store/sync.ts";
import { workersHydrated } from "../../store/sync-bootstrap.ts";
import { ROUTES, browseHref } from "../../routes.ts";
import { computeFolderActivity, type FolderActivity } from "../../lib/folderActivity.ts";
import { isCompact } from "../../browser/windowSizeClass.ts";
import { childPath, parentPath, pathCrumbs, collapseCrumbsTo, type CrumbView } from "../../lib/folderPalette.ts";
import { workerPathBasename } from "../../lib/nativePath.ts";
import { visibleFolders, visibleFiles } from "../../lib/browseEntries.ts";
import { initHistory, pushHistory as pushHistoryFn, goBack as goBackFn, goForward as goForwardFn, canGoBack as canBackFn, canGoForward as canFwdFn, type HistoryState } from "../../lib/browseHistory.ts";
import { uiStore, setHomeFolderShowFiles } from "../../store/uiStore.ts";
import { BrowseToolbar } from "./BrowseToolbar.tsx";
import { BrowsePathBar } from "./BrowsePathBar.tsx";
import { BrowseEntryList, type BrowseListingStatus } from "./BrowseEntryList.tsx";
import { NewFolderDialog } from "./NewFolderDialog.tsx";
import { createBrowseBreadcrumbCollapse } from "./browseBreadcrumbCollapse.ts";
import { createBrowseDirectoryListing } from "./browseDirectoryListing.ts";
import { createBrowseNewFolder } from "./browseNewFolder.ts";
import { createBrowsePickerKeys } from "./browsePickerKeys.ts";
import { launchWorkerBrowseTerminal } from "./workerBrowseActions.ts";
import type { WorkerFp } from "@roost/protocol/wire";
import { Button } from "../Settings/md/Button.tsx";
import { Chip } from "../Settings/md/Chip.tsx";
import { EmptyState } from "../Settings/md/EmptyState.tsx";
import { Surface } from "../Settings/md/Surface.tsx";
import { SectionTitle } from "../Settings/md/SectionTitle.tsx";
import { Sheet } from "../Settings/md/Sheet.tsx";
import type { TextFieldElement } from "../Settings/md/TextField.tsx";

export function WorkerBrowsePage(props: { workerFp: string }) {
  const workerFp = props.workerFp;
  const navigate = useNavigate();
  const initialDir = [...allSessions()]
    .filter((session) => String(session.worker_fp) === workerFp)
    .sort((a, b) => b.created_at - a.created_at)[0]?.cwd ?? "~";
  // Navigation is click-driven: `cwd` is the canonical current directory
  // (no trailing slash, "~" = home). Every worker-keyed owner starts with its
  // own newest session cwd and owns its complete browser/history lifecycle.
  const [cwd, setCwd] = createSignal(initialDir);
  const [startDir] = createSignal(initialDir);
  const [historyState, setHistoryState] = createSignal<HistoryState>(initHistory(initialDir));
  // -1 = no keyboard cursor yet: entry 0 must not look picked before an arrow
  // key says so.
  const [activeIdx, setActiveIdx] = createSignal(-1);
  const [filter, setFilter] = createSignal("");
  const [filterOpen, setFilterOpen] = createSignal(false);
  const [serverMenuOpen, setServerMenuOpen] = createSignal(false);
  const [crumbMenuOpen, setCrumbMenuOpen] = createSignal(false);
  const [crumbMenuPos, setCrumbMenuPos] = createSignal<{ top: number; left: number }>({ top: 0, left: 0 });
  let browseSurfaceRef: HTMLDivElement | undefined, resultsRef: HTMLDivElement | undefined;
  let unavailableRef: HTMLDivElement | undefined;
  const newFolder = createBrowseNewFolder({
    workerFp: () => workerFp,
    parent: () => cwd(),
    siblings: () => folderNames(),
    scoped: () => scopedWorker(),
    onCreated: (resolvedPath) => pushCwd(resolvedPath),
  });
  let filterInput: TextFieldElement | undefined;
  const folderServer = () => workerFp;
  const serverLabel = createMemo(() => rootStore.workers[folderServer()]?.label ?? folderServer().slice(0, 8));
  const serverOnline = createMemo(() => { const w = rootStore.workers[folderServer()]; return w ? workerOnline(w) : false; });
  const scopedWorker = createMemo(() => rootStore.workers[folderServer()] !== undefined);
  const scopeState = createMemo<"loading" | "available" | "unavailable">(() => {
    if (scopedWorker()) return "available";
    return workersHydrated() ? "unavailable" : "loading";
  });
  createEffect(() => {
    if (scopedWorker()) return;
    setServerMenuOpen(false);
    setCrumbMenuOpen(false);
    newFolder.close();
  });
  const onlineWorkers = createMemo(() =>
    Object.values(rootStore.workers).filter(workerOnline).sort((a, b) => a.label.localeCompare(b.label)),
  );
  const listing = createBrowseDirectoryListing({
    workerFp: folderServer,
    path: cwd,
    scoped: scopedWorker,
  });
  const folderRecents = createMemo<string[]>(() => {
    const fp = folderServer();
    const seen = new Set<string>();
    const out: string[] = [];
    for (const s of [...allSessions()].sort((a, b) => b.created_at - a.created_at)) {
      if (s.worker_fp !== fp || !s.cwd || seen.has(s.cwd)) continue;
      seen.add(s.cwd); out.push(s.cwd);
      if (out.length >= 5) break;
    }
    return out;
  });

  const cwdNow = createMemo(() => listing.resolvedPath() ?? cwd());
  const crumbs = createMemo(() => pathCrumbs(folderServer(), cwdNow()));
  const breadcrumbCollapse = createBrowseBreadcrumbCollapse(crumbs);
  const crumbViews = createMemo<CrumbView[]>(() => collapseCrumbsTo(crumbs(), breadcrumbCollapse.hideMiddle()));
  const backEnabled = createMemo(() => canBackFn(historyState()));
  const forwardEnabled = createMemo(() => canFwdFn(historyState()));
  const upEnabled = createMemo(() => parentPath(folderServer(), cwdNow()) !== cwdNow());
  const listedFolders = createMemo(() => visibleFolders(listing.entries(), filter()));
  const listedFiles = createMemo(() => visibleFiles(listing.entries(), filter()));
  // Siblings mkdir would collide with, including the dot-directories the list
  // hides — a hidden name still makes the RPC fail.
  const folderNames = createMemo(() =>
    listing.entries().filter((entry) => entry.isDir).map((entry) => entry.name));
  const listingStatus = createMemo<BrowseListingStatus>(() => {
    if (scopeState() === "loading" || listing.loading()) return "loading";
    if (listing.error() !== null) return "error";
    if (!serverOnline() && listing.entries().length === 0) return "offline";
    return "ready";
  });
  const folderActivity = createMemo<Map<string, FolderActivity>>(() => {
    const fp = folderServer();
    if (!fp) return new Map();
    const base = cwdNow();
    const childPaths = listedFolders().map((d) => childPath(fp, base, d.name));
    return computeFolderActivity(allSessions(), fp, childPaths);
  });
  const folderTerminalCounts = createMemo<Map<string, number>>(() => {
    const out = new Map<string, number>();
    for (const [path, activity] of folderActivity()) {
      if (activity.terminals > 0) out.set(path, activity.terminals);
    }
    return out;
  });
  createEffect(() => { cwd(); setActiveIdx(-1); setFilter(""); });
  // Keep the keyboard-highlighted row in view.
  createEffect(() => {
    const idx = activeIdx();
    const el = resultsRef?.querySelectorAll<HTMLElement>('[data-testid="browse-row"]')[idx];
    el?.scrollIntoView({ block: "nearest" });
  });
  createEffect(() => {
    const state = scopeState();
    if (state === "loading") return;
    const frame = requestAnimationFrame(() => {
      if (state === "available") resultsRef?.focus();
      else unavailableRef?.focus();
    });
    onCleanup(() => cancelAnimationFrame(frame));
  });
  function pushCwd(path: string) {
    setCwd(path);
    setHistoryState((s) => pushHistoryFn(s, path));
    setActiveIdx(-1);
  }
  function drill(name: string) { pushCwd(childPath(folderServer(), cwd(), name)); }
  function goUp() {
    const next = parentPath(folderServer(), cwdNow());
    if (next !== cwdNow()) pushCwd(next);
  }
  function toggleFilter() {
    if (filterOpen()) {
      closeFilter();
      return;
    }
    setFilterOpen(true);
    queueMicrotask(() => filterInput?.focus());
  }
  // Closing clears: a hidden filter that still hides entries is the "where did
  // my folders go" report.
  function closeFilter() {
    setFilterOpen(false);
    setFilter("");
  }
  function goBack() {
    const next = goBackFn(historyState());
    if (next === historyState()) return;
    setHistoryState(next);
    setCwd(next.entries[next.cursor]);
    setActiveIdx(-1);
  }
  function goForward() {
    const next = goForwardFn(historyState());
    if (next === historyState()) return;
    setHistoryState(next);
    setCwd(next.entries[next.cursor]);
    setActiveIdx(-1);
  }

  function pickFolder(path: string): void {
    const fp = folderServer();
    if (!fp || !scopedWorker()) return;
    void launchWorkerBrowseTerminal(fp as unknown as WorkerFp, path, navigate);
  }
  function selectServer(fp: string) {
    setServerMenuOpen(false);
    navigate(browseHref(fp));
  }
  const onKeydown = createBrowsePickerKeys({
    surface: () => browseSurfaceRef,
    results: () => resultsRef,
    dialogOpen: newFolder.open,
    scoped: scopedWorker,
    compact: isCompact,
    folderCount: () => listedFolders().length,
    // CSS owns the column count (auto-fill), so the live grid is the only
    // honest source for what "one row down" means.
    columns: () => {
      const grid = resultsRef?.querySelector(".md-list--grid");
      if (!grid) return 1;
      return getComputedStyle(grid).gridTemplateColumns.split(" ").filter(Boolean).length;
    },
    folderNameAt: (index) => listedFolders()[index]?.name,
    activeIdx,
    setActiveIdx,
    onDrill: drill,
    onBack: goBack,
    onParent: goUp,
    onOpenHere: () => pickFolder(cwdNow()),
    onEscape: () => navigate(ROUTES.ROOT),
  });
  onMount(() => {
    window.addEventListener("keydown", onKeydown);
    diag("browse.opened", { worker_fp: folderServer(), cwd: cwd(), compact: isCompact() });
  });
  onCleanup(() => window.removeEventListener("keydown", onKeydown));

  const showFiles = () => uiStore.homeFolderShowFiles;
  const compact = isCompact;

  const innerContent = (
    <div ref={(element) => { browseSurfaceRef = element; }} class="df-browse-page" data-testid="browse-page" data-compact={compact() ? "true" : "false"} data-overlay={!compact() ? "true" : undefined}>
      <BrowseToolbar
        folderName={workerPathBasename(folderServer(), cwdNow()) || cwdNow()}
        ready={scopeState() === "available"}
        showFiles={showFiles()}
        filterOpen={filterOpen()}
        serverFp={folderServer()}
        serverLabel={serverLabel()}
        serverOnline={serverOnline()}
        onlineWorkers={onlineWorkers()}
        serverMenuOpen={serverMenuOpen()}
        setServerMenuOpen={(open) => setServerMenuOpen(open)}
        onClose={() => navigate(ROUTES.ROOT)}
        onToggleFilter={toggleFilter}
        onToggleShowFiles={() => setHomeFolderShowFiles(!showFiles())}
        onNewFolder={newFolder.begin}
        onSelectServer={selectServer}
      />

      <BrowsePathBar
        crumbViews={crumbViews()}
        crumbs={crumbs()}
        menuOpen={crumbMenuOpen()}
        menuPos={crumbMenuPos()}
        backEnabled={backEnabled()}
        forwardEnabled={forwardEnabled()}
        upEnabled={upEnabled()}
        filterOpen={filterOpen()}
        filter={filter()}
        setMenuOpen={(open) => setCrumbMenuOpen(open)}
        setMenuPos={(pos) => setCrumbMenuPos(pos)}
        onNavigate={pushCwd}
        onBack={goBack}
        onForward={goForward}
        onUp={goUp}
        onHome={() => pushCwd("~")}
        onFilter={(value) => setFilter(value)}
        onCloseFilter={closeFilter}
        setStripRef={breadcrumbCollapse.setStripRef}
        setMirrorRef={breadcrumbCollapse.setMirrorRef}
        setFilterRef={(element) => { filterInput = element; }}
      />

      <Show
        when={scopeState() === "unavailable"}
        fallback={
          <BrowseEntryList
            status={listingStatus()}
            loadingCaption={scopeState() === "loading" ? "Loading machine…" : "Loading folders…"}
            folders={listedFolders()}
            files={listedFiles()}
            showFiles={showFiles()}
            filter={filter()}
            serverFp={folderServer()}
            cwd={cwdNow()}
            activeIdx={activeIdx()}
            terminalCounts={folderTerminalCounts()}
            errorMessage={listing.error()}
            header={
              <Show when={cwd() === startDir() && folderRecents().length > 0}>
                <div class="df-browse-recents">
                  <SectionTitle>Recent</SectionTitle>
                  <For each={folderRecents()}>
                    {(recent) => (
                      <Chip label={workerPathBasename(folderServer(), recent) || recent} icon="folder"
                        title={recent} testId="browse-recent" onClick={() => pickFolder(recent)} />
                    )}
                  </For>
                </div>
              </Show>
            }
            onDrill={drill}
            onClearFilter={closeFilter}
            onRetry={listing.reload}
            setAreaRef={(el) => { resultsRef = el; }}
          />
        }
      >
        <div
          ref={unavailableRef}
          class="df-browse-area"
          data-testid="browse-worker-unavailable"
          role="status"
          aria-live="polite"
          aria-label="Machine unavailable. This machine isn't available on this coordinator."
          aria-atomic="true"
          tabIndex={-1}
        >
          <EmptyState
            icon="folder_off"
            title="Machine unavailable"
            supporting="This machine isn't available on this coordinator."
            action={
              <Button variant="secondary" data-testid="browse-worker-unavailable-home"
              onClick={() => navigate("/", { replace: true })}>
                Go home
              </Button>
            }
          />
        </div>
      </Show>

      <Surface class="df-browse-actions" level={1} radius="none">
        <Button class="df-browse-open" data-testid="browse-open" icon="terminal"
          disabled={scopeState() !== "available"} onClick={() => pickFolder(cwdNow())}>
          Open terminal here
        </Button>
      </Surface>
      <NewFolderDialog
        open={newFolder.open()}
        name={newFolder.name()}
        busy={newFolder.busy()}
        error={newFolder.error()}
        targetPath={cwdNow()}
        onName={newFolder.setName}
        onClose={newFolder.close}
        onCreate={newFolder.commit}
        setInputRef={newFolder.setInputRef}
      />
    </div>
  )

  return !compact() ? (
    <Sheet open onClose={() => navigate(ROUTES.ROOT)} headline="Browse folders" side="center"
      class="roost-dialog--wide roost-dialog--browse" showCloseButton={false}
      onOpenAutoFocus={(event) => event.preventDefault()}>
      {innerContent}
    </Sheet>
  ) : innerContent;
}
