// This page owns one worker's directory browsing, history, and terminal-launch flow.
// Keeping those route-scoped behaviors together resets their lifecycle when the worker
// changes while extracted view controllers handle isolated presentation concerns.

import { createMemo, createSignal, createEffect, For, Show, onMount, onCleanup } from "solid-js";
import { useNavigate } from "@solidjs/router";
import { rootStore } from "../store/root.ts";
import { allSessions } from "../store/selectors.ts";
import { workerOnline } from "../store/sync.ts";
import { workersHydrated } from "../store/sync-bootstrap.ts";
import { coordClient } from "../connect.ts";
import { ROUTES, browseHref } from "../routes.ts";
import { computeFolderActivity, type FolderActivity } from "../lib/folderActivity.ts";
import { isCompact } from "../lib/windowSizeClass.ts";
import { addToast } from "../store/toastStore.ts";
import {
  captureAuthResourceToken,
  isCurrentAuthResourceToken,
} from "../store/auth-boundary.ts";
import { childPath, pathCrumbs, collapseCrumbsTo, type CrumbView } from "../lib/folderPalette.ts";
import { workerPathBasename } from "../lib/nativePath.ts";
import { initHistory, pushHistory as pushHistoryFn, goBack as goBackFn, goForward as goForwardFn, canGoBack as canBackFn, canGoForward as canFwdFn, type HistoryState } from "../lib/browseHistory.ts";
import { uiStore, setHomeFolderViewMode, setHomeFolderShowFiles } from "../store/uiStore.ts";
import { BrowseToolbar } from "./BrowseToolbar.tsx";
import { BrowseBreadcrumbs } from "./BrowseBreadcrumbs.tsx";
import { BrowseFolderGrid, type DirEntry } from "./BrowseFolderGrid.tsx";
import { NewFolderDialog } from "./NewFolderDialog.tsx";
import { createBrowseBreadcrumbCollapse } from "./browseBreadcrumbCollapse.ts";
import { launchWorkerBrowseTerminal } from "./workerBrowseActions.ts";
import type { WorkerFp } from "@roost/shared/wire";
import { Button } from "./Settings/md/Button.tsx";
import { EmptyState } from "./Settings/md/EmptyState.tsx";
import { Sheet } from "./Settings/md/Sheet.tsx";
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
  const [activeIdx, setActiveIdx] = createSignal(0);
  const [serverMenuOpen, setServerMenuOpen] = createSignal(false);
  const [crumbMenuOpen, setCrumbMenuOpen] = createSignal(false);
  const [crumbMenuPos, setCrumbMenuPos] = createSignal<{ top: number; left: number }>({ top: 0, left: 0 });
  let browseSurfaceRef: HTMLDivElement | undefined, resultsRef: HTMLDivElement | undefined;
  let unavailableRef: HTMLDivElement | undefined;
  const [newFolderOpen, setNewFolderOpen] = createSignal(false);
  const [newFolderName, setNewFolderName] = createSignal("");
  const [newFolderBusy, setNewFolderBusy] = createSignal(false);
  let newFolderInput: HTMLElement | undefined;
  const folderServer = () => workerFp;
  const serverLabel = createMemo(() => rootStore.workers[folderServer()]?.label ?? folderServer().slice(0, 8));
  const serverOnline = createMemo(() => { const w = rootStore.workers[folderServer()]; return w ? workerOnline(w) : false; });
  const scopedWorker = createMemo(() =>
    !rootStore.browser_unauthorized && rootStore.workers[folderServer()] !== undefined);
  const scopeState = createMemo<"loading" | "available" | "unavailable">(() => {
    if (scopedWorker()) return "available";
    return workersHydrated() || rootStore.browser_unauthorized ? "unavailable" : "loading";
  });
  createEffect(() => {
    if (scopedWorker()) return;
    setServerMenuOpen(false);
    setCrumbMenuOpen(false);
    setNewFolderOpen(false);
    setNewFolderName("");
    setNewFolderBusy(false);
  });
  const onlineWorkers = createMemo(() =>
    Object.values(rootStore.workers).filter(workerOnline).sort((a, b) => a.label.localeCompare(b.label)),
  );
  const [dirData, setDirData] = createSignal<{ resolved: string; entries: DirEntry[] } | null>(null);
  const [dirLoading, setDirLoading] = createSignal(false);
  createEffect(() => {
    const fp = folderServer();
    const dir = cwd();
    if (!fp || !scopedWorker()) {
      setDirData(null);
      setDirLoading(false);
      return;
    }
    const authToken = captureAuthResourceToken();
    let cancelled = false;
    setDirLoading(true);
    coordClient.filesListDir({ workerFp: fp as unknown as WorkerFp, path: dir })
      .then((response) => {
        if (cancelled || !isCurrentAuthResourceToken(authToken)) return;
        setDirData({
          resolved: response.resolvedPath || dir,
          entries: response.entries.map((entry) => ({
            name: entry.name, isDir: entry.isDir, mtimeMs: Number(entry.mtimeMs),
          })),
        });
      })
      .catch(() => {
        if (cancelled || !isCurrentAuthResourceToken(authToken)) return;
        setDirData(null);
      })
      .finally(() => {
        if (cancelled || !isCurrentAuthResourceToken(authToken)) return;
        setDirLoading(false);
      });
    onCleanup(() => { cancelled = true; });
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

  const cwdNow = createMemo(() => dirData()?.resolved ?? cwd());
  const crumbs = createMemo(() => pathCrumbs(folderServer(), cwdNow()));
  const breadcrumbCollapse = createBrowseBreadcrumbCollapse(crumbs);
  const crumbViews = createMemo<CrumbView[]>(() => collapseCrumbsTo(crumbs(), breadcrumbCollapse.hideMiddle()));
  const backEnabled = createMemo(() => canBackFn(historyState()));
  const forwardEnabled = createMemo(() => canFwdFn(historyState()));
  const filteredDirs = createMemo<DirEntry[]>(() => {
    const dirs = dirData()?.entries ?? [];
    return dirs.filter((d) => d.isDir && !d.name.startsWith("."));
  });
  const filteredFiles = createMemo<DirEntry[]>(() => {
    const files = (dirData()?.entries ?? [])
      .filter((d) => !d.isDir && !d.name.startsWith("."));
    return [...files].sort((a, b) => a.name.toLowerCase().localeCompare(b.name.toLowerCase()));
  });
  const folderActivity = createMemo<Map<string, FolderActivity>>(() => {
    const fp = folderServer();
    if (!fp) return new Map();
    const base = cwdNow();
    const childPaths = filteredDirs().map((d) => childPath(fp, base, d.name));
    return computeFolderActivity(allSessions(), fp, childPaths);
  });
  const folderSubtitles = createMemo<Map<string, string>>(() => {
    const out = new Map<string, string>();
    for (const [path, a] of folderActivity()) {
      if (a.terminals > 0) out.set(path, `${a.terminals} terminal${a.terminals === 1 ? "" : "s"}`);
    }
    return out;
  });
  createEffect(() => { cwd(); setActiveIdx(0); });
  // Keep the keyboard-highlighted tile in view.
  createEffect(() => {
    const idx = activeIdx();
    const el = resultsRef?.querySelectorAll<HTMLElement>('[data-testid="browse-tile"],[data-testid="browse-row"]')[idx];
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
    setActiveIdx(0);
  }
  function drill(name: string) { pushCwd(childPath(folderServer(), cwd(), name)); }
  function goToDir(path: string) { pushCwd(path); }
  function goBack() {
    const next = goBackFn(historyState());
    if (next === historyState()) return;
    setHistoryState(next);
    setCwd(next.entries[next.cursor]);
    setActiveIdx(0);
  }
  function goForward() {
    const next = goForwardFn(historyState());
    if (next === historyState()) return;
    setHistoryState(next);
    setCwd(next.entries[next.cursor]);
    setActiveIdx(0);
  }

  function pickFolder(path: string): void {
    const fp = folderServer();
    if (!fp || !scopedWorker()) return;
    void launchWorkerBrowseTerminal(fp as unknown as WorkerFp, path, navigate);
  }
  function newFolder() {
    if (!scopedWorker()) return;
    setNewFolderName("");
    setNewFolderBusy(false);
    setNewFolderOpen(true);
    queueMicrotask(() => newFolderInput?.focus());
  }
  async function commitNewFolder() {
    const name = newFolderName().trim();
    if (!name || newFolderBusy()) return;
    const fp = folderServer();
    if (!fp || !scopedWorker()) return;
    const authToken = captureAuthResourceToken();
    setNewFolderBusy(true);
    try {
      const target = childPath(fp, cwd(), name);
      const response = await coordClient.filesMkdir({
        workerFp: fp as unknown as WorkerFp,
        path: target,
      });
      if (!isCurrentAuthResourceToken(authToken)) return;
      setNewFolderOpen(false);
      pushCwd(response.resolvedPath || target);
    } catch (error) {
      if (!isCurrentAuthResourceToken(authToken)) return;
      addToast(`Create folder failed: ${error instanceof Error ? error.message : String(error)}`, "err");
      setNewFolderBusy(false);
    }
  }
  function selectServer(fp: string) {
    setServerMenuOpen(false);
    navigate(browseHref(fp));
  }
  function onKeydown(event: KeyboardEvent) {
    if (event.defaultPrevented) return;
    if (newFolderOpen()) return;
    const eventPath = event.composedPath();
    if (!browseSurfaceRef || !eventPath.includes(browseSurfaceRef)) return;
    if (event.key === "Escape") {
      if (isCompact()) {
        event.preventDefault();
        navigate(ROUTES.ROOT);
      }
      return;
    }
    if (!scopedWorker() || !resultsRef || !eventPath.includes(resultsRef)) return;
    if (event.key === "ArrowDown") {
      event.preventDefault();
      setActiveIdx((idx) => Math.min(filteredDirs().length - 1, idx + 1));
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      setActiveIdx((idx) => Math.max(0, idx - 1));
    } else if (event.key === "ArrowRight") {
      const directory = filteredDirs()[activeIdx()];
      if (directory) {
        event.preventDefault();
        drill(directory.name);
      }
    } else if (event.key === "ArrowLeft") {
      event.preventDefault();
      goBack();
    } else if (event.key === "Enter") {
      event.preventDefault();
      void pickFolder(cwdNow());
    }
  }
  onMount(() => window.addEventListener("keydown", onKeydown));
  onCleanup(() => window.removeEventListener("keydown", onKeydown));

  const viewMode = () => uiStore.homeFolderViewMode;
  const showFiles = () => uiStore.homeFolderShowFiles;
  const compact = isCompact;

  const innerContent = (
    <div ref={(element) => { browseSurfaceRef = element; }} class="df-browse-page" data-testid="browse-page" data-compact={compact() ? "true" : "false"} data-overlay={!compact() ? "true" : undefined}>
      <Show when={scopeState() === "loading"}>
        <div class="df-browse-area" aria-busy="true">
          <EmptyState
            icon="progress_activity"
            title="Loading machine…"
            supporting="Checking which machines this coordinator can reach."
          />
        </div>
      </Show>

      <Show when={scopeState() === "unavailable"}>
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

      <Show when={scopeState() === "available"}>
      <BrowseToolbar
        compact={compact()}
        viewMode={viewMode()}
        showFiles={showFiles()}
        backEnabled={backEnabled()}
        forwardEnabled={forwardEnabled()}
        serverFp={folderServer()}
        serverLabel={serverLabel()}
        serverOnline={serverOnline()}
        onlineWorkers={onlineWorkers()}
        serverMenuOpen={serverMenuOpen()}
        setServerMenuOpen={(open) => setServerMenuOpen(open)}
        onCancel={() => navigate(ROUTES.ROOT)}
        onBack={goBack}
        onForward={goForward}
        onViewMode={setHomeFolderViewMode}
        onToggleShowFiles={() => setHomeFolderShowFiles(!showFiles())}
        onNewFolder={newFolder}
        onSelectServer={selectServer}
      />

      <BrowseBreadcrumbs
        crumbViews={crumbViews()}
        crumbs={crumbs()}
        menuOpen={crumbMenuOpen()}
        menuPos={crumbMenuPos()}
        setMenuOpen={(open) => setCrumbMenuOpen(open)}
        setMenuPos={(pos) => setCrumbMenuPos(pos)}
        onNavigate={goToDir}
        setStripRef={breadcrumbCollapse.setStripRef}
        setMirrorRef={breadcrumbCollapse.setMirrorRef}
      />

      <Show when={cwd() === startDir() && folderRecents().length > 0}>
        <div class="df-browse-recents">
          <span class="df-browse-recents-label md-label-s">Recent</span>
          <For each={folderRecents()}>
            {(r) => (
              <Button class="df-browse-recent-chip" variant="outline" size="sm"
                data-testid="browse-recent" icon="folder" onClick={() => void pickFolder(r)} title={r}>
                {workerPathBasename(folderServer(), r) || r}
              </Button>
            )}
          </For>
        </div>
      </Show>

      <BrowseFolderGrid
        loading={dirLoading()}
        dirs={filteredDirs()}
        files={filteredFiles()}
        serverFp={folderServer()}
        serverOnline={serverOnline()}
        cwd={cwdNow()}
        viewMode={viewMode()}
        showFiles={showFiles()}
        activeIdx={activeIdx()}
        activity={folderActivity()}
        subtitles={folderSubtitles()}
        onActivate={(idx) => setActiveIdx(idx)}
        onDrill={drill}
        setAreaRef={(el) => { resultsRef = el; }}
      />

      <div class="df-browse-actions">
        <Button class="df-browse-open" data-testid="browse-open" icon="terminal"
          onClick={() => void pickFolder(cwdNow())}>
          Open terminal here
        </Button>
      </div>
      <NewFolderDialog
        open={newFolderOpen()}
        name={newFolderName()}
        busy={newFolderBusy()}
        targetPath={cwdNow()}
        onName={(v) => setNewFolderName(v)}
        onClose={() => setNewFolderOpen(false)}
        onCreate={() => void commitNewFolder()}
        setInputRef={(el) => { newFolderInput = el; }}
      />
      </Show>
    </div>
  )

  return !compact() ? (
    <Sheet open onClose={() => navigate(ROUTES.ROOT)} headline="Browse folders" side="center"
      class="roost-dialog--wide roost-dialog--browse" showCloseButton={scopeState() === "loading"}
      onOpenAutoFocus={(event) => event.preventDefault()}>
      {innerContent}
    </Sheet>
  ) : innerContent;
}
