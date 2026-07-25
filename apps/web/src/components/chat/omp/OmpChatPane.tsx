// OmpChatPane — the omp chat thread. Renders bubbles + inline tool cards, using
// the shared renderPlan routing so nothing the transcript holds is dropped:
// tool results/events/images fold into their matching call's card (by callId);
// call-less results render as their own card. Backfills history on first enter;
// stick-to-bottom scroll.

import { createMemo, createEffect, createSignal, untrack, For, Index, Switch, Match, Show, onMount, onCleanup } from "solid-js";
import type { ChatMessage, ContentBlock } from "@roost/shared/chat/wire";
import { ompChatForSession, backfillOmpChat } from "../../../store/chatOmp.ts";
import { rootStore } from "../../../store/root.ts";
import { enqueueAttachmentTo } from "../../../lib/attachments.ts";
import { buildToolIndex, type ToolMatch } from "./renderPlan.ts";
import { ProseBlock } from "./ProseBlock.tsx";
import { ThinkingBlock } from "./ThinkingBlock.tsx";
import { canonicalizeThinking, groupThinking } from "./thinkingText.ts";
import { ToolCard } from "./ToolCard.tsx";
import { Composer, type Pending } from "./Composer.tsx";
import { ChatWelcome } from "./ChatWelcome.tsx";
import { ApprovalCard } from "./ApprovalCard.tsx";
import { Icon } from "../../Settings/md/Icon.tsx";
import "@material/web/progress/linear-progress.js";
// Cascade order is load-bearing: token aliases → pane shell → transcript rows →
// machinery → composer. Do not reorder.
import "./styles/omp-tokens.css";
import "./styles/chat-pane.css";
import "./styles/chat-message.css";
import "./styles/chat-tool.css";
import "./styles/chat-composer.css";

interface Props {
  sessionId: string;
  /** True only for the pane the user is on — the deck keeps every open session
   *  mounted. Gates the composer's autofocus. */
  focused?: boolean;
}

