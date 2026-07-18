// MachineDeployDialog — mint a bootstrap token and surface the deploy command.
// Shown when user clicks "Add Machine" in MachinesPane.
// Depends on: trpc (auth.mintBootstrap), Solid signals.
// Callers: MachinesPane.tsx.

import { createSignal, Show } from "solid-js";
import { coordClient } from "../connect.ts";
import { TextField, Button, IconButton } from "./Settings/md/primitives.tsx";
import { animateOverlayPanel } from "../lib/overlayMotion.ts";

interface MachineDeployDialogProps {
  onClose: () => void;
}

const OVERLAY_STYLE = {
  position: "fixed" as const,
  inset: "0",
  background: "color-mix(in srgb, var(--md-scrim) 65%, transparent)",
  display: "flex",
  "align-items": "center",
  "justify-content": "center",
  "z-index": "200",
};

const DIALOG_STYLE = {
  background: "var(--bg-elev-1)",
  border: "1px solid var(--border-strong)",
  "border-radius": "var(--md-shape-md)",
  padding: "24px",
  width: "520px",
  "max-width": "calc(100vw - 32px)",
  "box-shadow": "var(--md-elev-5)",
};

// A new Mac (like a phone) can only reach an HTTPS tailnet origin — if
// Settings is open over loopback/http, the join command would point somewhere
// the new Mac can't reach. Mirror PairDevicePane's guard.
function originIsReachable(): boolean {
  if (typeof location === "undefined") return false;
  if (location.protocol !== "https:") return false;
  return !/^(localhost|127\.0\.0\.1|\[::1\])$/.test(location.hostname);
}

export function MachineDeployDialog(props: MachineDeployDialogProps) {
  const [label, setLabel] = createSignal("");
  const [loading, setLoading] = createSignal(false);
  const [error, setError] = createSignal("");
  const [deployCmd, setDeployCmd] = createSignal<string | null>(null);
  const [copied, setCopied] = createSignal(false);
  let copyTimer: ReturnType<typeof setTimeout> | null = null;
  const reachable = originIsReachable();

  async function mintAndShowCmd() {
    setLoading(true);
    setError("");
    setDeployCmd(null);
    try {
      const lbl = label().trim();
      const result = await coordClient.authMintBootstrap({ kind: "worker", label: lbl });
      // Compose the pasteable one-liner the user runs on the new Mac. It
      // fetches join.sh and self-installs + registers the worker over the
      // tailnet. location.origin on a tailnet-loaded SPA is exactly
      // https://<coord-fqdn>:4102 = the worker's ROOST_COORDINATOR_URL.
      const labelEnv = lbl ? ` ROOST_WORKER_LABEL=${JSON.stringify(lbl)}` : "";
      const cmd = `curl -fsSL https://raw.githubusercontent.com/cefege/roost/main/join.sh | `
        + `ROOST_COORDINATOR_URL=${JSON.stringify(location.origin)} `
        + `ROOST_BOOTSTRAP_TOKEN=${JSON.stringify(result.token)}${labelEnv} bash`;
      setDeployCmd(cmd);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }

  async function copyCmd() {
    const cmd = deployCmd();
    if (!cmd) return;
    try {
      await navigator.clipboard.writeText(cmd);
      setCopied(true);
      if (copyTimer !== null) clearTimeout(copyTimer);
      copyTimer = setTimeout(() => { copyTimer = null; setCopied(false); }, 2000);
    } catch {
      // clipboard unavailable — user can select manually
    }
  }

  function onKeyDown(e: KeyboardEvent) {
    if (e.key === "Escape") props.onClose();
    else if (e.key === "Enter" && !deployCmd()) void mintAndShowCmd();
  }

  return (
    <div
      data-testid="machine-deploy-dialog"
      style={OVERLAY_STYLE}
      onClick={(e) => { if (e.target === e.currentTarget) props.onClose(); }}
      onKeyDown={onKeyDown}
    >
      <div ref={animateOverlayPanel} style={DIALOG_STYLE}>
        <div style={{ display: "flex", "justify-content": "space-between", "align-items": "center", "margin-bottom": "20px" }}>
          <h2 style={{ "font-size": "15px", color: "var(--text-hi)", margin: "0" }}>Add Machine</h2>
          <IconButton icon="close" label="Close" onClick={props.onClose} />
        </div>

        <p style={{ "font-size": "var(--md-body-s-size)", color: "var(--text-lo)", "margin-bottom": "16px", "line-height": "1.5" }}>
          Mint a one-time bootstrap token for a new worker. Run the
          generated command on the target Mac to register it with this
          coordinator. The token expires after 24 hours.
        </p>

        <Show
          when={reachable}
          fallback={
            <div
              data-testid="machine-deploy-unreachable"
              style={{
                background: "var(--md-sys-color-surface-container)",
                border: "1px solid var(--md-sys-color-outline)",
                "border-radius": "var(--md-shape-sm)",
                padding: "16px",
                "font-size": "13px",
                "line-height": "1.5",
                color: "var(--text-lo)",
              }}
            >
              Open Roost over your tailnet URL
              (https://&lt;this-mac&gt;.&lt;tailnet&gt;.ts.net:4102), not localhost —
              the join command must point somewhere the new Mac can reach.
            </div>
          }
        >
        <Show when={!deployCmd()}>
          <div style={{ "margin-bottom": "12px" }}>
            <TextField
              testId="machine-deploy-label"
              label="Machine label"
              value={label()}
              onInput={(v) => setLabel(v)}
              placeholder="optional — defaults to the Mac's hostname"
              style={{ width: "100%" }}
            />
          </div>

          <Show when={error()}>
            <p style={{ color: "var(--color-err)", "font-size": "var(--md-body-s-size)", "margin-bottom": "10px" }}>{error()}</p>
          </Show>

          <div style={{ display: "flex", gap: "8px", "justify-content": "flex-end" }}>
            <Button variant="text" onClick={props.onClose}>
              Cancel
            </Button>
            <Button
              variant="filled"
              data-testid="machine-deploy-mint"
              onClick={() => void mintAndShowCmd()}
              disabled={loading()}
            >
              {loading() ? "Minting…" : "Mint token"}
            </Button>
          </div>
        </Show>

        <Show when={deployCmd()}>
          <div>
            <div style={{ "font-size": "11px", color: "var(--text-lo)", "margin-bottom": "8px", "text-transform": "uppercase", "letter-spacing": "0.06em" }}>
              Run this on the new Mac (Tailscale must be running there):
            </div>
            <div style={{
              background: "var(--bg-base)",
              border: "1px solid var(--md-sys-color-outline-variant)",
              "border-radius": "var(--md-shape-sm)",
              padding: "10px 12px",
              "font-family": "monospace",
              "font-size": "var(--md-body-s-size)",
              color: "var(--color-ok)",
              "word-break": "break-all" as const,
              "line-height": "1.6",
              "margin-bottom": "12px",
            }}>
              {deployCmd()}
            </div>

            <div style={{ display: "flex", gap: "8px", "justify-content": "flex-end" }}>
              <Button
                variant="tonal"
                data-testid="machine-deploy-copy"
                onClick={() => void copyCmd()}
                style={{ color: copied() ? "var(--color-ok)" : undefined }}
              >
                {copied() ? "Copied ✓" : "Copy command"}
              </Button>
              <Button variant="filled" onClick={props.onClose}>
                Done
              </Button>
            </div>
          </div>
        </Show>
        </Show>
      </div>
    </div>
  );
}
