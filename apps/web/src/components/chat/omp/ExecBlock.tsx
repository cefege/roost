// ExecBlock — a `!cmd` bash run or a `!py` eval executed from omp's composer
// (BashExecutionComponent / EvalExecutionComponent, chat-transcript-builder.ts:
// 251-262). Header is the prompt the shell would have shown (`$` / `>>>`), body
// is the captured output verbatim in a <pre> — never markdown, this is terminal
// bytes — and the trailing chip carries the outcome the TUI prints beside it.
//
// `excluded` is the `!!` form, whose output omp deliberately keeps OUT of the
// model's context; the dim `!!` chip is what tells the reader that.
//
// A capped body recovers through the same fetchChatBlock button ThinkingBlock
// uses, on press only — the output is always visible, so auto-pulling would
// inline every long build log in the thread on first paint.

import { createSignal, Show } from "solid-js";
import type { ExecBlock as ExecBlockData } from "@roost/shared/chat/wire";
import { fetchChatBlock } from "../../../store/chatOmp.ts";
import { Button } from "../../Settings/md/Button.tsx";

interface Props {
  block: ExecBlockData;
  sessionId: string;
  messageId: string;
  blockIndex: number;
  /** Parity-oracle stamp: the JSON TuiRow this element paints (see
   *  @roost/shared/chat/rows). Undefined on elements that anchor no row. */
  dataTuiRow?: string;
}

export function ExecBlock(props: Props) {
  const [full, setFull] = createSignal<string | undefined>(undefined);
  const [loading, setLoading] = createSignal(false);

  const loadFull = async () => {
    if (loading() || full() !== undefined) return;
    setLoading(true);
    const t = await fetchChatBlock(props.sessionId, props.messageId, props.blockIndex);
    setFull(t ?? props.block.output);
    setLoading(false);
  };

  // Two uses each, and both must stay lazy so a growing block repaints.
  const status = () => props.block.cancelled ? "cancelled"
    : props.block.exitCode === 0 ? "" : `exit ${props.block.exitCode}`;
  const output = () => full() ?? props.block.output;

  return (
    <div class="tr-exec" data-testid="omp-chat-exec" data-lang={props.block.lang}
      data-tui-row={props.dataTuiRow}>
      <div class="tr-exec-head">
        <code class="tr-exec-cmd">{props.block.lang === "bash" ? "$" : ">>>"} {props.block.command}</code>
        <Show when={props.block.excluded}>
          <span class="tr-chip tr-exec-excluded" title="output kept out of the model's context">!!</span>
        </Show>
        <Show when={status()}>
          <span class="tr-chip tr-exec-status" data-cancelled={String(props.block.cancelled)}>{status()}</span>
        </Show>
      </div>
      <Show when={output()}>
        <pre class="tr-exec-out">{output()}</pre>
      </Show>
      <Show when={props.block.truncated && full() === undefined}>
        <Button variant="text" class="tr-more" data-testid="omp-chat-exec-more"
          disabled={loading()} onClick={() => void loadFull()}>
          show full {props.block.fullLen} chars
        </Button>
      </Show>
    </div>
  );
}
