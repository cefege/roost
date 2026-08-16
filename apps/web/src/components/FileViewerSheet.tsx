// FileViewerSheet — full-screen modal showing a file from a worker.
// Opens when the current route is /file/:workerFp/*path (via URL params).
// Calls trpc.files.read.query({ worker_fp, path }) → { content_b64, size }.
// Decodes base64 to text if UTF-8 parseable, else shows "binary" notice.
// Syntax highlighting via syntaxLite.ts (JS-ish regex tokenizer).
// #L<n> fragment support: scrolls to line on mount; "copy link" per line.

import { Show, createSignal, createEffect, createMemo, For, Index } from "solid-js";
import { useLocation, useParams } from "@solidjs/router";
import { coordClient } from "../connect.ts";
import { tokenizeLines, shouldHighlight, extFromPath, type Token } from "../lib/syntaxLite.ts";
import "../styles/syntax-vars.css";
import { decodeWorkerPathRoute } from "../lib/nativePath.ts";

// ─── helpers ─────────────────────────────────────────────────────────────

function parseLineFromHash(): number {
  // Support #L42 fragment in the URL (e.g. /file/fp/src/foo.ts#L42).
  const hash = typeof location !== "undefined" ? location.hash : "";
  const m = hash.match(/^#L(\d+)$/);
  return m ? parseInt(m[1]!, 10) : 1;
}

// Attempt UTF-8 decode of base64 content. Returns null if bytes are not valid
// UTF-8 (signals binary content to the renderer).
function decodeBase64ToText(b64: string): string | null {
  try {
    const bin = atob(b64);
    const bytes = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    return null;
  }
}

// Build a URL with #L<n> appended, replacing any existing hash.
function lineUrl(lineNum: number): string {
  const url = new URL(location.href);
  url.hash = `L${lineNum}`;
  return url.toString();
}

// ─── sub-components ───────────────────────────────────────────────────────

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

// ─── component ───────────────────────────────────────────────────────────

export function FileViewerSheet() {
  const params = useParams<{ workerFp?: string; path?: string }>();
  const route = useLocation();

  const workerFp = createMemo(() => params.workerFp ?? "");
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
  const [copiedLine, setCopiedLine] = createSignal<number | null>(null);

  // Ref to the scroll container — used to scrollIntoView after load.
  let scrollRef: HTMLDivElement | undefined;

  createEffect(() => {
    const fp = workerFp();
    const path = filePath();
    if (!fp || !path) {
      setLines([]);
      setTokenGrid(null);
      setIsBinary(false);
      setFetchError(null);
      return;
    }
    setLoading(true);
    setFetchError(null);
    coordClient.filesRead({ workerFp: fp, path })
      .then((result) => {
        setByteSize(Number(result.size));
        // Connect returns raw bytes; decode UTF-8 via TextDecoder
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
        // After DOM settles, scroll to target line.
        requestAnimationFrame(() => {
          const tl = targetLine();
          const el = scrollRef?.querySelector<HTMLElement>(`[data-line="${tl}"]`);
          el?.scrollIntoView({ block: "center" });
        });
      })
      .catch((e: unknown) => {
        const msg = e instanceof Error ? e.message : String(e);
        setFetchError(msg);
        setLoading(false);
      });
  });

  function copyLineLink(lineNum: number) {
    navigator.clipboard.writeText(lineUrl(lineNum)).catch(() => {});
    setCopiedLine(lineNum);
    setTimeout(() => setCopiedLine(null), 1500);
  }

  // Only render when we have route params to show.
  const hasTarget = createMemo(() => !!workerFp() && !!filePath());

  // 1-based target line.
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
          {/* Header */}
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
            <Show when={byteSize() > 0}>
              <span
                data-testid="file-viewer-sheet-size"
                style={{ "font-size": "10px", color: "var(--text-lo)", "font-family": "ui-monospace, monospace" }}
              >
                {byteSize()} B
              </span>
            </Show>
          </div>

          {/* Loading */}
          <Show when={loading()}>
            <span style={{ color: "var(--text-lo)", "font-size": "var(--md-body-s-size)" }}>Loading…</span>
          </Show>

          {/* Error */}
          <Show when={fetchError()}>
            <span
              data-testid="file-viewer-sheet-error"
              style={{ color: "var(--color-err)", "font-size": "var(--md-body-s-size)" }}
            >
              {fetchError()}
            </span>
          </Show>

          {/* Binary notice */}
          <Show when={isBinary()}>
            <span
              data-testid="file-viewer-sheet-binary"
              style={{ color: "var(--color-warn)", "font-size": "var(--md-body-s-size)" }}
            >
              binary file ({byteSize()} bytes) — not renderable as text
            </span>
          </Show>

          {/* Line listing */}
          <Show when={lines().length > 0}>
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
              {/* Target-line marker bar (1-based) */}
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
                        // Show copy button on hover via group pattern.
                        position: "relative",
                      }}
                      class="fvs-line"
                    >
                      {/* Line number */}
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

                      {/* Source text (highlighted or plain) */}
                      <Show
                        when={toks()}
                        fallback={<PlainLine text={lineText()} />}
                      >
                        {(t) => <HighlightedLine tokens={t()} />}
                      </Show>

                      {/* Copy-line-as-link button */}
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
                          color: copiedLine() === lineNum ? "var(--color-ok)" : "var(--text-lo)",
                          "font-size": "10px",
                          "padding": "0 4px",
                          opacity: "0",
                          // Revealed by .fvs-line:hover via injected style below.
                        }}
                        class="fvs-copy-btn"
                        aria-label={`Copy link to line ${lineNum}`}
                      >
                        {copiedLine() === lineNum ? "✓" : "#"}
                      </button>
                    </div>
                  );
                }}
              </Index>
            </div>
          </Show>
        </div>
      </div>

      {/* Inline style for hover reveal — avoids a separate CSS file import. */}
      <style>{`
        .fvs-line:hover .fvs-copy-btn { opacity: 1 !important; }
      `}</style>
    </Show>
  );
}
