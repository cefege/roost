// Permissions pane. Lists rootStore.permission_rules; form to create
// a new rule. Delegates per-row edit/delete to PermissionRuleEditor.
// M3: top-level Card with create form (label + input + segmented
// decision picker); rules rendered through PermissionRuleEditor inside
// a second Card. Empty state explains the inbox fallback.
// Callers: SettingsRoot.tsx (Permissions pane).
// Depends on: rootStore, coordClient, PermissionRuleEditor, md/primitives.

import { createMemo, createSignal, For, Show } from "solid-js";
import { rootStore } from "../../store/root.ts";
import { upsertPermissionRule } from "../../store/mutations.ts";
import { coordClient } from "../../connect.ts";
import type { PermissionDecision } from "@roost/shared/wire";
import { PermissionRuleEditor } from "../PermissionRuleEditor.tsx";
import { addToast } from "../../lib/toastStore.ts";
import { Card, Button, EmptyState, Icon, TextField } from "./md/primitives.tsx";

const DECISIONS: { id: PermissionDecision; label: string; icon: string }[] = [
  { id: "allow",               label: "Allow",     icon: "check_circle" },
  { id: "allow-and-remember",  label: "Remember",  icon: "verified" },
  { id: "deny",                label: "Deny",      icon: "block" },
];

export function PermissionsPane() {
  const rules = createMemo(() =>
    Object.values(rootStore.permission_rules).sort(
      (a, b) => b.created_at_ms - a.created_at_ms,
    ),
  );

  const [toolPattern, setToolPattern] = createSignal("");
  const [folderGlob, setFolderGlob] = createSignal("*");
  const [decision, setDecision] = createSignal<PermissionDecision>("allow");
  const [busy, setBusy] = createSignal(false);
  const [err, setErr] = createSignal<string | null>(null);

  async function createRule() {
    const tp = toolPattern().trim();
    if (!tp) { setErr("Tool pattern is required"); return; }
    setBusy(true);
    setErr(null);
    try {
      const created = await coordClient.permissionsCreate({
        toolPattern: tp,
        folderGlob: folderGlob().trim() || "*",
        decision: decision(),
        enabled: true,
      });
      const rule = created.rule!;
      const wireRule = {
        id: rule.id as never,
        tool_pattern: rule.toolPattern,
        folder_glob: rule.folderGlob,
        decision: rule.decision as never,
        enabled: rule.enabled,
        created_at_ms: Number(rule.createdAtMs),
      };
      upsertPermissionRule(wireRule);
      setToolPattern("");
      setFolderGlob("*");
      addToast("Rule created");
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div data-testid="permissions-pane" style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-5)" }}>
      <Card
        title="Permission rules"
        supporting="Permission rules are evaluated in order: first match wins. No match → the request lands in the inbox for manual review."
      >
        <form
          data-testid="permission-rules-create"
          onSubmit={(e) => { e.preventDefault(); void createRule(); }}
          style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-4)" }}
        >
          <div style={{ display: "grid", "grid-template-columns": "1fr 1fr", gap: "var(--md-space-3)" }}>
            <div class="md-form-row">
              <TextField
                label="Tool pattern"
                testId="rule-tool-pattern"
                value={toolPattern()}
                onInput={setToolPattern}
                placeholder="Bash(pnpm test*"
                style={{ "font-family": "ui-monospace, SFMono-Regular, Menlo, monospace" }}
              />
            </div>
            <div class="md-form-row">
              <TextField
                label="Folder glob"
                testId="rule-folder-glob"
                value={folderGlob()}
                onInput={setFolderGlob}
                placeholder="* or /Users/me/code/**"
                style={{ "font-family": "ui-monospace, SFMono-Regular, Menlo, monospace" }}
              />
            </div>
          </div>

          <div class="md-form-row">
            <span class="md-form-row__label">Decision</span>
            <div
              role="radiogroup"
              data-testid="rule-decision"
              style={{
                display: "inline-flex",
                background: "var(--md-sys-color-surface-container-high)",
                "border-radius": "var(--md-shape-full)",
                padding: "4px",
                gap: "var(--md-space-1)",
                "align-self": "flex-start",
              }}
            >
              <For each={DECISIONS}>
                {(opt) => (
                  <button
                    type="button"
                    role="radio"
                    aria-checked={decision() === opt.id}
                    data-testid={`rule-decision-${opt.id}`}
                    onClick={() => setDecision(opt.id)}
                    class="md-label-l"
                    style={{
                      padding: "8px 16px",
                      "border-radius": "var(--md-shape-full)",
                      border: "none",
                      cursor: "pointer",
                      display: "inline-flex",
                      "align-items": "center",
                      gap: "var(--md-space-2)",
                      background: decision() === opt.id ? "var(--md-sys-color-primary)" : "transparent",
                      color: decision() === opt.id ? "var(--md-sys-color-on-primary)" : "var(--md-sys-color-on-surface)",
                      "font-family": "inherit",
                    }}
                  >
                    <Icon name={opt.icon} size="sm" filled={decision() === opt.id} />
                    {opt.label}
                  </button>
                )}
              </For>
            </div>
          </div>

          <Show when={err()}>
            <div
              data-testid="rule-create-error"
              class="md-body-s"
              style={{ color: "var(--md-sys-color-error)", display: "flex", "align-items": "center", gap: "var(--md-space-2)" }}
            >
              <Icon name="error" size="sm" />
              {err()}
            </div>
          </Show>

          <div style={{ display: "flex", "justify-content": "flex-end" }}>
            <Button
              type="submit"
              variant="filled"
              icon={busy() ? undefined : "add"}
              data-testid="rule-create-submit"
              disabled={busy() || !toolPattern().trim()}
            >
              {busy() ? "Adding…" : "Add rule"}
            </Button>
          </div>
        </form>
      </Card>

      <Card title="Active rules">
        <Show
          when={rules().length > 0}
          fallback={
            <EmptyState
              icon="rule"
              title="No rules yet"
              supporting="Every permission request will land in the inbox until you add a rule. Use the form above to auto-allow or auto-deny common patterns."
            />
          }
        >
          <div data-testid="permission-rules-list" style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-2)" }}>
            <For each={rules()}>
              {(rule) => <PermissionRuleEditor rule={rule} />}
            </For>
          </div>
        </Show>
      </Card>
    </div>
  );
}
