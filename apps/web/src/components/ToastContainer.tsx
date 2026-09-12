// ToastContainer — portal-mounted at bottom-right; renders the toast stack.
// Reads toasts signal from store/toastStore. Auto-dismissal handled by the
// store. Text inside each toast is selectable (user-select: text +
// pointer-events: auto on the card) so the user can copy error output.
// Error toasts persist until manually dismissed; ok/warn auto-dismiss.

import { For, Show, createSignal, onMount, onCleanup } from "solid-js";
import { Portal } from "solid-js/web";
import { Button, IconButton, StatusDot, Surface } from "./Settings/md/primitives.tsx";
import { toasts, dismissToast } from "../store/toastStore.ts";
import type { ToastKind } from "../store/toastStore.ts";
import { copyToClipboard } from "../lib/clipboard.ts";
import { createTrackedTimeouts } from "./trackedTimeout.ts";

const TOAST_PRESENTATION: Record<
  ToastKind,
  { accent: string; border: string; status: "ok" | "warn" | "error" }
> = {
  ok: {
    accent: "var(--status-ok)",
    border: "var(--workbench-border-width) solid var(--md-sys-color-outline-variant)",
    status: "ok",
  },
  warn: {
    accent: "var(--status-warn)",
    border: "var(--workbench-border-width) solid var(--status-warn)",
    status: "warn",
  },
  err: {
    accent: "var(--md-sys-color-error)",
    border: "var(--workbench-border-width) solid var(--md-sys-color-error)",
    status: "error",
  },
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
          bottom: "var(--md-space-5)",
          right: "var(--md-space-5)",
          display: "flex",
          "flex-direction": "column",
          gap: "var(--md-space-2)",
          "z-index": "9999",
          "pointer-events": "none",
          "max-width": "min(70ch, calc(100vw - var(--md-space-9)))",
          "padding-bottom": "env(safe-area-inset-bottom, 0px)",
        }}
      >
        <For each={toasts()}>
          {(toast) => {
            const presentation = TOAST_PRESENTATION[toast.kind];
            const [copied, setCopied] = createSignal(false);
            async function copy(e: MouseEvent) {
              e.stopPropagation();
              // Denial leaves the label unchanged — the text stays selectable
              // in the card, so the user can still copy it by hand.
              if (!(await copyToClipboard(toast.details ? `${toast.msg}\n${toast.details}` : toast.msg))) return;
              setCopied(true);
              setTimeoutTracked(() => setCopied(false), 1500);
            }
            return (
              <div
                data-testid="toast"
                data-kind={toast.kind}
                style={{
                  animation: "toast-in var(--md-sys-motion-duration-short4) var(--md-sys-motion-easing-emphasized-decelerate)",
                  "pointer-events": "auto",
                  "user-select": "text",
                }}
              >
                <Surface
                  level={2}
                  elevation={3}
                  radius="md"
                  style={{
                    position: "relative",
                    display: "flex",
                    "flex-direction": "column",
                    gap: "var(--md-space-2)",
                    padding: "var(--md-space-3)",
                    border: presentation.border,
                    color: "var(--md-sys-color-on-surface)",
                    overflow: "hidden",
                    "white-space": "pre-wrap",
                    "word-break": "break-word",
                  }}
                >
                  <div style={{ display: "flex", "align-items": "flex-start", gap: "var(--md-space-2)" }}>
                    <StatusDot status={presentation.status} />
                    <span
                      class="md-body-m"
                      style={{
                        flex: "1",
                        "min-width": 0,
                        "user-select": "text",
                        // Clamp the headline so a giant error cannot turn the
                        // card into a wall; Copy retains the full message.
                        display: "-webkit-box",
                        "-webkit-line-clamp": "4",
                        "-webkit-box-orient": "vertical",
                        overflow: "hidden",
                      }}
                    >
                      {toast.msg}
                    </span>
                    <Show when={toast.action}>
                      <Button
                        type="button"
                        variant="ghost"
                        size="xs"
                        title={toast.action!.label}
                        onClick={(e) => { e.stopPropagation(); toast.action!.onClick(); }}
                        style={{ "flex-shrink": "0" }}
                      >
                        {toast.action!.label}
                      </Button>
                    </Show>
                    <Button
                      type="button"
                      variant="ghost"
                      size="xs"
                      title={copied() ? "Copied to clipboard" : "Copy full message"}
                      onClick={copy}
                      style={{ "flex-shrink": "0" }}
                    >
                      {copied() ? "Copied" : "Copy"}
                    </Button>
                    <IconButton
                      icon="close"
                      label="Dismiss"
                      title="Dismiss"
                      size="icon-xs"
                      onClick={(e) => { e.stopPropagation(); dismissToast(toast.id); }}
                    />
                  </div>
                  <Show when={toast.details}>
                    <pre
                      data-testid="toast-details"
                      class="md-body-s"
                      style={{
                        margin: "0",
                        padding: "var(--md-space-2)",
                        background: "var(--md-sys-color-surface-container-highest)",
                        "border-radius": "var(--md-shape-xs)",
                        "font-family": "var(--term-font-family)",
                        color: "var(--md-sys-color-on-surface-variant)",
                        "white-space": "pre-wrap",
                        "word-break": "break-word",
                        "max-height": "calc(var(--md-space-9) * 5)",
                        overflow: "auto",
                        "user-select": "text",
                      }}
                    >
                      {toast.details}
                    </pre>
                  </Show>
                  {/* M3 snackbar countdown — one bar per toast, shrinks 1→0 over
                      its own ttlMs so each card visibly owns its timer. Omitted
                      for persistent (ttlMs:null) toasts. */}
                  <Show when={toast.ttlMs !== null}>
                    <span
                      aria-hidden="true"
                      style={{
                        position: "absolute",
                        inset: "auto 0 0",
                        height: "var(--workbench-border-width)",
                        background: presentation.accent,
                        "transform-origin": "left center",
                        animation: `toast-countdown ${toast.ttlMs}ms linear forwards`,
                      }}
                    />
                  </Show>
                </Surface>
              </div>
            );
          }}
        </For>
      </div>
    </Portal>
  );
}
