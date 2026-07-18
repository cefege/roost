// Per-folder tiling layout: persisted to localStorage, reactive via a per-key
// signal map (same pattern as lib/tabOrder.ts). One Layout per folderKeyOf
// bucket. Every tiling op resolves the current layout, applies a pure transform
// from store/paneLayout.ts, and persists — so pane identities stay stable across
// renders (a re-derived default would churn paneIds and thrash the deck).
// Callers: MainPane.tsx (read via resolveLayout in a memo), TabBar / pane
// headers / dividers (mutate via updateLayout). Bump STORAGE_KEY's version
// suffix on any breaking Layout shape change (old data is then ignored).
//
// Per-key signals (NOT one signal over the whole record): commitLayout(fkA)
// must not wake readers of fkB. NOT solid createStore: setStore merges Layout
// objects IN PLACE, which would corrupt the pre-close snapshot TerminalDeck's
// undo path captures (doClose `before`) — signal values stay immutable.
// Persist is a trailing 300ms debounce (commitLayout fires per focus click /
// tab select / divider commit — one JSON.stringify per burst, not per click),
// flushed on pagehide so layouts survive tab close.

import { createSignal, type Accessor, type Setter } from "solid-js";
import { type Layout, defaultLayout, reconcile } from "./paneLayout.ts";

const STORAGE_KEY = "roost.paneLayout.v1";
const PERSIST_DEBOUNCE_MS = 300;

function load(): Record<string, Layout> {
  try {
    return JSON.parse(localStorage.getItem(STORAGE_KEY) || "{}");
  } catch {
    return {};
  }
}

// Plain record = the persist source of truth; signals mirror it per key.
const _record: Record<string, Layout> = load();
const _sigs = new Map<string, [Accessor<Layout | undefined>, Setter<Layout | undefined>]>();

function _sig(folderKey: string): [Accessor<Layout | undefined>, Setter<Layout | undefined>] {
  let s = _sigs.get(folderKey);
  if (!s) {
    s = createSignal<Layout | undefined>(_record[folderKey]);
    _sigs.set(folderKey, s);
  }
  return s;
}

let _persistTimer: Timer | undefined;
function _flushPersist(): void {
  clearTimeout(_persistTimer);
  _persistTimer = undefined;
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(_record));
  } catch {
    /* private mode / quota — layout just won't survive a reload */
  }
}
function _schedulePersist(): void {
  clearTimeout(_persistTimer);
  _persistTimer = setTimeout(_flushPersist, PERSIST_DEBOUNCE_MS);
}
/** The pagehide handler, exported as a test seam: bun test has no window, and
 *  the module may be loaded by ANOTHER suite first (shared module cache), so
 *  capturing the handler via a window stub is order-fragile — tests call this
 *  directly instead. No-op when nothing is pending. */
export function _flushPendingPersist(): void {
  if (_persistTimer !== undefined) _flushPersist();
}
if (typeof window !== "undefined") {
  window.addEventListener("pagehide", _flushPendingPersist);
}

/** Reactive stored layout for a folder bucket (undefined until seeded). */
function rawLayout(folderKey: string): Layout | undefined {
  return _sig(folderKey)[0]();
}

// ui-cc: commit subscribers (lib/uiStateReport.ts listens to report the tab's
// spatial state to coord). Plain Set, no Solid primitive — subscribers debounce
// on their side, and a throwing subscriber must never break a tiling commit.
type LayoutCommitFn = (folderKey: string, layout: Layout) => void;
const _commitSubs = new Set<LayoutCommitFn>();
export function onLayoutCommit(fn: LayoutCommitFn): () => void {
  _commitSubs.add(fn);
  return () => { _commitSubs.delete(fn); };
}

export function commitLayout(folderKey: string, layout: Layout): void {
  _record[folderKey] = layout;
  _sig(folderKey)[1](layout);
  _schedulePersist();
  for (const fn of _commitSubs) { try { fn(folderKey, layout); } catch { /* see above */ } }
}

/** Persist a stable default the first time a folder becomes active — call from
 *  an effect so the paneId stops churning between renders. */
export function seedIfAbsent(folderKey: string, liveIds: string[]): void {
  if (!rawLayout(folderKey)) commitLayout(folderKey, defaultLayout(liveIds));
}

/** Current layout for a folder, reconciled against the live session set.
 *  Read-only (reactive): resolves stored-or-default, folds live sessions in. */
export function resolveLayout(folderKey: string, liveIds: string[]): Layout {
  return reconcile(rawLayout(folderKey) ?? defaultLayout(liveIds), liveIds);
}

/** Resolve → transform → persist. The path every tiling mutation takes. */
function updateLayout(folderKey: string, liveIds: string[], fn: (l: Layout) => Layout): void {
  commitLayout(folderKey, fn(resolveLayout(folderKey, liveIds)));
}
