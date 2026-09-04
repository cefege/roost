// FileViewerSheet — full-screen modal showing a file from a worker.
// Opens when the current route is /file/:workerFp/*path (via URL params).
// Fetches via coordClient.filesRead({ workerFp, path }) → { data, size }.
// Decodes bytes to text if UTF-8 parseable, else shows "binary" notice.
// Syntax highlighting via syntaxLite.ts (JS-ish regex tokenizer).
// #L<n> fragment support: scrolls to line on mount; "copy link" per line.

import { Show, createSignal, createEffect, createMemo, onCleanup, For, Index } from "solid-js";
import { useLocation, useNavigate, useParams } from "@solidjs/router";
import { coordClient } from "../connect.ts";
import { tokenizeLines, shouldHighlight, extFromPath, type Token } from "../lib/syntaxLite.ts";
import "../styles/syntax-vars.css";
import { decodeWorkerPathRoute } from "../lib/nativePath.ts";
import { copyToClipboard } from "../lib/clipboard.ts";
import { createTrackedTimeouts } from "./trackedTimeout.ts";
import { rootStore } from "../store/root.ts";
import { workersHydrated } from "../store/sync-bootstrap.ts";
import {
  captureDashboardResourceToken,
  isCurrentDashboardResourceToken,
} from "../store/dashboard-selection.ts";
import { Button } from "./Settings/md/Button.tsx";
import { EmptyState } from "./Settings/md/EmptyState.tsx";

