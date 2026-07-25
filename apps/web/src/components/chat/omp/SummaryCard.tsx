// SummaryCard — a compaction rollup or a returned-from-branch digest, collapsed
// to the one line omp paints for it (export/html/template.js:1184-1186 and
// compaction-summary-message.ts:118): `[compaction] Compacted from N tokens`,
// opening to the full markdown summary. Branch summaries collapse to
// `[branch] Branch summary` (compaction-summary-message.ts:176).
//
// Collapsed by default because omp's own card is: a compaction digest is a page
// of prose that would otherwise sit between two real turns.
//
// Capped bodies lazy-load through fetchChatBlock — the same affordance
// ThinkingBlock uses, so this pane has exactly ONE way to recover capped text.

import { createEffect, createSignal, Show } from "solid-js";
import type { SummaryBlock } from "@roost/shared/chat/wire";
import { fetchChatBlock } from "../../../store/chatOmp.ts";
import { renderMarkdown } from "./markdown.ts";
import { Button } from "../../Settings/md/Button.tsx";

interface Props {
  block: SummaryBlock;
  sessionId: string;
  messageId: string;
  blockIndex: number;
}

export function SummaryCard(props: Props) {
  const [open, setOpen] = createSignal(false);
  const [full, setFull] = createSignal<string | undefined>(undefined);
  const [loading, setLoading] = createSignal(false);

  const loadFull = async () => {
    if (loading() || full() !== undefined) return;
    setLoading(true);
    const t = await fetchChatBlock(props.sessionId, props.messageId, props.blockIndex);
    setFull(t ?? props.block.text);
    setLoading(false);
  };
  // Fetch on OPEN, not on the button's click: a reader who expands the card
  // never presses the button, and would otherwise read the capped body.
  createEffect(() => { if (open() && props.block.truncated) void loadFull(); });

  return (
    <details class="tr-summary" data-testid="omp-chat-summary" data-variant={props.block.variant}
      onToggle={(e) => setOpen(e.currentTarget.open)}>
      <summary class="tr-summary-head">
        {props.block.variant === "compaction"
          ? `[compaction] Compacted from ${props.block.tokensBefore.toLocaleString()} tokens`
          : "[branch] Branch summary"}
      </summary>
      <div class="tr-md" innerHTML={renderMarkdown(full() ?? props.block.text)} />
      <Show when={props.block.truncated && full() === undefined}>
        <Button variant="text" class="tr-more" data-testid="omp-chat-summary-more"
          disabled={loading()} onClick={() => void loadFull()}>
          show full {props.block.fullLen} chars
        </Button>
      </Show>
    </details>
  );
}
