// Portable layout documents describe pane geometry and session placement.
// Browser and persistence adapters share this strict versioned boundary.
// Iterative resource preflight precedes recursive shape and graph validation.

import { z } from "zod";
import {
  LAYOUT_DOCUMENT_MAX_BINDINGS,
  LAYOUT_DOCUMENT_MAX_DEPTH,
  LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES,
  LAYOUT_DOCUMENT_MAX_NODES,
  LAYOUT_DOCUMENT_MAX_SESSION_ID_UTF8_BYTES,
  LAYOUT_DOCUMENT_MAX_SLOTS,
  preflightLayoutDocumentResources,
} from "./layout-document-preflight.ts";
import { hasAtMostUtf8Bytes } from "./ui-state.ts";

export {
  LAYOUT_DOCUMENT_MAX_BINDINGS,
  LAYOUT_DOCUMENT_MAX_DEPTH,
  LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES,
  LAYOUT_DOCUMENT_MAX_NODES,
  LAYOUT_DOCUMENT_MAX_SESSION_ID_UTF8_BYTES,
  LAYOUT_DOCUMENT_MAX_SLOTS,
};

export type LayoutDocumentBinding = {
  slot_key: string;
  session_id: string;
};

export type LayoutDocumentLeaf = {
  kind: "leaf";
  leaf_key: string;
  slot_keys: string[];
  selected_slot_key: string | null;
};

export type LayoutDocumentSplit = {
  kind: "split";
  direction: "row" | "col";
  ratio: number;
  first: LayoutDocumentNode;
  second: LayoutDocumentNode;
};

export type LayoutDocumentNode = LayoutDocumentLeaf | LayoutDocumentSplit;

export type LayoutDocumentV1 = {
  schema_version: 1;
  root: LayoutDocumentNode;
  focused_leaf_key: string;
  bindings: LayoutDocumentBinding[];
};

export const LAYOUT_RATIO_MIN = 0.1;
export const LAYOUT_RATIO_MAX = 0.9;

const LayoutKeySchema = boundedNonemptyUtf8String(
  LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES,
  "layout key",
);
const LayoutDocumentBindingSchema = z.object({
  slot_key: LayoutKeySchema,
  session_id: boundedNonemptyUtf8String(
    LAYOUT_DOCUMENT_MAX_SESSION_ID_UTF8_BYTES,
    "layout session_id",
  ),
}).strict();
const LayoutDocumentLeafSchema = z.object({
  kind: z.literal("leaf"),
  leaf_key: LayoutKeySchema,
  slot_keys: z.array(LayoutKeySchema),
  selected_slot_key: LayoutKeySchema.nullable(),
}).strict();
const LayoutDocumentNodeSchema: z.ZodType<LayoutDocumentNode> = z.lazy(
  () => LayoutDocumentNodeUnionSchema,
);
const LayoutDocumentSplitSchema = z.object({
  kind: z.literal("split"),
  direction: z.enum(["row", "col"]),
  ratio: z.number().finite().min(LAYOUT_RATIO_MIN).max(LAYOUT_RATIO_MAX),
  first: LayoutDocumentNodeSchema,
  second: LayoutDocumentNodeSchema,
}).strict();
const LayoutDocumentNodeUnionSchema = z.discriminatedUnion("kind", [
  LayoutDocumentLeafSchema,
  LayoutDocumentSplitSchema,
]);

const RecursiveLayoutDocumentV1Schema: z.ZodType<LayoutDocumentV1> = z.object({
  schema_version: z.literal(1),
  root: LayoutDocumentNodeSchema,
  focused_leaf_key: LayoutKeySchema,
  bindings: z.array(LayoutDocumentBindingSchema).max(LAYOUT_DOCUMENT_MAX_BINDINGS),
}).strict().superRefine(validateLayoutDocumentGraph);
const LayoutDocumentResourcePreflightSchema = z.unknown().superRefine(
  preflightLayoutDocumentResources,
);

export const LayoutDocumentV1Schema = LayoutDocumentResourcePreflightSchema.pipe(
  RecursiveLayoutDocumentV1Schema,
);

export function parseLayoutDocumentV1(input: unknown): LayoutDocumentV1 {
  return LayoutDocumentV1Schema.parse(input);
}

export function isLayoutDocumentV1(input: unknown): input is LayoutDocumentV1 {
  return LayoutDocumentV1Schema.safeParse(input).success;
}

type LayoutDocumentPath = (string | number)[];

type LayoutLeafRecord = {
  leaf: LayoutDocumentLeaf;
  path: LayoutDocumentPath;
};

function boundedNonemptyUtf8String(maxBytes: number, field: string) {
  return z.string().min(1).refine(
    value => hasAtMostUtf8Bytes(value, maxBytes),
    `${field} must not exceed ${maxBytes} UTF-8 bytes`,
  );
}

