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
import {
  Button,
  Card,
  EmptyState,
  SectionTitle,
  StatusDot,
  Surface,
  TextField,
} from "./Settings/md/primitives.tsx";
import { PairRequestCard, isPairRequestExpired } from "./PairRequestCard.tsx";
import { animateOverlayPanel } from "../lib/overlayMotion.ts";

type PairPollStatus = "idle" | "pending" | "approved" | "denied" | "expired" | "error";

export function Onboarding(props: { embedded?: boolean } = {}) {
  const [bootstrapToken, setBootstrapToken] = createSignal("");
  const [status, setStatus] = createSignal<"idle" | "loading" | "done" | "error">("idle");
  const [errorMsg, setErrorMsg] = createSignal("");

  // tap-to-pair local state
  const [pairEphemeralId, setPairEphemeralId] = createSignal<string | null>(null);

  const [pairPollStatus, setPairPollStatus] = createSignal<PairPollStatus>("idle");
  let pairPollTimer: ReturnType<typeof setInterval> | null = null;
  const [busyRequestId, setBusyRequestId] = createSignal<string | null>(null);

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
  const [pairRequestClock, setPairRequestClock] = createSignal(Date.now());
  const pairRequestExpiryTimer = setInterval(() => setPairRequestClock(Date.now()), 1_000);
  const pendingPairRequests = createMemo(() => {
    const currentNow = pairRequestClock();
    return Object.values(rootStore.pair_requests)
      .filter((request) => !isPairRequestExpired(request, currentNow));
  });
  const pairPollStatusIndicator = createMemo(() => {
    switch (pairPollStatus()) {
      case "approved":
        return "ok";
      case "denied":
      case "expired":
        return "warn";
      case "error":
        return "error";
      default:
        return "info";
    }
  });
  onCleanup(() => {
    if (pairPollTimer) clearInterval(pairPollTimer);
    clearInterval(pairRequestExpiryTimer);
  });

  // Pair-request deltas can arrive while this embedded approval list is open.
  // The expiry clock keeps the list from retaining a stale approval action.

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

  function autoRedeemPastedToken(event: ClipboardEvent): void {
    const pastedToken = event.clipboardData?.getData("text") ?? "";
    if (!pastedToken.startsWith("roost_bt_")) return;
    setBootstrapToken(pastedToken);
    setTimeout(() => void redeemToken(), 0);
    event.preventDefault();
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
        setPairPollStatus(s as PairPollStatus);
        if (s === "approved") {
          if (pairPollTimer) clearInterval(pairPollTimer);
          addToast("Browser approved — reloading", "ok");
          window.location.reload();
        } else if (s === "denied") {
          if (pairPollTimer) clearInterval(pairPollTimer);
          addToast("Pair request denied", "warn");
        } else if (s === "expired") {
          if (pairPollTimer) clearInterval(pairPollTimer);
          addToast("Pair request expired — request again", "warn");
        }
      } catch (e) {
        setPairPollStatus("error");
        const msg = e instanceof Error ? e.message : String(e);
        addToast(`Pair poll failed: ${msg}`, "err");
        if (pairPollTimer) clearInterval(pairPollTimer);
      }
    }, 2_000);
  }

  async function approvePairRequest(ephemeral_id: string): Promise<void> {
    if (busyRequestId()) return;
    setBusyRequestId(ephemeral_id);
    try {
      await coordClient.pairApprove({ ephemeralId: ephemeral_id });
      deletePairRequest(ephemeral_id);
      addToast("Approved", "ok");
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      addToast(`Approve failed: ${msg}`, "err");
    } finally {
      setBusyRequestId(null);
    }
  }

  async function denyPairRequest(ephemeral_id: string): Promise<void> {
    if (busyRequestId()) return;
    setBusyRequestId(ephemeral_id);
    try {
      await coordClient.pairDeny({ ephemeralId: ephemeral_id });
      deletePairRequest(ephemeral_id);
      addToast("Denied", "ok");
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      addToast(`Deny failed: ${msg}`, "err");
    } finally {
      setBusyRequestId(null);
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
    <div
      ref={animateOverlayPanel}
      data-testid="onboarding"
      style={{
        display: "flex",
        "flex-direction": "column",
        gap: "var(--md-space-5)",
        padding: props.embedded ? "0" : "var(--md-space-6)",
        "max-width": "var(--roost-dialog-max-inline-size)",
      }}
    >
      <Show when={!props.embedded}>
        <h2 class="md-headline-s" style={{ margin: 0 }}>Pair this browser</h2>
      </Show>
      <Show when={props.embedded && isAuthorized() && pendingPairRequests().length === 0}>
        <div data-testid="onboarding-no-pending">
          <EmptyState
            icon="devices"
            title="No browsers are waiting for approval"
            supporting="When you open Roost in a new browser and request access, it'll show up here to approve."
          />
        </div>
      </Show>
      <Show when={!isAuthorized()}>
        <p
          class="md-body-m"
          style={{ margin: 0, color: "var(--md-sys-color-on-surface-variant)" }}
        >
          This browser isn't authorized by the coordinator yet. Either paste a
          pairing code below, or request approval from a browser that's
          already paired.
        </p>
      </Show>
      <Show when={resetEligible()}>
        <div>
          <Button variant="secondary" onClick={() => void resetRejectedKey()}>
            Reset this device key
          </Button>
        </div>
      </Show>
      <Show when={isAuthorized() && workerCount() === 0}>
        <p
          class="md-body-m"
          style={{ margin: 0, color: "var(--md-sys-color-on-surface-variant)" }}
        >
          This browser is authorized, but no machines have registered as workers yet.
        </p>
      </Show>

      <Show when={!isAuthorized()}>
        <Card
          data-testid="onboarding-token-step"
          title="I have a pairing code"
          supporting="Paste the roost_bt_… token you minted on the coordinator host."
          variant="outlined"
        >
          <div style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-3)" }}>
            <TextField
              type="text"
              testId="onboarding-token-input"
              value={bootstrapToken()}
              onInput={setBootstrapToken}
              placeholder="roost_bt_..."
              label="Pairing code"
              autofocus
              ref={(element) => {
                if (element instanceof HTMLInputElement) {
                  element.onpaste = autoRedeemPastedToken;
                }
              }}
            />
            <div>
              <Button
                variant="default"
                data-testid="onboarding-token-submit"
                onClick={redeemToken}
                disabled={!bootstrapToken() || status() === "loading"}
              >
                {status() === "loading" ? "Pairing…" : "Pair"}
              </Button>
            </div>
          </div>
        </Card>
      </Show>

      <Show when={!isAuthorized()}>
        <Card
          data-testid="onboarding-pair-step"
          title="I don't have a code"
          supporting="Request approval from a browser that's already paired."
          variant="outlined"
        >
          <Show
            when={pairEphemeralId() !== null}
            fallback={
              <div style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-3)" }}>
                <p
                  class="md-body-m"
                  style={{ margin: 0, color: "var(--md-sys-color-on-surface-variant)" }}
                >
                  You'll get a short code — open Roost on the paired browser and approve the
                  request from <strong>Settings → Devices</strong>.
                </p>
                <div>
                  <Button
                    variant="secondary"
                    data-testid="onboarding-pair-start-btn"
                    onClick={startPairFlow}
                    disabled={status() === "loading"}
                  >
                    {status() === "loading" ? "..." : "Request approval"}
                  </Button>
                </div>
              </div>
            }
          >
            <div style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-3)" }}>
              <p
                class="md-body-m"
                style={{ margin: 0, color: "var(--md-sys-color-on-surface-variant)" }}
              >
                On the already-paired browser, open <strong>Settings → Devices</strong>.
                You'll see this code listed under "Pending pair requests" — click
                <strong> Approve</strong>. This page reloads itself when approved.
              </p>
              <Surface
                level={2}
                radius="sm"
                pad={3}
                border
                data-testid="onboarding-pair-ephemeral-id"
              >
                <code class="md-title-m" style={{ display: "block", "overflow-wrap": "anywhere" }}>
                  {pairEphemeralId()}
                </code>
              </Surface>
              <div
                data-testid="onboarding-pair-poll-status"
                role="status"
                aria-live="polite"
                aria-atomic="true"
                style={{ display: "flex", "align-items": "center", gap: "var(--md-space-2)" }}
              >
                <StatusDot status={pairPollStatusIndicator()} />
                <span class="md-body-s">
                  <Show when={pairPollStatus() === "pending"}>Waiting for approval…</Show>
                  <Show when={pairPollStatus() === "approved"}>Approved. Reloading…</Show>
                  <Show when={pairPollStatus() === "denied"}>Request denied.</Show>
                  <Show when={pairPollStatus() === "expired"}>Request expired — request again.</Show>
                  <Show when={pairPollStatus() === "error"}>Poll error — try again.</Show>
                </span>
              </div>
            </div>
          </Show>
        </Card>
      </Show>

      <Show when={isAuthorized() && pendingPairRequests().length > 0}>
        <div
          data-testid="pair-approval-list"
          style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-3)" }}
        >
          <SectionTitle>Pending pair requests</SectionTitle>
          <For each={pendingPairRequests()}>
            {(request) => (
              <div data-testid="pair-approval-row" data-ephemeral-id={request.ephemeral_id}>
                <PairRequestCard
                  request={request}
                  busy={busyRequestId() === request.ephemeral_id}
                  onApprove={() => void approvePairRequest(request.ephemeral_id)}
                  onDeny={() => void denyPairRequest(request.ephemeral_id)}
                />
              </div>
            )}
          </For>
        </div>
      </Show>

      <Show when={status() === "done"}>
        <Surface
          level={2}
          radius="sm"
          pad={3}
          border
          role="status"
          aria-live="polite"
          aria-atomic="true"
        >
          <div style={{ display: "flex", "align-items": "center", gap: "var(--md-space-2)" }}>
            <StatusDot status="ok" />
            <span class="md-body-m">Registered. Reload to connect.</span>
          </div>
        </Surface>
      </Show>
      <Show when={status() === "error"}>
        <Surface level={2} radius="sm" pad={3} border role="alert">
          <div style={{ display: "flex", "align-items": "center", gap: "var(--md-space-2)" }}>
            <StatusDot status="error" />
            <span class="md-body-m" style={{ color: "var(--md-sys-color-error)" }}>
              Error: {errorMsg()}
            </span>
          </div>
        </Surface>
      </Show>
    </div>
  );
}
