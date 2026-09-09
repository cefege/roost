// Typed command-palette catalog and row compiler. Session rows reuse the
// shared navigation-search projection; contextual actions receive scalar
// route/session targets and delegate to their existing action owners.
// The identity cache keeps unchanged Solid <For> rows mounted.

import type { Navigator } from "@solidjs/router";
import type { Session } from "@roost/shared/wire";
import { spawnSessionSibling } from "../lib/sessionSiblingAction.ts";
import { rootStore } from "../store/root.ts";
import {
  navigationSearchDocuments,
  normalizeNavigationSearchQuery,
  type NavigationSearchDocument,
} from "../store/navigation-search.ts";
import { queueTaskDialogStore } from "../store/queueTaskDialog.ts";
import type { ItemKind } from "./CommandPalettePieces.tsx";

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

export interface CommandPaletteSessionTarget {
  readonly id: string;
  readonly workerFp: Session["worker_fp"];
  readonly cwd: string;
}

export interface CommandPaletteFolderTarget {
  readonly id: string;
  readonly workerFp: Session["worker_fp"];
  readonly cwd: string;
}

export interface CommandPaletteContext {
  readonly pathname: string;
  readonly authGeneration: number;
  readonly activeSession: CommandPaletteSessionTarget | null;
  readonly activeFolder: CommandPaletteFolderTarget | null;
  readonly workerRoutable: boolean;
}

export interface CommandPaletteDataDeps {
  navigationSearchDocuments: () => readonly Pick<
    NavigationSearchDocument,
    "sessionId" | "href" | "displayTitle" | "workerLabel" | "searchText" | "available"
  >[];
  spawnSessionSibling: typeof spawnSessionSibling;
}

const defaultCommandPaletteDataDeps: CommandPaletteDataDeps = {
  navigationSearchDocuments,
  spawnSessionSibling,
};

export type CoreActionId =
  | "core.search.all"
  | "core.attention.open"
  | "core.task.queue-folder"
  | "core.session.new-sibling";

export interface CoreActionDefinition<ActionId extends CoreActionId = CoreActionId> {
  readonly id: ActionId;
  readonly compile: (
    navigate: Navigator,
    context: CommandPaletteContext,
    deps: CommandPaletteDataDeps,
  ) => PaletteItem | null;
}

export const CORE_ACTION_DEFINITIONS = [
  { id: "core.search.all", compile: compileSearchAll },
  { id: "core.attention.open", compile: compileAttentionOpen },
  { id: "core.task.queue-folder", compile: compileQueueFolder },
  { id: "core.session.new-sibling", compile: compileNewSibling },
] as const satisfies readonly [
  CoreActionDefinition<"core.search.all">,
  CoreActionDefinition<"core.attention.open">,
  CoreActionDefinition<"core.task.queue-folder">,
  CoreActionDefinition<"core.session.new-sibling">,
];

// The caller normalizes and splits once per filter run. Row matching only
// normalizes each candidate and checks every cross-field query term.
export function matchesQuery(
  text: string,
  normalizedQueryTerms: readonly string[],
): boolean {
  if (normalizedQueryTerms.length === 0) return true;
  const normalizedText = normalizeNavigationSearchQuery(text);
  return normalizedQueryTerms.every(term => normalizedText.includes(term));
}

let itemCache = new Map<string, PaletteItem>();

/** A credential boundary must not retain labels, paths, or actions captured by
 * rows from the prior credential. */
export function clearCommandPaletteCacheForAccountBoundary(): void {
  itemCache.clear();
}

