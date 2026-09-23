// Notification-dock pair requests for already-trusted browsers.
// It renders server provenance and delegates generated-code approval to the
// root provider so no notification surface can own another approver secret.

import { useLocation } from "@solidjs/router";
import { createMemo, createSignal, For, onCleanup, Show } from "solid-js";
import { coordClient } from "../connect.ts";
import { deletePairRequest } from "../store/mutations.ts";
import { rootStore } from "../store/root.ts";
import { addToast } from "../store/toastStore.ts";
import { usePairApproval } from "./PairApprovalProvider.tsx";
import { PairRequestCard, isPairRequestExpired } from "./PairRequestCard.tsx";

export function PairRequestNotifier() {
  const [now, setNow] = createSignal(Date.now());
  const [denyingRequestId, setDenyingRequestId] = createSignal<string | null>(null);
  const pairApproval = usePairApproval();
  const expiryTimer = setInterval(() => setNow(Date.now()), 1_000);
  onCleanup(() => clearInterval(expiryTimer));

  const location = useLocation();
  const isPairRequestNotifierSuppressed = createMemo(() =>
    location.pathname === "/settings/devices"
    || (location.pathname === "/pair" && !rootStore.browser_unauthorized)
  );
  const pending = createMemo(() => {
    const currentNow = now();
    return Object.values(rootStore.pair_requests)
      .filter((request) => !isPairRequestExpired(request, currentNow));
  });
  const approvalOwnerIsStillPending = createMemo(() => {
    const busyRequestId = pairApproval.busyRequestId();
    return busyRequestId === null
      || pending().some((request) => request.ephemeral_id === busyRequestId);
  });

  async function deny(ephemeralId: string): Promise<void> {
    if (denyingRequestId() !== null) return;
    setDenyingRequestId(ephemeralId);
    try {
      await coordClient.pairDeny({ ephemeralId });
      deletePairRequest(ephemeralId);
      addToast("Pair request dismissed", "ok");
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      addToast(`Dismiss failed: ${message}`, "err");
    } finally {
      setDenyingRequestId(null);
    }
  }

  return (
    <Show when={
      !rootStore.browser_unauthorized
      && !isPairRequestNotifierSuppressed()
      && approvalOwnerIsStillPending()
    }>
      <For each={pending()}>
        {(request) => (
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
            onDeny={() => void deny(request.ephemeral_id)}
          />
        )}
      </For>
    </Show>
  );
}
