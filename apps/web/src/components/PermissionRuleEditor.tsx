// Row editor for a single permission rule. Renders inline toggle + delete
// controls. Exposed as a named export; called by PermissionsPane for each
// rule in the list. Mutations via trpc.permissions.update / .delete.
// Depends on: trpc.ts, @roost/shared/wire (PermissionRule, PermissionDecision).

import { createSignal, Show } from "solid-js";
import type { PermissionRule, PermissionDecision } from "@roost/shared/wire";
import { asPermissionRuleId } from "@roost/shared/wire";
import { coordClient } from "../connect.ts";
import { setRootStore } from "../store/root.ts";
import { addToast } from "../lib/toastStore.ts";
import { Switch, Button } from "./Settings/md/primitives.tsx";

interface PermissionRuleEditorProps {
  rule: PermissionRule;
  onError?: (msg: string) => void;
}

const DECISION_COLORS: Record<PermissionDecision, { bg: string; color: string }> = {
  allow: { bg: "color-mix(in srgb, var(--status-ok) 14%, transparent)", color: "var(--status-ok)" },
  "allow-and-remember": { bg: "color-mix(in srgb, var(--status-ok) 14%, transparent)", color: "var(--status-ok)" },
  deny: { bg: "color-mix(in srgb, var(--status-err) 14%, transparent)", color: "var(--status-err)" },
};

export function PermissionRuleEditor(props: PermissionRuleEditorProps) {
  const [busy, setBusy] = createSignal(false);
  const [rowErr, setRowErr] = createSignal<string | null>(null);

  function reportError(msg: string) {
    setRowErr(msg);
    props.onError?.(msg);
  }

  async function toggleEnabled(enabled: boolean) {
    if (busy()) return;
    setBusy(true);
    setRowErr(null);
    try {
      const updated = await coordClient.permissionsUpdate({
        id: asPermissionRuleId(props.rule.id),
        enabled,
      });
      // Optimistic projection in case SSE delta is lost/delayed.
      // Connect returns proto-shape; convert to wire shape for store.
      setRootStore("permission_rules", updated.rule!.id, {
        id: asPermissionRuleId(updated.rule!.id),
        tool_pattern: updated.rule!.toolPattern,
        folder_glob: updated.rule!.folderGlob,
        decision: updated.rule!.decision as never,
        enabled: updated.rule!.enabled,
        created_at_ms: Number(updated.rule!.createdAtMs),
      });
      addToast(enabled ? "Rule enabled" : "Rule disabled");
    } catch (e) {
      reportError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  async function deleteRule() {
    if (busy()) return;
    setBusy(true);
    setRowErr(null);
    try {
      await coordClient.permissionsDelete({ id: asPermissionRuleId(props.rule.id) });
      // Optimistic remove: don't wait for the SSE delta; if it's lost the row
      // would otherwise persist forever with the delete button stuck disabled.
      // Per-key delete; setRootStore(key, fn → newRecord) silently no-ops
      // on a Record subtree.
      setRootStore(
        "permission_rules",
        props.rule.id,
        undefined as unknown as import("@roost/shared/wire").PermissionRule,
      );
      addToast("Rule deleted");
      // Row unmounts on the store mutation above; no setBusy(false) needed.
    } catch (e) {
      reportError(e instanceof Error ? e.message : String(e));
      setBusy(false); // only reset on error; on success the row unmounts
    }
  }

  const decisionStyle = () => DECISION_COLORS[props.rule.decision] ?? DECISION_COLORS.allow;

  return (
    <div
      data-testid="permission-rule-editor"
      data-rule-id={props.rule.id}
      data-enabled={props.rule.enabled ? "true" : "false"}
      data-decision={props.rule.decision}
      style={{
        display: "grid",
        "grid-template-columns": "auto 1fr auto auto auto",
        gap: "8px",
        "align-items": "center",
        padding: "8px 10px",
        background: props.rule.enabled ? "var(--bg-elev-1)" : "var(--bg-elev-0)",
        border: "1px solid var(--border-subtle)",
        "border-radius": "var(--md-shape-sm)",
        opacity: props.rule.enabled ? 1 : 0.5,
        transition: "opacity 0.15s",
      }}
    >
      {/* enabled toggle */}
      <Switch
        testId="rule-toggle"
        label={props.rule.enabled ? "Disable rule" : "Enable rule"}
        checked={props.rule.enabled}
        onChange={(checked) => void toggleEnabled(checked)}
      />

      {/* pattern + folder */}
      <div style={{ overflow: "hidden" }}>
        <span
          style={{
            "font-family": "ui-monospace, monospace",
            "font-size": "12px",
            color: "var(--text-hi)",
            display: "block",
            overflow: "hidden",
            "text-overflow": "ellipsis",
            "white-space": "nowrap",
          }}
          title={props.rule.tool_pattern}
        >
          {props.rule.tool_pattern}
        </span>
        <Show when={props.rule.folder_glob && props.rule.folder_glob !== "*"}>
          <span
            style={{
              "font-family": "ui-monospace, monospace",
              "font-size": "10px",
              color: "var(--text-mid)",
              display: "block",
              overflow: "hidden",
              "text-overflow": "ellipsis",
              "white-space": "nowrap",
            }}
            title={props.rule.folder_glob}
          >
            @ {props.rule.folder_glob}
          </span>
        </Show>
      </div>

      {/* decision badge */}
      <span
        style={{
          display: "inline-flex",
          "align-items": "center",
          "font-size": "10px",
          "font-weight": "700",
          padding: "2px 8px",
          "border-radius": "999px",
          background: decisionStyle().bg,
          color: decisionStyle().color,
          "text-transform": "uppercase",
          "letter-spacing": "0.04em",
          "white-space": "nowrap",
        }}
      >
        {props.rule.decision === "deny" ? "✕" : "✓"} {props.rule.decision}
      </span>

      {/* created_at */}
      <span
        style={{
          "font-size": "10px",
          color: "var(--text-mid)",
          "white-space": "nowrap",
        }}
        title={new Date(props.rule.created_at_ms).toISOString()}
      >
        {relativeTime(props.rule.created_at_ms)}
      </span>

      {/* delete */}
      <Button
        variant="text"
        data-testid="rule-delete"
        disabled={busy()}
        onClick={() => void deleteRule()}
        style={{ color: "var(--status-err)" }}
      >
        delete
      </Button>

      <Show when={rowErr()}>
        <div
          data-testid="rule-row-error"
          style={{
            "grid-column": "1 / span 5",
            color: "var(--status-err)",
            "font-size": "11px",
            "padding-top": "4px",
          }}
        >
          {rowErr()}
        </div>
      </Show>
    </div>
  );
}

function relativeTime(tsMs: number): string {
  const diff = Date.now() - tsMs;
  if (diff < 60_000) return "just now";
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)}m ago`;
  if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)}h ago`;
  return `${Math.floor(diff / 86_400_000)}d ago`;
}
