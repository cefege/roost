// BrowsePage — the Google-Drive-style file manager at /browse/:workerFp.
// Opened by the "+" (new terminal) flow (FlatNewTerminal, HomeLanding CTAs,
// ⌘K "New terminal on…" rows). Replaces the CommandPalette folder-mode picker
// (CommandPaletteBody.tsx desktop/mobile folder branches) with a real route:
// folder GRID of tiles (default) or list, breadcrumb, New-folder, and
// "Open terminal here". Phone (compact) = same page, path field hidden,
// tap-to-drill only — Drive's Move-to mobile.
//
// Navigation is click-driven: folder tiles drill in, breadcrumb segments go up,
// back/forward traverse a history stack (lib/browseHistory.ts). No path/filter
// input — every dir change is an explicit click that pushes history, so
// back/forward are a faithful browser stack.
//
// Data layer: filesListDir/filesMkdir RPCs, childPath/pathCrumbs
// (lib/folderPalette.ts), pickFolder→spawnShell, createFolder.
//
// This file is the state owner: listing, filters, history, the breadcrumb
// collapse measurement, and the New-folder flow. Presentation is split across
// BrowseToolbar.tsx, BrowseBreadcrumbs.tsx, BrowseFolderGrid.tsx, and
// NewFolderDialog.tsx — each is fed already-computed values.

import { createMemo, createSignal, createEffect, For, Show, onMount, onCleanup, on } from "solid-js";
import { useNavigate, useParams, Navigate } from "@solidjs/router";
import { rootStore } from "../store/root.ts";
import { allSessions } from "../store/selectors.ts";
import { workerOnline } from "../store/sync.ts";
import { coordClient } from "../connect.ts";
import { spawnShell, waitForSession, maybeAutoLaunchAgent } from "../lib/spawnSession.ts";
import { terminalHref } from "../lib/terminalHref.ts";
import { pushRecent } from "../lib/sidebarRecent.ts";
import { browseHref, sessionHref } from "../routes.ts";
import { computeFolderActivity, type FolderActivity } from "../lib/folderActivity.ts";
import { isCompact } from "../lib/windowSizeClass.ts";
import { addToast } from "../store/toastStore.ts";
import { childPath, pathCrumbs, collapseCrumbsTo, type CrumbView } from "../lib/folderPalette.ts";
import { workerPathBasename } from "../lib/nativePath.ts";
import { initHistory, pushHistory as pushHistoryFn, goBack as goBackFn, goForward as goForwardFn, canGoBack as canBackFn, canGoForward as canFwdFn, type HistoryState } from "../lib/browseHistory.ts";
import { uiStore, setHomeFolderViewMode, setHomeFolderShowFiles } from "../store/uiStore.ts";
import { FolderGlyph } from "./FolderGlyph.tsx";
import { BrowseToolbar } from "./BrowseToolbar.tsx";
import { BrowseBreadcrumbs } from "./BrowseBreadcrumbs.tsx";
import { BrowseFolderGrid, type DirEntry } from "./BrowseFolderGrid.tsx";
import { NewFolderDialog } from "./NewFolderDialog.tsx";
import type { WorkerFp } from "@roost/shared/wire";

export function BrowsePage() {
  const params = useParams<{ workerFp: string }>();
  return (
    <Show when={params.workerFp} keyed>
      {(workerFp) => <WorkerBrowsePage workerFp={workerFp} />}
    </Show>
  );
}