function parseLineFromHash(): number {
  const hash = typeof location !== "undefined" ? location.hash : "";
  const m = hash.match(/^#L(\d+)$/);
  return m ? parseInt(m[1]!, 10) : 1;
}

function lineUrl(lineNum: number): string {
  const url = new URL(location.href);
  url.hash = `L${lineNum}`;
  return url.toString();
}

const KIND_COLORS: Record<Token["kind"], string> = {
  keyword: "var(--syntax-keyword)",
  string:  "var(--syntax-string)",
  comment: "var(--syntax-comment)",
  number:  "var(--syntax-number)",
  plain:   "var(--syntax-plain)",
};
function HighlightedLine(props: { tokens: Token[] }) {
  return (
    <span style={{ "white-space": "pre" }}>
      <For each={props.tokens}>
        {(tok) => (
          <span style={{ color: KIND_COLORS[tok.kind] }}>{tok.text}</span>
        )}
      </For>
    </span>
  );
}
function PlainLine(props: { text: string }) {
  return (
    <span style={{ color: "var(--syntax-plain)", "white-space": "pre" }}>
      {props.text}
    </span>
  );
}
export function FileViewerSheet() {
  const params = useParams<{ workerFp?: string; path?: string }>();
  const route = useLocation();
  const navigate = useNavigate();

  const workerFp = createMemo(() => params.workerFp ?? "");
  const scopedWorker = createMemo(() => !rootStore.browser_unauthorized && !!rootStore.workers[workerFp()]);
  const scopePending = createMemo(() => !scopedWorker() && !workersHydrated() && !rootStore.browser_unauthorized);
  const scopeUnavailable = createMemo(() => !scopedWorker() && (workersHydrated() || rootStore.browser_unauthorized));
  const filePath = createMemo(() => {
    const fp = workerFp();
    const encoded = route.pathname.match(/^\/file\/[^/]+\/(.+)$/)?.[1];
    if (!fp || !encoded) return "";
    try { return decodeWorkerPathRoute(fp, encoded); }
    catch { return ""; }
  });
  const targetLine = createMemo(() => parseLineFromHash());

  const [lines, setLines] = createSignal<string[]>([]);
  const [tokenGrid, setTokenGrid] = createSignal<Token[][] | null>(null);
  const [isBinary, setIsBinary] = createSignal(false);
  const [byteSize, setByteSize] = createSignal(0);
  const [loading, setLoading] = createSignal(false);
  const [fetchError, setFetchError] = createSignal<string | null>(null);
  const [copiedLine, setCopiedLine] = createSignal<{ line: number; ok: boolean } | null>(null);
  let scrollRef: HTMLDivElement | undefined;
  let unavailableRef: HTMLDivElement | undefined;
  // Superseded route reads and replies after disposal cannot publish.
  // Dashboard generation separately fences a scope cutover at the same path.
  let fetchToken = 0;
  onCleanup(() => { fetchToken++; });
  const setTimeoutTracked = createTrackedTimeouts();

  createEffect(() => {
    const fp = workerFp();
    const path = filePath();
    const scoped = scopedWorker();
    const mine = ++fetchToken;
    if (!fp || !path || !scoped) {
      setLines([]);
      setTokenGrid(null);
      setIsBinary(false);
      setByteSize(0);
      setLoading(false);
      setFetchError(null);
      setCopiedLine(null);
      return;
    }
    const dashboardToken = captureDashboardResourceToken();
    setLoading(true);
    setFetchError(null);
    setCopiedLine(null);
    coordClient.filesRead({ workerFp: fp, path })
      .then((result) => {
        if (mine !== fetchToken || !isCurrentDashboardResourceToken(dashboardToken)) return;
        setByteSize(Number(result.size));
        const text = (() => {
          try { return new TextDecoder("utf-8", { fatal: true }).decode(result.data); }
          catch { return null; }
        })();
        if (text === null) {
          setIsBinary(true);
          setLines([]);
          setTokenGrid(null);
        } else {
          setIsBinary(false);
          const split = text.split("\n");
          setLines(split);
          const ext = extFromPath(path, fp);
          setTokenGrid(shouldHighlight(ext) ? tokenizeLines(split) : null);
        }
        setLoading(false);
        requestAnimationFrame(() => {
          if (mine !== fetchToken || !isCurrentDashboardResourceToken(dashboardToken)) return;
          const tl = targetLine();
          const el = scrollRef?.querySelector<HTMLElement>(`[data-line="${tl}"]`);
          el?.scrollIntoView({ block: "center" });
        });
      })
      .catch((e: unknown) => {
        if (mine !== fetchToken || !isCurrentDashboardResourceToken(dashboardToken)) return;
        const msg = e instanceof Error ? e.message : String(e);
        setFetchError(msg);
        setLoading(false);
      });
  });

  function copyLineLink(lineNum: number) {
    const mine = fetchToken;
    const dashboardToken = captureDashboardResourceToken();
    void copyToClipboard(lineUrl(lineNum)).then((ok) => {
      if (mine !== fetchToken || !isCurrentDashboardResourceToken(dashboardToken)) return;
      setCopiedLine({ line: lineNum, ok });
      setTimeoutTracked(() => {
        if (mine === fetchToken && isCurrentDashboardResourceToken(dashboardToken)) {
          setCopiedLine(null);
        }
      }, 1500);
    });
  }

  createEffect(() => {
    if (!scopeUnavailable()) return;
    const frame = requestAnimationFrame(() => unavailableRef?.focus());
    onCleanup(() => cancelAnimationFrame(frame));
  });

  const hasTarget = createMemo(() => !!workerFp() && !!filePath());
  const tl = createMemo(() => targetLine());

  return (
    <Show when={hasTarget()}>
      <div
        style={{
          position: "fixed",
          inset: "0",
          background: "color-mix(in srgb, var(--md-scrim) 70%, transparent)",
          display: "flex",
          "align-items": "center",
          "justify-content": "center",
          "z-index": "50",
        }}
      >
        <div
          data-testid="file-viewer-sheet"
          style={{
            background: "var(--bg-base)",
            border: "1px solid var(--border-strong)",
            "box-sizing": "border-box",
            "border-radius": "8px",
            padding: "20px",
            width: "700px",
            "max-width": "94vw",
            "max-height": "80vh",
            display: "flex",
            "flex-direction": "column",
            gap: "10px",
            overflow: "hidden",
          }}
        >
          <div style={{ display: "flex", "align-items": "center", gap: "10px" }}>
            <span
              data-testid="file-viewer-sheet-title"
              style={{
                color: "var(--text-lo)",
                flex: "1",
                overflow: "hidden",
                "text-overflow": "ellipsis",
                "white-space": "nowrap",
                "font-size": "var(--md-body-s-size)",
                "font-family": "ui-monospace, monospace",
              }}
            >
              {filePath()}
            </span>
            <span
              style={{
                "font-size": "10px",
                color: "var(--text-lo)",
                "font-family": "ui-monospace, monospace",
                "white-space": "nowrap",
              }}
            >
              {workerFp().slice(0, 8)}
            </span>
            <Show when={scopedWorker() && byteSize() > 0}>
              <span
                data-testid="file-viewer-sheet-size"
                style={{ "font-size": "10px", color: "var(--text-lo)", "font-family": "ui-monospace, monospace" }}
              >
                {byteSize()} B
              </span>
            </Show>
          </div>
          <Show when={scopePending() || (scopedWorker() && loading())}>
            <span style={{ color: "var(--text-lo)", "font-size": "var(--md-body-s-size)" }}>
              Loading file…
            </span>
          </Show>
          <Show when={scopeUnavailable()}>
            <div
              ref={unavailableRef}
              data-testid="file-viewer-unavailable"
              role="status"
              aria-live="polite"
              aria-label="File unavailable. This file isn't available in the current dashboard."
              aria-atomic="true"
              tabIndex={-1}
              style={{ flex: "1", "min-height": "0", "overflow-y": "auto" }}
            >
              <EmptyState
                icon="draft"
                title="File unavailable"
                supporting="This file isn't available in the current dashboard."
                action={
                  <Button variant="tonal" data-testid="file-viewer-unavailable-home"
                    onFocus={(event) => event.currentTarget.scrollIntoView({ block: "center" })}
                    onClick={() => navigate("/", { replace: true })}>
                    Go home
                  </Button>
                }
              />
            </div>
          </Show>
          <Show when={scopedWorker() && fetchError()}>
            <span
              data-testid="file-viewer-sheet-error"
              style={{ color: "var(--color-err)", "font-size": "var(--md-body-s-size)" }}
            >
              {fetchError()}
            </span>
          </Show>
          <Show when={scopedWorker() && isBinary()}>
            <span
              data-testid="file-viewer-sheet-binary"
              style={{ color: "var(--color-warn)", "font-size": "var(--md-body-s-size)" }}
            >
              binary file ({byteSize()} bytes) — not renderable as text
            </span>
          </Show>
          <Show when={scopedWorker() && lines().length > 0}>
            <div
              ref={scrollRef}
              data-testid="file-viewer-sheet-body"
              style={{
                "overflow-y": "auto",
                "font-family": "ui-monospace, monospace",
                "font-size": "var(--md-body-s-size)",
                flex: "1",
                background: "var(--bg-base)",
                "border-radius": "4px",
                padding: "8px",
                position: "relative",
              }}
            >
              <div
                data-testid="file-viewer-sheet-target-marker"
                style={{
                  position: "absolute",
                  left: "0",
                  width: "3px",
                  top: `${(tl() - 1) * 20}px`,
                  height: "20px",
                  background: "var(--color-warn)",
                }}
              />
              <Index each={lines()}>
                {(lineText, idx) => {
                  const lineNum = idx + 1;
                  const isTarget = lineNum === tl();
                  const toks = createMemo(() => tokenGrid()?.[idx] ?? null);
                  return (
                    <div
                      data-line={lineNum}
                      data-testid={`file-viewer-sheet-line-${lineNum}`}
                      style={{
                        display: "flex",
                        gap: "12px",
                        "line-height": "20px",
                        background: isTarget ? "color-mix(in srgb, var(--ansi-bright-yellow) 10%, transparent)" : undefined,
                        position: "relative",
                      }}
                      class="fvs-line"
                    >
                      <span
                        data-testid={`file-viewer-sheet-line-num-${lineNum}`}
                        data-target={isTarget ? "true" : undefined}
                        style={{
                          color: "var(--text-lo)",
                          "min-width": "36px",
                          "text-align": "right",
                          "user-select": "none",
                          "flex-shrink": "0",
                        }}
                      >
                        {lineNum}
                      </span>
                      <Show
                        when={toks()}
                        fallback={<PlainLine text={lineText()} />}
                      >
                        {(t) => <HighlightedLine tokens={t()} />}
                      </Show>
                      <button
                        data-testid={`file-viewer-sheet-copy-link-${lineNum}`}
                        title={`Copy link to line ${lineNum}`}
                        onClick={() => copyLineLink(lineNum)}
                        style={{
                          "margin-left": "auto",
                          "flex-shrink": "0",
                          background: "none",
                          border: "none",
                          cursor: "pointer",
                          color: copiedLine()?.line === lineNum
                            ? (copiedLine()!.ok ? "var(--color-ok)" : "var(--color-err)")
                            : "var(--text-lo)",
                          "font-size": "10px",
                          "padding": "0 4px",
                          opacity: "0",
                        }}
                        class="fvs-copy-btn"
                        aria-label={`Copy link to line ${lineNum}`}
                      >
                        {copiedLine()?.line === lineNum ? (copiedLine()!.ok ? "✓" : "✕") : "#"}
                      </button>
                    </div>
                  );
                }}
              </Index>
            </div>
          </Show>
        </div>
      </div>
      <style>{`
        .fvs-line:hover .fvs-copy-btn { opacity: 1 !important; }
      `}</style>
    </Show>
  );
}
