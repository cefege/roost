// McpPane — MCP relay registry pane.
// Lists rootStore.mcp_relays; inline add via McpRelayEditor; delete with
// confirm gate. Mutations go through coordClient.mcpCreate/mcpDelete.
// M3: top-level Card with header + supporting + trailing add CTA; relays
// rendered as list rows (icon by kind, command/url as supporting text);
// remove flow inline trailing.
// Callers: SettingsRoot.tsx. Depends on: rootStore.mcp_relays, coordClient,
// McpRelayEditor, toastStore, md/primitives.

import { createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import type { McpRelay, McpRelayId } from "@roost/shared/wire";
import { rootStore, setRootStore } from "../../store/root.ts";
import { coordClient } from "../../connect.ts";
import { McpRelayEditor } from "../McpRelayEditor.tsx";
import { addToast } from "../../lib/toastStore.ts";
import { Card, Button, EmptyState, List, ListRow, Icon } from "./md/primitives.tsx";

export function McpPane() {
  const relays = createMemo(() =>
    Object.values(rootStore.mcp_relays).sort((a, b) => a.created_at_ms - b.created_at_ms),
  );
  const [showEditor, setShowEditor] = createSignal(false);
  const [confirmingId, setConfirmingId] = createSignal<string | null>(null);
  const [deleteError, setDeleteError] = createSignal("");
  const [loadErr, setLoadErr] = createSignal("");
  let confirmTimer: ReturnType<typeof setTimeout> | null = null;

  async function reload() {
    setLoadErr("");
    try {
      const res = await coordClient.mcpList({});
      const rec: Record<string, McpRelay> = {};
      for (const r of res.relays) rec[r.id] = {
        id: r.id as never, label: r.label, kind: r.kind as never,
        config: JSON.parse(r.configJson),
        created_at_ms: Number(r.createdAtMs),
      };
      setRootStore("mcp_relays", rec);
    } catch (e) {
      setLoadErr(e instanceof Error ? e.message : String(e));
    }
  }
  onMount(() => { void reload(); });
  onCleanup(() => { if (confirmTimer !== null) clearTimeout(confirmTimer); });

  function beginConfirm(id: string) {
    setConfirmingId(id);
    if (confirmTimer !== null) clearTimeout(confirmTimer);
    confirmTimer = setTimeout(() => {
      confirmTimer = null;
      setConfirmingId(null);
    }, 3000);
  }
  function cancelConfirm() {
    if (confirmTimer !== null) { clearTimeout(confirmTimer); confirmTimer = null; }
    setConfirmingId(null);
  }

  async function handleDelete(id: McpRelayId) {
    cancelConfirm();
    setDeleteError("");
    try {
      await coordClient.mcpDelete({ id });
      setRootStore("mcp_relays", id, undefined as unknown as import("@roost/shared/wire").McpRelay);
      addToast("Relay removed");
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setDeleteError(msg);
      addToast(`Delete failed: ${msg}`);
    }
  }

  async function handleCreate(label: string, kind: "stdio" | "sse", config: Record<string, unknown>) {
    try {
      const res = await coordClient.mcpCreate({ label, kind, configJson: JSON.stringify(config) });
      const r = res.relay!;
      const relay: McpRelay = {
        id: r.id as never, label: r.label, kind: r.kind as never,
        config: JSON.parse(r.configJson),
        created_at_ms: Number(r.createdAtMs),
      };
      setRootStore("mcp_relays", relay.id, relay);
      setShowEditor(false);
      addToast("Relay added");
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      addToast(`Add failed: ${msg}`);
      throw e;
    }
  }

  return (
    <div data-testid="settings-mcp-pane" style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-5)" }}>
      <Card
        title="MCP relays"
        supporting="Bridge agents to external tools and data sources. Each relay points at an MCP server (binary path or URL); agents on any worker can call it."
        trailing={
          <Show when={!showEditor()}>
            <Button
              variant="filled"
              icon="add"
              data-testid="mcp-add-btn"
              onClick={() => setShowEditor(true)}
            >
              Add relay
            </Button>
          </Show>
        }
      >
        <Show when={showEditor()}>
          <McpRelayEditor
            onSave={handleCreate}
            onCancel={() => setShowEditor(false)}
          />
        </Show>
        <Show when={deleteError()}>
          <div class="md-body-s" style={{ color: "var(--md-sys-color-error)" }}>{deleteError()}</div>
        </Show>
        <Show when={loadErr()}>
          <div data-testid="mcp-load-err" style={{ display: "flex", "align-items": "center", gap: "var(--md-space-2)" }}>
            <Icon name="error" style={{ color: "var(--md-sys-color-error)" }} />
            <span class="md-body-m" style={{ color: "var(--md-sys-color-error)" }}>Failed to load relays: {loadErr()}</span>
            <Button variant="text" data-testid="mcp-reload-btn" onClick={() => void reload()}>
              Retry
            </Button>
          </div>
        </Show>
      </Card>

      <Show
        when={relays().length > 0}
        fallback={
          <Card>
            <EmptyState
              icon="extension"
              title="No MCP relays configured"
              supporting="Add a relay to make MCP-served tools available to every agent on every machine."
              action={
                <Show when={!showEditor()}>
                  <Button variant="filled" icon="add" onClick={() => setShowEditor(true)}>
                    Add relay
                  </Button>
                </Show>
              }
            />
          </Card>
        }
      >
        <Card>
          <List contained>
            <For each={relays()}>
              {(relay) => (
                <ListRow
                  testId={`mcp-relay-row-${relay.id}`}
                  leading={relay.kind === "sse" ? "cloud" : "terminal"}
                  headline={
                    <span style={{ display: "flex", "align-items": "center", gap: "var(--md-space-2)" }}>
                      {relay.label}
                      <span class="md-label-s" style={{
                        padding: "2px 8px", "border-radius": "var(--md-shape-full)",
                        background: "var(--md-sys-color-tertiary-container)",
                        color: "var(--md-sys-color-on-tertiary-container)",
                        "text-transform": "uppercase",
                      }}>
                        {relay.kind}
                      </span>
                    </span>
                  }
                  support={
                    <Show when={relay.config["command"] ?? relay.config["url"]}>
                      <span style={{ "font-family": "ui-monospace, SFMono-Regular, Menlo, monospace" }}>
                        {String(relay.config["command"] ?? relay.config["url"] ?? "")}
                      </span>
                    </Show>
                  }
                  trailing={
                    <Show
                      when={confirmingId() === relay.id}
                      fallback={
                        <Button
                          variant="text"
                          icon="delete_outline"
                          data-testid={`mcp-delete-${relay.id}`}
                          onClick={() => beginConfirm(relay.id)}
                          style={{ color: "var(--md-sys-color-error)" }}
                        >
                          Remove
                        </Button>
                      }
                    >
                      <Button
                        variant="filled"
                        data-testid={`mcp-confirm-delete-${relay.id}`}
                        onClick={() => void handleDelete(relay.id as McpRelayId)}
                        style={{ background: "var(--md-sys-color-error)", color: "var(--md-on-primary)" }}
                      >
                        Confirm
                      </Button>
                      <Button
                        variant="text"
                        data-testid={`mcp-cancel-delete-${relay.id}`}
                        onClick={cancelConfirm}
                      >
                        Cancel
                      </Button>
                    </Show>
                  }
                />
              )}
            </For>
          </List>
        </Card>
      </Show>
    </div>
  );
}