/** Compile current session/workspace rows and the closed core-action catalog. */
export function buildDefaultItems(
  navigate: Navigator,
  context: CommandPaletteContext,
  deps: CommandPaletteDataDeps = defaultCommandPaletteDataDeps,
): PaletteItem[] {
  const items: PaletteItem[] = [];
  for (const navigationDocument of deps.navigationSearchDocuments()) {
    items.push({
      id: `session:${navigationDocument.sessionId}`,
      kind: "session",
      label: navigationDocument.displayTitle,
      hint: navigationDocument.available
        ? navigationDocument.workerLabel
        : `${navigationDocument.workerLabel} · unavailable`,
      search: navigationDocument.searchText,
      href: navigationDocument.href,
    });
  }
  for (const [workspaceId, workspace] of Object.entries(rootStore.workspaces)) {
    items.push({
      id: `workspace:${workspaceId}`,
      kind: "workspace",
      label: workspace.name,
      hint: `${workspace.session_ids.length} sessions`,
      href: `/w/${workspaceId}`,
    });
  }
  for (const definition of CORE_ACTION_DEFINITIONS) {
    const actionItem = definition.compile(navigate, context, deps);
    if (actionItem) items.push(actionItem);
  }

  // Rebuild from this run's ids so closed sessions and hidden contextual
  // actions cannot accumulate across a long page life.
  const nextCache = new Map<string, PaletteItem>();
  const stableItems = items.map((item) => stableItem(item, nextCache));
  itemCache = nextCache;
  return stableItems;
}

function compileSearchAll(
  _navigate: Navigator,
  _context: CommandPaletteContext,
  _deps: CommandPaletteDataDeps,
): PaletteItem {
  return {
    id: "core.search.all",
    kind: "action",
    label: "Search all sessions",
    hint: "metadata",
    search: "global search sessions workspaces workers git ports",
    href: "/search",
  };
}

function compileAttentionOpen(
  _navigate: Navigator,
  _context: CommandPaletteContext,
  _deps: CommandPaletteDataDeps,
): PaletteItem {
  return {
    id: "core.attention.open",
    kind: "action",
    label: "Open attention",
    hint: "blocked and completed agents",
    search: "attention blocked done unseen agents",
    href: "/search?scope=attention",
  };
}

function compileQueueFolder(
  _navigate: Navigator,
  context: CommandPaletteContext,
  _deps: CommandPaletteDataDeps,
): PaletteItem | null {
  const target = context.activeFolder;
  if (!target) return null;
  const generation = context.authGeneration;
  return {
    id: targetedActionId("core.task.queue-folder", target.id, generation),
    kind: "action",
    label: "Queue task for this folder",
    hint: target.cwd,
    search: `queue task ${target.cwd}`,
    action: () => {
      if (!capturedGenerationIsCurrent(generation)) return;
      queueTaskDialogStore.open({
        cwd: target.cwd,
        workerFp: target.workerFp,
      });
    },
  };
}

function compileNewSibling(
  navigate: Navigator,
  context: CommandPaletteContext,
  deps: CommandPaletteDataDeps,
): PaletteItem | null {
  const target = context.activeSession;
  if (!target || !context.workerRoutable) return null;
  const generation = context.authGeneration;
  return {
    id: targetedActionId("core.session.new-sibling", target.id, generation),
    kind: "action",
    label: "New sibling terminal",
    hint: target.cwd,
    search: `new terminal sibling ${target.cwd}`,
    action: () => {
      if (!capturedGenerationIsCurrent(generation)) return;
      return deps.spawnSessionSibling({
        worker_fp: target.workerFp,
        cwd: target.cwd,
      }, navigate);
    },
  };
}

function targetedActionId(
  actionId: "core.task.queue-folder" | "core.session.new-sibling",
  targetId: string,
  authGeneration: number,
): string {
  return `${actionId}:${targetId}:generation:${authGeneration}`;
}

function capturedGenerationIsCurrent(capturedGeneration: number): boolean {
  return rootStore.auth_generation === capturedGeneration;
}

function stableItem(
  next: PaletteItem,
  nextCache: Map<string, PaletteItem>,
): PaletteItem {
  const previous = itemCache.get(next.id);
  const unchanged = previous !== undefined
    && previous.kind === next.kind
    && previous.label === next.label
    && previous.hint === next.hint
    && previous.search === next.search
    && previous.href === next.href
    && (previous.action === undefined) === (next.action === undefined);
  const item = unchanged ? previous : next;
  nextCache.set(next.id, item);
  return item;
}
