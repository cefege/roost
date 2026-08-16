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

import { createMemo, createSignal, createEffect, For, Show, onMount, onCleanup, on } from "solid-js";
import { useNavigate, useParams, Navigate } from "@solidjs/router";
import { rootStore } from "../store/root.ts";
import { allSessions } from "../store/selectors.ts";
import { workerOnline } from "../store/sync.ts";
import { coordClient } from "../connect.ts";
import { spawnShell, waitForSession, maybeAutoLaunchAgent } from "../lib/spawnSession.ts";
import { terminalHref } from "../lib/terminalHref.ts";
import { pushRecent } from "../lib/sidebarRecent.ts";
import { computeFolderActivity, type FolderActivity } from "../lib/folderActivity.ts";
import { colorForFp } from "../lib/fpColor.ts";
import { isCompact } from "../lib/windowSizeClass.ts";
import { addToast } from "../lib/toastStore.ts";
import { childPath, pathCrumbs, collapseCrumbsTo, type CrumbView } from "../lib/folderPalette.ts";
import { workerPathBasename } from "../lib/nativePath.ts";
import { initHistory, pushHistory as pushHistoryFn, goBack as goBackFn, goForward as goForwardFn, canGoBack as canBackFn, canGoForward as canFwdFn, type HistoryState } from "../lib/browseHistory.ts";
import { uiStore, setHomeFolderViewMode, setHomeFolderShowFiles } from "../store/uiStore.ts";
import { FolderGlyph } from "./FolderGlyph.tsx";
import { FileGlyph } from "./FileGlyph.tsx";
import { StatusDot } from "./Settings/md/StatusDot.tsx";
import { Dialog, Button, TextField } from "./Settings/md/primitives.tsx";
import type { WorkerFp } from "@roost/shared/wire";

interface DirEntry { name: string; isDir: boolean; mtimeMs: number }

