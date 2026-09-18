// ToastCard — one toast surface inside the notification dock. Owns the kind
// presentation (StatusDot plus a countdown tint), the compact two-row action
// layout, the copy affordance for error and detail text, and the hover/focus
// hold that freezes auto-dismiss and rings the toast's target session.
// Rendered by ToastStack.tsx; state and dismissal come from store/toastStore.ts,
// target highlighting from store/notifyTarget.ts.

import { Show, createSignal, onCleanup } from "solid-js";
import { Button, IconButton, StatusDot, Surface } from "./Settings/md/primitives.tsx";
import {
  dismissToast,
  holdToastDismiss,
  releaseToastDismiss,
  type Toast,
  type ToastKind,
} from "../store/toastStore.ts";
import { holdNotifyTarget, releaseNotifyTarget } from "../store/notifyTarget.ts";
import { copyToClipboard } from "../lib/clipboard.ts";
import { createTrackedTimeouts } from "./trackedTimeout.ts";
import { isCompact } from "../lib/windowSizeClass.ts";
import { hoverCardAvailable } from "./PaneTabHoverCard.tsx";
import { prefersReducedMotion } from "../lib/prefersReducedMotion.ts";

const TOAST_STATUS: Record<ToastKind, "ok" | "warn" | "error"> = {
  ok: "ok",
  warn: "warn",
  err: "error",
};

const TOAST_ACCENT: Record<ToastKind, string> = {
  ok: "var(--status-ok)",
  warn: "var(--status-warn)",
  err: "var(--md-sys-color-error)",
};

export function ToastCard(props: { toast: Toast }) {
  const setTimeoutTracked = createTrackedTimeouts();
  const [copied, setCopied] = createSignal(false);
  const [dismissHeld, setDismissHeld] = createSignal(false);
  // Captured at body time: a Toast row is never mutated after addToast, and
  // reading props.* inside onCleanup is not allowed.
  const toastId = props.toast.id;

  function holdToast(): void {
    // hoverCardAvailable() is this repo's single owner of "this device really
    // hovers". A tap on a touch device synthesizes mouseenter with no matching
    // mouseleave, which would freeze the dismissal and pin the target ring
    // until the user hit ✕; width alone does not catch a touch laptop or TV.
    if (!hoverCardAvailable()) return;
    holdToastDismiss(toastId);
    setDismissHeld(true);
    const sessionId = props.toast.targetSessionId;
    if (sessionId) holdNotifyTarget(toastId, sessionId);
  }

  function releaseToast(): void {
    releaseToastDismiss(toastId);
    setDismissHeld(false);
    releaseNotifyTarget(toastId);
  }

  onCleanup(() => { releaseNotifyTarget(toastId); });

  async function copy(event: MouseEvent) {
    event.stopPropagation();
    const text = props.toast.details
      ? `${props.toast.msg}\n${props.toast.details}`
      : props.toast.msg;
    // Denial leaves the label unchanged — the card text stays selectable.
    if (!(await copyToClipboard(text))) return;
    setCopied(true);
    setTimeoutTracked(() => setCopied(false), 1500);
  }

  const actionRow = () => (
    <>
      <Show when={props.toast.action}>
        <Button
          variant="ghost"
          size={isCompact() ? "sm" : "xs"}
          title={props.toast.action!.label}
          onClick={(event) => { event.stopPropagation(); props.toast.action!.onClick(); }}
          style={{ "flex-shrink": "0" }}
        >
          {props.toast.action!.label}
        </Button>
      </Show>
      {/* Copy is noise on a three-word success line; it earns its slot only
          where there is output worth keeping. */}
      <Show when={props.toast.kind === "err" || Boolean(props.toast.details)}>
        <Button
          variant="ghost"
          size={isCompact() ? "sm" : "xs"}
          title={copied() ? "Copied to clipboard" : "Copy full message"}
          onClick={copy}
          style={{ "flex-shrink": "0" }}
        >
          {copied() ? "Copied" : "Copy"}
        </Button>
      </Show>
      <IconButton
        icon="close"
        label="Dismiss"
        title="Dismiss"
        size={isCompact() ? "icon-sm" : "icon-xs"}
        onClick={(event) => { event.stopPropagation(); dismissToast(props.toast.id); }}
      />
    </>
  );

  return (
    <div
      data-testid="toast"
      data-kind={props.toast.kind}
      class="roost-toast-slot"
      data-compact={isCompact() ? "true" : "false"}
      data-dismiss-held={dismissHeld() ? "true" : undefined}
      style={{ "user-select": "text" }}
      onMouseEnter={holdToast}
      onMouseLeave={releaseToast}
      onFocusIn={holdToast}
      onFocusOut={releaseToast}
    >
      <Surface
        level={2}
        elevation={3}
        radius="md"
        border
        class="roost-toast"
        role={props.toast.kind === "err" ? "alert" : "status"}
        aria-live={props.toast.kind === "err" ? "assertive" : "polite"}
        aria-atomic="true"
        style={{
          display: "flex",
          "flex-direction": "column",
          gap: "var(--md-space-2)",
          padding: "var(--md-space-3)",
          color: "var(--md-sys-color-on-surface)",
          "white-space": "pre-wrap",
          "word-break": "break-word",
          ...(prefersReducedMotion() ? {} : {
            animation: "roost-toast-in var(--md-sys-motion-duration-short4) var(--md-sys-motion-easing-emphasized-decelerate)",
          }),
        }}
      >
        <div style={{ display: "flex", "align-items": "flex-start", gap: "var(--md-space-2)" }}>
          <StatusDot status={TOAST_STATUS[props.toast.kind]} />
          <span
            class="md-body-m"
            style={{
              flex: "1",
              "min-width": 0,
              "user-select": "text",
              // Clamp the headline so a giant error cannot turn the card into a
              // wall; Copy still yields the full message.
              display: "-webkit-box",
              "-webkit-line-clamp": "4",
              "-webkit-box-orient": "vertical",
              overflow: "hidden",
            }}
          >
            {props.toast.msg}
          </span>
          <Show when={!isCompact()}>{actionRow()}</Show>
        </div>
        <Show when={props.toast.details}>
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
            {props.toast.details}
          </pre>
        </Show>
        {/* Coarse pointers promote every roost-button to 44px
            (md/controls.css), so three of them will not share a row with the
            message on a phone — they get their own row. */}
        <Show when={isCompact()}>
          <div style={{ display: "flex", "justify-content": "flex-end", "align-items": "center", gap: "var(--md-space-2)" }}>
            {actionRow()}
          </div>
        </Show>
        <Show when={props.toast.ttlMs !== null && !prefersReducedMotion()}>
          <span
            aria-hidden="true"
            style={{
              position: "absolute",
              inset: "auto 0 0",
              height: "var(--workbench-border-width)",
              background: TOAST_ACCENT[props.toast.kind],
              "transform-origin": "left center",
              animation: `roost-toast-countdown ${props.toast.ttlMs}ms linear forwards`,
              // Inline, beside the shorthand that sets the duration: the
              // shorthand resets play-state at inline specificity, so a
              // stylesheet rule could never pause the bar. Same signal as the
              // JS freeze, so the bar cannot outlive or outrun the timer.
              "animation-play-state": dismissHeld() ? "paused" : "running",
            }}
          />
        </Show>
      </Surface>
    </div>
  );
}
