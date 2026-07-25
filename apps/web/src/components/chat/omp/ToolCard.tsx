// ToolCard — renders a tool call + ALL its matched result blocks INLINE and
// visible by default (no click needed to see what happened). Header: name +
// one-line arg summary + status; click to collapse. Body: the primary arg
// payload (write/edit/bash) as code, then each result block (a tool can emit
// several — text interleaved with images), then images. A truncated result
// fetches its untruncated text via SessionsGetChatBlock on "show full".

import { createSignal, Show, For, createMemo } from "solid-js";
import type { ToolCallBlock, ToolEventBlock, ImageBlock } from "@roost/shared/chat/wire";
import type { ResultRef } from "./renderPlan.ts";
import { fetchChatBlock } from "../../../store/chatOmp.ts";
import { parseArgs, toolSummary, toolPayload } from "./toolView.ts";
import { ChatImage } from "./ChatImage.tsx";

interface Props {
  sessionId: string;
  call?: ToolCallBlock | null;
  results?: ResultRef[];
  event?: ToolEventBlock | null;
  images?: ImageBlock[];
}

/** Result rows past this fold themselves away once the tool finishes: a read of
 *  a 400-line file must not bury the sentence the agent wrote after it. */
const LONG_RESULT_LINES = 12;

export function ToolCard(props: Props) {
  // null = follow the default; set once the user clicks. A RUNNING tool always
  // stays open — its live output is the point. A finished one folds itself away
  // when it is long enough to bury the conversation, and says how much it hid.
  const [override, setOverride] = createSignal<boolean | null>(null);

  const name = () => props.call?.name ?? props.results?.[0]?.block.name ?? props.event?.name ?? "tool";
  const results = () => props.results ?? [];
  // "update" is still in flight — only the final result or an end event stops it.
  const running = () => results().length === 0 && (!props.event || props.event.phase !== "end");
  // Live output omp streams while the tool runs. Superseded by the real result.
  const liveOutput = () => (results().length === 0 ? props.event?.output ?? "" : "");
  const isError = () => results().some((r) => r.block.isError);
  const args = createMemo(() => parseArgs(props.call?.argsJson ?? ""));
  const summary = () => (props.call ? toolSummary(name(), args()) : props.event?.intent ?? "");
  const payload = createMemo(() => (props.call ? toolPayload(name(), args()) : null));

  const resultLines = () => results().reduce((n, r) => n + r.block.text.split("\n").length, 0);
  const bulky = () => !running() && resultLines() > LONG_RESULT_LINES;
  const collapsed = () => override() ?? bulky();

  return (
    <div class="omp-tool" classList={{ "omp-tool--error": isError(), "omp-tool--running": running() }} data-testid="omp-chat-tool">
      <button type="button" class="omp-tool__head" aria-expanded={!collapsed()} onClick={() => setOverride(!collapsed())}>
        <span class="omp-tool__chevron">{collapsed() ? "▸" : "▾"}</span>
        <span class="omp-tool__name">{name()}</span>
        <Show when={summary()}><span class="omp-tool__summary">{summary()}</span></Show>
        <span class="omp-tool__status">
          <Show when={running()}>running</Show>
          <Show when={isError()}>error</Show>
          <Show when={collapsed() && !isError()}>{resultLines()} lines</Show>
        </span>
      </button>

      <Show when={!collapsed()}>
        <div class="omp-tool__body">
          <Show when={payload()}>
            <pre class="omp-tool__code" data-lang={payload()!.lang}><code>{payload()!.text}</code></pre>
          </Show>
          <Show when={liveOutput()}>
            <pre class="omp-tool__result omp-tool__result--live" data-testid="omp-tool-live"><code>{liveOutput()}</code></pre>
          </Show>
          <For each={results()}>
            {(r) => <ToolResultView sessionId={props.sessionId} r={r} />}
          </For>
          <Show when={props.images && props.images.length > 0}>
            <div class="omp-tool__images">
              <For each={props.images}>
                {(im) => <ChatImage sessionId={props.sessionId} blobPath={im.blobPath} mime={im.mime} />}
              </For>
            </div>
          </Show>
        </div>
      </Show>
    </div>
  );
}

/** One result block: shows (capped) text; "show full" fetches the untruncated
 *  text from the block's own message/index. */
function ToolResultView(props: { sessionId: string; r: ResultRef }) {
  const [full, setFull] = createSignal<string | null>(null);
  const [loading, setLoading] = createSignal(false);
  const text = () => full() ?? props.r.block.text;

  const loadFull = async () => {
    if (loading() || full() !== null) return;
    setLoading(true);
    const t = await fetchChatBlock(props.sessionId, props.r.msgId, props.r.blockIndex);
    setFull(t ?? props.r.block.text);
    setLoading(false);
  };

  return (
    <Show when={text().length > 0}>
      <pre class="omp-tool__result" classList={{ "omp-tool__result--error": props.r.block.isError }}><code>{text()}</code></pre>
      <Show when={loading()}><span class="omp-tool__loading">loading full…</span></Show>
      <Show when={props.r.block.truncated && full() === null}>
        <button class="omp-tool__more" onClick={() => void loadFull()}>
          show full {props.r.block.fullLen.toLocaleString()} chars
        </button>
      </Show>
    </Show>
  );
}
