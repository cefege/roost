// Webhooks pane. Lists minted tokens (label + last4). "Mint new" button
// opens WebhookTokenMintDialog. On mint, the plaintext is shown ONCE
// inside the dialog then discarded. Revoke via delete on each row.
// M3: top-level Card with usage instructions + mint CTA; tokens
// rendered as ListRows with key icon and "last used" relative time.
// Callers: SettingsRoot.tsx (Webhooks pane).
// Depends on: coordClient, registerWebhookDelta, WebhookTokenMintDialog,
// toastStore, md/primitives.

import { createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { coordClient } from "../../connect.ts";
import { registerWebhookDelta } from "../../store/sync.ts";
import type { WebhookToken, WebhookTokenMint, WebhookTokenDelta } from "@roost/shared/wire";
import { asWebhookTokenId } from "@roost/shared/wire";
import { WebhookTokenMintDialog } from "../WebhookTokenMintDialog.tsx";
import { addToast } from "../../lib/toastStore.ts";
import { Card, Button, EmptyState, List, ListRow, Icon } from "./md/primitives.tsx";

function relativeTime(tsMs: number): string {
  const diff = Date.now() - tsMs;
  if (diff < 60_000) return "just now";
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)}m ago`;
  if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)}h ago`;
  return `${Math.floor(diff / 86_400_000)}d ago`;
}

export function WebhooksPane() {
  const [tokens, setTokens] = createSignal<WebhookToken[]>([]);
  const [loadErr, setLoadErr] = createSignal<string | null>(null);
  const [showMintDialog, setShowMintDialog] = createSignal(false);
  const [deleteErr, setDeleteErr] = createSignal<string | null>(null);
  const [deletingId, setDeletingId] = createSignal<string | null>(null);

  async function loadTokens() {
    try {
      const res = await coordClient.webhookTokensList({});
      const list: WebhookToken[] = res.tokens.map((t) => ({
        id: t.id as never,
        label: t.label, last4: t.last4,
        scopes: t.scopes as never,
        created_at_ms: Number(t.createdAtMs),
        last_used_at_ms: t.lastUsedAtMs !== undefined ? Number(t.lastUsedAtMs) : null,
      }));
      setTokens(list.sort((a, b) => b.created_at_ms - a.created_at_ms));
      setLoadErr(null);
    } catch (e) {
      setLoadErr(e instanceof Error ? e.message : String(e));
    }
  }

  let sseUnsub: (() => void) | null = null;
  let disposed = false;
  onMount(() => {
    void loadTokens();
    const unsub = registerWebhookDelta((raw) => {
      const delta = raw as WebhookTokenDelta;
      if (delta.kind === "created") {
        const t = delta.token;
        setTokens((prev) => {
          if (prev.some((x) => x.id === t.id)) return prev;
          return [t, ...prev].sort((a, b) => b.created_at_ms - a.created_at_ms);
        });
      } else if (delta.kind === "deleted") {
        setTokens((prev) => prev.filter((x) => x.id !== delta.id));
      }
    });
    if (disposed) { unsub(); return; }
    sseUnsub = unsub;
  });
  onCleanup(() => {
    disposed = true;
    try { sseUnsub?.(); } catch { /* ignore */ }
    sseUnsub = null;
  });

  function handleMinted(_token: WebhookTokenMint) {
    addToast("Token minted — copy it now");
  }

  async function revokeToken(id: string) {
    setDeletingId(id);
    setDeleteErr(null);
    try {
      await coordClient.webhookTokensDelete({ id: asWebhookTokenId(id) });
      setTokens((prev) => prev.filter((x) => x.id !== id));
      addToast("Token revoked");
    } catch (e) {
      setDeleteErr(e instanceof Error ? e.message : String(e));
    } finally {
      setDeletingId(null);
    }
  }

  return (
    <div data-testid="webhooks-pane" style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-5)" }}>
      <Card
        title="Webhook tokens"
        supporting={"External triggers (GitHub Actions, iOS Shortcuts, cron) can enqueue tasks without signing a JWT. Send the token in the X-Roost-Token header on POST /api/tasks."}
        trailing={
          <Button
            variant="filled"
            icon="add"
            data-testid="webhook-token-mint-open"
            onClick={() => setShowMintDialog(true)}
          >
            Mint new
          </Button>
        }
      >
        <Show when={loadErr()}>
          <div style={{ display: "flex", "align-items": "center", gap: "var(--md-space-2)" }}>
            <Icon name="error" style={{ color: "var(--md-sys-color-error)" }} />
            <span class="md-body-m" style={{ color: "var(--md-sys-color-error)" }}>Failed to load tokens: {loadErr()}</span>
          </div>
        </Show>
        <Show when={deleteErr()}>
          <div data-testid="webhook-token-delete-error" class="md-body-s" style={{ color: "var(--md-sys-color-error)" }}>
            {deleteErr()}
          </div>
        </Show>
      </Card>

      <Show
        when={tokens().length > 0}
        fallback={
          <Card>
            <EmptyState
              data-testid="webhook-tokens-empty"
              icon="key"
              title="No tokens yet"
              supporting="Mint a token to wire up an external trigger. The plaintext is shown once — copy it immediately."
              action={
                <Button variant="filled" icon="add" onClick={() => setShowMintDialog(true)}>
                  Mint new
                </Button>
              }
            />
          </Card>
        }
      >
        <Card>
          <List contained>
            <For each={tokens()}>
              {(token) => {
                const isDeleting = () => deletingId() === token.id;
                return (
                  <ListRow
                    testId="webhook-token-row"
                    leading="key"
                    headline={token.label}
                    support={
                      <span style={{ display: "inline-flex", "align-items": "center", gap: "var(--md-space-3)" }}>
                        <span style={{ "font-family": "ui-monospace, SFMono-Regular, Menlo, monospace" }}>
                          roost_wh_…{token.last4}
                        </span>
                        <span title={token.last_used_at_ms ? new Date(token.last_used_at_ms).toISOString() : "never used"}>
                          {token.last_used_at_ms ? `used ${relativeTime(token.last_used_at_ms)}` : "never used"}
                        </span>
                      </span>
                    }
                    trailing={
                      <Button
                        variant="text"
                        icon="key_off"
                        data-testid="webhook-token-revoke"
                        disabled={isDeleting()}
                        onClick={() => void revokeToken(token.id)}
                        style={{ color: "var(--md-sys-color-error)" }}
                      >
                        Revoke
                      </Button>
                    }
                  />
                );
              }}
            </For>
          </List>
        </Card>
      </Show>

      <Show when={showMintDialog()}>
        <WebhookTokenMintDialog
          onClose={() => { setShowMintDialog(false); void loadTokens(); }}
          onMinted={handleMinted}
        />
      </Show>
    </div>
  );
}
