// Resource-bound tests for portable layout documents and protobuf adapters.
// These cases pin exact UTF-8/tree/cardinality endpoints and iterative rejection.
// Deep hostile inputs must fail before either Zod or adapter recursion begins.

import { create } from "@bufbuild/protobuf";
import { describe, expect, test } from "bun:test";
import {
  LayoutDirection,
  LayoutDocumentBindingSchema,
  LayoutDocumentLeafSchema,
  LayoutDocumentNodeSchema,
  LayoutDocumentSplitSchema,
  LayoutDocumentV1Schema as LayoutDocumentV1ProtoSchema,
  type LayoutDocumentNode as ProtoLayoutDocumentNode,
  type LayoutDocumentV1 as ProtoLayoutDocumentV1,
} from "../src/gen/roost/v1/sync_pb.ts";
import {
  LAYOUT_DOCUMENT_MAX_BINDINGS,
  LAYOUT_DOCUMENT_MAX_DEPTH,
  LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES,
  LAYOUT_DOCUMENT_MAX_NODES,
  LAYOUT_DOCUMENT_MAX_SESSION_ID_UTF8_BYTES,
  LAYOUT_DOCUMENT_MAX_SLOTS,
  LayoutDocumentV1Schema,
  isLayoutDocumentV1,
  parseLayoutDocumentV1,
  type LayoutDocumentBinding,
  type LayoutDocumentLeaf,
  type LayoutDocumentNode,
  type LayoutDocumentV1,
} from "../src/layout-document.ts";
import {
  layoutDocumentFromProto,
  layoutDocumentToProto,
} from "../src/layout-document-proto.ts";

function leaf(leafKey: string): LayoutDocumentLeaf {
  return { kind: "leaf", leaf_key: leafKey, slot_keys: [], selected_slot_key: null };
}

function split(first: LayoutDocumentNode, second: LayoutDocumentNode): LayoutDocumentNode {
  return { kind: "split", direction: "row", ratio: 0.5, first, second };
}

function document(
  root: LayoutDocumentNode,
  focusedLeafKey: string,
  bindings: LayoutDocumentBinding[] = [],
): LayoutDocumentV1 {
  return { schema_version: 1, root, focused_leaf_key: focusedLeafKey, bindings };
}

function depthDocument(depth: number): LayoutDocumentV1 {
  let root: LayoutDocumentNode = leaf("deep-leaf");
  for (let level = 2; level <= depth; level++) {
    root = split(leaf(`side-${level}`), root);
  }
  return document(root, "deep-leaf");
}

function completeTree(depth: number, nextKey: { value: number }): LayoutDocumentNode {
  if (depth === 1) return leaf(`leaf-${nextKey.value++}`);
  return split(completeTree(depth - 1, nextKey), completeTree(depth - 1, nextKey));
}

function documentWithSlots(slotCount: number, bindingCount = slotCount): LayoutDocumentV1 {
  const slotKeys = Array.from({ length: slotCount }, (_, index) => `slot-${index}`);
  const bindings = Array.from({ length: bindingCount }, (_, index) => ({
    slot_key: index < slotKeys.length ? slotKeys[index]! : `extra-${index}`,
    session_id: `session-${index}`,
  }));
  return document({
    kind: "leaf",
    leaf_key: "leaf-slots",
    slot_keys: slotKeys,
    selected_slot_key: slotKeys[0] ?? null,
  }, "leaf-slots", bindings);
}

function protoLeaf(leafKey: string): ProtoLayoutDocumentNode {
  return create(LayoutDocumentNodeSchema, {
    node: {
      case: "leaf",
      value: create(LayoutDocumentLeafSchema, { leafKey }),
    },
  });
}

function protoSplit(
  first: ProtoLayoutDocumentNode,
  second: ProtoLayoutDocumentNode,
): ProtoLayoutDocumentNode {
  return create(LayoutDocumentNodeSchema, {
    node: {
      case: "split",
      value: create(LayoutDocumentSplitSchema, {
        direction: LayoutDirection.ROW,
        ratio: 0.5,
        first,
        second,
      }),
    },
  });
}

function protoDepthDocument(depth: number): ProtoLayoutDocumentV1 {
  let root = protoLeaf("deep-leaf");
  for (let level = 2; level <= depth; level++) {
    root = protoSplit(protoLeaf(`side-${level}`), root);
  }
  return create(LayoutDocumentV1ProtoSchema, {
    schemaVersion: 1,
    root,
    focusedLeafKey: "deep-leaf",
  });
}

