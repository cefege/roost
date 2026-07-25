// ThinkingBlock — one contiguous run of assistant reasoning, in omp's HTML-export
// idiom (template.js:1116-1119): collapsed to a single dim italic `Thinking …`
// line, expanding to the full passage. No per-block label, no chevron — omp's
// terminal prints reasoning unlabeled and its export collapses it to that one
// literal. Open state is local OR the pane-level toggle.
//
// Truncated parts lazy-load their full text via fetchChatBlock
// (sessionsGetChatBlock RPC) — omp has no equivalent, so the fetch is Roost's
// and survives per part.

import { createSignal, createEffect, For, Show } from "solid-js";
import { fetchChatBlock } from "../../../store/chatOmp.ts";
import type { ThinkingPart } from "./thinkingText.ts";
import { Button } from "../../Settings/md/Button.tsx";

interface Props {
  sessionId: string;
  messageId: string;
  parts: ThinkingPart[];
  expandAll: boolean;
  /** Parity-oracle stamp: the JSON TuiRow this element paints (see
   *  @roost/shared/chat/rows). Undefined on elements that anchor no row. */
  dataTuiRow?: string;
}

export function ThinkingBlock(props: Props) {
  const [localOpen, setLocalOpen] = createSignal(false);
  // Full text per part, keyed by the part's block index within the message.
  const [full, setFull] = createSignal<Record<number, string>>({});
  const [loading, setLoading] = createSignal<Record<number, boolean>>({});

  const open = () => props.expandAll || localOpen();

  const loadFull = async (part: ThinkingPart) => {
    if (loading()[part.index] || full()[part.index] !== undefined) return;
    setLoading((m) => ({ ...m, [part.index]: true }));
    const t = await fetchChatBlock(props.sessionId, props.messageId, part.index);
    setFull((m) => ({ ...m, [part.index]: t ?? part.block.text }));
    setLoading((m) => ({ ...m, [part.index]: false }));
  };

  // Fetch on open regardless of HOW it opened: the pane-level toggle never runs
  // the click handler, so hanging the fetch off onClick would leave every
  // expand-all run showing capped text.
  createEffect(() => {
    if (!open()) return;
    for (const part of props.parts) if (part.block.truncated) void loadFull(part);
  });

  return (
    <div class="tr-think" data-testid="omp-chat-thinking" data-open={String(open())} data-tui-row={props.dataTuiRow}>
      <button type="button" class="tr-think-collapsed" aria-expanded={open()}
        onClick={() => setLocalOpen((v) => !v)}>Thinking …</button>
      <Show when={open()}>
        <div class="tr-think-text">
          <For each={props.parts}>
            {(part) => {
              const text = () => full()[part.index] ?? part.block.text;
              const showMore = () => part.block.truncated && full()[part.index] === undefined;
              return (
                <div class="tr-think-part">
                  {text()}{loading()[part.index] ? "…" : ""}
                  <Show when={showMore()}>
                    <Button variant="text" class="tr-think-more" data-testid="omp-chat-thinking-more"
                      onClick={() => void loadFull(part)}>
                      show full {part.block.fullLen} chars
                    </Button>
                  </Show>
                </div>
              );
            }}
          </For>
        </div>
      </Show>
    </div>
  );
}
