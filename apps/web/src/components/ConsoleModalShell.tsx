// Shared docked-bottom-right live-console shell. TransferConsoleModal is the
// only consumer today; the shell stays separate so a second live console gets
// identical chrome (spinner header, collapse-to-header, Copy, auto-scrolling
// monospace <pre>) instead of a divergent copy. Each caller owns its own stream
// subscription + header text; this owns only the presentation.

import { createSignal, onMount, onCleanup, Show, For, createEffect } from "solid-js";
import { Portal } from "solid-js/web";
import { Button } from "./Settings/md/primitives.tsx";
import { animateOverlayDock } from "../lib/overlayMotion.ts";
import { isPageVisible } from "../lib/pageVisible.ts";
import { copyToClipboard } from "../lib/clipboard.ts";

export interface ConsoleDone {
  exit: number | null;
  error?: string;
}

interface Props {
  testId: string;
  width: string;
  maxHeight: string;
  waitingText: string;
  runningHint: string;
  lines: () => string[];
  done: () => ConsoleDone | null;
  headerText: () => string;
  onClose: () => void;
}

export function ConsoleModalShell(props: Props) {
  const [collapsed, setCollapsed] = createSignal(false);
  let scrollRef: HTMLPreElement | undefined;

  // Auto-scroll to bottom on each new line — but only while the user is parked
  // there. Sampled in onScroll (pre-append geometry); by the time the effect
  // runs the new line is already in the DOM and the at-bottom read is off by a
  // line. The programmatic pin below fires its own scroll event, which re-arms
  // stick.
  let stick = true;
  const onScroll = () => {
    const el = scrollRef;
    if (el) stick = el.scrollHeight - el.scrollTop - el.clientHeight <= 32;
  };
  createEffect(() => {
    props.lines();
    if (stick && scrollRef) scrollRef.scrollTop = scrollRef.scrollHeight;
  });

  const headerColor = () => {
    const d = props.done();
    if (!d) return "var(--color-info)";
    if (d.exit === 0 && !d.error) return "var(--color-ok)";
    return "var(--color-err)";
  };

  async function copyAll(e: MouseEvent) {
    e.stopPropagation();
    // Denial drops silently — the log text remains selectable in the pane.
    await copyToClipboard(props.lines().join("\n"));
  }

  return (
    <Portal mount={document.body}>
      <div
        ref={animateOverlayDock}
        data-testid={props.testId}
        style={{
          position: "fixed",
          right: "20px",
          bottom: "20px",
          width: props.width,
          "max-height": collapsed() ? "auto" : props.maxHeight,
          background: "var(--bg-elev-2)",
          border: "1px solid var(--border-strong)",
          "border-radius": "var(--md-shape-sm)",
          "z-index": "9998",
          display: "flex",
          "flex-direction": "column",
          "box-shadow": "var(--md-elev-4)",
        }}
      >
        <div
          style={{
            display: "flex",
            "align-items": "center",
            gap: "8px",
            padding: "10px 12px",
            "border-bottom": collapsed() ? "none" : "1px solid var(--border-strong)",
            cursor: "pointer",
          }}
          onClick={() => setCollapsed((v) => !v)}
          title={collapsed() ? "Expand console" : "Collapse to header"}
        >
          <Show when={!props.done()}>
            <Spinner />
          </Show>
          <span style={{ flex: "1", color: headerColor(), "font-weight": "600", "font-size": "var(--md-body-s-size)", "user-select": "text" }}>
            {props.headerText()}
          </span>
          <Button variant="text" onClick={copyAll} title="Copy full output to clipboard">Copy</Button>
          <Button
            variant="text"
            onClick={(e) => { e.stopPropagation(); props.onClose(); }}
            title={props.done() ? "Close" : `Hide (${props.runningHint})`}
          >{props.done() ? "✕" : "Hide"}</Button>
        </div>
        <Show when={!collapsed()}>
          <pre
            ref={scrollRef}
            data-testid={`${props.testId}-output`}
            onScroll={onScroll}
            style={{
              margin: "0",
              padding: "12px",
              background: "var(--bg-app)",
              color: "var(--term-color-7)",
              "font-family": "var(--term-font-family, ui-monospace, Menlo, monospace)",
              "font-size": "11.5px",
              "line-height": "1.45",
              "white-space": "pre-wrap",
              "word-break": "break-word",
              overflow: "auto",
              flex: "1",
              "user-select": "text",
              "border-radius": "0 0 8px 8px",
            }}
          >
            <Show when={props.lines().length === 0 && !props.done()}>
              <span style={{ color: "var(--text-mid)" }}>{props.waitingText}</span>
            </Show>
            <For each={props.lines()}>{(line) => <>{line}{"\n"}</>}</For>
          </pre>
        </Show>
      </div>
    </Portal>
  );
}

function Spinner() {
  const [idx, setIdx] = createSignal(0);
  const frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
  onMount(() => {
    // Hidden-tab gate: don't spin at 11fps for a tab nobody can see.
    const h = setInterval(() => { if (isPageVisible()) setIdx((i) => (i + 1) % frames.length); }, 90);
    onCleanup(() => clearInterval(h));
  });
  return (
    <span aria-hidden="true" style={{ color: "var(--color-info)", "font-size": "var(--md-body-m-size)", "font-family": "monospace" }}>
      {frames[idx()]}
    </span>
  );
}
