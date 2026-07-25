// CustomCard — an extension-injected message (advisor, irc:incoming,
// async-result, hook output). omp frames these with the customType as a bold
// header and the content as markdown below it (message-frame.ts:68-72), falling
// back to exactly that shape whenever the extension registered no renderer —
// which is always, here, since Roost cannot execute an omp renderer. So label +
// markdown IS parity, and the bracketed `[type]` chip is Roost's idiom for the
// same label.
//
// Expanded, not collapsed: omp renders extension messages in full
// (message-frame.ts:6-8). `detailsJson` rides the wire but is not painted — the
// TUI does not paint it either.
//
// Capped bodies recover through the same fetchChatBlock button ThinkingBlock
// uses, and ONLY through it: this card is always visible, so auto-pulling would
// inline every advisor replay in the thread on first paint.

import { createSignal, Show } from "solid-js";
import type { CustomCardBlock } from "@roost/shared/chat/wire";
import { fetchChatBlock } from "../../../store/chatOmp.ts";
import { renderMarkdown } from "./markdown.ts";
import { Button } from "../../Settings/md/Button.tsx";

interface Props {
  block: CustomCardBlock;
  sessionId: string;
  messageId: string;
  blockIndex: number;
  /** Parity-oracle stamp: the JSON TuiRow this element paints (see
   *  @roost/shared/chat/rows). Undefined on elements that anchor no row. */
  dataTuiRow?: string;
}

export function CustomCard(props: Props) {
  const [full, setFull] = createSignal<string | undefined>(undefined);
  const [loading, setLoading] = createSignal(false);

  const loadFull = async () => {
    if (loading() || full() !== undefined) return;
    setLoading(true);
    const t = await fetchChatBlock(props.sessionId, props.messageId, props.blockIndex);
    setFull(t ?? props.block.text);
    setLoading(false);
  };

  return (
    <div class="tr-custom" data-testid="omp-chat-custom" data-custom-type={props.block.customType}
      data-tui-row={props.dataTuiRow}>
      <span class="tr-chip tr-custom-label">[{props.block.customType}]</span>
      <div class="tr-md" innerHTML={renderMarkdown(full() ?? props.block.text)} />
      <Show when={props.block.truncated && full() === undefined}>
        <Button variant="text" class="tr-more" data-testid="omp-chat-custom-more"
          disabled={loading()} onClick={() => void loadFull()}>
          show full {props.block.fullLen} chars
        </Button>
      </Show>
    </div>
  );
}
