// Corner notifier for inbound pair requests on already-trusted browsers.
// Reads rootStore.pair_requests (fed by the Sync firehose pairRequestDelta
// frames + per-connect snapshot seed — store/sync.ts, perf sweep C2.4).
// Approval and rendering are delegated to PairRequestCard so every surface
// presents the same provenance and controls.

import { useLocation } from "@solidjs/router";
import { For, Show, createMemo, createSignal, onCleanup } from "solid-js";
import { Portal } from "solid-js/web";
import { rootStore } from "../store/root.ts";
import { deletePairRequest } from "../store/mutations.ts";
import { coordClient } from "../connect.ts";
import { addToast } from "../store/toastStore.ts";
import { PairRequestCard, isPairRequestExpired } from "./PairRequestCard.tsx";

const canApprovePairRequests = () => !rootStore.browser_unauthorized;

export function PairRequestNotifier() {
  const [now, setNow] = createSignal(Date.now());
  const [busyRequestId, setBusyRequestId] = createSignal<string | null>(null);
  const expiryTimer = setInterval(() => setNow(Date.now()), 1_000);
  onCleanup(() => clearInterval(expiryTimer));

  const location = useLocation();
  const isPairRequestNotifierSuppressed = createMemo(() =>
    location.pathname === "/settings/devices"
    || (location.pathname === "/pair" && canApprovePairRequests())
  );

  const pending = createMemo(() => {
    const currentNow = now();
    return Object.values(rootStore.pair_requests)
      .filter((request) => !isPairRequestExpired(request, currentNow));
  });

  async function approve(id: string): Promise<void> {
    if (busyRequestId()) return;
    setBusyRequestId(id);
    try {
      await coordClient.pairApprove({ ephemeralId: id });
      deletePairRequest(id);
      addToast("Browser approved", "ok");
    } catch (error) {
      addToast(`Approve failed: ${error instanceof Error ? error.message : String(error)}`, "err");
    } finally {
      setBusyRequestId(null);
    }
  }

  async function deny(id: string): Promise<void> {
    if (busyRequestId()) return;
    setBusyRequestId(id);
    try {
      await coordClient.pairDeny({ ephemeralId: id });
      deletePairRequest(id);
      addToast("Pair request dismissed", "ok");
    } catch (error) {
      addToast(`Dismiss failed: ${error instanceof Error ? error.message : String(error)}`, "err");
    } finally {
      setBusyRequestId(null);
    }
  }

  return (
    <Show when={canApprovePairRequests() && !isPairRequestNotifierSuppressed() && pending().length > 0}>
      <Portal mount={document.body}>
        <div
          data-testid="pair-request-notifier"
          style={{
            position: "fixed",
            bottom: "calc(var(--md-space-5) + var(--toast-stack-height) + var(--md-space-1))",
            right: "var(--md-space-5)",
            display: "flex",
            "flex-direction": "column",
            gap: "var(--md-space-3)",
            "z-index": "10000",
            "max-width": "calc(100vw - var(--md-space-6))",
            "padding-bottom": "env(safe-area-inset-bottom, 0px)",
          }}
        >
          <For each={pending()}>
            {(request) => (
              <PairRequestCard
                request={request}
                busy={busyRequestId() === request.ephemeral_id}
                onApprove={() => void approve(request.ephemeral_id)}
                onDeny={() => void deny(request.ephemeral_id)}
              />
            )}
          </For>
        </div>
      </Portal>
    </Show>
  );
}
