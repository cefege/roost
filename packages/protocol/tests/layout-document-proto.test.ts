// Protobuf adapter tests for the strict portable layout-document boundary.
// These cases pin recursive JSON/proto mapping, optional null selection encoding,
// and rejection of malformed or semantically invalid protobuf messages.

import { create } from "@bufbuild/protobuf";
import { describe, expect, test } from "bun:test";
import {
  LayoutDirection,
  LayoutDocumentBindingSchema,
  LayoutDocumentLeafSchema,
  LayoutDocumentNodeSchema,
  LayoutDocumentSplitSchema,
  LayoutDocumentV1Schema as LayoutDocumentV1ProtoSchema,
  type LayoutDocumentLeaf as ProtoLayoutDocumentLeaf,
  type LayoutDocumentNode as ProtoLayoutDocumentNode,
  type LayoutDocumentSplit as ProtoLayoutDocumentSplit,
  type LayoutDocumentV1 as ProtoLayoutDocumentV1,
} from "../src/gen/roost/v1/sync_pb.ts";
import {
  layoutDocumentFromProto,
  layoutDocumentToProto,
} from "../src/layout-document-proto.ts";
import {
  LAYOUT_RATIO_MAX,
  LAYOUT_RATIO_MIN,
  type LayoutDocumentV1,
} from "../src/layout-document.ts";

const SESSION_A = "00000000-0000-4000-8000-000000000001";
const SESSION_B = "00000000-0000-4000-8000-000000000002";

function nestedDocument(): LayoutDocumentV1 {
  return {
    schema_version: 1,
    root: {
      kind: "split",
      direction: "row",
      ratio: LAYOUT_RATIO_MIN,
      first: {
        kind: "leaf",
        leaf_key: "leaf-a",
        slot_keys: ["slot-a"],
        selected_slot_key: "slot-a",
      },
      second: {
        kind: "split",
        direction: "col",
        ratio: LAYOUT_RATIO_MAX,
        first: {
          kind: "leaf",
          leaf_key: "leaf-empty",
          slot_keys: [],
          selected_slot_key: null,
        },
        second: {
          kind: "leaf",
          leaf_key: "leaf-b",
          slot_keys: ["slot-b"],
          selected_slot_key: "slot-b",
        },
      },
    },
    focused_leaf_key: "leaf-empty",
    bindings: [
      { slot_key: "slot-a", session_id: SESSION_A },
      { slot_key: "slot-b", session_id: SESSION_B },
    ],
  };
}

function leafNode(
  leafKey: string,
  slotKeys: string[] = [],
  selectedSlotKey?: string,
): ProtoLayoutDocumentNode {
  return create(LayoutDocumentNodeSchema, {
    node: {
      case: "leaf",
      value: create(LayoutDocumentLeafSchema, {
        leafKey,
        slotKeys,
        ...(selectedSlotKey === undefined ? {} : { selectedSlotKey }),
      }),
    },
  });
}

function splitNode(
  direction: LayoutDirection,
  ratio: number,
  first?: ProtoLayoutDocumentNode,
  second?: ProtoLayoutDocumentNode,
): ProtoLayoutDocumentNode {
  return create(LayoutDocumentNodeSchema, {
    node: {
      case: "split",
      value: create(LayoutDocumentSplitSchema, { direction, ratio, first, second }),
    },
  });
}

function occupiedLeafProto(): ProtoLayoutDocumentV1 {
  return create(LayoutDocumentV1ProtoSchema, {
    schemaVersion: 1,
    root: leafNode("leaf-a", ["slot-a"], "slot-a"),
    focusedLeafKey: "leaf-a",
    bindings: [create(LayoutDocumentBindingSchema, {
      slotKey: "slot-a",
      sessionId: SESSION_A,
    })],
  });
}

function emptySplitProto(
  direction: LayoutDirection = LayoutDirection.ROW,
  ratio = 0.5,
): ProtoLayoutDocumentV1 {
  return create(LayoutDocumentV1ProtoSchema, {
    schemaVersion: 1,
    root: splitNode(
      direction,
      ratio,
      leafNode("leaf-a"),
      leafNode("leaf-b"),
    ),
    focusedLeafKey: "leaf-a",
    bindings: [],
  });
}

function requireRootLeaf(document: ProtoLayoutDocumentV1): ProtoLayoutDocumentLeaf {
  if (document.root?.node.case !== "leaf") throw new Error("expected root leaf");
  return document.root.node.value;
}

function requireRootSplit(document: ProtoLayoutDocumentV1): ProtoLayoutDocumentSplit {
  if (document.root?.node.case !== "split") throw new Error("expected root split");
  return document.root.node.value;
}