function validateLayoutDocumentGraph(
  document: LayoutDocumentV1,
  context: z.RefinementCtx,
): void {
  const leafKeys = new Set<string>();
  const leaves: LayoutLeafRecord[] = [];
  const slotOwners = new Map<string, LayoutLeafRecord>();
  collectLayoutGraph(
    document.root,
    ["root"],
    leafKeys,
    leaves,
    slotOwners,
    context,
  );

  if (!leafKeys.has(document.focused_leaf_key)) {
    addGraphIssue(
      context,
      ["focused_leaf_key"],
      "focused_leaf_key must reference a leaf in root",
    );
  }

  for (const record of leaves) {
    validateSelectedSlot(record, slotOwners, context);
  }
  validateBindings(document.bindings, slotOwners, context);
}

function collectLayoutGraph(
  node: LayoutDocumentNode,
  path: LayoutDocumentPath,
  leafKeys: Set<string>,
  leaves: LayoutLeafRecord[],
  slotOwners: Map<string, LayoutLeafRecord>,
  context: z.RefinementCtx,
): void {
  if (node.kind === "split") {
    collectLayoutGraph(
      node.first,
      [...path, "first"],
      leafKeys,
      leaves,
      slotOwners,
      context,
    );
    collectLayoutGraph(
      node.second,
      [...path, "second"],
      leafKeys,
      leaves,
      slotOwners,
      context,
    );
    return;
  }

  const record = { leaf: node, path };
  leaves.push(record);
  if (leafKeys.has(node.leaf_key)) {
    addGraphIssue(context, [...path, "leaf_key"], "leaf_key values must be unique");
  } else {
    leafKeys.add(node.leaf_key);
  }

  for (let index = 0; index < node.slot_keys.length; index++) {
    const slotKey = node.slot_keys[index]!;
    if (slotOwners.has(slotKey)) {
      addGraphIssue(
        context,
        [...path, "slot_keys", index],
        "slot_key values must be unique across the document",
      );
    } else {
      slotOwners.set(slotKey, record);
    }
  }
}

function validateSelectedSlot(
  record: LayoutLeafRecord,
  slotOwners: Map<string, LayoutLeafRecord>,
  context: z.RefinementCtx,
): void {
  const { leaf, path } = record;
  const hasSlots = leaf.slot_keys.length > 0;
  if ((leaf.selected_slot_key === null) === hasSlots) {
    addGraphIssue(
      context,
      [...path, "selected_slot_key"],
      "selected_slot_key must be null exactly when slot_keys is empty",
    );
  }
  if (leaf.selected_slot_key === null) return;

  const owner = slotOwners.get(leaf.selected_slot_key);
  if (!owner) {
    addGraphIssue(
      context,
      [...path, "selected_slot_key"],
      "selected_slot_key must reference a slot in root",
    );
  } else if (owner !== record) {
    addGraphIssue(
      context,
      [...path, "selected_slot_key"],
      "selected_slot_key must belong to its leaf",
    );
  }
}

function validateBindings(
  bindings: LayoutDocumentBinding[],
  slotOwners: Map<string, LayoutLeafRecord>,
  context: z.RefinementCtx,
): void {
  const boundSlotKeys = new Set<string>();
  const boundSessionIds = new Set<string>();
  for (let index = 0; index < bindings.length; index++) {
    const binding = bindings[index]!;
    if (boundSlotKeys.has(binding.slot_key)) {
      addGraphIssue(
        context,
        ["bindings", index, "slot_key"],
        "each slot_key must have exactly one binding",
      );
    } else {
      boundSlotKeys.add(binding.slot_key);
    }
    if (!slotOwners.has(binding.slot_key)) {
      addGraphIssue(
        context,
        ["bindings", index, "slot_key"],
        "binding slot_key must reference a slot in root",
      );
    }
    if (boundSessionIds.has(binding.session_id)) {
      addGraphIssue(
        context,
        ["bindings", index, "session_id"],
        "bound session_id values must be unique",
      );
    } else {
      boundSessionIds.add(binding.session_id);
    }
  }

  for (const [slotKey, owner] of slotOwners) {
    if (!boundSlotKeys.has(slotKey)) {
      const slotIndex = owner.leaf.slot_keys.indexOf(slotKey);
      addGraphIssue(
        context,
        [...owner.path, "slot_keys", slotIndex],
        "each slot_key must have exactly one binding",
      );
    }
  }
}

function addGraphIssue(
  context: z.RefinementCtx,
  path: LayoutDocumentPath,
  message: string,
): void {
  context.addIssue({ code: z.ZodIssueCode.custom, path, message });
}
