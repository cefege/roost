// Browser pairing surface. While rootStore.browser_access_state is
// "unauthorized" it is the full-screen pairing gate App.tsx shows instead of
// the workbench (PairingGatePanel); for authorized browsers (/pair, the
// zero-machine home, Settings → Devices) it lists pending requests to approve.
// PairApprovalProvider owns approver codes; PairingRequesterProvider the requester.

import { createMemo, createSignal, For, onCleanup, Show } from "solid-js";
import { coordClient } from "../../client/rpc/connect.ts";
import { animateOverlayPanel } from "../../lib/overlayMotion.ts";
import { deletePairRequest } from "../../store/mutations.ts";
import { rootStore } from "../../store/root.ts";
import { addToast } from "../../store/toastStore.ts";
import { EmptyState, SectionTitle } from "../Settings/md/primitives.tsx";
import { usePairApproval } from "./PairApprovalProvider.tsx";
import { PairingGatePanel } from "./PairingGatePanel.tsx";
import { PairingPageHeader } from "./PairingPageHeader.tsx";
import { PairRequestCard, isPairRequestExpired } from "./PairRequestCard.tsx";
import "./Onboarding.css";

export function Onboarding(props: { embedded?: boolean } = {}) {
  const [denyingRequestId, setDenyingRequestId] = createSignal<string | null>(null);
  let onboardingTouchClientY: number | null = null;
  const pairApproval = usePairApproval();
  const workerCount = () => Object.keys(rootStore.workers).length;
  const [pairRequestClock, setPairRequestClock] = createSignal(Date.now());
  const pairRequestExpiryTimer = setInterval(() => setPairRequestClock(Date.now()), 1_000);
  const pendingPairRequests = createMemo(() => {
    const currentNow = pairRequestClock();
    return Object.values(rootStore.pair_requests)
      .filter((request) => !isPairRequestExpired(request, currentNow));
  });
  onCleanup(() => clearInterval(pairRequestExpiryTimer));

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
      onWheel={handleOnboardingWheel}
      onTouchStart={handleOnboardingTouchStart}
      onTouchEnd={endOnboardingTouch}
      onTouchCancel={endOnboardingTouch}
    >
      <div class="onboarding-panel">
        <Show when={rootStore.browser_access_state === "unauthorized"}>
          <PairingGatePanel />
        </Show>
        <Show when={rootStore.browser_access_state !== "unauthorized"}>
          <Show when={!props.embedded}>
            <PairingPageHeader title="Browser pairing" />
          </Show>
          <Show when={workerCount() === 0}>
            <p class="md-body-m pairing-header__body">
              This browser is authorized, but no machines have registered as workers yet.
            </p>
          </Show>
          <Show when={pendingPairRequests().length === 0}>
            <div data-testid="onboarding-no-pending">
              <EmptyState
                icon="devices"
                title="No browsers are waiting for approval"
                supporting="When you open Roost in a new browser and request access, it'll show up here to approve."
              />
            </div>
          </Show>
          <Show when={pendingPairRequests().length > 0}>
            <div data-testid="pair-approval-list" class="pairing-approval-list">
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
        </Show>
      </div>
    </div>
  );
}
