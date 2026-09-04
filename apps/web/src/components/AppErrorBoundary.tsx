// AppErrorBoundary — outermost error fence wrapping the Router tree.
// On render-time crash: shows error message + "Copy diagnostic" + "Reload".
// Callers: App.tsx (outermost wrapper).
// Depends on: @roost/shared/log for warn on clipboard failure.

import { type Component, type JSX, ErrorBoundary, onCleanup } from "solid-js";
import { log } from "@roost/shared/log";
import { copyToClipboard } from "../lib/clipboard.ts";
import { credentialFreeUrl } from "../auth/fragment-credential.ts";

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
      class="h-screen w-screen grid place-items-center"
      style={{ background: "var(--bg-base)", color: "var(--text-hi)" }}
    >
      <div
        class="max-w-md w-full rounded-lg p-6 flex flex-col gap-4"
        style={{ background: "var(--surface-1)", border: "1px solid var(--surface-2)", "border-radius": "var(--md-shape-md)" }}
      >
        <h1
          style={{ "font-size": "15px", "font-weight": "600", color: "var(--status-err)" }}
        >
          Unexpected error
        </h1>
        <p
          style={{
            "font-size": "12px",
            "font-family": "monospace",
            "word-break": "break-all",
            color: "var(--text-lo)",
          }}
        >
          {msg}
        </p>
        <div style={{ display: "flex", gap: "8px" }}>
          <button
            type="button"
            onClick={() => void copyDiagnostic()}
            style={{
              "font-size": "13px",
              padding: "6px 12px",
              "border-radius": "var(--md-shape-sm)",
              background: "var(--surface-3)",
              color: "var(--text-hi)",
              border: "none",
              cursor: "pointer",
            }}
          >
            Copy diagnostic
          </button>
          <button
            type="button"
            onClick={() => {
              reset();
              window.location.reload();
            }}
            style={{
              "font-size": "13px",
              padding: "6px 12px",
              "border-radius": "var(--md-shape-sm)",
              background: "var(--status-info)",
              color: "var(--bg-base)",
              border: "none",
              cursor: "pointer",
            }}
          >
            Reload
          </button>
        </div>
      </div>
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
