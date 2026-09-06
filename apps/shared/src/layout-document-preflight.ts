// Iteratively preflights untrusted portable layout documents before Zod recursion.
// The public layout-document module re-exports these deterministic resource bounds.
// This pass inspects bounded work and leaves structural acceptance to the sole parser.

import type { RefinementCtx } from "zod";
import { hasAtMostUtf8Bytes } from "./ui-state.ts";

export const LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES = 256;
export const LAYOUT_DOCUMENT_MAX_SESSION_ID_UTF8_BYTES = 256;
// The root node is depth 1; every split child advances one level.
export const LAYOUT_DOCUMENT_MAX_DEPTH = 32;
export const LAYOUT_DOCUMENT_MAX_NODES = 255;
export const LAYOUT_DOCUMENT_MAX_SLOTS = 512;
export const LAYOUT_DOCUMENT_MAX_BINDINGS = 512;

type LayoutDocumentPath = (string | number)[];
type LayoutResourceViolation = { path: LayoutDocumentPath; message: string };
type PendingLayoutNode = { node: unknown; path: LayoutDocumentPath; depth: number };

export function preflightLayoutDocumentResources(
  input: unknown,
  context: RefinementCtx,
): void {
  const violation = findLayoutResourceViolation(input);
  if (violation) {
    context.addIssue({
      code: "custom",
      fatal: true,
      path: violation.path,
      message: violation.message,
    });
  }
}

function findLayoutResourceViolation(input: unknown): LayoutResourceViolation | null {
  const document = jsonObject(input);
  if (!document) return null;

  const focusViolation = utf8BoundViolation(
    document.focused_leaf_key,
    LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES,
    ["focused_leaf_key"],
    "layout key",
  );
  if (focusViolation) return focusViolation;

  const bindings = document.bindings;
  if (Array.isArray(bindings)) {
    if (bindings.length > LAYOUT_DOCUMENT_MAX_BINDINGS) {
      return {
        path: ["bindings"],
        message: `layout document exceeds ${LAYOUT_DOCUMENT_MAX_BINDINGS} bindings`,
      };
    }
    for (let index = 0; index < bindings.length; index++) {
      const candidate = jsonObject(bindings[index]);
      if (!candidate) continue;
      const slotViolation = utf8BoundViolation(
        candidate.slot_key,
        LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES,
        ["bindings", index, "slot_key"],
        "layout key",
      );
      if (slotViolation) return slotViolation;
      const sessionViolation = utf8BoundViolation(
        candidate.session_id,
        LAYOUT_DOCUMENT_MAX_SESSION_ID_UTF8_BYTES,
        ["bindings", index, "session_id"],
        "layout session_id",
      );
      if (sessionViolation) return sessionViolation;
    }
  }

  const pending: PendingLayoutNode[] = [
    { node: document.root, path: ["root"], depth: 1 },
  ];
  let nodes = 0;
  let slots = 0;
  while (pending.length > 0) {
    const current = pending.pop()!;
    if (current.depth > LAYOUT_DOCUMENT_MAX_DEPTH) {
      return {
        path: current.path,
        message: `layout document exceeds depth ${LAYOUT_DOCUMENT_MAX_DEPTH}`,
      };
    }
    const node = jsonObject(current.node);
    if (!node || (node.kind !== "leaf" && node.kind !== "split")) continue;
    nodes++;
    if (nodes > LAYOUT_DOCUMENT_MAX_NODES) {
      return {
        path: current.path,
        message: `layout document exceeds ${LAYOUT_DOCUMENT_MAX_NODES} nodes`,
      };
    }
    if (node.kind === "split") {
      pending.push(
        { node: node.second, path: [...current.path, "second"], depth: current.depth + 1 },
        { node: node.first, path: [...current.path, "first"], depth: current.depth + 1 },
      );
      continue;
    }

    const leafViolation = utf8BoundViolation(
      node.leaf_key,
      LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES,
      [...current.path, "leaf_key"],
      "layout key",
    );
    if (leafViolation) return leafViolation;
    const selectedViolation = utf8BoundViolation(
      node.selected_slot_key,
      LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES,
      [...current.path, "selected_slot_key"],
      "layout key",
    );
    if (selectedViolation) return selectedViolation;
    if (!Array.isArray(node.slot_keys)) continue;
    if (node.slot_keys.length > LAYOUT_DOCUMENT_MAX_SLOTS - slots) {
      return {
        path: [...current.path, "slot_keys"],
        message: `layout document exceeds ${LAYOUT_DOCUMENT_MAX_SLOTS} slots`,
      };
    }
    slots += node.slot_keys.length;
    for (let index = 0; index < node.slot_keys.length; index++) {
      const slotViolation = utf8BoundViolation(
        node.slot_keys[index],
        LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES,
        [...current.path, "slot_keys", index],
        "layout key",
      );
      if (slotViolation) return slotViolation;
    }
  }
  return null;
}

function utf8BoundViolation(
  value: unknown,
  maxBytes: number,
  path: LayoutDocumentPath,
  field: string,
): LayoutResourceViolation | null {
  return typeof value === "string" && !hasAtMostUtf8Bytes(value, maxBytes)
    ? { path, message: `${field} must not exceed ${maxBytes} UTF-8 bytes` }
    : null;
}

function jsonObject(value: unknown): Record<string, unknown> | null {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null;
}
