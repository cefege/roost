// OmpChatPane — the omp chat thread. Renders bubbles + inline tool cards, using
// the shared renderPlan routing so nothing the transcript holds is dropped:
// tool results/events/images fold into their matching call's card (by callId);
// call-less results render as their own card. Backfills history on first enter;
// stick-to-bottom scroll.

import { createMemo, createEffect, For, Show, onMount } from "solid-js";
import type { ChatMessage, ContentBlock } from "@roost/shared/chat/wire";
import { ompChatForSession, backfillOmpChat } from "../../../store/chatOmp.ts";
import { buildToolIndex, type ToolMatch } from "./renderPlan.ts";
import { MessageBubble } from "./MessageBubble.tsx";
import { ThinkingBlock } from "./ThinkingBlock.tsx";
import { ToolCard } from "./ToolCard.tsx";
import { Composer } from "./Composer.tsx";
import { ApprovalCard } from "./ApprovalCard.tsx";
import "./omp-chat.css";

interface Props {
  sessionId: string;
}

export function OmpChatPane(props: Props) {
  let threadEl: HTMLDivElement | undefined;

  const state = createMemo(() => ompChatForSession(props.sessionId));

  // Backfill on first enter (status !== resolved). Idempotent.
  onMount(() => { void backfillOmpChat(props.sessionId); });

  // Per-callId index across ALL messages (a toolResult is a separate entry).
  const toolIndex = createMemo(() => buildToolIndex(state().messages));

  // Stick-to-bottom: scroll to end on append unless the user scrolled up.
  let stick = true;
  const onScroll = () => {
    if (!threadEl) return;
    stick = threadEl.scrollHeight - threadEl.scrollTop - threadEl.clientHeight < 80;
  };
  createEffect(() => {
    const n = state().messages.length;
    if (stick && threadEl) queueMicrotask(() => { threadEl?.scrollTo({ top: threadEl.scrollHeight }); });
    void n;
  });

  return (
    <div class="omp-chat" data-testid="omp-chat-pane" data-session-id={props.sessionId}>
      <div class="omp-chat__thread" ref={threadEl} onScroll={onScroll}>
        <Show when={state().status !== "loading"} fallback={<div class="omp-chat__skeleton">Loading chat…</div>}>
          <Show when={state().messages.length > 0} fallback={<div class="omp-chat__skeleton">No messages yet</div>}>
            <For each={state().messages}>
              {(msg) => <MessageRow msg={msg} sessionId={props.sessionId} toolIndex={toolIndex()} />}
            </For>
          </Show>
        </Show>
        <Show when={state().streaming}>
          <div class="omp-chat__thinking" data-testid="omp-chat-thinking">π thinking…</div>
        </Show>
      </div>
      <Composer sessionId={props.sessionId} />
    </div>
  );
}

function MessageRow(props: { msg: ChatMessage; sessionId: string; toolIndex: Map<string, ToolMatch> }) {
  return (
    <For each={props.msg.blocks}>
      {(b, i) => <BlockView block={b} msg={props.msg} blockIndex={i()} sessionId={props.sessionId} toolIndex={props.toolIndex} />}
    </For>
  );
}

function BlockView(props: { block: ContentBlock; msg: ChatMessage; blockIndex: number; sessionId: string; toolIndex: Map<string, ToolMatch> }) {
  const block = props.block;
  switch (block.kind) {
    case "text":
    case "image":
      // Inline images live on non-toolResult messages; toolResult images fold
      // into the tool card (rendered below), so skip them here.
      if (block.kind === "image" && props.msg.role === "toolResult") return null;
      return <MessageBubble msg={{ ...props.msg, blocks: [block] }} />;
    case "thinking":
      return <ThinkingBlock sessionId={props.sessionId} messageId={props.msg.id} blockIndex={props.blockIndex} data={block} />;
    case "toolCall": {
      const m = props.toolIndex.get(block.callId);
      return (
        <ToolCard
          sessionId={props.sessionId}
          call={block}
          results={m?.results}
          event={m?.event}
          images={m?.images}
        />
      );
    }
    case "toolResult": {
      // Rendered at the matching toolCall site. If there is NO call (orphan
      // result), render ONE card here (on the first result block of the callId)
      // holding all of that call's results, so the output is never lost.
      const m = props.toolIndex.get(block.callId);
      if (m?.call) return null;
      const first = m?.results[0];
      if (!first || first.msgId !== props.msg.id || first.blockIndex !== props.blockIndex) return null;
      return (
        <ToolCard sessionId={props.sessionId} results={m!.results} event={m?.event} images={m?.images} />
      );
    }
    case "toolEvent": {
      // Drives the matching card's running chip. Orphan event (no call, no
      // result) → a standalone running card so it isn't lost.
      const m = props.toolIndex.get(block.callId);
      if (m?.call || (m && m.results.length > 0)) return null;
      return <ToolCard sessionId={props.sessionId} event={block} images={m?.images} />;
    }
    case "approval":
      // Native RPC chat only — the mirror engine never produces these.
      return <ApprovalCard sessionId={props.sessionId} block={block} />;
  }
}
