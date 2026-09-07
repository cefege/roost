// Converts browser-local pane layouts to the portable V1 document and back.
// Shared validation and the canonical live-session set fence imported bindings;
// only a fully materialized runtime tree crosses paneLayoutStore.commitLayout,
// and a leaf left holding no session collapses into its sibling before commit.
// Runtime pane and split UUIDs never enter the document.

import { parseLayoutDocumentV1 } from "@roost/shared/layout-document";
import type {
  LayoutDocumentNode,
  LayoutDocumentV1,
} from "@roost/shared/layout-document";
import { allLeaves, collapseEmpties, findLeaf, fixFocus } from "./paneLayout.ts";
import type { Layout, PaneNode } from "./paneLayout.ts";
import { commitLayout, resolveLayout } from "./paneLayoutStore.ts";

export interface AppliedLayoutDocument {
  layout: Layout;
  selectedSessionId: string | null;
}

export interface DegradedLayoutDocument {
  document: LayoutDocumentV1;
  droppedSessionCount: number;
}

/** Parse the whole document and prove every binding belongs to this folder's
 * current canonical live-session set. Extra live sessions are valid. */
export function validateLayoutDocumentImport(
  document: unknown,
  liveSessionIds: readonly string[],
): LayoutDocumentV1 {
  const parsed = parseLayoutDocumentV1(document);
  const liveSessions = validatedLiveSessionSet(liveSessionIds);
  for (const binding of parsed.bindings) {
    if (!liveSessions.has(binding.session_id)) {
      throw new Error(`Layout session ${binding.session_id} is not live in the current folder.`);
    }
  }
  return parsed;
}

/** Human-confirmed import degrades instead of failing: a binding whose session
 * is no longer live is dropped and the leaf it emptied collapses, so one closed
 * session does not make a saved layout single-use. The unattended apply path
 * keeps exact liveness — a scripted caller needs an unambiguous outcome. */
export function degradeLayoutDocumentToLiveSessions(
  document: unknown,
  liveSessionIds: readonly string[],
): DegradedLayoutDocument {
  const parsed = parseLayoutDocumentV1(document);
  const liveSessions = validatedLiveSessionSet(liveSessionIds);
  const bindings = parsed.bindings.filter((binding) => liveSessions.has(binding.session_id));
  const droppedSessionCount = parsed.bindings.length - bindings.length;
  if (droppedSessionCount === 0 && !hasSessionLessLeaf(parsed.root)) {
    return { document: parsed, droppedSessionCount };
  }
  const degraded = materializeLayoutDocument({ ...parsed, bindings }, []);
  return { document: documentFromLayout(degraded.layout), droppedSessionCount };
}

/** Export stable leaf/slot keys in first-before-second preorder. */
export function exportLayoutDocument(
  folderKey: string,
  liveSessionIds: readonly string[],
): LayoutDocumentV1 {
  assertFolderKey(folderKey);
  validatedLiveSessionSet(liveSessionIds);
  return documentFromLayout(resolveLayout(folderKey, [...liveSessionIds]));
}

/** Validate and materialize entirely in locals, then publish one store commit. */
export function applyLayoutDocument(
  folderKey: string,
  document: unknown,
  liveSessionIds: readonly string[],
): AppliedLayoutDocument {
  assertFolderKey(folderKey);
  const parsed = validateLayoutDocumentImport(document, liveSessionIds);
  const boundSessions = new Set(parsed.bindings.map((binding) => binding.session_id));
  const applied = materializeLayoutDocument(
    parsed,
    liveSessionIds.filter((sessionId) => !boundSessions.has(sessionId)),
  );
  commitLayout(folderKey, applied.layout);
  return applied;
}

function assertFolderKey(folderKey: string): void {
  if (!folderKey) throw new Error("A current folder is required for layout import or export.");
}

function validatedLiveSessionSet(liveSessionIds: readonly string[]): Set<string> {
  const liveSessions = new Set<string>();
  for (const sessionId of liveSessionIds) {
    if (!sessionId) throw new Error("Live session IDs must be non-empty.");
    if (liveSessions.has(sessionId)) throw new Error(`Duplicate live session ID: ${sessionId}.`);
    liveSessions.add(sessionId);
  }
  return liveSessions;
}

