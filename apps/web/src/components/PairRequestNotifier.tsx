// Corner notifier for inbound pair requests on already-trusted browsers.
// Reads rootStore.pair_requests (fed by the Sync firehose pairRequestDelta
// frames + per-connect snapshot seed — store/sync.ts, perf sweep C2.4).
// Calls coordClient.pairApprove / pairDeny; removes the row via per-key
// setRootStore write (NOT Record-replace — feedback_solid_setstore_record_replace).
// Material-3-styled card stack, bottom-right, mounted alongside ToastContainer.

import { For, Show, createMemo } from "solid-js";
import { Portal } from "solid-js/web";
import { rootStore, setRootStore, type PairRequest } from "../store/root.ts";
import { coordClient } from "../connect.ts";
import { addToast } from "../lib/toastStore.ts";
import { Button } from "./Settings/md/Button.tsx";

const isAuthorized = () => !rootStore.browser_unauthorized;

export function PairRequestNotifier() {
  const pending = createMemo(() => Object.values(rootStore.pair_requests));

  async function approve(id: string) {
    try {
      await coordClient.pairApprove({ ephemeralId: id });
      setRootStore("pair_requests", id, undefined as unknown as PairRequest);
      addToast("Browser approved", "ok");
    } catch (e) {
      addToast(`Approve failed: ${e instanceof Error ? e.message : String(e)}`, "err");
    }
  }

  async function deny(id: string) {
    try {
      await coordClient.pairDeny({ ephemeralId: id });
      setRootStore("pair_requests", id, undefined as unknown as PairRequest);
      addToast("Pair request dismissed", "ok");
    } catch (e) {
      addToast(`Dismiss failed: ${e instanceof Error ? e.message : String(e)}`, "err");
    }
  }

  return (
    <Show when={isAuthorized() && pending().length > 0}>
      <Portal mount={document.body}>
        <style>{`
          @keyframes pair-card-in {
            from { opacity: 0; transform: translateY(16px) scale(0.98); }
            to   { opacity: 1; transform: translateY(0)    scale(1);    }
          }
        `}</style>
        <div
          data-testid="pair-request-notifier"
          style={{
            position: "fixed",
            bottom: "calc(20px + var(--toast-stack-height) + 8px)",
            right: "20px",
            display: "flex",
            "flex-direction": "column",
            gap: "12px",
            "z-index": "10000",
            "max-width": "min(380px, calc(100vw - 40px))",
            "padding-bottom": "env(safe-area-inset-bottom, 0px)",
            "font-family":
              'Roboto, "Helvetica Neue", -apple-system, BlinkMacSystemFont, system-ui, sans-serif',
          }}
        >
          <For each={pending()}>{(req) => <PairCard req={req} onApprove={approve} onDeny={deny} />}</For>
        </div>
      </Portal>
    </Show>
  );
}

function PairCard(props: {
  req: PairRequest;
  onApprove: (id: string) => void;
  onDeny: (id: string) => void;
}) {
  return (
    <div
      data-testid="pair-request-card"
      data-ephemeral-id={props.req.ephemeral_id}
      style={{
        background: "var(--bg-elev-2)",
        color: "var(--text-hi)",
        "border-radius": "var(--md-shape-md)",
        padding: "16px",
        "box-shadow": "var(--md-elev-3)",
        border: "1px solid var(--border-strong)",
        animation: "pair-card-in 220ms cubic-bezier(0.2, 0, 0, 1)",
        display: "flex",
        "flex-direction": "column",
        gap: "12px",
      }}
    >
      <div style={{ display: "flex", "align-items": "center", gap: "12px" }}>
        <div
          aria-hidden="true"
          style={{
            width: "40px",
            height: "40px",
            "border-radius": "20px",
            background: "var(--color-accent-tonal)",
            color: "var(--color-accent)",
            display: "flex",
            "align-items": "center",
            "justify-content": "center",
            "font-size": "20px",
            "flex-shrink": "0",
          }}
        >
          {/* M3-ish device icon */}
          <svg width="22" height="22" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
            <path d="M4 6h16v10H4zM2 18h20v2H2z" />
          </svg>
        </div>
        <div style={{ "min-width": "0", flex: "1" }}>
          <div style={{ "font-size": "var(--md-title-s-size)", "font-weight": 500, "line-height": "20px" }}>
            New browser wants to pair
          </div>
          <div
            style={{
              "font-size": "var(--md-body-s-size)",
              "line-height": "16px",
              color: "var(--text-lo)",
              overflow: "hidden",
              "text-overflow": "ellipsis",
              "white-space": "nowrap",
            }}
            title={props.req.label}
          >
            {props.req.label || props.req.ephemeral_id}
          </div>
        </div>
      </div>
      <div
        style={{
          "font-size": "var(--md-body-s-size)",
          color: "var(--text-lo)",
          "line-height": "16px",
          "font-family": "ui-monospace, SFMono-Regular, Menlo, monospace",
          "letter-spacing": "0.05em",
        }}
      >
        Code: {props.req.ephemeral_id}
      </div>
      <div
        style={{
          display: "flex",
          "justify-content": "flex-end",
          gap: "8px",
          "margin-top": "-4px",
        }}
      >
        <Button
          variant="text"
          data-testid="pair-card-dismiss"
          onClick={(e) => { e.stopPropagation(); props.onDeny(props.req.ephemeral_id); }}
        >Dismiss</Button>
        <Button
          variant="filled"
          data-testid="pair-card-approve"
          onClick={(e) => { e.stopPropagation(); props.onApprove(props.req.ephemeral_id); }}
        >Approve</Button>
      </div>
    </div>
  );
}
