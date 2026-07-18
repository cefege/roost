// AttentionToasts — portal-mounted top-right stack of clickable attention
// cards (agent went blocked/needs-input, finished, or offline). The whole
// card navigates to that session's terminal. Ephemeral source is
// attentionToastStore; the persistent log stays in notifyStore (bell dropdown).
//
// Hidden while the notification bell dropdown is open: the dropdown already
// lists the same events top-right and would overlap. Generic error/action
// toasts stay bottom-right in ToastContainer — untouched.

import { For, Show } from "solid-js";
import { Portal } from "solid-js/web";
import { useNavigate } from "@solidjs/router";
import { attentionToasts, dismissAttentionToast, type AttentionToast } from "../lib/attentionToastStore.ts";
import { markRead } from "../lib/notifyStore.ts";
import { presentationOf } from "../lib/agentStatus.ts";
import { uiStore } from "../store/uiStore.ts";
import { Icon } from "./Settings/md/primitives.tsx";

const KIND: Record<AttentionToast["kind"], { level: "blocked" | "done"; verb: string; icon: string }> = {
  blocked: { level: "blocked", verb: "Needs your input", icon: "priority_high" },
  done:    { level: "done",    verb: "Finished",         icon: "check_circle" },
  offline: { level: "blocked", verb: "Went offline",     icon: "cloud_off" },
};

export function AttentionToasts() {
  const navigate = useNavigate();

  function jump(t: AttentionToast) {
    navigate(`/s/${t.sessionId}`);
    markRead(t.notifId);
    dismissAttentionToast(t.id);
  }

  return (
    <Show when={!uiStore.notificationBellOpen}>
      <Portal mount={document.body}>
        <style>{`
          @keyframes attention-toast-in { from { opacity:0; transform: translateY(-8px); } to { opacity:1; transform: translateY(0); } }
          @keyframes attention-countdown { from { transform: scaleX(1); } to { transform: scaleX(0); } }
        `}</style>
        <div
          data-testid="attention-toast-container"
          style={{
            position: "fixed",
            top: "calc(20px + env(safe-area-inset-top, 0px))",
            right: "20px",
            display: "flex",
            "flex-direction": "column",
            gap: "8px",
            "z-index": 9999,
            "pointer-events": "none",
            "max-width": "min(400px, calc(100vw - 40px))",
          }}
        >
          <For each={attentionToasts()}>
            {(t) => {
              const color = presentationOf(KIND[t.kind].level).color;
              return (
                <div
                  role="button"
                  tabindex="0"
                  data-testid="attention-toast"
                  data-kind={t.kind}
                  onClick={() => jump(t)}
                  onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); jump(t); } }}
                  onMouseEnter={(e) => { e.currentTarget.style.background = "var(--md-sys-color-surface-container-highest)"; }}
                  onMouseLeave={(e) => { e.currentTarget.style.background = "var(--md-sys-color-surface-container-high)"; }}
                  style={{
                    "pointer-events": "auto",
                    cursor: "pointer",
                    position: "relative",
                    overflow: "hidden",
                    display: "flex",
                    "align-items": "flex-start",
                    gap: "10px",
                    padding: "12px 14px",
                    background: "var(--md-sys-color-surface-container-high)",
                    border: "1px solid var(--md-sys-color-outline-variant)",
                    "border-left": `3px solid ${color}`,
                    "border-radius": "var(--md-shape-md)",
                    "box-shadow": "var(--md-elev-3)",
                    animation: "attention-toast-in var(--md-sys-motion-duration-short4) var(--md-sys-motion-easing-emphasized-decelerate)",
                  }}
                >
                  <span style={{ color, "flex-shrink": 0, "margin-top": "1px" }}>
                    <Icon name={KIND[t.kind].icon} size="sm" filled />
                  </span>
                  <div style={{ flex: 1, "min-width": 0, display: "flex", "flex-direction": "column", gap: "2px" }}>
                    <span style={{
                      "font-weight": 600,
                      "font-size": "13px",
                      color: "var(--md-sys-color-on-surface)",
                      "white-space": "nowrap",
                      overflow: "hidden",
                      "text-overflow": "ellipsis",
                    }}>{t.title}</span>
                    <span style={{ "font-size": "12px", color: "var(--md-sys-color-on-surface-variant)" }}>
                      {KIND[t.kind].verb}
                    </span>
                  </div>
                  <button
                    type="button"
                    aria-label="Dismiss"
                    data-testid="attention-toast-dismiss"
                    onClick={(e) => { e.stopPropagation(); dismissAttentionToast(t.id); }}
                    style={{
                      "flex-shrink": 0,
                      border: "none",
                      background: "transparent",
                      cursor: "pointer",
                      "font-size": "12px",
                      color: "var(--md-sys-color-on-surface-variant)",
                      padding: "2px 4px",
                      "border-radius": "var(--md-shape-xs)",
                      "line-height": "1",
                    }}
                  >✕</button>
                  <Show when={t.ttlMs !== null}>
                    <span
                      aria-hidden="true"
                      style={{
                        position: "absolute",
                        left: 0,
                        right: 0,
                        bottom: 0,
                        height: "2px",
                        background: color,
                        "transform-origin": "left center",
                        animation: `attention-countdown ${t.ttlMs}ms linear forwards`,
                      }}
                    />
                  </Show>
                </div>
              );
            }}
          </For>
        </div>
      </Portal>
    </Show>
  );
}
