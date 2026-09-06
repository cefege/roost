// Portable layout-document boundary tests.
// These cases pin strict recursive shape, shared ratio bounds, graph integrity,
// and non-mutating parsing for browser and persistence adapters.

import { describe, expect, test } from "bun:test";
import {
  LAYOUT_RATIO_MAX,
  LAYOUT_RATIO_MIN,
  LayoutDocumentV1Schema,
  isLayoutDocumentV1,
  parseLayoutDocumentV1,
} from "../src/layout-document.ts";
import type {
  LayoutDocumentBinding,
  LayoutDocumentLeaf,
  LayoutDocumentNode,
  LayoutDocumentSplit,
  LayoutDocumentV1,
} from "../src/layout-document.ts";

const SESSION_A = "00000000-0000-4000-8000-000000000001";
const SESSION_B = "00000000-0000-4000-8000-000000000002";
const SESSION_C = "00000000-0000-4000-8000-000000000003";

function leaf(
  leafKey: string,
  slotKeys: string[],
  selectedSlotKey: string | null,
): LayoutDocumentLeaf {
  return {
    kind: "leaf",
    leaf_key: leafKey,
    slot_keys: slotKeys,
    selected_slot_key: selectedSlotKey,
  };
}

function split(
  first: LayoutDocumentNode,
  second: LayoutDocumentNode,
  direction: "row" | "col" = "row",
  ratio = 0.5,
): LayoutDocumentSplit {
  return { kind: "split", direction, ratio, first, second };
}

function binding(slotKey: string, sessionId: string): LayoutDocumentBinding {
  return { slot_key: slotKey, session_id: sessionId };
}

function layout(
  root: LayoutDocumentNode,
  focusedLeafKey: string,
  bindings: LayoutDocumentBinding[],
): LayoutDocumentV1 {
  return {
    schema_version: 1,
    root,
    focused_leaf_key: focusedLeafKey,
    bindings,
  };
}

function oneLeafDocument(): LayoutDocumentV1 {
  return layout(
    leaf("leaf-a", ["slot-a"], "slot-a"),
    "leaf-a",
    [binding("slot-a", SESSION_A)],
  );
}

function nestedDocument(): LayoutDocumentV1 {
  return layout(
    split(
      leaf("leaf-a", ["slot-a", "slot-b"], "slot-b"),
      split(
        leaf("leaf-b", ["slot-c"], "slot-c"),
        leaf("leaf-c", [], null),
        "col",
        LAYOUT_RATIO_MAX,
      ),
      "row",
      LAYOUT_RATIO_MIN,
    ),
    "leaf-b",
    [
      binding("slot-c", SESSION_C),
      binding("slot-a", SESSION_A),
      binding("slot-b", SESSION_B),
    ],
  );
}

function ratioDocument(ratio: number): LayoutDocumentV1 {
  return layout(
    split(leaf("leaf-a", [], null), leaf("leaf-b", [], null), "row", ratio),
    "leaf-a",
    [],
  );
}

function expectRejected(candidate: unknown): void {
  expect(LayoutDocumentV1Schema.safeParse(candidate).success).toBe(false);
}

describe("LayoutDocumentV1 parsing", () => {
  test("parses exact one-leaf and nested documents", () => {
    expect(parseLayoutDocumentV1(oneLeafDocument())).toEqual(oneLeafDocument());
    expect(parseLayoutDocumentV1(nestedDocument())).toEqual(nestedDocument());
    expect(isLayoutDocumentV1(oneLeafDocument())).toBe(true);
  });

  test("returns a normalized copy without mutating the input", () => {
    const input = nestedDocument();
    const before = structuredClone(input);
    const parsed = parseLayoutDocumentV1(input);

    expect(input).toEqual(before);
    expect(parsed).toEqual(before);
    expect(parsed).not.toBe(input);
    expect(parsed.root).not.toBe(input.root);
    expect(parsed.bindings).not.toBe(input.bindings);
  });
});

describe("LayoutDocumentV1 strict recursive shape", () => {
  test("rejects unknown schema versions and unknown document keys", () => {
    const document = oneLeafDocument();
    expectRejected({ ...document, schema_version: 2 });
    expectRejected({ ...document, schema_version: "1" });
    expectRejected({ ...document, unknown: true });
    expect(isLayoutDocumentV1({ ...document, schema_version: 2 })).toBe(false);
    expect(() => parseLayoutDocumentV1({ ...document, schema_version: 2 })).toThrow();
  });

  test("rejects unknown keys on every recursive object kind", () => {
    const document = oneLeafDocument();
    expectRejected({
      ...document,
      root: { ...document.root, unknown: true },
    });
    expectRejected({
      ...document,
      bindings: [{ ...document.bindings[0]!, unknown: true }],
    });

    const nested = nestedDocument();
    expectRejected({
      ...nested,
      root: { ...nested.root, unknown: true },
    });
    if (nested.root.kind !== "split") throw new Error("expected nested split");
    expectRejected({
      ...nested,
      root: {
        ...nested.root,
        first: { ...nested.root.first, unknown: true },
      },
    });
  });

  test("rejects malformed nodes at any recursive depth", () => {
    const document = oneLeafDocument();
    expectRejected({ ...document, root: null });
    expectRejected({
      ...document,
      root: {
        kind: "leaf",
        leaf_key: "leaf-a",
        slot_keys: ["slot-a"],
      },
    });
    expectRejected({
      ...document,
      root: {
        kind: "branch",
        leaf_key: "leaf-a",
        slot_keys: ["slot-a"],
        selected_slot_key: "slot-a",
      },
    });
    expectRejected({
      ...ratioDocument(0.5),
      root: {
        kind: "split",
        direction: "column",
        ratio: 0.5,
        first: leaf("leaf-a", [], null),
        second: leaf("leaf-b", [], null),
      },
    });
    expectRejected({
      ...ratioDocument(0.5),
      root: {
        kind: "split",
        direction: "row",
        ratio: 0.5,
        first: leaf("leaf-a", [], null),
        second: { kind: "leaf", leaf_key: "leaf-b" },
      },
    });
  });
});

