// Command registry + item model for CommandPalette. Pure data builders (no JSX,
// no component-local reactivity) split out of CommandPalette.tsx to keep it
// under the 400-line cap. The reactive reads (allSessions / rootStore) happen
// inside these builders, so a caller wrapping them in a createMemo still tracks
// their dependencies.

import type { Navigator } from "@solidjs/router";
import { rootStore } from "../store/root.ts";
import { allSessions } from "../store/selectors.ts";
import { workerOnline } from "../store/sync.ts";
import { queueTaskDialogStore } from "./QueueTaskDialog.tsx";
import { startQuickChat } from "../lib/quickChat.ts";
import type { ItemKind } from "./CommandPalettePieces.tsx";

// ── Types ───────────────────────────────────────────────────────────────────

export interface PaletteItem {
  id: string;
  kind: ItemKind;
  label: string;
  hint?: string;
  /** Extra text folded into the fuzzy match but not displayed. */
  search?: string;
  href?: string;
  action?: () => Promise<void> | void;
}

// ── Static action list (default mode) ────────────────────────────────────────

function buildStaticActions(navigate: Navigator): PaletteItem[] {
  const items: PaletteItem[] = [
    { id: "action:new-chat", kind: "action", label: "New chat", hint: "quick AI chat", action: () => startQuickChat(navigate) },
    { id: "action:settings:machines", kind: "action", label: "Settings — Machines", hint: "⌘,", action: () => navigate("/settings/machines") },
    { id: "action:settings:permissions", kind: "action", label: "Settings — Access", action: () => navigate("/settings/permissions") },
    { id: "action:settings:webhooks", kind: "action", label: "Settings — Webhooks", action: () => navigate("/settings/webhooks") },
    { id: "action:queue-new-task", kind: "action", label: "Queue new task", hint: "task queue", action: () => queueTaskDialogStore.open() },
  ];
  // Only online servers can spawn — a down/asleep Mac's row would hang on spawn.
  for (const [fp, w] of Object.entries(rootStore.workers)) {
    if (!workerOnline(w)) continue;
    items.push({
      id: `action:new-shell:${fp}`,
      kind: "action",
      label: `New terminal on ${w.label}`,
      hint: "browse",
      action: () => navigate(`/browse/${fp}`),
    });
  }
  return items;
}


// `qLower` MUST be pre-lowercased by the caller (once per filter run — the
// filter loop runs per item, so lowering here re-allocated per row).
export function matchesQuery(text: string, qLower: string): boolean {
  if (!qLower) return true;
  return text.toLowerCase().includes(qLower);
}

// ── Row identity cache ───────────────────────────────────────────────────────
// buildDefaultItems mints fresh objects per run (any WS tick while the palette
// is open), and <For> keys by reference → fresh objects would recreate every
// row's DOM. stableItem returns the PREVIOUS object for an id when nothing
// visible changed, so unchanged rows keep identity and their DOM survives.
// `action` closures are rebuilt per run but capture only values baked into the
// id (fp) or stable (navigate) — reusing the old closure is behaviorally
// identical, so actions compare by presence, not reference.
let _itemCache = new Map<string, PaletteItem>();

function stableItem(next: PaletteItem, nextCache: Map<string, PaletteItem>): PaletteItem {
  const prev = _itemCache.get(next.id);
  const keep = prev !== undefined
    && prev.kind === next.kind && prev.label === next.label
    && prev.hint === next.hint && prev.search === next.search
    && prev.href === next.href
    && (prev.action === undefined) === (next.action === undefined);
  const out = keep ? prev : next;
  nextCache.set(next.id, out);
  return out;
}

// Default-mode item list: open sessions + workspaces + static actions.
export function buildDefaultItems(navigate: Navigator): PaletteItem[] {
  const items: PaletteItem[] = [];
  for (const s of allSessions()) {
    const worker = rootStore.workers[s.worker_fp];
    items.push({
      id: `session:${s.id}`,
      kind: "session",
      label: s.cwd.split("/").pop() || s.cwd,
      hint: worker?.label ?? s.worker_fp.slice(0, 8),
      // Searchable-but-not-displayed: full cwd + the agent's last message, so
      // ⌘K finds a session by path fragment or message content (parity with
      // the retired scrollback-search modal).
      search: `${s.cwd} ${s.agent?.last_message?.text ?? ""}`,
      href: `/s/${s.id}`,
    });
  }
  for (const [id, ws] of Object.entries(rootStore.workspaces)) {
    items.push({ id: `workspace:${id}`, kind: "workspace", label: ws.name, hint: `${ws.session_ids.length} sessions`, href: `/w/${id}` });
  }
  const all = [...items, ...buildStaticActions(navigate)];
  // Rebuild the cache from this run's ids — self-pruning (closed sessions
  // don't accumulate stale entries across a long page life).
  const nextCache = new Map<string, PaletteItem>();
  const out = all.map((it) => stableItem(it, nextCache));
  _itemCache = nextCache;
  return out;
}