describe("LayoutDocumentV1 resource bounds", () => {
  test("accepts exact UTF-8 key and session-id boundaries", () => {
    const exactKey = "🙂".repeat(LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES / 4);
    const exactSession = "🙂".repeat(LAYOUT_DOCUMENT_MAX_SESSION_ID_UTF8_BYTES / 4);
    const exact = document({
      kind: "leaf",
      leaf_key: exactKey,
      slot_keys: [exactKey],
      selected_slot_key: exactKey,
    }, exactKey, [{ slot_key: exactKey, session_id: exactSession }]);
    expect(parseLayoutDocumentV1(exact)).toEqual(exact);
  });

  test("rejects keys and session IDs one byte over their UTF-8 bounds", () => {
    const overKey = `${"🙂".repeat(LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES / 4)}x`;
    const keyResult = LayoutDocumentV1Schema.safeParse(document(leaf(overKey), "other"));
    expect(keyResult.success).toBe(false);
    if (!keyResult.success) {
      expect(keyResult.error.issues[0]?.message).toContain("layout key");
    }

    const overSession = `${"🙂".repeat(LAYOUT_DOCUMENT_MAX_SESSION_ID_UTF8_BYTES / 4)}x`;
    const withSession = documentWithSlots(1);
    withSession.bindings[0]!.session_id = overSession;
    expect(isLayoutDocumentV1(withSession)).toBe(false);
  });

  test("accepts exact depth and node-count boundaries", () => {
    expect(parseLayoutDocumentV1(depthDocument(LAYOUT_DOCUMENT_MAX_DEPTH))).toEqual(
      depthDocument(LAYOUT_DOCUMENT_MAX_DEPTH),
    );
    const completeDepth = Math.log2(LAYOUT_DOCUMENT_MAX_NODES + 1);
    expect(Number.isInteger(completeDepth)).toBe(true);
    const exactNodes = document(completeTree(completeDepth, { value: 0 }), "leaf-0");
    expect(parseLayoutDocumentV1(exactNodes)).toEqual(exactNodes);
  });

  test("rejects over-limit depth and node count before recursive parsing", () => {
    const depthResult = LayoutDocumentV1Schema.safeParse(
      depthDocument(LAYOUT_DOCUMENT_MAX_DEPTH + 1),
    );
    expect(depthResult.success).toBe(false);
    if (!depthResult.success) {
      expect(depthResult.error.issues[0]?.message).toContain("exceeds depth");
    }

    const exactTree = completeTree(Math.log2(LAYOUT_DOCUMENT_MAX_NODES + 1), { value: 0 });
    const overNodes = document(split(exactTree, leaf("extra-leaf")), "leaf-0");
    const nodeResult = LayoutDocumentV1Schema.safeParse(overNodes);
    expect(nodeResult.success).toBe(false);
    if (!nodeResult.success) {
      expect(nodeResult.error.issues[0]?.message).toContain(
        `exceeds ${LAYOUT_DOCUMENT_MAX_NODES} nodes`,
      );
    }
  });

  test("accepts exact slot and binding counts and rejects each next entry", () => {
    const exact = documentWithSlots(LAYOUT_DOCUMENT_MAX_SLOTS);
    expect(LAYOUT_DOCUMENT_MAX_BINDINGS).toBe(LAYOUT_DOCUMENT_MAX_SLOTS);
    expect(parseLayoutDocumentV1(exact)).toEqual(exact);

    const slotResult = LayoutDocumentV1Schema.safeParse(
      documentWithSlots(LAYOUT_DOCUMENT_MAX_SLOTS + 1, LAYOUT_DOCUMENT_MAX_BINDINGS),
    );
    expect(slotResult.success).toBe(false);
    if (!slotResult.success) {
      expect(slotResult.error.issues[0]?.message).toContain(
        `exceeds ${LAYOUT_DOCUMENT_MAX_SLOTS} slots`,
      );
    }

    const bindingResult = LayoutDocumentV1Schema.safeParse(
      documentWithSlots(LAYOUT_DOCUMENT_MAX_SLOTS, LAYOUT_DOCUMENT_MAX_BINDINGS + 1),
    );
    expect(bindingResult.success).toBe(false);
    if (!bindingResult.success) {
      expect(bindingResult.error.issues[0]?.message).toContain(
        `exceeds ${LAYOUT_DOCUMENT_MAX_BINDINGS} bindings`,
      );
    }
  });

  test("bounds extreme arbitrary JSON without throwing a recursion error", () => {
    const hostile = depthDocument(10_000);
    expect(() => LayoutDocumentV1Schema.safeParse(hostile)).not.toThrow();
    expect(LayoutDocumentV1Schema.safeParse(hostile).success).toBe(false);
    expect(isLayoutDocumentV1(hostile)).toBe(false);
  });
});

