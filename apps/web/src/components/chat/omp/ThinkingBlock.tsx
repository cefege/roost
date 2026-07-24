// ThinkingBlock — collapsible assistant reasoning. Truncated blocks lazy-load
// full text on expand via fetchChatBlock (sessionsGetChatBlock RPC).

import { createSignal, Show } from "solid-js";
import type { ThinkingBlock as ThinkingData } from "@roost/shared/chat/wire";
import { fetchChatBlock } from "../../../store/chatOmp.ts";

interface Props {
  sessionId: string;
  messageId: string;
  blockIndex: number;
  data: ThinkingData;
}

export function ThinkingBlock(props: Props) {
  const [open, setOpen] = createSignal(false);
  const [full, setFull] = createSignal<string | null>(null);
  const [loading, setLoading] = createSignal(false);

  const text = () => full() ?? props.data.text;
  const showMore = () => props.data.truncated && full() === null;

  const onToggle = async () => {
    setOpen((v) => !v);
    if (!open() || full() !== null) return;
    if (!props.data.truncated) return;
    setLoading(true);
    const t = await fetchChatBlock(props.sessionId, props.messageId, props.blockIndex);
    setFull(t ?? props.data.text);
    setLoading(false);
  };

  const loadFull = async (e: Event) => {
    e.preventDefault();
    e.stopPropagation();
    setLoading(true);
    const t = await fetchChatBlock(props.sessionId, props.messageId, props.blockIndex);
    setFull(t ?? props.data.text);
    setLoading(false);
  };

  return (
    <details class="omp-thinking" open={open()} data-testid="omp-chat-thinking">
      <summary onClick={(e) => { e.preventDefault(); void onToggle(); }}>
        {open() ? "▾" : "▸"} thinking{showMore() ? ` (${props.data.fullLen} chars)` : ""}
      </summary>
      <Show when={open()}>
        <div class="omp-thinking__body">{text()}{loading() ? "…" : ""}</div>
        <Show when={showMore()}>
          <button class="omp-thinking__more" onClick={(e) => void loadFull(e)}>
            show full {props.data.fullLen} chars
          </button>
        </Show>
      </Show>
    </details>
  );
}
