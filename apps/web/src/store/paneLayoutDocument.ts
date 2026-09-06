// Converts browser-local pane layouts to the portable V1 document and back.
// Shared validation and the canonical live-session set fence imported bindings;
// only a fully materialized runtime tree crosses paneLayoutStore.commitLayout.
// Runtime pane and split UUIDs never enter the document.

import { parseLayoutDocumentV1 } from "@roost/shared/layout-document";
import type {
  LayoutDocumentNode,
  LayoutDocumentV1,
} from "@roost/shared/layout-document";
import type { Layout, PaneNode } from "./paneLayout.ts";
import { commitLayout, resolveLayout } from "./paneLayoutStore.ts";

export interface AppliedLayoutDocument {
  layout: Layout;
  selectedSessionId: string | null;
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

/** Export stable leaf/slot keys in first-before-second preorder. */
export function exportLayoutDocument(
  folderKey: string,
  liveSessionIds: readonly string[],
): LayoutDocumentV1 {
  assertFolderKey(folderKey);
  validatedLiveSessionSet(liveSessionIds);
  const layout = resolveLayout(folderKey, [...liveSessionIds]);
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

/** Validate and materialize entirely in locals, then publish one store commit. */
export function applyLayoutDocument(
  folderKey: string,
  document: unknown,
  liveSessionIds: readonly string[],
): AppliedLayoutDocument {
  assertFolderKey(folderKey);
  const parsed = validateLayoutDocumentImport(document, liveSessionIds);
  const sessionBySlot = new Map(
    parsed.bindings.map((binding) => [binding.slot_key, binding.session_id]),
  );
  const boundSessions = new Set(parsed.bindings.map((binding) => binding.session_id));
  const extraSessions = liveSessionIds.filter((sessionId) => !boundSessions.has(sessionId));
  const usedRuntimeIds = new Set<string>();
  let focusedPaneId: string | null = null;
  let selectedSessionId: string | null = null;

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
    const tabs = node.slot_keys.map((slotKey) => sessionBySlot.get(slotKey)!);
    let selectedTab = node.selected_slot_key
      ? sessionBySlot.get(node.selected_slot_key)!
      : "";
    if (node.leaf_key === parsed.focused_leaf_key) {
      focusedPaneId = paneId;
      tabs.push(...extraSessions);
      selectedTab ||= tabs[0] ?? "";
      selectedSessionId = selectedTab || null;
    }
    return { kind: "leaf", paneId, tabs, selectedTab };
  }

  const root = materializeNode(parsed.root);
  if (!focusedPaneId) throw new Error("The focused layout leaf could not be materialized.");
  const layout: Layout = { root, focusedPaneId };
  commitLayout(folderKey, layout);
  return { layout, selectedSessionId };
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

function mintRuntimeId(usedRuntimeIds: Set<string>): string {
  for (;;) {
    const runtimeId = crypto.randomUUID();
    if (usedRuntimeIds.has(runtimeId)) continue;
    usedRuntimeIds.add(runtimeId);
    return runtimeId;
  }
}
