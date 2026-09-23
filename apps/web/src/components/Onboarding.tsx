// First-boot pairing surface for a browser without coordinator authority.
// It renders requester ceremony state and the authorized approval list, while
// PairApprovalProvider owns generated approver codes and their only dialog.

import { createMemo, createResource, createSignal, For, onCleanup, Show } from "solid-js";
import { isResetWebKeyEligible, resetWebKey } from "../auth/web-key.ts";
import { redeemPairToken } from "../auth/redeemPairToken.ts";
import { coordClient } from "../connect.ts";
import { animateOverlayPanel } from "../lib/overlayMotion.ts";
import { deletePairRequest } from "../store/mutations.ts";
import { rootStore } from "../store/root.ts";
import { addToast } from "../store/toastStore.ts";
import {
  Button,
  Card,
  EmptyState,
  SectionTitle,
  StatusDot,
  Surface,
  TextField,
} from "./Settings/md/primitives.tsx";
import { OnboardingRequestCard } from "./OnboardingRequestCard.tsx";
import { usePairApproval } from "./PairApprovalProvider.tsx";
import { PairRequestCard, isPairRequestExpired } from "./PairRequestCard.tsx";
import { createOnboardingPairingCeremony } from "./onboarding-pairing-ceremony.ts";

export function Onboarding(props: { embedded?: boolean } = {}) {
  const [bootstrapToken, setBootstrapToken] = createSignal("");
  const [status, setStatus] = createSignal<"idle" | "loading" | "done" | "error">("idle");
  const [errorMsg, setErrorMsg] = createSignal("");
  const [denyingRequestId, setDenyingRequestId] = createSignal<string | null>(null);
  let onboardingTouchClientY: number | null = null;
  const pairApproval = usePairApproval();
  const requesterPairing = createOnboardingPairingCeremony({
    redirectAfterPairing,
    reportRequestError: (message) => {
      setStatus("error");
      setErrorMsg(message);
    },
  });
  const workerCount = () => Object.keys(rootStore.workers).length;
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
  onCleanup(() => clearInterval(pairRequestExpiryTimer));

  function redirectAfterPairing(): void {
    window.location.replace("/");
  }

  async function redeemToken(): Promise<void> {
    setStatus("loading");
    const result = await redeemPairToken(bootstrapToken());
    if (result.ok) {
      setStatus("done");
      redirectAfterPairing();
      return;
    }
    setStatus("error");
    setErrorMsg(result.error);
    addToast(`Redeem failed: ${result.error}`, "err");
  }

  function autoRedeemPastedToken(event: ClipboardEvent): void {
    const pastedToken = event.clipboardData?.getData("text") ?? "";
    if (!pastedToken.startsWith("roost_bt_")) return;
    setBootstrapToken(pastedToken);
    setTimeout(() => void redeemToken(), 0);
    event.preventDefault();
  }

  async function denyPairRequest(ephemeralId: string): Promise<void> {
    if (denyingRequestId() !== null) return;
    setDenyingRequestId(ephemeralId);
    try {
      await coordClient.pairDeny({ ephemeralId });
      deletePairRequest(ephemeralId);
      addToast("Denied", "ok");
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      addToast(`Deny failed: ${message}`, "err");
    } finally {
      setDenyingRequestId(null);
    }
  }

  async function resetRejectedKey(): Promise<void> {
    if (!confirm("Reset this device key? This browser will need to pair again.")) return;
    requesterPairing.clear();
    try {
      await resetWebKey();
    } catch (error) {
      addToast(`Key reset failed: ${error instanceof Error ? error.message : String(error)}`, "err");
    }
  }

  function startRequesterPairing(): void {
    setStatus("idle");
    setErrorMsg("");
    void requesterPairing.start();
  }
  function scrollOnboardingRoot(element: HTMLDivElement, deltaY: number): boolean {
    if (props.embedded || deltaY === 0) return false;
    const nextScrollTop = Math.max(
      0,
      Math.min(element.scrollHeight - element.clientHeight, element.scrollTop + deltaY),
    );
    if (nextScrollTop === element.scrollTop) return false;
    element.scrollTop = nextScrollTop;
    return true;
  }

  function handleOnboardingWheel(event: WheelEvent): void {
    if (event.ctrlKey) return;
    if (scrollOnboardingRoot(event.currentTarget as HTMLDivElement, event.deltaY)) {
      event.preventDefault();
    }
  }

  function handleOnboardingTouchStart(event: TouchEvent): void {
    onboardingTouchClientY = !props.embedded && event.touches.length === 1
      ? event.touches[0]!.clientY
      : null;
  }

  function handleOnboardingTouchMove(event: TouchEvent): void {
    if (event.touches.length !== 1) {
      onboardingTouchClientY = null;
      return;
    }
    const clientY = event.touches[0]!.clientY;
    if (onboardingTouchClientY === null) return;
    const moved = scrollOnboardingRoot(
      event.currentTarget as HTMLDivElement,
      onboardingTouchClientY - clientY,
    );
    onboardingTouchClientY = clientY;
    if (moved) event.preventDefault();
  }

  function endOnboardingTouch(): void {
    onboardingTouchClientY = null;
  }
  function mountOnboardingRoot(element: HTMLDivElement): void {
    animateOverlayPanel(element);
    element.addEventListener("touchmove", handleOnboardingTouchMove, { passive: false });
    onCleanup(() => element.removeEventListener("touchmove", handleOnboardingTouchMove));
  }

  return (
    <div
      ref={mountOnboardingRoot}
      data-testid="onboarding"
      class="onboarding-root"
      data-embedded={props.embedded ? "true" : "false"}
      style={{
        height: props.embedded ? undefined : "100dvh",
        "overflow-y": props.embedded ? undefined : "auto",
      }}
      onWheel={handleOnboardingWheel}
      onTouchStart={handleOnboardingTouchStart}
      onTouchEnd={endOnboardingTouch}
      onTouchCancel={endOnboardingTouch}
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
                if (element instanceof HTMLInputElement) element.onpaste = autoRedeemPastedToken;
              }}
            />
            <div>
              <Button
                variant="default"
                data-testid="onboarding-token-submit"
                onClick={() => void redeemToken()}
                disabled={!bootstrapToken() || status() === "loading"}
              >
                {status() === "loading" ? "Pairing…" : "Pair"}
              </Button>
            </div>
          </div>
        </Card>
      </Show>

      <Show when={!isAuthorized()}>
        <OnboardingRequestCard
          ephemeralId={requesterPairing.ephemeralId()}
          pollStatus={requesterPairing.pollStatus()}
          verificationCode={requesterPairing.verificationCode()}
          confirmationError={requesterPairing.confirmationError()}
          busy={requesterPairing.busy()}
          onStart={startRequesterPairing}
          onVerificationCodeInput={requesterPairing.updateVerificationCode}
          onConfirm={() => void requesterPairing.confirm()}
        />
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
                  busy={
                    denyingRequestId() === request.ephemeral_id
                    || pairApproval.busyRequestId() !== null
                  }
                  onApprove={() => void pairApproval.approve({
                    ephemeralId: request.ephemeral_id,
                    requesterLabel: request.label,
                    expiresAtMs: request.expiresAtMs,
                  })}
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
