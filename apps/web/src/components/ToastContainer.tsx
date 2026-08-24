// ToastContainer — portal-mounted at bottom-right; renders the toast stack.
// Reads toasts signal from store/toastStore. Auto-dismissal handled by the
// store. Text inside each toast is selectable (user-select: text +
// pointer-events: auto on the card) so the user can copy error output.
// Error toasts persist until manually dismissed; ok/warn auto-dismiss.

import { For, Show, createSignal, onMount, onCleanup } from "solid-js";
import { Portal } from "solid-js/web";
import { toasts, dismissToast, type ToastKind } from "../store/toastStore.ts";
import { copyToClipboard } from "../lib/clipboard.ts";
import { createTrackedTimeouts } from "./trackedTimeout.ts";

const KIND_ACCENT: Record<ToastKind, string> = {
  ok: "var(--color-ok)",
  warn: "var(--color-warn)",
  err: "var(--color-err)",
};

const KIND_STYLES: Record<ToastKind, { border: string; color: string }> = {
  ok:   { border: "1px solid var(--md-sys-color-outline-variant)", color: "var(--color-ok)" },
  warn: { border: "1px solid var(--color-warn)", color: "var(--color-warn)" },
  err:  { border: "1px solid var(--color-err)", color: "var(--color-err)" },
};

export function ToastContainer() {
  const setTimeoutTracked = createTrackedTimeouts();
  let containerRef: HTMLDivElement | undefined;

  // Publish the toast stack height as a CSS var so PairRequestNotifier can
  // offset above it instead of overlapping the same bottom-right corner.
  onMount(() => {
    if (!containerRef) return;
    const ro = new ResizeObserver((entries) => {
      const h = entries[0]?.contentRect.height ?? 0;
      document.documentElement.style.setProperty("--toast-stack-height", `${h}px`);
    });
    ro.observe(containerRef);
    onCleanup(() => {
      ro.disconnect();
      document.documentElement.style.setProperty("--toast-stack-height", "0px");
    });
  });

  return (
    <Portal mount={document.body}>
      <style>{`
        @keyframes toast-countdown { from { transform: scaleX(1); } to { transform: scaleX(0); } }
      `}</style>
      <div
        ref={containerRef}
        data-testid="toast-container"
        style={{
          position: "fixed",
          bottom: "20px",
          right: "20px",
          display: "flex",
          "flex-direction": "column",
          gap: "8px",
          "z-index": "9999",
          "pointer-events": "none",
          "max-width": "min(560px, calc(100vw - 40px))",
          "padding-bottom": "env(safe-area-inset-bottom, 0px)",
        }}
      >
        <For each={toasts()}>
          {(toast) => {
            const s = KIND_STYLES[toast.kind];
            const fullText = () => toast.details ? `${toast.msg}\n${toast.details}` : toast.msg;
            const [copied, setCopied] = createSignal(false);
            async function copy(e: MouseEvent) {
              e.stopPropagation();
              // Denial leaves the label unchanged — the text stays selectable
              // in the card, so the user can still copy it by hand.
              if (!(await copyToClipboard(fullText()))) return;
              setCopied(true);
              setTimeoutTracked(() => setCopied(false), 1500);
            }
            return (
              <div
                data-testid="toast"
                data-kind={toast.kind}
                style={{
                  padding: "10px 12px",
                  background: "var(--md-sys-color-surface-container-high)",
                  border: s.border,
                  "border-radius": "var(--md-shape-md)",
                  "font-size": "13px",
                  color: s.color,
                  "box-shadow": "var(--md-elev-3)",
                  animation: "toast-in var(--md-sys-motion-duration-short4) var(--md-sys-motion-easing-emphasized-decelerate)",
                  "pointer-events": "auto",
                  "user-select": "text",
                  position: "relative",
                  overflow: "hidden",
                  display: "flex",
                  "flex-direction": "column",
                  gap: "6px",
                  // Wrap so long error lines don't overflow viewport.
                  "white-space": "pre-wrap",
                  "word-break": "break-word",
                }}
              >
                <div style={{ display: "flex", "align-items": "flex-start", gap: "8px" }}>
                  <span
                    style={{
                      flex: "1",
                      "user-select": "text",
                      // ponytail: clamp the headline to 4 lines so a giant error
                      // string can't grow the card into a wall of text — Copy
                      // grabs the full message, details go in the <pre> below.
                      display: "-webkit-box",
                      "-webkit-line-clamp": "4",
                      "-webkit-box-orient": "vertical",
                      overflow: "hidden",
                    }}
                  >{toast.msg}</span>
                  <Show when={toast.action}>
                    <ToastButton
                      label={toast.action!.label}
                      title={toast.action!.label}
                      onClick={(e) => { e.stopPropagation(); toast.action!.onClick(); }}
                    />
                  </Show>
                  <ToastButton
                    label={copied() ? "Copied" : "Copy"}
                    title={copied() ? "Copied to clipboard" : "Copy full message"}
                    onClick={copy}
                  />
                  <ToastButton
                    label="✕"
                    title="Dismiss"
                    onClick={(e) => { e.stopPropagation(); dismissToast(toast.id); }}
                  />
                </div>
                <Show when={toast.details}>
                  <pre
                    data-testid="toast-details"
                    style={{
                      margin: "0",
                      padding: "6px 8px",
                      background: "rgba(0,0,0,0.25)",
                      "border-radius": "4px",
                      "font-family": "var(--term-font-family, ui-monospace, Menlo, monospace)",
                      "font-size": "11.5px",
                      color: "var(--text-mid)",
                      "white-space": "pre-wrap",
                      "word-break": "break-word",
                      "max-height": "240px",
                      overflow: "auto",
                      "user-select": "text",
                    }}
                  >{toast.details}</pre>
                </Show>
                {/* M3 snackbar countdown — one bar per toast, shrinks 1→0 over
                    its own ttlMs so each card visibly owns its timer. Omitted
                    for persistent (ttlMs:null) toasts. */}
                <Show when={toast.ttlMs !== null}>
                  <span
                    aria-hidden="true"
                    style={{
                      position: "absolute",
                      left: 0, right: 0, bottom: 0,
                      height: "2px",
                      background: KIND_ACCENT[toast.kind],
                      "transform-origin": "left center",
                      animation: `toast-countdown ${toast.ttlMs}ms linear forwards`,
                    }}
                  />
                </Show>
              </div>
            );
          }}
        </For>
      </div>
    </Portal>
  );
}

function ToastButton(props: { label: string; title: string; onClick: (e: MouseEvent) => void }) {
  return (
    <button
      type="button"
      onClick={props.onClick}
      title={props.title}
      style={{
        padding: "2px 8px",
        background: "transparent",
        border: "1px solid var(--md-sys-color-outline-variant)",
        "border-radius": "var(--md-shape-xs)",
        color: "inherit",
        font: "inherit",
        "font-size": "11px",
        cursor: "pointer",
        "flex-shrink": "0",
        opacity: "0.85",
      }}
    >{props.label}</button>
  );
}