describe("layout-document protobuf resource preflight", () => {
  test("round-trips the exact depth boundary", () => {
    const exact = depthDocument(LAYOUT_DOCUMENT_MAX_DEPTH);
    expect(layoutDocumentFromProto(layoutDocumentToProto(exact))).toEqual(exact);
    expect(layoutDocumentFromProto(protoDepthDocument(LAYOUT_DOCUMENT_MAX_DEPTH))).toEqual(exact);
  });

  test("round-trips exact string, slot, binding, and node-count boundaries", () => {
    const exactKey = "🙂".repeat(LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES / 4);
    const exactSession = "🙂".repeat(LAYOUT_DOCUMENT_MAX_SESSION_ID_UTF8_BYTES / 4);
    const exactRepeated = documentWithSlots(LAYOUT_DOCUMENT_MAX_SLOTS);
    if (exactRepeated.root.kind !== "leaf") throw new Error("expected leaf");
    exactRepeated.root.leaf_key = exactKey;
    exactRepeated.focused_leaf_key = exactKey;
    exactRepeated.root.slot_keys[0] = exactKey;
    exactRepeated.root.selected_slot_key = exactKey;
    exactRepeated.bindings[0]!.slot_key = exactKey;
    exactRepeated.bindings[0]!.session_id = exactSession;
    expect(layoutDocumentFromProto(layoutDocumentToProto(exactRepeated))).toEqual(exactRepeated);

    const completeDepth = Math.log2(LAYOUT_DOCUMENT_MAX_NODES + 1);
    const exactNodes = document(completeTree(completeDepth, { value: 0 }), "leaf-0");
    expect(layoutDocumentFromProto(layoutDocumentToProto(exactNodes))).toEqual(exactNodes);
  });

  test("rejects deep protobufs before recursive conversion", () => {
    expect(() => layoutDocumentFromProto(
      protoDepthDocument(LAYOUT_DOCUMENT_MAX_DEPTH + 1),
    )).toThrow(`layout document exceeds depth ${LAYOUT_DOCUMENT_MAX_DEPTH}`);
    expect(() => layoutDocumentToProto(
      depthDocument(LAYOUT_DOCUMENT_MAX_DEPTH + 1),
    )).toThrow();
  });

  test("rejects cyclic malformed protobuf recursion in bounded work", () => {
    const recursive = protoSplit(protoLeaf("side"), protoLeaf("placeholder"));
    if (recursive.node.case !== "split") throw new Error("expected split");
    recursive.node.value.second = recursive;
    const malformed = create(LayoutDocumentV1ProtoSchema, {
      schemaVersion: 1,
      root: recursive,
      focusedLeafKey: "side",
    });
    expect(() => layoutDocumentFromProto(malformed)).toThrow(
      `layout document exceeds depth ${LAYOUT_DOCUMENT_MAX_DEPTH}`,
    );
  });

  test("rejects over-limit protobuf strings and repeated fields", () => {
    const overKey = protoDepthDocument(1);
    overKey.focusedLeafKey = "x".repeat(LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES + 1);
    expect(() => layoutDocumentFromProto(overKey)).toThrow("layout key");

    const overSession = layoutDocumentToProto(documentWithSlots(1));
    overSession.bindings[0]!.sessionId =
      "🙂".repeat(LAYOUT_DOCUMENT_MAX_SESSION_ID_UTF8_BYTES / 4) + "x";
    expect(() => layoutDocumentFromProto(overSession)).toThrow("layout session_id");

    const overSlots = layoutDocumentToProto(documentWithSlots(LAYOUT_DOCUMENT_MAX_SLOTS));
    if (overSlots.root?.node.case !== "leaf") throw new Error("expected leaf");
    overSlots.root.node.value.slotKeys.push("slot-extra");
    expect(() => layoutDocumentFromProto(overSlots)).toThrow("slots");

    const completeDepth = Math.log2(LAYOUT_DOCUMENT_MAX_NODES + 1);
    const exactNodes = layoutDocumentToProto(
      document(completeTree(completeDepth, { value: 0 }), "leaf-0"),
    );
    exactNodes.root = protoSplit(exactNodes.root!, protoLeaf("extra-leaf"));
    expect(() => layoutDocumentFromProto(exactNodes)).toThrow("nodes");

    const overBindings = protoDepthDocument(1);
    overBindings.bindings = Array.from(
      { length: LAYOUT_DOCUMENT_MAX_BINDINGS + 1 },
      (_, index) => create(LayoutDocumentBindingSchema, {
        slotKey: `slot-${index}`,
        sessionId: `session-${index}`,
      }),
    );
    expect(() => layoutDocumentFromProto(overBindings)).toThrow("bindings");
  });
});