export function OmpChatPane(props: Props) {
  let threadEl: HTMLDivElement | undefined;
  let rootEl: HTMLDivElement | undefined;

  const state = createMemo(() => ompChatForSession(props.sessionId));

  // Backfill on first enter (status !== resolved). Idempotent.
  onMount(() => { void backfillOmpChat(props.sessionId); });

  // ── attachments ───────────────────────────────────────────────────────
  // The PANE owns the pending list because it owns the drop target (the whole
  // surface, not just the composer strip); the composer owns the tray and the
  // send encoding. Four transient values — not worth a store slice.
  const [pending, setPending] = createSignal<Pending[]>([]);
  const addPending = (absPath: string, file: File) => {
    // Dedup by absPath: the worker's SHA-256 probe returns the same path for
    // the same bytes, so dropping one file twice must not make two chips.
    setPending((cur) => cur.some((p) => p.absPath === absPath) ? cur : [...cur, {
      absPath, name: file.name, mime: file.type, isImage: file.type.startsWith("image/"),
    }]);
    if (!file.type.startsWith("image/")) return;
    // Thumbnail from the File the browser already holds. Without it the chip
    // would pull the whole image back down from the worker to paint 24px.
    const reader = new FileReader();
    reader.onload = () => {
      if (typeof reader.result !== "string") return;
      setPending((cur) => cur.map((p) => (p.absPath === absPath ? { ...p, thumb: reader.result as string } : p)));
    };
    reader.readAsDataURL(file);
  };

  // Element-scoped, not document-scoped: the pane is a real box (unlike the
  // letterboxed cell grid that forced CellTerminal onto document), so scoping
  // here avoids fighting the terminal's handlers. No `focused` guard — the
  // chat pane is only visible on a pane the user is looking at.
  onMount(() => {
    const el = rootEl;
    if (!el) return;
    const take = (list: DataTransferItemList | undefined) => {
      const s = rootStore.sessions[props.sessionId];
      if (!s || !list) return;
      for (let i = 0; i < list.length; i++) {
        if (list[i]!.kind !== "file") continue;
        const f = list[i]!.getAsFile();
        if (f) void enqueueAttachmentTo(s, f, addPending);
      }
    };
    const hasFiles = (dt: DataTransfer | null | undefined) =>
      !!dt && Array.from(dt.types).includes("Files");
    const onDragOver = (e: DragEvent) => {
      if (!hasFiles(e.dataTransfer)) return;
      e.preventDefault();  // allow the drop + stop the browser opening the file
    };
    const onDrop = (e: DragEvent) => {
      if (!hasFiles(e.dataTransfer)) return;
      e.preventDefault();
      take(e.dataTransfer?.items);
    };
    const onPaste = (e: ClipboardEvent) => take(e.clipboardData?.items);
    el.addEventListener("dragover", onDragOver);
    el.addEventListener("drop", onDrop);
    el.addEventListener("paste", onPaste);
    onCleanup(() => {
      el.removeEventListener("dragover", onDragOver);
      el.removeEventListener("drop", onDrop);
      el.removeEventListener("paste", onPaste);
    });
  });

  // Per-callId index across ALL messages (a toolResult is a separate entry).
  const toolIndex = createMemo(() => buildToolIndex(state().messages));

  // Stick-to-bottom: scroll to end on append unless the user scrolled up.
  const [stick, setStick] = createSignal(true);
  const [scrolled, setScrolled] = createSignal(false);
  const onScroll = () => {
    if (!threadEl) return;
    setStick(threadEl.scrollHeight - threadEl.scrollTop - threadEl.clientHeight < 80);
    setScrolled(threadEl.scrollTop > 0);
  };
  // untrack(stick): the effect must fire on APPEND only. Tracking `stick` would
  // re-run it on every scroll flip and yank the view back down mid-read.
  createEffect(() => {
    const n = state().messages.length;
    if (untrack(stick) && threadEl) queueMicrotask(() => { threadEl?.scrollTo({ top: threadEl.scrollHeight }); });
    void n;
  });
  const toBottom = () => threadEl?.scrollTo({ top: threadEl.scrollHeight });

  // Pane-level thinking switch, omp's export idiom (template.js:1585-1596): one
  // control expands every run at once. Per-pane and non-persistent by design —
  // omp's own toggle resets per view, so this must not live in uiStore.
  const [expandThinking, setExpandThinking] = createSignal(false);
  const hasThinking = createMemo(() =>
    state().messages.some((m) => m.blocks.some((b) => b.kind === "thinking" && canonicalizeThinking(b.text) !== "")),
  );
  // "Conversation started" is NOT messages.length: omp posts a `developer`
  // notice the instant its child boots (MCP mounts, warnings), so a brand-new
  // chat's transcript is never empty and a length test would hide the welcome
  // card on exactly the sessions it was built for. Any non-developer message
  // means a real turn exists.
  const started = createMemo(() => state().messages.some((m) => m.role !== "developer"));

  return (
    <div ref={rootEl} class="omp-chat" data-testid="omp-chat-pane" data-session-id={props.sessionId} data-scrolled={String(scrolled())}>
      {/* Context readout only — model + effort live permanently on the
          composer chip now. Native engine only: the mirror engine reports
          nothing and the row stays hidden. */}
      <Show when={state().contextTokens > 0 || hasThinking()}>
        <div class="omp-chat__status" data-testid="omp-chat-status">
          <Show when={hasThinking()}>
            <button type="button" class="omp-chat__status-toggle" data-testid="omp-chat-thinking-all"
              aria-pressed={expandThinking()} onClick={() => setExpandThinking((v) => !v)}>thinking</button>
          </Show>
          <Show when={state().contextTokens > 0}>
            <span class="omp-chat__status-ctx">
              {state().contextPct}% context · {state().contextTokens.toLocaleString()} tokens
            </span>
          </Show>
        </div>
      </Show>
      <Show when={state().streaming}>
        <md-linear-progress class="omp-chat__progress" prop:indeterminate={true} />
      </Show>
      <div class="omp-chat__thread" role="log" aria-label="Chat transcript" ref={threadEl} onScroll={onScroll}>
        <Show when={state().status !== "loading"} fallback={<div class="omp-chat__skeleton">Loading chat…</div>}>
          {/* Card sits ABOVE the rows rather than replacing them: a boot-time
              developer notice must stay visible while the pane is still
              conversation-less. */}
          <Show when={!started()}>
            <ChatWelcome sessionId={props.sessionId} focused={props.focused} />
          </Show>
          <For each={state().messages}>
            {(msg) => <MessageRow msg={msg} sessionId={props.sessionId} expandThinking={expandThinking()} toolIndex={toolIndex()} />}
          </For>
        </Show>
        <Show when={state().streaming}>
          <div class="omp-chat__busy" role="status" data-testid="omp-chat-busy"><Icon name="autorenew" size="sm" />working</div>
        </Show>
      </div>
      <Show when={!stick()}>
        <button type="button" class="omp-chat__jump" data-testid="omp-chat-jump-bottom"
          aria-label="Scroll to latest" onClick={toBottom}>
          <Icon name="arrow_downward" />
        </button>
      </Show>
      <Composer
        sessionId={props.sessionId}
        focused={props.focused}
        pending={pending}
        addPending={addPending}
        removePending={(absPath) => setPending((cur) => cur.filter((p) => p.absPath !== absPath))}
        clearPending={() => setPending([])}
      />
    </div>
  );
}

