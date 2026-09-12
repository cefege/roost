// AppErrorBoundary — outermost error fence wrapping the Router tree.
// On render-time crash: shows error message + "Copy diagnostic" + "Reload".
// Callers: App.tsx (outermost wrapper).
// Depends on: @roost/shared/log for warn on clipboard failure.

import { ErrorBoundary, onCleanup } from "solid-js";
import type { Component, JSX } from "solid-js";
import { log } from "@roost/shared/log";
import { copyToClipboard } from "../lib/clipboard.ts";
import { credentialFreeUrl } from "../auth/fragment-credential.ts";
import { Button, Icon, Surface } from "./Settings/md/primitives.tsx";
interface Props {
  children: JSX.Element;
}

function ErrorFallback(err: unknown, reset: () => void): JSX.Element {
  const msg = err instanceof Error ? err.message : String(err);
  const stack = err instanceof Error ? (err.stack ?? "") : "";
  console.error("[error-boundary] caught", err);
  (window as Window & { __lastBoundaryErr?: unknown }).__lastBoundaryErr = err;

  // Auto-reset on the next navigation. The fallback is shown because a
  // Solid cleanup re-entered during route transition (cleanNode iterating
  // owned[] saw it nulled by a deep child cleanup). The error is in the
  // OLD tree; the new route is fine. Watch popstate so any nav recovers
  // without forcing the user to hit Reload. The handler unsubscribes itself
  // via onCleanup when the fallback unmounts (on reset).
  const onPop = (): void => {
    setTimeout(reset, 0);
  };
  window.addEventListener("popstate", onPop);
  onCleanup(() => window.removeEventListener("popstate", onPop));

  async function copyDiagnostic(): Promise<void> {
    const payload = {
      url: typeof window !== "undefined" ? credentialFreeUrl(window.location) : "",
      ua: typeof navigator !== "undefined" ? navigator.userAgent : "",
      time: new Date().toISOString(),
      error: msg,
      stack,
    };
    // Failure is logged, never surfaced — the fallback already prints the
    // error, and the payload stays in __lastBoundaryErr for tooling.
    if (!(await copyToClipboard(JSON.stringify(payload, null, 2)))) {
      log.warn("AppErrorBoundary", "copy_diagnostic_failed", {});
    }
  }

  return (
    <div
      data-testid="error-boundary"
      style={{
        "min-height": "100dvh",
        "min-width": "100vw",
        display: "grid",
        "place-items": "center",
        padding: "var(--md-space-6)",
        background: "var(--surface-0)",
        color: "var(--md-sys-color-on-surface)",
      }}
    >
      <Surface
        level={1}
        elevation={3}
        radius="md"
        pad={6}
        border
        style={{
          width: "min(100%, 64ch)",
          display: "flex",
          "flex-direction": "column",
          gap: "var(--md-space-4)",
        }}
      >
        <div style={{ display: "flex", "align-items": "center", gap: "var(--md-space-2)" }}>
          <Icon name="error" style={{ color: "var(--md-sys-color-error)" }} />
          <h1 class="md-title-m" style={{ margin: 0, color: "var(--md-sys-color-error)" }}>
            Unexpected error
          </h1>
        </div>
        <p
          class="md-body-s"
          style={{
            margin: 0,
            "font-family": "var(--font-mono)",
            "word-break": "break-all",
            "white-space": "pre-wrap",
            "user-select": "text",
            color: "var(--md-sys-color-on-surface-variant)",
          }}
        >
          {msg}
        </p>
        <div style={{ display: "flex", gap: "var(--md-space-2)", "flex-wrap": "wrap" }}>
          <Button
            variant="outline"
            size="sm"
            onClick={() => void copyDiagnostic()}
          >
            Copy diagnostic
          </Button>
          <Button
            variant="default"
            size="sm"
            onClick={() => {
              reset();
              window.location.reload();
            }}
          >
            Reload
          </Button>
        </div>
      </Surface>
    </div>
  );
}

export const AppErrorBoundary: Component<Props> = (props) => {
  return (
    <ErrorBoundary fallback={ErrorFallback}>
      {props.children}
    </ErrorBoundary>
  );
};
