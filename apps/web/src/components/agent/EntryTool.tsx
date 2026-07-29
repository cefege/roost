// One tool call in an agent transcript. Collapsed it is a single chip — tool
// name plus a one-line summary pulled from the call's arguments — because a
// turn can fire a dozen tools and a transcript that expands them all is
// unreadable. Expanded it shows the joined text result and, for `edit`, the
// unified diff omp puts on AgentToolResult.details.
//
// Status colour comes from StatusDot, whose running/ok/error tokens are exactly
// --md-primary / --status-ok (= --md-success) / --status-err (= --md-error).
//
// Caller: components/agent/Transcript.tsx.

import { For, Show, createMemo, createSignal, type Component } from "solid-js";
import { Chip } from "../Settings/md/Chip.tsx";
import { StatusDot } from "../Settings/md/StatusDot.tsx";
import { Surface } from "../Settings/md/Surface.tsx";
import type { AgentToolEntry } from "@roost/shared/wire/agent-entry";

// The argument a human reads first, in priority order. omp's own tools key on
// these names; anything unrecognised falls back to the key list so the chip
// still says something.
const SUMMARY_KEYS = ["command", "path", "file_path", "pattern", "query", "url", "prompt", "message"];
const SUMMARY_MAX = 96;
// A 64 KiB diff is ~1500 lines; painting them all stalls the transcript for a
// result nobody scrolls. Truncate with an honest tail count.
const DIFF_MAX_LINES = 400;

const MONO = {
  "font-size": "var(--md-body-s-size)",
  "line-height": "var(--md-body-s-line)",
} as const;

export const EntryTool: Component<{ entry: AgentToolEntry }> = (props) => {
  const [open, setOpen] = createSignal(false);
  const summary = createMemo(() => summarize(props.entry.args_json));
  const diff = createMemo(() => (props.entry.name === "edit" ? diffOf(props.entry.details_json) : null));

  return (
    <div data-testid="agent-entry-tool" data-seq={props.entry.seq} data-status={props.entry.status}>
      <div style={{ display: "flex", "align-items": "center", gap: "var(--md-space-2)", "min-width": 0 }}>
        <StatusDot status={props.entry.status} title={props.entry.status} />
        <Chip
          label={summary() ? `${props.entry.name} · ${summary()}` : props.entry.name}
          icon={open() ? "expand_less" : "expand_more"}
          onClick={() => setOpen((o) => !o)}
          testId={`agent-tool-chip-${props.entry.tool_call_id}`}
        />
      </div>

      <Show when={open()}>
        <Surface
          level={1}
          radius="sm"
          pad={3}
          border
          style={{ "margin-top": "var(--md-space-2)", "margin-left": "var(--md-space-4)", "min-width": 0 }}
        >
          <Show when={props.entry.intent}>
            <div
              style={{
                color: "var(--md-on-surface-variant)",
                "font-size": "var(--md-label-m-size)",
                "line-height": "var(--md-label-m-line)",
                "margin-bottom": "var(--md-space-2)",
              }}
            >
              {props.entry.intent}
            </div>
          </Show>

          <Show when={props.entry.text}>
            <pre style={{ margin: "0", "white-space": "pre-wrap", "overflow-wrap": "anywhere", color: "var(--md-on-surface)", ...MONO }}>
              {props.entry.text}
            </pre>
          </Show>

          <Show when={diff()}>
            {(text) => (
              <pre data-testid="agent-tool-diff" style={{ margin: "var(--md-space-2) 0 0", overflow: "auto", ...MONO }}>
                <For each={diffLines(text())}>
                  {(line) => <div style={{ color: diffColor(line) }}>{line || " "}</div>}
                </For>
              </pre>
            )}
          </Show>

          <Show when={!props.entry.text && !diff()}>
            <div style={{ color: "var(--md-on-surface-variant)", ...MONO }}>
              {props.entry.status === "running" ? "Running…" : "No output"}
            </div>
          </Show>
        </Surface>
      </Show>
    </div>
  );
};

// args_json is whatever the tool declared, so every branch degrades to "show
// something short" rather than throwing on an unexpected shape.
function summarize(argsJson: string): string {
  if (!argsJson) return "";
  let parsed: unknown;
  try {
    parsed = JSON.parse(argsJson);
  } catch {
    return clip(argsJson);
  }
  if (typeof parsed !== "object" || parsed === null) return clip(String(parsed));
  const args = parsed as Record<string, unknown>;
  for (const key of SUMMARY_KEYS) {
    const v = args[key];
    if (typeof v === "string" && v) return clip(v);
  }
  return clip(Object.keys(args).join(", "));
}

function clip(s: string): string {
  const oneLine = s.replace(/\s+/g, " ").trim();
  return oneLine.length <= SUMMARY_MAX ? oneLine : `${oneLine.slice(0, SUMMARY_MAX - 1)}…`;
}

function diffOf(detailsJson: string): string | null {
  if (!detailsJson) return null;
  try {
    const details: unknown = JSON.parse(detailsJson);
    if (details && typeof details === "object") {
      const d = (details as { diff?: unknown }).diff;
      if (typeof d === "string" && d) return d;
    }
  } catch { /* details is best-effort metadata; a non-JSON blob just has no diff */ }
  return null;
}

function diffLines(text: string): string[] {
  const lines = text.split("\n");
  if (lines.length <= DIFF_MAX_LINES) return lines;
  return [...lines.slice(0, DIFF_MAX_LINES), `… ${lines.length - DIFF_MAX_LINES} more lines`];
}

// Unified-diff colouring on the syntax token set. `---`/`+++` are file headers,
// not content, so they must be tested before the single-character markers.
function diffColor(line: string): string {
  if (line.startsWith("+++") || line.startsWith("---") || line.startsWith("@@")) return "var(--syntax-comment)";
  if (line.startsWith("+")) return "var(--syntax-string)";
  if (line.startsWith("-")) return "var(--md-error)";
  return "var(--syntax-plain)";
}