/** One transcript row: a right-aligned gutter label naming the speaker, then the
 *  message body. Rows whose every block folded into a tool card elsewhere paint
 *  nothing — chat-message.css hides a row with an empty body. */
const ROW_KIND: Record<ChatMessage["role"], string> = {
  user: "user", assistant: "assistant", developer: "custom", toolResult: "custom",
};
const GUTTER_LABEL: Record<ChatMessage["role"], string> = {
  user: "host", assistant: "agent", developer: "", toolResult: "",
};

function MessageRow(props: { msg: ChatMessage; sessionId: string; expandThinking: boolean; toolIndex: Map<string, ToolMatch> }) {
  const grouped = createMemo(() => groupThinking(props.msg.blocks));

  return (
    <div class={`tr-row tr-row--${ROW_KIND[props.msg.role]}`}>
      <div class="tr-gutter" title={props.msg.ts}>{GUTTER_LABEL[props.msg.role]}</div>
      <div class="tr-body">
        <Show when={props.msg.role === "developer"}>
          <span class="tr-chip">developer</span>
        </Show>
        {/* Index, not For: it keys by position, so a streaming message that
            re-groups on every frame updates props in place instead of
            remounting each run and dropping its local expand state. */}
        <Index each={grouped()}>
          {(item) => {
            const run = () => { const v = item(); return v.kind === "thinking" ? v : null; };
            const blk = () => { const v = item(); return v.kind === "block" ? v : null; };
            return (
              <Switch>
                <Match when={run()}>
                  {(r) => (
                    <ThinkingBlock sessionId={props.sessionId} messageId={props.msg.id}
                      parts={r().parts} expandAll={props.expandThinking} />
                  )}
                </Match>
                <Match when={blk()}>
                  {(b) => (
                    <BlockView block={b().block} msg={props.msg} blockIndex={b().index}
                      sessionId={props.sessionId} toolIndex={props.toolIndex} />
                  )}
                </Match>
              </Switch>
            );
          }}
        </Index>
      </div>
    </div>
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
      return <ProseBlock block={block} sessionId={props.sessionId} />;
    case "toolCall": {
      // Accessor, NOT a captured value: this component body runs once, but the
      // tool's index entry keeps changing as start → update → end → result
      // frames land. Capturing froze `event` at mount, so a card mounted before
      // its first toolEvent never showed live output or a phase change.
      const m = () => props.toolIndex.get(block.callId);
      return (
        <ToolCard
          sessionId={props.sessionId}
          call={block}
          results={m()?.results}
          event={m()?.event}
          images={m()?.images}
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