function hasSessionLessLeaf(node: LayoutDocumentNode): boolean {
  if (node.kind === "leaf") return node.slot_keys.length === 0;
  return hasSessionLessLeaf(node.first) || hasSessionLessLeaf(node.second);
}

/** Mint fresh runtime IDs, append `extraSessions` to the focused leaf, then
 * collapse every leaf still holding no session: a committed empty pane renders
 * no tab strip, so it has no close affordance and survives every reconcile.
 * An empty root leaf is the only legal empty result. */
function materializeLayoutDocument(
  parsed: LayoutDocumentV1,
  extraSessions: readonly string[],
): AppliedLayoutDocument {
  const sessionBySlot = new Map(
    parsed.bindings.map((binding) => [binding.slot_key, binding.session_id]),
  );
  const usedRuntimeIds = new Set<string>();
  let focusedLeafPaneId: string | null = null;

  function materializeNode(node: LayoutDocumentNode): PaneNode {
    if (node.kind === "split") {
      return {
        kind: "split",
        id: mintRuntimeId(usedRuntimeIds),
        dir: node.direction,
        ratio: node.ratio,
        a: materializeNode(node.first),
        b: materializeNode(node.second),
      };
    }

    const paneId = mintRuntimeId(usedRuntimeIds);
    const tabs = node.slot_keys
      .map((slotKey) => sessionBySlot.get(slotKey))
      .filter((sessionId): sessionId is string => sessionId !== undefined);
    if (node.leaf_key === parsed.focused_leaf_key) {
      focusedLeafPaneId = paneId;
      tabs.push(...extraSessions);
    }
    const selectedSession = node.selected_slot_key
      ? sessionBySlot.get(node.selected_slot_key)
      : undefined;
    return { kind: "leaf", paneId, tabs, selectedTab: selectedSession ?? tabs[0] ?? "" };
  }

  const materialized = materializeNode(parsed.root);
  if (!focusedLeafPaneId) throw new Error("The focused layout leaf could not be materialized.");
  const sessionLessPaneIds = new Set(
    allLeaves(materialized)
      .filter((leaf) => leaf.tabs.length === 0)
      .map((leaf) => leaf.paneId),
  );
  const root = collapseEmpties(materialized, sessionLessPaneIds);
  const focusedPaneId = fixFocus(root, focusedLeafPaneId);
  return {
    layout: { root, focusedPaneId },
    selectedSessionId: findLeaf(root, focusedPaneId)?.selectedTab || null,
  };
}

/** Assign stable leaf/slot keys in first-before-second preorder. */
function documentFromLayout(layout: Layout): LayoutDocumentV1 {
  const bindings: LayoutDocumentV1["bindings"] = [];
  const leafKeyByPaneId = new Map<string, string>();
  let nextLeaf = 1;
  let nextSlot = 1;

  function exportNode(node: PaneNode): LayoutDocumentNode {
    if (node.kind === "split") {
      return {
        kind: "split",
        direction: node.dir,
        ratio: node.ratio,
        first: exportNode(node.a),
        second: exportNode(node.b),
      };
    }

    const leafKey = `leaf-${nextLeaf++}`;
    leafKeyByPaneId.set(node.paneId, leafKey);
    let selectedSlotKey: string | null = null;
    const slotKeys = node.tabs.map((sessionId) => {
      const slotKey = `slot-${nextSlot++}`;
      bindings.push({ slot_key: slotKey, session_id: sessionId });
      if (sessionId === node.selectedTab) selectedSlotKey = slotKey;
      return slotKey;
    });
    return {
      kind: "leaf",
      leaf_key: leafKey,
      slot_keys: slotKeys,
      selected_slot_key: selectedSlotKey,
    };
  }

  const root = exportNode(layout.root);
  const focusedLeafKey = leafKeyByPaneId.get(layout.focusedPaneId);
  if (!focusedLeafKey) throw new Error("The focused runtime pane does not exist in the layout tree.");
  return parseLayoutDocumentV1({
    schema_version: 1,
    root,
    focused_leaf_key: focusedLeafKey,
    bindings,
  });
}

function mintRuntimeId(usedRuntimeIds: Set<string>): string {
  for (;;) {
    const runtimeId = crypto.randomUUID();
    if (usedRuntimeIds.has(runtimeId)) continue;
    usedRuntimeIds.add(runtimeId);
    return runtimeId;
  }
}
