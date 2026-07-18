// Modal dialog for minting a new webhook token. Form: label input + submit.
// On success, displays the plaintext ONCE with a copy-to-clipboard button.
// Caller passes onClose() and onMinted(token) callbacks.
// Depends on: trpc.ts, @roost/shared/wire (WebhookTokenMint).

import { createSignal, Show } from "solid-js";
import type { WebhookTokenMint } from "@roost/shared/wire";
import { coordClient } from "../connect.ts";
import { TextField, Button, IconButton } from "./Settings/md/primitives.tsx";
import { animateOverlayPanel } from "../lib/overlayMotion.ts";

interface WebhookTokenMintDialogProps {
  onClose: () => void;
  onMinted: (token: WebhookTokenMint) => void;
}

export function WebhookTokenMintDialog(props: WebhookTokenMintDialogProps) {
  const [label, setLabel] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [err, setErr] = createSignal<string | null>(null);
  const [minted, setMinted] = createSignal<WebhookTokenMint | null>(null);
  const [copied, setCopied] = createSignal(false);

  async function submit() {
    const l = label().trim();
    if (!l) { setErr("label required"); return; }
    setBusy(true);
    setErr(null);
    try {
      const res = await coordClient.webhookTokensMint({ label: l, scopes: ["tasks.enqueue"] });
      const t = res.token!;
      const minted: WebhookTokenMint = {
        id: t.id as never, label: t.label, plaintext: t.plaintext,
        scopes: t.scopes as never,
        created_at_ms: Number(t.createdAtMs),
      };
      setMinted(minted);
      props.onMinted(minted);
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  async function copyPlaintext() {
    const m = minted();
    if (!m) return;
    try {
      await navigator.clipboard.writeText(m.plaintext);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch {
      setCopied(false);
    }
  }

  return (
    // Backdrop
    <div
      data-testid="webhook-token-mint-dialog-backdrop"
      onClick={(e) => { if (e.target === e.currentTarget) props.onClose(); }}
      style={{
        position: "fixed",
        inset: 0,
        background: "color-mix(in srgb, var(--md-scrim) 60%, transparent)",
        display: "flex",
        "align-items": "center",
        "justify-content": "center",
        "z-index": 1000,
      }}
    >
      <div
        ref={animateOverlayPanel}
        data-testid="webhook-token-mint-dialog"
        style={{
          background: "var(--bg-elev-1)",
          border: "1px solid var(--border-subtle)",
          "border-radius": "var(--md-shape-md)",
          padding: "20px",
          width: "360px",
          "max-width": "90vw",
          display: "flex",
          "flex-direction": "column",
          gap: "14px",
        }}
      >
        {/* Header */}
        <div style={{ display: "flex", "justify-content": "space-between", "align-items": "center" }}>
          <span style={{ "font-size": "var(--md-body-m-size)", "font-weight": "600", color: "var(--text-hi)" }}>
            Mint Webhook Token
          </span>
          <IconButton
            icon="close"
            label="Close"
            data-testid="webhook-token-mint-dialog-close"
            onClick={props.onClose}
          />
        </div>

        {/* Form (hidden after mint) */}
        <Show when={!minted()}>
          <form
            data-testid="webhook-token-mint-form"
            onSubmit={(e) => { e.preventDefault(); void submit(); }}
            style={{ display: "flex", "flex-direction": "column", gap: "10px" }}
          >
            <TextField
              testId="webhook-token-label-input"
              label="Label *"
              value={label()}
              onInput={(v) => setLabel(v)}
              placeholder="GitHub Actions"
              style={{ width: "100%" }}
            />
            <div style={{ "font-size": "11px", color: "var(--text-mid)" }}>
              Scope: <code>tasks.enqueue</code> — allows external triggers to POST tasks.
            </div>
            <Button
              type="submit"
              variant="filled"
              data-testid="webhook-token-mint-submit"
              disabled={busy() || !label().trim()}
              style={{ "align-self": "flex-start" }}
            >
              {busy() ? "Generating…" : "Generate token"}
            </Button>
            <Show when={err()}>
              <div
                data-testid="webhook-token-mint-error"
                style={{ color: "var(--status-err)", "font-size": "11px" }}
              >
                {err()}
              </div>
            </Show>
          </form>
        </Show>

        {/* One-time plaintext reveal */}
        <Show when={minted()}>
          {(m) => (
            <div
              data-testid="webhook-token-mint-result"
              style={{ display: "flex", "flex-direction": "column", gap: "10px" }}
            >
              <div
                style={{
                  padding: "10px 12px",
                  background: "color-mix(in srgb, var(--status-ok) 12%, transparent)",
                  border: "1px solid color-mix(in srgb, var(--status-ok) 40%, transparent)",
                  "border-radius": "var(--md-shape-sm)",
                  display: "flex",
                  "flex-direction": "column",
                  gap: "6px",
                }}
              >
                <div style={{ "font-size": "var(--md-body-s-size)", "font-weight": "600", color: "var(--text-hi)" }}>
                  ⚠ Copy now — this value will not be shown again
                </div>
                <div style={{ "font-size": "11px", color: "var(--text-mid)" }}>
                  If lost, revoke this token and mint a fresh one.
                </div>
                <div
                  style={{
                    display: "flex",
                    gap: "6px",
                    "align-items": "center",
                    padding: "6px 8px",
                    background: "var(--bg-app)",
                    border: "1px solid var(--border-subtle)",
                    "border-radius": "var(--md-shape-sm)",
                  }}
                >
                  <code
                    data-testid="webhook-token-plaintext"
                    style={{
                      flex: 1,
                      "font-size": "11px",
                      color: "var(--text-hi)",
                      "overflow-wrap": "anywhere",
                      "font-family": "ui-monospace, monospace",
                    }}
                  >
                    {m().plaintext}
                  </code>
                  <Button
                    variant="tonal"
                    data-testid="webhook-token-copy"
                    onClick={() => void copyPlaintext()}
                    style={{ "flex-shrink": 0 }}
                  >
                    {copied() ? "✓ Copied" : "Copy"}
                  </Button>
                </div>
              </div>

              <div style={{ "font-size": "11px", color: "var(--text-mid)" }}>
                Label: <strong style={{ color: "var(--text-hi)" }}>{m().label}</strong>
              </div>

              <Button
                variant="text"
                data-testid="webhook-token-mint-done"
                onClick={props.onClose}
                style={{ "align-self": "flex-start" }}
              >
                Done
              </Button>
            </div>
          )}
        </Show>
      </div>
    </div>
  );
}