function expectProtoRejected(document: ProtoLayoutDocumentV1): void {
  expect(() => layoutDocumentFromProto(document)).toThrow();
}

describe("layout-document protobuf adapter", () => {
  test("round-trips nested trees, empty leaves, null selection, and ratio endpoints", () => {
    const source = nestedDocument();
    const proto = layoutDocumentToProto(source);

    expect(layoutDocumentFromProto(proto)).toEqual(source);
    if (proto.root?.node.case !== "split") throw new Error("expected root split");
    expect(proto.root.node.value.direction).toBe(LayoutDirection.ROW);
    expect(proto.root.node.value.ratio).toBe(LAYOUT_RATIO_MIN);

    const nested = proto.root.node.value.second;
    if (nested?.node.case !== "split") throw new Error("expected nested split");
    expect(nested.node.value.direction).toBe(LayoutDirection.COL);
    expect(nested.node.value.ratio).toBe(LAYOUT_RATIO_MAX);

    const emptyLeaf = nested.node.value.first;
    if (emptyLeaf?.node.case !== "leaf") throw new Error("expected empty leaf");
    expect(emptyLeaf.node.value).toMatchObject({
      leafKey: "leaf-empty",
      slotKeys: [],
    });
    expect(emptyLeaf.node.value.selectedSlotKey).toBeUndefined();
  });

  test("toProto revalidates type-cast input before mapping", () => {
    const invalid = {
      ...nestedDocument(),
      focused_leaf_key: "leaf-missing",
    } as LayoutDocumentV1;

    expect(() => layoutDocumentToProto(invalid)).toThrow();
  });

  test("rejects missing root, oneof node, and recursive split children", () => {
    const missingRoot = occupiedLeafProto();
    missingRoot.root = undefined;
    expectProtoRejected(missingRoot);

    const missingNode = occupiedLeafProto();
    missingNode.root = create(LayoutDocumentNodeSchema, {});
    expectProtoRejected(missingNode);

    const missingFirst = emptySplitProto();
    requireRootSplit(missingFirst).first = undefined;
    expectProtoRejected(missingFirst);

    const missingSecond = emptySplitProto();
    requireRootSplit(missingSecond).second = undefined;
    expectProtoRejected(missingSecond);
  });

  test("rejects unspecified and unknown split directions", () => {
    expectProtoRejected(emptySplitProto(LayoutDirection.UNSPECIFIED));
    expectProtoRejected(emptySplitProto(99 as LayoutDirection));
  });

  test("rejects invalid versions and split ratios", () => {
    for (const schemaVersion of [0, 2]) {
      const document = occupiedLeafProto();
      document.schemaVersion = schemaVersion;
      expectProtoRejected(document);
    }

    for (const ratio of [
      LAYOUT_RATIO_MIN - Number.EPSILON,
      LAYOUT_RATIO_MAX + Number.EPSILON,
      Number.NaN,
      Number.POSITIVE_INFINITY,
      Number.NEGATIVE_INFINITY,
    ]) {
      expectProtoRejected(emptySplitProto(LayoutDirection.ROW, ratio));
    }
  });

  test("rejects duplicate and missing bindings", () => {
    const duplicate = occupiedLeafProto();
    duplicate.bindings.push(create(LayoutDocumentBindingSchema, {
      slotKey: "slot-a",
      sessionId: SESSION_B,
    }));
    expectProtoRejected(duplicate);

    const missing = occupiedLeafProto();
    missing.bindings = [];
    expectProtoRejected(missing);
  });

  test("rejects empty leaf, slot, focus, binding, and session IDs", () => {
    const emptyLeafKey = occupiedLeafProto();
    requireRootLeaf(emptyLeafKey).leafKey = "";
    expectProtoRejected(emptyLeafKey);

    const emptySlotKey = occupiedLeafProto();
    requireRootLeaf(emptySlotKey).slotKeys = [""];
    expectProtoRejected(emptySlotKey);

    const emptySelection = occupiedLeafProto();
    requireRootLeaf(emptySelection).selectedSlotKey = "";
    expectProtoRejected(emptySelection);

    const emptyFocus = occupiedLeafProto();
    emptyFocus.focusedLeafKey = "";
    expectProtoRejected(emptyFocus);

    const emptyBindingSlot = occupiedLeafProto();
    emptyBindingSlot.bindings[0]!.slotKey = "";
    expectProtoRejected(emptyBindingSlot);

    const emptySession = occupiedLeafProto();
    emptySession.bindings[0]!.sessionId = "";
    expectProtoRejected(emptySession);
  });
});
