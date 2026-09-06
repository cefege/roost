// Converts the strict portable layout document between JSON-facing and protobuf shapes.
// Both directions resource-preflight before recursion and validate through one parser.
// Coordinator, browser, and CLI share this adapter instead of hand-mapping recursion.

import { create } from "@bufbuild/protobuf";
import {
  LayoutDirection,
  LayoutDocumentBindingSchema,
  LayoutDocumentLeafSchema,
  LayoutDocumentNodeSchema,
  LayoutDocumentSplitSchema,
  LayoutDocumentV1Schema as LayoutDocumentV1ProtoSchema,
  type LayoutDocumentNode as ProtoLayoutDocumentNode,
  type LayoutDocumentV1 as ProtoLayoutDocumentV1,
} from "./gen/roost/v1/sync_pb.ts";
import {
  LAYOUT_DOCUMENT_MAX_BINDINGS,
  LAYOUT_DOCUMENT_MAX_DEPTH,
  LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES,
  LAYOUT_DOCUMENT_MAX_NODES,
  LAYOUT_DOCUMENT_MAX_SESSION_ID_UTF8_BYTES,
  LAYOUT_DOCUMENT_MAX_SLOTS,
  parseLayoutDocumentV1,
  type LayoutDocumentNode,
  type LayoutDocumentV1,
} from "./layout-document.ts";
import { hasAtMostUtf8Bytes } from "./ui-state.ts";

export function layoutDocumentToProto(document: LayoutDocumentV1): ProtoLayoutDocumentV1 {
  const checked = parseLayoutDocumentV1(document);
  return create(LayoutDocumentV1ProtoSchema, {
    schemaVersion: checked.schema_version,
    root: layoutNodeToProto(checked.root),
    focusedLeafKey: checked.focused_leaf_key,
    bindings: checked.bindings.map((binding) => create(LayoutDocumentBindingSchema, {
      slotKey: binding.slot_key,
      sessionId: binding.session_id,
    })),
  });
}

export function layoutDocumentFromProto(document: ProtoLayoutDocumentV1): LayoutDocumentV1 {
  assertProtoLayoutDocumentResources(document);
  return parseLayoutDocumentV1({
    schema_version: document.schemaVersion,
    root: layoutNodeFromProto(document.root),
    focused_leaf_key: document.focusedLeafKey,
    bindings: document.bindings.map((binding) => ({
      slot_key: binding.slotKey,
      session_id: binding.sessionId,
    })),
  });
}

function layoutNodeToProto(node: LayoutDocumentNode): ProtoLayoutDocumentNode {
  if (node.kind === "leaf") {
    return create(LayoutDocumentNodeSchema, {
      node: {
        case: "leaf",
        value: create(LayoutDocumentLeafSchema, {
          leafKey: node.leaf_key,
          slotKeys: node.slot_keys,
          selectedSlotKey: node.selected_slot_key ?? undefined,
        }),
      },
    });
  }
  return create(LayoutDocumentNodeSchema, {
    node: {
      case: "split",
      value: create(LayoutDocumentSplitSchema, {
        direction: node.direction === "row" ? LayoutDirection.ROW : LayoutDirection.COL,
        ratio: node.ratio,
        first: layoutNodeToProto(node.first),
        second: layoutNodeToProto(node.second),
      }),
    },
  });
}

function layoutNodeFromProto(node: ProtoLayoutDocumentNode | undefined): unknown {
  if (node?.node.case === "leaf") {
    const leaf = node.node.value;
    return {
      kind: "leaf",
      leaf_key: leaf.leafKey,
      slot_keys: [...leaf.slotKeys],
      selected_slot_key: leaf.selectedSlotKey ?? null,
    };
  }
  if (node?.node.case !== "split") return null;
  const split = node.node.value;
  return {
    kind: "split",
    direction: split.direction === LayoutDirection.ROW
      ? "row"
      : split.direction === LayoutDirection.COL
        ? "col"
        : null,
    ratio: split.ratio,
    first: layoutNodeFromProto(split.first),
    second: layoutNodeFromProto(split.second),
  };
}

function assertProtoLayoutDocumentResources(document: ProtoLayoutDocumentV1): void {
  assertUtf8Bound(
    document.focusedLeafKey,
    LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES,
    "layout key",
  );
  if (document.bindings.length > LAYOUT_DOCUMENT_MAX_BINDINGS) {
    throw new RangeError(`layout document exceeds ${LAYOUT_DOCUMENT_MAX_BINDINGS} bindings`);
  }
  for (const binding of document.bindings) {
    assertUtf8Bound(binding.slotKey, LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES, "layout key");
    assertUtf8Bound(
      binding.sessionId,
      LAYOUT_DOCUMENT_MAX_SESSION_ID_UTF8_BYTES,
      "layout session_id",
    );
  }

  const pending: Array<{ node: ProtoLayoutDocumentNode | undefined; depth: number }> = [
    { node: document.root, depth: 1 },
  ];
  let nodes = 0;
  let slots = 0;
  while (pending.length > 0) {
    const current = pending.pop()!;
    if (current.depth > LAYOUT_DOCUMENT_MAX_DEPTH) {
      throw new RangeError(`layout document exceeds depth ${LAYOUT_DOCUMENT_MAX_DEPTH}`);
    }
    if (!current.node?.node.case) continue;
    nodes++;
    if (nodes > LAYOUT_DOCUMENT_MAX_NODES) {
      throw new RangeError(`layout document exceeds ${LAYOUT_DOCUMENT_MAX_NODES} nodes`);
    }
    if (current.node.node.case === "split") {
      const split = current.node.node.value;
      pending.push(
        { node: split.second, depth: current.depth + 1 },
        { node: split.first, depth: current.depth + 1 },
      );
      continue;
    }
    const leaf = current.node.node.value;
    assertUtf8Bound(leaf.leafKey, LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES, "layout key");
    if (leaf.selectedSlotKey !== undefined) {
      assertUtf8Bound(
        leaf.selectedSlotKey,
        LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES,
        "layout key",
      );
    }
    if (leaf.slotKeys.length > LAYOUT_DOCUMENT_MAX_SLOTS - slots) {
      throw new RangeError(`layout document exceeds ${LAYOUT_DOCUMENT_MAX_SLOTS} slots`);
    }
    slots += leaf.slotKeys.length;
    for (const slotKey of leaf.slotKeys) {
      assertUtf8Bound(slotKey, LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES, "layout key");
    }
  }
}

function assertUtf8Bound(value: string, maxBytes: number, field: string): void {
  if (!hasAtMostUtf8Bytes(value, maxBytes)) {
    throw new RangeError(`${field} must not exceed ${maxBytes} UTF-8 bytes`);
  }
}