describe("LayoutDocumentV1 split ratios", () => {
  test("accepts both inclusive ratio endpoints", () => {
    expect(LayoutDocumentV1Schema.safeParse(
      ratioDocument(LAYOUT_RATIO_MIN),
    ).success).toBe(true);
    expect(LayoutDocumentV1Schema.safeParse(
      ratioDocument(LAYOUT_RATIO_MAX),
    ).success).toBe(true);
  });

  test("rejects ratios outside the endpoints and non-finite numbers", () => {
    for (const ratio of [
      LAYOUT_RATIO_MIN - Number.EPSILON,
      LAYOUT_RATIO_MAX + Number.EPSILON,
      Number.NaN,
      Number.POSITIVE_INFINITY,
      Number.NEGATIVE_INFINITY,
    ]) {
      expectRejected(ratioDocument(ratio));
    }
  });
});

describe("LayoutDocumentV1 keys and references", () => {
  test("rejects empty leaf and slot keys in definitions and references", () => {
    expectRejected(layout(
      split(leaf("leaf-a", [], null), leaf("", [], null)),
      "leaf-a",
      [],
    ));
    expectRejected({ ...oneLeafDocument(), focused_leaf_key: "" });
    expectRejected(layout(
      leaf("leaf-a", ["", "slot-a"], "slot-a"),
      "leaf-a",
      [binding("slot-a", SESSION_A)],
    ));
    expectRejected(layout(
      leaf("leaf-a", ["slot-a"], ""),
      "leaf-a",
      [binding("slot-a", SESSION_A)],
    ));
    expectRejected(layout(
      leaf("leaf-a", ["slot-a"], "slot-a"),
      "leaf-a",
      [binding("", SESSION_A)],
    ));
  });

  test("rejects duplicate leaf keys", () => {
    expectRejected(layout(
      split(
        leaf("leaf-a", ["slot-a"], "slot-a"),
        leaf("leaf-a", ["slot-b"], "slot-b"),
      ),
      "leaf-a",
      [binding("slot-a", SESSION_A), binding("slot-b", SESSION_B)],
    ));
  });

  test("rejects duplicate slot keys within or across leaves", () => {
    expectRejected(layout(
      leaf("leaf-a", ["slot-a", "slot-a"], "slot-a"),
      "leaf-a",
      [binding("slot-a", SESSION_A)],
    ));
    expectRejected(layout(
      split(
        leaf("leaf-a", ["slot-a"], "slot-a"),
        leaf("leaf-b", ["slot-a"], "slot-a"),
      ),
      "leaf-a",
      [binding("slot-a", SESSION_A)],
    ));
  });

  test("rejects a focused key that does not name a leaf", () => {
    expectRejected({ ...oneLeafDocument(), focused_leaf_key: "leaf-missing" });
  });

  test("rejects selected slots that are absent or owned by another leaf", () => {
    expectRejected(layout(
      leaf("leaf-a", ["slot-a"], "slot-missing"),
      "leaf-a",
      [binding("slot-a", SESSION_A)],
    ));
    expectRejected(layout(
      split(
        leaf("leaf-a", ["slot-a"], "slot-b"),
        leaf("leaf-b", ["slot-b"], "slot-b"),
      ),
      "leaf-a",
      [binding("slot-a", SESSION_A), binding("slot-b", SESSION_B)],
    ));
  });

  test("requires null selection exactly for leaves without slots", () => {
    expectRejected(layout(
      leaf("leaf-a", ["slot-a"], null),
      "leaf-a",
      [binding("slot-a", SESSION_A)],
    ));
    expectRejected(layout(
      split(
        leaf("leaf-a", [], "slot-b"),
        leaf("leaf-b", ["slot-b"], "slot-b"),
      ),
      "leaf-a",
      [binding("slot-b", SESSION_B)],
    ));
  });
});

describe("LayoutDocumentV1 slot bindings", () => {
  test("rejects a missing binding", () => {
    expectRejected(layout(
      leaf("leaf-a", ["slot-a", "slot-b"], "slot-a"),
      "leaf-a",
      [binding("slot-a", SESSION_A)],
    ));
  });

  test("rejects an extra binding", () => {
    expectRejected(layout(
      leaf("leaf-a", ["slot-a"], "slot-a"),
      "leaf-a",
      [binding("slot-a", SESSION_A), binding("slot-extra", SESSION_B)],
    ));
  });

  test("rejects duplicate bindings for one slot", () => {
    expectRejected(layout(
      leaf("leaf-a", ["slot-a"], "slot-a"),
      "leaf-a",
      [binding("slot-a", SESSION_A), binding("slot-a", SESSION_B)],
    ));
  });

  test("rejects an empty bound session ID", () => {
    expectRejected(layout(
      leaf("leaf-a", ["slot-a"], "slot-a"),
      "leaf-a",
      [binding("slot-a", "")],
    ));
  });

  test("rejects a session bound to more than one slot", () => {
    expectRejected(layout(
      leaf("leaf-a", ["slot-a", "slot-b"], "slot-a"),
      "leaf-a",
      [binding("slot-a", SESSION_A), binding("slot-b", SESSION_A)],
    ));
  });
});