function relativeTime(ms: number): string {
  if (!ms) return "";
  const diff = Date.now() - ms;
  if (diff < 60_000) return "just now";
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)}m ago`;
  if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)}h ago`;
  if (diff < 604_800_000) return `${Math.floor(diff / 86_400_000)}d ago`;
  return new Date(ms).toLocaleDateString(undefined, { month: "short", day: "numeric" });
}
// Self-explanatory timestamp: clock glyph + relative text, with the full
// locale date+time in the `title` so a hover says exactly what "5m ago" means.
function MetaTime(props: { ms: number; class: string }) {
  return (
    <Show when={props.ms > 0}>
      <span class={props.class} title={`Modified ${new Date(props.ms).toLocaleString()}`}>
        <svg class="df-browse-meta-clock" width="11" height="11" viewBox="0 0 24 24" fill="none"
          stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <circle cx="12" cy="12" r="9" /><path d="M12 7v5l3 2" />
        </svg>
        {relativeTime(props.ms)}
      </span>
    </Show>
  );
}
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
  let crumbOverflowBtn: HTMLButtonElement | undefined;
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
      navigate(session ? terminalHref(session) : `/s/${sessionId}`);
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
    navigate(`/browse/${fp}`);
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
      <div class="df-browse-toolbar">
        <Show when={compact()}>
          <button type="button" class="df-browse-close" data-testid="browse-close"
            aria-label="Cancel" title="Cancel"
            onClick={() => navigate("/")}>
            <svg width="24" height="24" viewBox="0 0 24 24" fill="none" aria-hidden="true">
              <path d="M18 6L6 18M6 6l12 12" stroke="currentColor" stroke-width="2"
                stroke-linecap="round" stroke-linejoin="round" />
            </svg>
          </button>
        </Show>
        <button type="button" class="df-browse-back" data-testid="browse-back" aria-label="Back"
          onClick={goBack} disabled={!backEnabled()} title="Back"
        >
          <svg width="24" height="24" viewBox="0 0 24 24" fill="none" aria-hidden="true">
            <path d="M15 18l-6-6 6-6" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" />
          </svg>
        </button>
        <button type="button" class="df-browse-forward" data-testid="browse-forward" aria-label="Forward"
          onClick={goForward} disabled={!forwardEnabled()} title="Forward"
        >
          <svg width="24" height="24" viewBox="0 0 24 24" fill="none" aria-hidden="true">
            <path d="M9 18l6-6-6-6" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" />
          </svg>
        </button>

        <div class="df-browse-toggle" role="group" aria-label="View mode">
          <button type="button" class="df-browse-toggle-btn" data-testid="browse-view-grid"
            data-active={viewMode() === "grid" ? "true" : "false"} aria-label="Grid view"
            aria-pressed={viewMode() === "grid"} onClick={() => setHomeFolderViewMode("grid")}
          ><svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="3" y="3" width="7" height="7" rx="1" /><rect x="14" y="3" width="7" height="7" rx="1" /><rect x="3" y="14" width="7" height="7" rx="1" /><rect x="14" y="14" width="7" height="7" rx="1" /></svg></button>
          <button type="button" class="df-browse-toggle-btn" data-testid="browse-view-list"
            data-active={viewMode() === "list" ? "true" : "false"} aria-label="List view"
            aria-pressed={viewMode() === "list"} onClick={() => setHomeFolderViewMode("list")}
          ><svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><line x1="8" y1="6" x2="21" y2="6" /><line x1="8" y1="12" x2="21" y2="12" /><line x1="8" y1="18" x2="21" y2="18" /><circle cx="3.5" cy="6" r="1" /><circle cx="3.5" cy="12" r="1" /><circle cx="3.5" cy="18" r="1" /></svg></button>
        </div>
        <div class="df-browse-toolbar-actions">
        <button type="button" class="df-browse-toggle-btn" data-testid="browse-show-files"
          data-active={showFiles() ? "true" : "false"} aria-pressed={showFiles()}
          onClick={() => setHomeFolderShowFiles(!showFiles())} title="Show files in this folder"
        >
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M14 3v4a1 1 0 0 0 1 1h4" /><path d="M17 21H7a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h7l5 5v11a2 2 0 0 1-2 2z" />
          </svg>
        </button>

        <button type="button" class="df-browse-new" data-testid="browse-new" onClick={newFolder}>
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"
          ><path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7z" /><path d="M12 11v5M9.5 13.5h5" /></svg>
          <span class="df-browse-new-label">New folder</span>
        </button>

        <Show when={onlineWorkers().length > 1}>
          <div style={{ position: "relative", "flex-shrink": "0" }}>
            <button type="button" class="df-browse-server" data-testid="browse-server"
              onClick={(e) => { e.stopPropagation(); setServerMenuOpen((v) => !v); }} title={serverLabel()}
            >
              <StatusDot status={serverOnline() ? "ok" : "idle"} size={7} />
              <span class="df-browse-server-label">{serverLabel()}</span>
              <span aria-hidden="true" style={{ "font-size": "10px", opacity: "0.8" }}>▼</span>
            </button>
            <Show when={serverMenuOpen()}>
              <div data-testid="browse-server-menu"
                style={{ position: "absolute", top: "calc(100% + 6px)", right: "0", "min-width": "180px", "z-index": "1", display: "flex", "flex-direction": "column", padding: "4px", background: "var(--md-sys-color-surface-container-high)", border: "1px solid var(--md-sys-color-outline-variant)", "border-radius": "var(--md-shape-md)", "box-shadow": "var(--md-elev-2)" }}
              >
                <For each={onlineWorkers()}>
                  {(w) => (
                    <button type="button" data-testid="browse-server-option"
                      onClick={(e) => { e.stopPropagation(); selectServer(String(w.fp)); }}
                      style={{ display: "flex", "align-items": "center", gap: "8px", width: "100%", padding: "6px 10px", "border-radius": "var(--md-shape-sm)", border: "none", background: String(w.fp) === folderServer() ? "var(--md-sys-color-secondary-container)" : "transparent", color: String(w.fp) === folderServer() ? "var(--md-sys-color-on-secondary-container)" : "var(--md-sys-color-on-surface)", "font-size": "var(--md-body-s-size)", "font-family": "inherit", cursor: "pointer", "text-align": "left" }}
                    >
                      <StatusDot status="ok" size={7} />
                      <span style={{ flex: "1", overflow: "hidden", "text-overflow": "ellipsis", "white-space": "nowrap" }}>{w.label}</span>
                      <Show when={String(w.fp) === folderServer()}><span aria-hidden="true" style={{ opacity: "0.7" }}>✓</span></Show>
                    </button>
                  )}
                </For>
              </div>
            </Show>
          </div>
        </Show>
        </div>
      </div>

      <div class="df-browse-crumbs" ref={crumbsRef} data-testid="browse-crumbs">
        <For each={crumbViews()}>
          {(v, i) => (
            <>
              <Show when={i() > 0}>
                <span class="df-browse-crumb-sep" aria-hidden="true">▸</span>
              </Show>
              <Show
                when={v.kind === "crumb"}
                fallback={
                  <div style={{ position: "relative", "flex-shrink": "0" }}>
                    <button type="button" class="df-browse-crumb df-browse-crumb-overflow"
                      ref={crumbOverflowBtn}
                      data-testid="browse-crumb-overflow" aria-label="Show hidden folders"
                      aria-haspopup="menu" aria-expanded={crumbMenuOpen()}
                      onClick={(e) => {
                        e.stopPropagation();
                        const willOpen = !crumbMenuOpen();
                        if (willOpen && crumbOverflowBtn) {
                          const r = crumbOverflowBtn.getBoundingClientRect();
                          setCrumbMenuPos({ top: r.bottom + 6, left: r.left });
                        }
                        setCrumbMenuOpen(willOpen);
                      }}
                    >…</button>
                    <Show when={crumbMenuOpen()}>
                      <div onClick={() => setCrumbMenuOpen(false)}
                        style={{ position: "fixed", inset: "0", "z-index": "1" }} />
                      <div data-testid="browse-crumb-menu"
                        style={{ position: "fixed", top: `${crumbMenuPos().top}px`, left: `${crumbMenuPos().left}px`, "min-width": "180px", "max-height": "50vh", overflow: "auto", "z-index": "2", display: "flex", "flex-direction": "column", padding: "4px", background: "var(--md-sys-color-surface-container-high)", border: "1px solid var(--md-sys-color-outline-variant)", "border-radius": "var(--md-shape-md)", "box-shadow": "var(--md-elev-2)" }}
                      >
                        <For each={(v as Extract<CrumbView, { kind: "ellipsis" }>).hidden}>
                          {(h) => (
                            <button type="button" data-testid="browse-crumb-menu-item"
                              onClick={(e) => { e.stopPropagation(); setCrumbMenuOpen(false); goToDir(h.path); }}
                              title={h.path}
                              style={{ display: "flex", "align-items": "center", gap: "8px", width: "100%", padding: "6px 10px", "border-radius": "var(--md-shape-sm)", border: "none", background: "transparent", color: "var(--md-sys-color-on-surface)", "font-size": "var(--md-body-s-size)", "font-family": "inherit", cursor: "pointer", "text-align": "left", "white-space": "nowrap", overflow: "hidden", "text-overflow": "ellipsis" }}
                            >{h.label}</button>
                          )}
                        </For>
                      </div>
                    </Show>
                  </div>
                }
              >
                <button type="button" class="df-browse-crumb" data-testid="browse-crumb"
                  data-current={i() === crumbViews().length - 1 ? "true" : "false"}
                  onClick={() => goToDir((v as Extract<CrumbView, { kind: "crumb" }>).path)}
                  title={(v as Extract<CrumbView, { kind: "crumb" }>).path}
                >{(v as Extract<CrumbView, { kind: "crumb" }>).label}</button>
              </Show>
            </>
          )}
        </For>
      </div>
      <div class="df-browse-crumbs-measure" ref={crumbsMeasureRef} aria-hidden="true">
        <For each={crumbs()}>
          {(c, i) => (
            <>
              <Show when={i() > 0}>
                <span class="df-browse-crumb-sep" data-mirror-sep aria-hidden="true">▸</span>
              </Show>
              <button type="button" class="df-browse-crumb" data-mirror-crumb tabindex="-1">{c.label}</button>
            </>
          )}
        </For>
        {/* one sample of each non-crumb token so its width is measurable */}
        <span class="df-browse-crumb-sep" aria-hidden="true">▸</span>
        <button type="button" class="df-browse-crumb df-browse-crumb-overflow" data-mirror-overflow tabindex="-1">…</button>
      </div>

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

      <div ref={resultsRef} class="df-browse-area" tabIndex="-1">
        <Show when={dirLoading() && filteredDirs().length === 0}>
          <div class="df-browse-empty">Loading…</div>
        </Show>
        <Show when={!dirLoading() && filteredDirs().length === 0}>
          <div class="df-browse-empty">
            <div class="df-browse-empty-icon"><FolderGlyph size={24} /></div>
            {serverOnline() ? "Empty folder" : "Server offline"}
            <Show when={serverOnline()} fallback={
              <span class="df-browse-empty-sub">Reconnect to this server to browse folders</span>
            }>
              <span class="df-browse-empty-sub">Create a new folder or open a terminal here</span>
              <div class="df-browse-empty-actions">
                <button type="button" class="df-browse-new" onClick={newFolder}>
                  <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor"
                    stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"
                  ><path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7z" /><path d="M12 11v5M9.5 13.5h5" /></svg>
                  New folder
                </button>
              </div>
            </Show>
          </div>
        </Show>

        <Show when={viewMode() === "grid"} fallback={
          <div class="df-browse-list">
            <For each={filteredDirs()}>
              {(d, i) => {
                const path = childPath(folderServer(), cwdNow(), d.name);
                const activity = folderActivity().get(path);
                const terminals = activity?.terminals ?? 0;
                return (
                  <button type="button" class="df-browse-row" data-testid="browse-row"
                    data-active={activeIdx() === i() ? "true" : "false"}
                    onClick={() => drill(d.name)} onmouseenter={() => setActiveIdx(i())}
                  >
                    <span class="df-browse-row-icon">
                      <FolderGlyph size={20} />
                    </span>
                    <span class="df-browse-row-name">{d.name}</span>
                    <MetaTime ms={d.mtimeMs} class="df-browse-row-meta" />
                    <Show when={terminals > 0}>
                      <span class="df-browse-row-badges">
                        <span class="df-browse-badge df-browse-badge-terminals">{terminals}</span>
                      </span>
                    </Show>
                    <span class="df-browse-row-chev" aria-hidden="true">›</span>
                  </button>
                );
              }}
            </For>
            <Show when={showFiles()}>
              <For each={filteredFiles()}>
                {(f) => (
                  <div class="df-browse-row df-browse-row-file" data-testid="browse-file-row" aria-label={f.name}>
                    <span class="df-browse-row-icon"><FileGlyph size={20} /></span>
                    <span class="df-browse-row-name">{f.name}</span>
                    <MetaTime ms={f.mtimeMs} class="df-browse-row-meta" />
                  </div>
                )}
              </For>
            </Show>
          </div>
        }>
          <div class="df-browse-grid">
            <For each={filteredDirs()}>
              {(d, i) => {
                const path = childPath(folderServer(), cwdNow(), d.name);
                const activity = folderActivity().get(path);
                const terminals = activity?.terminals ?? 0;
                const subtitle = folderSubtitles().get(path);
                const hue = colorForFp(folderServer()).hue;
                return (
                  <button type="button" class="df-browse-tile" data-testid="browse-tile"
                    data-active={activeIdx() === i() ? "true" : "false"}
                    onClick={() => drill(d.name)} onmouseenter={() => setActiveIdx(i())}
                  >
                    <span class="df-browse-tile-icon" style={{ color: `hsl(${hue} 48% 42%)` }}>
                      <FolderGlyph size={22} />
                    </span>
                    <span class="df-browse-tile-text">
                      <span class="df-browse-tile-name">{d.name}</span>
                      <Show when={subtitle} fallback={<MetaTime ms={d.mtimeMs} class="df-browse-tile-meta" />}>
                        <span class="df-browse-tile-subtitle">{subtitle}</span>
                      </Show>
                    </span>
                    <Show when={terminals > 0}>
                      <span class="df-browse-tile-badges">
                        <span class="df-browse-badge df-browse-badge-terminals">{terminals}</span>
                      </span>
                    </Show>
                  </button>
                );
              }}
            </For>
            <Show when={showFiles()}>
              <For each={filteredFiles()}>
                {(f) => (
                  <div class="df-browse-tile df-browse-tile-file" data-testid="browse-file-tile" aria-label={f.name}>
                    <span class="df-browse-tile-icon" style={{ color: "var(--md-sys-color-on-surface-variant)" }}>
                      <FileGlyph size={22} />
                    </span>
                    <span class="df-browse-tile-text">
                      <span class="df-browse-tile-name">{f.name}</span>
                      <MetaTime ms={f.mtimeMs} class="df-browse-tile-meta" />
                    </span>
                  </div>
                )}
              </For>
            </Show>
          </div>
        </Show>
      </div>

      <div class="df-browse-actions">
        <button type="button" class="df-browse-open" data-testid="browse-open"
          onClick={() => void pickFolder(cwdNow())}
        >
          <span aria-hidden="true">❯</span>
          Open terminal here
        </button>
      </div>
      <Dialog
        open={newFolderOpen()}
        onClose={() => setNewFolderOpen(false)}
        headline="New folder"
        actions={
          <>
            <span style={{ flex: "1" }} />
            <Button variant="text" onClick={() => setNewFolderOpen(false)}>Cancel</Button>
            <Button variant="filled" data-testid="newfolder-confirm"
              onClick={() => void commitNewFolder()} disabled={newFolderBusy() || !newFolderName().trim()}>
              {newFolderBusy() ? "Creating…" : "Create"}
            </Button>
          </>
        }
      >
        <div style={{ display: "flex", "flex-direction": "column", gap: "12px", "min-width": "320px" }}>
          <TextField value={newFolderName()} onInput={(v) => setNewFolderName(v)} label="Folder name"
            testId="newfolder-input" style={{ width: "100%" }} ref={(el) => { newFolderInput = el; }}
            onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); void commitNewFolder(); } }} />
          <div style={{ "font-size": "12px", color: "var(--md-sys-color-on-surface-variant)" }}>
            Creates a folder in {cwdNow()}.
          </div>
        </div>
      </Dialog>
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
  const navigate = useNavigate();
  const fp = createMemo(() => {
    const recent = [...allSessions()].sort((a, b) => b.created_at - a.created_at)
      .find((s) => { const w = rootStore.workers[s.worker_fp]; return w ? workerOnline(w) : false; });
    return recent?.worker_fp ?? Object.values(rootStore.workers).find(workerOnline)?.fp;
  });
  return <Navigate href={fp() ? `/browse/${fp()}` : "/"} />;
}