function WorkerBrowsePage(props: { workerFp: string }) {
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
  let resultsRef: HTMLDivElement | undefined;
  let crumbsRef: HTMLDivElement | undefined;
  const [hideMiddle, setHideMiddle] = createSignal(0);
  let crumbsMeasureRef: HTMLDivElement | undefined;   // hidden mirror row
  const [newFolderOpen, setNewFolderOpen] = createSignal(false);
  const [newFolderName, setNewFolderName] = createSignal("");
  const [newFolderBusy, setNewFolderBusy] = createSignal(false);
  let newFolderInput: HTMLElement | undefined;

  const folderServer = () => workerFp;
  const serverLabel = createMemo(() => rootStore.workers[folderServer()]?.label ?? folderServer().slice(0, 8));
  const serverOnline = createMemo(() => { const w = rootStore.workers[folderServer()]; return w ? workerOnline(w) : false; });

  const onlineWorkers = createMemo(() =>
    Object.values(rootStore.workers).filter(workerOnline).sort((a, b) => a.label.localeCompare(b.label)),
  );


  // Directory listing — eager createEffect, last-write-wins cancellation (same
  // pattern as the retired palette folder mode).
  const [dirData, setDirData] = createSignal<{ resolved: string; entries: DirEntry[] } | null>(null);
  const [dirLoading, setDirLoading] = createSignal(false);
  createEffect(() => {
    const fp = folderServer();
    const dir = cwd();
    if (!fp) { setDirData(null); setDirLoading(false); return; }
    let cancelled = false;
    setDirLoading(true);
    coordClient.filesListDir({ workerFp: fp as unknown as WorkerFp, path: dir })
      .then((res) => { if (!cancelled) setDirData({ resolved: res.resolvedPath || dir, entries: res.entries.map((e) => ({ name: e.name, isDir: e.isDir, mtimeMs: Number(e.mtimeMs) })) }); })
      .catch(() => { if (!cancelled) setDirData(null); })
      .finally(() => { if (!cancelled) setDirLoading(false); });
    onCleanup(() => { cancelled = true; });
  });

  // Server-scoped recent cwds, shown as a chip strip above the grid.
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
  const crumbViews = createMemo<CrumbView[]>(() => collapseCrumbsTo(crumbs(), hideMiddle()));
  const backEnabled = createMemo(() => canBackFn(historyState()));
  const forwardEnabled = createMemo(() => canFwdFn(historyState()));

  // Dotfiles always hidden (no frag — the path input is gone, so there is
  // no filter to reveal dotfiles matching a typed prefix).
  const filteredDirs = createMemo<DirEntry[]>(() => {
    const dirs = dirData()?.entries ?? [];
    return dirs.filter((d) => d.isDir && !d.name.startsWith("."));
  });
  const filteredFiles = createMemo<DirEntry[]>(() => {
    const files = (dirData()?.entries ?? [])
      .filter((d) => !d.isDir && !d.name.startsWith("."));
    return [...files].sort((a, b) => a.name.toLowerCase().localeCompare(b.name.toLowerCase()));
  });
  // Cumulative terminal counts for each visible subdirectory, including its subtree.
  const folderActivity = createMemo<Map<string, FolderActivity>>(() => {
    const fp = folderServer();
    if (!fp) return new Map();
    const base = cwdNow();
    const childPaths = filteredDirs().map((d) => childPath(fp, base, d.name));
    return computeFolderActivity(allSessions(), fp, childPaths);
  });
  // Subtitle per folder: human-readable summary from session activity.
  const folderSubtitles = createMemo<Map<string, string>>(() => {
    const out = new Map<string, string>();
    for (const [path, a] of folderActivity()) {
      if (a.terminals > 0) out.set(path, `${a.terminals} terminal${a.terminals === 1 ? "" : "s"}`);
    }
    return out;
  });


  // Reset cursor on query change only (not on background WS ticks).
  createEffect(() => { cwd(); setActiveIdx(0); });

  // Keep the keyboard-highlighted tile in view.
  createEffect(() => {
    const idx = activeIdx();
    const el = resultsRef?.querySelectorAll<HTMLElement>('[data-testid="browse-tile"],[data-testid="browse-row"]')[idx];
    el?.scrollIntoView({ block: "nearest" });
  });

  // Keep the current-folder crumb in view: scroll the strip fully right on
  // every path change (older ancestors scroll off the left, Drive-style).
  createEffect(() => {
    crumbViews();
    queueMicrotask(() => { if (crumbsRef) crumbsRef.scrollLeft = crumbsRef.scrollWidth; });
  });

  // Width-aware breadcrumb collapse. A hidden mirror row (carrying the real
  // .df-browse-crumb classes → exact font/padding) gives each segment's natural
  // width; we fold middle crumbs from the left until the trail fits the strip's
  // content box, never folding when it already fits. PaneStrip.tsx:54-82 is the
  // reference idiom (ResizeObserver + rAF measure).
  function availableCrumbWidth(): number {
    if (!crumbsRef) return Infinity;
    const cs = getComputedStyle(crumbsRef);
    const pad = parseFloat(cs.paddingLeft || "0") + parseFloat(cs.paddingRight || "0");
    return crumbsRef.clientWidth - pad;
  }
  function measureCrumbs(): void {
    const mirror = crumbsMeasureRef;
    if (!mirror) return;
    const sampleCrumb = mirror.querySelector<HTMLElement>("[data-mirror-crumb]");
    const sepEl = mirror.querySelector<HTMLElement>("[data-mirror-sep]");
    const ovEl = mirror.querySelector<HTMLElement>("[data-mirror-overflow]");
    if (!sampleCrumb || !sepEl || !ovEl) return;
    const widths = Array.from(mirror.querySelectorAll<HTMLElement>("[data-mirror-crumb]"), (el) => el.offsetWidth);
    if (widths.length <= 3) { setHideMiddle(0); return; }
    const sepW = sepEl.offsetWidth;
    const overflowW = ovEl.offsetWidth;
    const avail = availableCrumbWidth();
    const n = widths.length;
    const middleCount = n - 3;
    const sumAll = widths.reduce((a, b) => a + b, 0) + (n - 1) * sepW;
    if (sumAll <= avail) { setHideMiddle(0); return; }
    // Smallest k in [1, middleCount] that fits; default max-collapse if none does.
    let best = middleCount;
    for (let k = 1; k <= middleCount; k++) {
      const keptWidth = widths.slice(1 + k).reduce((a, b) => a + b, 0); // tail side incl. parent+current
      const visibleCount = 1 + 1 + (n - 1 - k);                          // head + ellipsis + kept
      const w = widths[0] + overflowW + keptWidth + (visibleCount - 1) * sepW;
      if (w <= avail) { best = k; break; }
    }
    setHideMiddle(best);
  }
  const crumbResizeObs = new ResizeObserver(() => measureCrumbs());
  onMount(() => {
    if (crumbsRef) crumbResizeObs.observe(crumbsRef);
    measureCrumbs();
  });
  onCleanup(() => crumbResizeObs.disconnect());
  // Re-measure when the path changes: mirror re-renders, then read post-layout.
  createEffect(on(crumbs, () => {
    queueMicrotask(measureCrumbs);
  }));

  onMount(() => {
    const frame = requestAnimationFrame(() => resultsRef?.focus());
    onCleanup(() => cancelAnimationFrame(frame));
  });


  // Every navigation is an explicit click: push the new dir onto history
  // (truncating any forward branch) and set cwd. No typed-path tracking,
  // no navigating guard — click ops are the only thing that moves cwd.
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

  // Open a terminal in the selected folder.
  async function pickFolder(path: string) {
    const fp = folderServer();
    if (!fp) return;
    try {
      const sessionId = await spawnShell(fp as unknown as WorkerFp, path);
      pushRecent(sessionId);
      const session = await waitForSession(sessionId);
      maybeAutoLaunchAgent(sessionId);
      navigate(session ? terminalHref(session) : sessionHref(sessionId));
    } catch (err) {
      addToast(`New terminal failed: ${err instanceof Error ? err.message : String(err)}`, "err");
    }
  }

  function newFolder() {
    setNewFolderName("");
    setNewFolderBusy(false);
    setNewFolderOpen(true);
    queueMicrotask(() => newFolderInput?.focus());
  }

  async function commitNewFolder() {
    const name = newFolderName().trim();
    if (!name || newFolderBusy()) return;
    const fp = folderServer();
    if (!fp) return;
    setNewFolderBusy(true);
    try {
      const target = childPath(fp, cwd(), name);
      const res = await coordClient.filesMkdir({ workerFp: fp as unknown as WorkerFp, path: target });
      setNewFolderOpen(false);
      pushCwd(res.resolvedPath || target);
    } catch (err) {
      addToast(`Create folder failed: ${err instanceof Error ? err.message : String(err)}`, "err");
      setNewFolderBusy(false);
    }
  }

  function selectServer(fp: string) {
    setServerMenuOpen(false);
    navigate(browseHref(fp));
  }

  function onKeydown(e: KeyboardEvent) {
    // ESC: close overlay on desktop
    if (e.key === "Escape") {
      const dlg = document.querySelector("md-dialog");
      if (dlg && dlg.open) return;          // let New-folder dialog close itself
      e.preventDefault(); navigate("/"); return;
    }
    if (e.key === "ArrowDown") { e.preventDefault(); setActiveIdx((i) => Math.min(filteredDirs().length - 1, i + 1)); }
    else if (e.key === "ArrowUp") { e.preventDefault(); setActiveIdx((i) => Math.max(0, i - 1)); }
    else if (e.key === "ArrowRight") {
      const d = filteredDirs()[activeIdx()];
      if (d) { e.preventDefault(); drill(d.name); }
    }
    else if (e.key === "ArrowLeft") { e.preventDefault(); goBack(); }
    else if (e.key === "Tab") {
      const d = filteredDirs()[activeIdx()];
      if (d) { e.preventDefault(); drill(d.name); }
    }
    else if (e.key === "Enter") { e.preventDefault(); void pickFolder(cwdNow()); }
  }
  onMount(() => window.addEventListener("keydown", onKeydown));
  onCleanup(() => window.removeEventListener("keydown", onKeydown));

  const viewMode = () => uiStore.homeFolderViewMode;
  const showFiles = () => uiStore.homeFolderShowFiles;
  const compact = isCompact;

  const innerContent = (
    <div class="df-browse-page" data-testid="browse-page" data-compact={compact() ? "true" : "false"} data-overlay={!compact() ? "true" : undefined}>
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
        onCancel={() => navigate("/")}
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
        setStripRef={(el) => { crumbsRef = el; }}
        setMirrorRef={(el) => { crumbsMeasureRef = el; }}
      />

      <Show when={cwd() === startDir() && folderRecents().length > 0}>
        <div class="df-browse-recents">
          <span class="df-browse-recents-label">Recent</span>
          <For each={folderRecents()}>
            {(r) => (
              <button type="button" class="df-browse-recent-chip" data-testid="browse-recent"
                onClick={() => void pickFolder(r)} title={r}
              >
                <FolderGlyph size={11} />
                {workerPathBasename(folderServer(), r) || r}
              </button>
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
        onNewFolder={newFolder}
        setAreaRef={(el) => { resultsRef = el; }}
      />

      <div class="df-browse-actions">
        <button type="button" class="df-browse-open" data-testid="browse-open"
          onClick={() => void pickFolder(cwdNow())}
        >
          <span aria-hidden="true">❯</span>
          Open terminal here
        </button>
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
    </div>
  )

  return !compact() ? (
    <div
      style={{ position: "fixed", inset: 0, "z-index": "100", display: "flex", "align-items": "center", "justify-content": "center", background: "color-mix(in srgb, var(--md-scrim) 55%, transparent)" }}
      onClick={() => navigate("/")}
    >
      <div
        style={{ width: "min(640px, 94vw)", "height": "85vh", display: "flex", "flex-direction": "column", background: "var(--md-sys-color-surface)", "border-radius": "var(--md-shape-xl)", "box-shadow": "var(--md-elev-3)", overflow: "hidden" }}
        onClick={(e) => e.stopPropagation()}
      >
        {innerContent}
      </div>
    </div>
  ) : innerContent
}

// /browse (no server) → most-recent online server, else Home.
export function BrowseRedirect() {
  const fp = createMemo(() => {
    const recent = [...allSessions()].sort((a, b) => b.created_at - a.created_at).find((s) => { const w = rootStore.workers[s.worker_fp]; return w ? workerOnline(w) : false; });
    return recent?.worker_fp ?? Object.values(rootStore.workers).find(workerOnline)?.fp;
  });
  const resolved = fp();
  return <Navigate href={resolved ? browseHref(resolved) : "/"} />;
}
