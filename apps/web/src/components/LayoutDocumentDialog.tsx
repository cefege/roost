// Presents one local layout document before any pane-store mutation.
// TerminalDeck owns file reading, dashboard/folder fencing, and application;
// this component renders validation state and requires an explicit Apply click.
// Material primitives supply the dialog, actions, and preview surfaces.

import { For, Show, createMemo } from "solid-js";
import type {
  LayoutDocumentNode,
  LayoutDocumentV1,
} from "@roost/shared/layout-document";
import { Button, Dialog, Surface } from "./Settings/md/primitives.tsx";
import "./Settings/md/tokens.css";

interface LayoutDocumentDialogProps {
  open: boolean;
  fileName: string;
  document: LayoutDocumentV1 | null;
  droppedSessionCount: number;
  error: string | null;
  reading: boolean;
  onClose: () => void;
  onApply: () => void;
}

export function LayoutDocumentDialog(props: LayoutDocumentDialogProps) {
  const previewRows = createMemo(() =>
    props.document ? describeDocument(props.document) : []);

  return (
    <Dialog
      open={props.open}
      onClose={props.onClose}
      headline="Import pane layout"
      actions={
        <>
          <Button variant="text" onClick={props.onClose}>Cancel</Button>
          <Button
            variant="filled"
            data-testid="layout-import-apply"
            disabled={props.reading || !props.document || !!props.error}
            onClick={props.onApply}
          >
            Apply layout
          </Button>
        </>
      }
    >
      <div
        data-testid="layout-import-preview"
        style={{
          display: "flex",
          "flex-direction": "column",
          gap: "var(--md-space-4)",
        }}
      >
        <p class="md-body-m" style={{ margin: "0", color: "var(--md-sys-color-on-surface-variant)" }}>
          {props.fileName || "Selected layout document"}
        </p>
        <Show when={props.reading}>
          <Surface level={2} radius="md" pad={4} aria-live="polite">
            <span class="md-body-m">Reading and validating locally…</span>
          </Surface>
        </Show>
        <Show when={props.error}>
          {(message) => (
            <Surface
              level={2}
              radius="md"
              pad={4}
              border
              role="alert"
              data-testid="layout-import-error"
            >
              <span class="md-body-m" style={{ color: "var(--md-sys-color-error)" }}>
                {message()}
              </span>
            </Surface>
          )}
        </Show>
        <Show when={props.droppedSessionCount > 0}>
          <Surface
            level={2}
            radius="md"
            pad={4}
            border
            role="status"
            data-testid="layout-import-dropped"
          >
            <span class="md-body-m" style={{ color: "var(--md-sys-color-tertiary)" }}>
              {droppedNotice(props.droppedSessionCount)}
            </span>
          </Surface>
        </Show>
        <Show when={!props.reading && !props.error && props.document}>
          <Surface level={2} radius="md" pad={4} border>
            <div
              style={{
                display: "flex",
                "flex-direction": "column",
                gap: "var(--md-space-2)",
              }}
            >
              <For each={previewRows()}>
                {(row) => <code class="md-body-s">{row}</code>}
              </For>
            </div>
          </Surface>
          <p class="md-body-s" style={{ margin: "0", color: "var(--md-sys-color-on-surface-variant)" }}>
            Existing terminals stay live. Sessions opened since export are appended to the focused pane.
          </p>
        </Show>
      </div>
    </Dialog>
  );
}

function droppedNotice(droppedSessionCount: number): string {
  return droppedSessionCount === 1
    ? "1 saved session is no longer live and was dropped from this layout."
    : `${droppedSessionCount} saved sessions are no longer live and were dropped from this layout.`;
}

function describeDocument(document: LayoutDocumentV1): string[] {
  const sessionBySlot = new Map(
    document.bindings.map((binding) => [binding.slot_key, binding.session_id]),
  );
  const rows: string[] = [];

  function visit(node: LayoutDocumentNode, depth: number): void {
    const prefix = "· ".repeat(depth);
    if (node.kind === "split") {
      rows.push(`${prefix}Split ${node.direction} · ${Math.round(node.ratio * 100)}% first`);
      visit(node.first, depth + 1);
      visit(node.second, depth + 1);
      return;
    }
    const focus = node.leaf_key === document.focused_leaf_key ? " · focused" : "";
    rows.push(`${prefix}${node.leaf_key} · ${node.slot_keys.length} session${node.slot_keys.length === 1 ? "" : "s"}${focus}`);
    for (const slotKey of node.slot_keys) {
      const selected = slotKey === node.selected_slot_key ? " · selected" : "";
      rows.push(`${prefix}· ${slotKey} → ${sessionBySlot.get(slotKey)}${selected}`);
    }
  }

  visit(document.root, 0);
  return rows;
}
