// First-boot component. Shown when coord is reachable but no workers are registered.
// Two enrollment flows:
//   1. redeem a one-shot browser grant, pasted or captured from a #pair fragment;
//   2. tap-to-pair: this browser posts its public key and an already-authorized
//      browser sees the request in #pair-approval-list and approves it.
// When already authorized, the pair-approval-list lets this browser approve
// pending requests from other browsers (rootStore.pair_requests).

import { createSignal, createMemo, createResource, For, Show, onCleanup } from "solid-js";
import { coordClient } from "../connect.ts";
import { getPublicKeyB64, isResetWebKeyEligible, resetWebKey } from "../auth/web-key.ts";
import { redeemPairToken } from "../auth/redeemPairToken.ts";
import { rootStore } from "../store/root.ts";
import { deletePairRequest } from "../store/mutations.ts";
import { addToast } from "../store/toastStore.ts";
import { browserSelfLabel } from "../lib/browserSelfLabel.ts";
import { Button } from "./Settings/md/primitives.tsx";
import { animateOverlayPanel } from "../lib/overlayMotion.ts";


export function Onboarding(props: { embedded?: boolean } = {}) {
  const [bootstrapToken, setBootstrapToken] = createSignal("");
  const [status, setStatus] = createSignal<"idle" | "loading" | "done" | "error">("idle");
  const [errorMsg, setErrorMsg] = createSignal("");

  // tap-to-pair local state
  const [pairEphemeralId, setPairEphemeralId] = createSignal<string | null>(null);
  const [pairPollStatus, setPairPollStatus] = createSignal<"idle" | "pending" | "approved" | "denied" | "error">("idle");
  let pairPollTimer: ReturnType<typeof setInterval> | null = null;

  const workerCount = () => Object.keys(rootStore.workers).length;
  // authCoordIdentity is a PUBLIC endpoint — coord_identity is populated
  // even when the browser has not been authorized by the coordinator. Use the
  // browser_unauthorized flag set by sync.ts (true when authenticated list
  // calls return Connect Unauthenticated). That is the authoritative signal.
  // Without this gate, Onboarding renders only the <h2>Welcome</h2> on
  // an unauthorized second browser (every <Show when={!isAuthorized()}> hides
  // the tabs / token mode / pair mode → black screen with one heading).
  const isAuthorized = createMemo(() => !rootStore.browser_unauthorized);
  const [resetEligible] = createResource(
    () => rootStore.browser_unauthorized,
    async (unauthorized) => unauthorized ? isResetWebKeyEligible() : false,
  );
  const pendingPairRequests = createMemo(() => Object.values(rootStore.pair_requests));

  onCleanup(() => {
    if (pairPollTimer) clearInterval(pairPollTimer);
  });

  async function redeemToken() {
    setStatus("loading");
    const res = await redeemPairToken(bootstrapToken());
    if (res.ok) {
      setStatus("done");
      window.location.reload();
    } else {
      setStatus("error");
      setErrorMsg(res.error);
      addToast(`Redeem failed: ${res.error}`, "err");
    }
  }

  // tap-to-pair: this browser publishes its pubkey, then polls until
  // another authorized browser approves. On approve → reload so
  // bootstrapSync re-runs with the now-authorized JWT.
  async function startPairFlow() {
    setStatus("loading");
    try {
      const pubkeyB64 = await getPublicKeyB64();
      const { ephemeralId: ephemeral_id } = await coordClient.pairCreate({
        sshPubkeyB64: pubkeyB64,
        label: browserSelfLabel(),
      });
      setPairEphemeralId(ephemeral_id);
      setPairPollStatus("pending");
      setStatus("idle");
      _beginPairPoll(ephemeral_id);
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setStatus("error");
      setErrorMsg(msg);
      addToast(`Pair create failed: ${msg}`, "err");
    }
  }

  function _beginPairPoll(ephemeral_id: string) {
    if (pairPollTimer) clearInterval(pairPollTimer);
    pairPollTimer = setInterval(async () => {
      try {
        const { status: s } = await coordClient.pairPoll({ ephemeralId: ephemeral_id });
        setPairPollStatus(s as "pending" | "approved" | "denied" | "error");
        if (s === "approved") {
          if (pairPollTimer) clearInterval(pairPollTimer);
          addToast("Browser approved — reloading", "ok");
          window.location.reload();
        } else if (s === "denied") {
          if (pairPollTimer) clearInterval(pairPollTimer);
          addToast("Pair request denied", "warn");
        }
      } catch (e) {
        setPairPollStatus("error");
        const msg = e instanceof Error ? e.message : String(e);
        addToast(`Pair poll failed: ${msg}`, "err");
        if (pairPollTimer) clearInterval(pairPollTimer);
      }
    }, 2_000);
  }

  async function approvePairRequest(ephemeral_id: string) {
    try {
      await coordClient.pairApprove({ ephemeralId: ephemeral_id });
      deletePairRequest(ephemeral_id);
      addToast("Approved", "ok");
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      addToast(`Approve failed: ${msg}`, "err");
    }
  }

  async function denyPairRequest(ephemeral_id: string) {
    try {
      await coordClient.pairDeny({ ephemeralId: ephemeral_id });
      deletePairRequest(ephemeral_id);
      addToast("Denied", "ok");
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      addToast(`Deny failed: ${msg}`, "err");
    }
  }

  async function resetRejectedKey(): Promise<void> {
    if (!confirm("Reset this device key? This browser will need to pair again.")) return;
    try {
      await resetWebKey();
    } catch (error) {
      addToast(`Key reset failed: ${error instanceof Error ? error.message : String(error)}`, "err");
    }
  }

  return (
    <div ref={animateOverlayPanel} data-testid="onboarding" style={{ padding: props.embedded ? "0" : "40px", color: "var(--md-sys-color-on-surface)", "max-width": "520px" }}>
      <Show when={!props.embedded}>
        <h2 style={{ "font-size": "20px", "margin-bottom": "6px", color: "var(--md-sys-color-on-surface)" }}>Pair this browser</h2>
      </Show>
      {/* Embedded in DevicesPane: when this browser is already authorized and
          nothing is pending, the card would otherwise be blank. */}
      <Show when={props.embedded && isAuthorized() && pendingPairRequests().length === 0}>
        <p data-testid="onboarding-no-pending" style={{ "font-size": "13px", color: "var(--md-sys-color-on-surface-variant)", margin: "0", "line-height": "1.5" }}>
          No browsers are waiting for approval. When you open Roost in a new
          browser and request access, it'll show up here to approve.
        </p>
      </Show>
      <Show when={!isAuthorized()}>
        <p style={{ "font-size": "13px", color: "var(--md-sys-color-on-surface-variant)", "margin-bottom": "24px", "line-height": "1.5" }}>
          This browser isn't authorized by the coordinator yet. Either paste a
          pairing code below, or request approval from a browser that's
          already paired.
        </p>
      </Show>
        <Show when={resetEligible()}>
          <Button variant="tonal" onClick={() => void resetRejectedKey()}>
            Reset this device key
          </Button>
        </Show>
      <Show when={isAuthorized() && workerCount() === 0}>
        <p style={{ "font-size": "13px", color: "var(--md-sys-color-on-surface-variant)", "margin-bottom": "20px" }}>
          This browser is authorized, but no machines have registered as workers yet.
        </p>
      </Show>

      {/* Card A — paste a pairing code (PRIMARY path for cross-Mac access) */}
      <Show when={!isAuthorized()}>
        <div
          data-testid="onboarding-token-step"
          style={{
            background: "var(--md-sys-color-surface-container)",
            border: "1px solid var(--md-sys-color-outline)",
            "border-radius": "var(--md-shape-sm)",
            padding: "20px",
            "margin-bottom": "16px",
          }}
        >
          <div style={{ "font-size": "14px", "font-weight": 600, color: "var(--md-sys-color-on-surface)", "margin-bottom": "4px" }}>
            I have a pairing code
          </div>
          <div style={{ "font-size": "12px", color: "var(--md-sys-color-on-surface-variant)", "margin-bottom": "12px" }}>
            Paste the <code style={{ color: "var(--md-sys-color-on-surface)" }}>roost_bt_…</code> token you minted on the coordinator host.
          </div>
          <input
            type="text"
            data-testid="onboarding-token-input"
            value={bootstrapToken()}
            onInput={(e) => setBootstrapToken(e.currentTarget.value)}
            onPaste={(e) => {
              // Auto-submit on paste of a valid-looking token so the user
              // doesn't have to hunt for a button after pasting.
              const text = e.clipboardData?.getData("text") ?? "";
              if (text.startsWith("roost_bt_")) {
                setBootstrapToken(text);
                setTimeout(() => void redeemToken(), 0);
                e.preventDefault();
              }
            }}
            placeholder="roost_bt_..."
            autofocus
            style={{
              width: "100%",
              background: "var(--md-sys-color-surface)",
              border: "1px solid var(--md-sys-color-outline)",
              color: "var(--md-sys-color-on-surface)",
              padding: "10px 12px",
              "font-size": "13px",
              "font-family": "ui-monospace, SFMono-Regular, Menlo, monospace",
              "border-radius": "var(--md-shape-xs)",
            }}
          />
          <Button
            variant="filled"
            data-testid="onboarding-token-submit"
            onClick={redeemToken}
            disabled={!bootstrapToken() || status() === "loading"}
            style={{ "margin-top": "10px" }}
          >
            {status() === "loading" ? "Pairing…" : "Pair"}
          </Button>
        </div>
      </Show>

      {/* Card B — tap to pair (request approval from another browser) */}
      <Show when={!isAuthorized()}>
        <div
          data-testid="onboarding-pair-step"
          style={{
            background: "var(--md-sys-color-surface-container)",
            border: "1px solid var(--md-sys-color-outline)",
            "border-radius": "var(--md-shape-sm)",
            padding: "20px",
            "margin-bottom": "20px",
          }}
        >
          <div style={{ "font-size": "14px", "font-weight": 600, color: "var(--md-sys-color-on-surface)", "margin-bottom": "4px" }}>
            I don't have a code
          </div>
          <Show
            when={pairEphemeralId() !== null}
            fallback={
              <>
                <div style={{ "font-size": "12px", color: "var(--md-sys-color-on-surface-variant)", "margin-bottom": "12px", "line-height": "1.5" }}>
                  Request approval from a browser that's already paired.
                  You'll get a short code — open Roost on the paired
                  browser and approve the request from <strong>Settings →
                  Devices</strong>.
                </div>
                <Button
                  variant="tonal"
                  data-testid="onboarding-pair-start-btn"
                  onClick={startPairFlow}
                  disabled={status() === "loading"}
                >
                  {status() === "loading" ? "..." : "Request approval"}
                </Button>
              </>
            }
          >
            <div style={{ "font-size": "13px", color: "var(--md-sys-color-on-surface-variant)", "margin-bottom": "8px", "line-height": "1.5" }}>
              On the already-paired browser, open <strong>Settings → Devices</strong>.
              You'll see this code listed under "Pending pair requests" — click
              <strong> Approve</strong>. This page reloads itself when approved.
            </div>
            <div
              data-testid="onboarding-pair-ephemeral-id"
              style={{
                background: "var(--md-sys-color-surface-container)",
                border: "1px solid var(--md-sys-color-outline)",
                padding: "10px 14px",
                "font-family": "ui-monospace, SFMono-Regular, Menlo, monospace",
                "font-size": "14px",
                color: "var(--md-sys-color-on-surface)",
                "border-radius": "var(--md-shape-xs)",
                "margin-bottom": "10px",
                "word-break": "break-all",
              }}
            >
              {pairEphemeralId()}
            </div>
            <p data-testid="onboarding-pair-poll-status" style={{ "font-size": "12px", color: "var(--md-sys-color-on-surface-variant)" }}>
              <Show when={pairPollStatus() === "pending"}>Waiting for approval…</Show>
              <Show when={pairPollStatus() === "approved"}>Approved. Reloading…</Show>
              <Show when={pairPollStatus() === "denied"}>Request denied.</Show>
              <Show when={pairPollStatus() === "error"}>Poll error — try again.</Show>
            </p>
          </Show>
        </div>
      </Show>

      {/* approve-list (visible to already-authorized browsers) */}
      <Show when={isAuthorized() && pendingPairRequests().length > 0}>
        <div data-testid="pair-approval-list" style={{ "margin-bottom": "20px" }}>
          <h3 style={{ "font-size": "13px", color: "var(--md-sys-color-on-surface)", "margin-bottom": "8px" }}>
            Pending pair requests
          </h3>
          <For each={pendingPairRequests()}>
            {(req) => (
              <div
                data-testid="pair-approval-row"
                data-ephemeral-id={req.ephemeral_id}
                style={{
                  display: "flex",
                  "align-items": "center",
                  "justify-content": "space-between",
                  background: "var(--md-sys-color-surface-container)",
                  border: "1px solid var(--md-sys-color-outline)",
                  padding: "8px 12px",
                  "border-radius": "var(--md-shape-xs)",
                  "margin-bottom": "6px",
                }}
              >
                <div style={{ "font-size": "12px", "font-family": "ui-monospace, SFMono-Regular, Menlo, monospace", color: "var(--md-sys-color-on-surface)" }}>
                  {req.ephemeral_id}
                </div>
                <div style={{ display: "flex", gap: "var(--md-space-2)" }}>
                  <Button
                    variant="filled"
                    data-testid="pair-approve-btn"
                    onClick={() => approvePairRequest(req.ephemeral_id)}
                  >
                    Approve
                  </Button>
                  <Button
                    variant="text"
                    data-testid="pair-deny-btn"
                    onClick={() => denyPairRequest(req.ephemeral_id)}
                  >
                    Deny
                  </Button>
                </div>
              </div>
            )}
          </For>
        </div>
      </Show>

      <Show when={status() === "done"}>
        <p style={{ color: "var(--md-sys-color-primary)", "font-size": "13px" }}>Registered. Reload to connect.</p>
      </Show>
      <Show when={status() === "error"}>
        <p style={{ color: "var(--md-sys-color-error)", "font-size": "13px" }}>Error: {errorMsg()}</p>
      </Show>
    </div>
  );
}
