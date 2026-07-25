// OmpChatPane — the omp chat thread. Renders bubbles + inline tool cards, using
// the shared renderPlan routing so nothing the transcript holds is dropped:
// tool results/events/images fold into their matching call's card (by callId);
// call-less results render as their own card. Backfills history on first enter;
// stick-to-bottom scroll.

import { createMemo, createEffect, createSignal, untrack, For, Index, Switch, Match, Show, onMount, onCleanup } from "solid-js";
import type { ChatMessage, ContentBlock } from "@roost/shared/chat/wire";
import { roostMessageRows } from "@roost/shared/chat/rows";
import { ompChatForSession, backfillOmpChat } from "../../../store/chatOmp.ts";
import { rootStore } from "../../../store/root.ts";
import { enqueueAttachmentTo } from "../../../lib/attachments.ts";
import { buildToolIndex, type ToolMatch } from "./renderPlan.ts";
import { ProseBlock } from "./ProseBlock.tsx";
import { ThinkingBlock } from "./ThinkingBlock.tsx";
import { canonicalizeThinking, groupThinking } from "./thinkingText.ts";
import { latestTodoPhases, buildTodoHud, phaseRomanNumeral } from "./todoHud.ts";
import { ToolCard } from "./ToolCard.tsx";
import { Composer, type Pending } from "./Composer.tsx";
import { ChatWelcome } from "./ChatWelcome.tsx";
import { ApprovalCard } from "./ApprovalCard.tsx";
import { NoticeRow } from "./NoticeRow.tsx";
import { SummaryCard } from "./SummaryCard.tsx";
import { CustomCard } from "./CustomCard.tsx";
import { ExecBlock } from "./ExecBlock.tsx";
import { FileMentionRow } from "./FileMentionRow.tsx";
import { cap } from "./ModelMenu.tsx";
import { shortCwd } from "../../../lib/sidebarFormat.ts";
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

  // Pinned Todos HUD: omp's anchored todoContainer has no wire representation,
  // so the board is re-derived from the newest successful `todo` toolResult.
  // Its collapse switch is per-pane and non-persistent for the same reason as
  // `expandThinking` below — the terminal's own toggle resets per view.
  const [todoExpanded, setTodoExpanded] = createSignal(false);
  const todoPhases = createMemo(() => latestTodoPhases(state().messages));
  const todoHud = createMemo(() => buildTodoHud(todoPhases(), todoExpanded()));

  // Stick-to-bottom: scroll to end on append unless the user scrolled up.
  const [stick, setStick] = createSignal(true);
  const [scrolled, setScrolled] = createSignal(false);
  const onScroll = () => {
    if (!threadEl) return;
    setStick(threadEl.scrollHeight - threadEl.scrollTop - threadEl.clientHeight < 80);
    setScrolled(threadEl.scrollTop > 0);
  };
  // A streaming message GROWS in place, so message count alone never re-fires
  // this and a live reply scrolls off the bottom while it is still being
  // written. Size of the tail message is the cheap proxy for "it changed".
  const tailSize = createMemo(() => {
    const msgs = state().messages;
    const last = msgs[msgs.length - 1];
    if (!last) return 0;
    let n = 0;
    for (const b of last.blocks) n += (b.kind === "text" || b.kind === "thinking") ? b.text.length : 1;
    return n;
  });
  // untrack(stick): the effect must fire on APPEND/GROWTH only. Tracking
  // `stick` would re-run it on every scroll flip and yank the view back down
  // mid-read.
  createEffect(() => {
    const n = state().messages.length + tailSize();
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

  // ── status row ────────────────────────────────────────────────────────
  // cwd + branch come from the session record, not the chat frame: Roost
  // already tracks both, and omp's transcript carries neither.
  const session = () => rootStore.sessions[props.sessionId];
  // Same head as the composer's ModelMenu label, minus the "Claude " prefix
  // omp itself strips in modelSegment. Falls back to the raw provider/id
  // selector when the catalog could not name the model.
  const modelLabel = () => {
    const head = (state().modelName || state().model).replace(/^Claude /, "");
    const lvl = state().thinkingLevel;
    return lvl && lvl !== "off" ? `${head} · ${cap(lvl)}` : head;
  };
  // Percentage is derived here, not on the wire: an unknown window (no catalog
  // entry) must stay distinguishable from a genuine 0%. null = unknown.
  const ctxPct = () => {
    const w = state().contextWindow;
    return w > 0 ? (state().contextTokens / w) * 100 : null;
  };
  // omp's own context bands (context-thresholds.ts). Absolute-token thresholds
  // are deliberately omitted: they only bite on windows where they are stricter
  // than the percentages, and the bands carry the same warning where it counts.
  const ctxLevel = () => {
    const pct = ctxPct();
    if (pct === null) return "normal";
    if (pct >= 90) return "error";
    if (pct >= 70) return "high";
    if (pct >= 50) return "warning";
    return "normal";
  };
  // Window unknown → "<tokens> / ?", the same degraded form omp's own
  // formatContextUsage falls back to. One decimal otherwise, so the chip reads
  // identically to the bar omp paints in the terminal view.
  const ctxLabel = () => {
    const tokens = state().contextTokens.toLocaleString();
    const pct = ctxPct();
    return pct === null ? `${tokens} / ?` : `${pct.toFixed(1)}% · ${tokens} tokens`;
  };

  return (
    <div ref={rootEl} class="omp-chat" data-testid="omp-chat-pane" data-session-id={props.sessionId} data-scrolled={String(scrolled())}>
      {/* Roost-native mirror of the bar omp paints into its own input-box top
          border. Every chip is independently gated: an unknown fact renders
          nothing rather than a placeholder. Read-only by design — the
          interactive model picker lives on the composer. */}
      <div class="omp-chat__status" data-testid="omp-chat-status">
        <Show when={hasThinking()}>
          <button type="button" class="omp-chat__status-toggle" data-testid="omp-chat-thinking-all"
            aria-pressed={expandThinking()} onClick={() => setExpandThinking((v) => !v)}>thinking</button>
        </Show>
        <Show when={state().model}>
          <span class="omp-chat__chip" data-testid="omp-chat-status-model" title={state().model}>{modelLabel()}</span>
        </Show>
        {/* "none" is what omp writes on EXITING a mode — not a mode to show. */}
        <Show when={state().mode && state().mode !== "none" && state().mode !== "default"}>
          <span class="omp-chat__chip" data-testid="omp-chat-status-mode">{cap(state().mode)}</span>
        </Show>
        <Show when={session()?.cwd}>
          {(cwd) => (
            <span class="omp-chat__chip" data-testid="omp-chat-status-cwd" title={cwd()}>{shortCwd(cwd())}</span>
          )}
        </Show>
        <Show when={session()?.git_branch}>
          {(branch) => (
            <span class="df-flat-branch" data-testid="omp-chat-status-branch" title={`On branch ${branch()}`}>
              <svg class="df-flat-branch-icon" width="11" height="11" viewBox="0 0 24 24" fill="none"
                stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                <line x1="6" y1="3" x2="6" y2="15" /><circle cx="18" cy="6" r="3" />
                <circle cx="6" cy="18" r="3" /><path d="M18 9a9 9 0 0 1-9 9" />
              </svg>
              <span class="df-flat-branch-text">{branch()}</span>
            </span>
          )}
        </Show>
        {/* margin-left:auto lives on -ctx, so the context chip sits right. */}
        <Show when={state().contextTokens > 0}>
          <span class="omp-chat__status-ctx" data-testid="omp-chat-status-ctx" data-level={ctxLevel()}>{ctxLabel()}</span>
        </Show>
      </div>
      <Show when={state().streaming}>
        <md-linear-progress class="omp-chat__progress" prop:indeterminate={true} />
      </Show>
      <div class="omp-chat__thread" role="log" aria-label="Chat transcript" ref={threadEl} onScroll={onScroll}>
        <Show when={state().status !== "loading"} fallback={<div class="omp-chat__skeleton">Loading chat…</div>}>
          {/* Card sits ABOVE the rows rather than replacing them: a boot-time
              developer notice must stay visible while the pane is still
              conversation-less. Gated on `resolved`, not on `!started()`: a
              backfill that FAILED has no messages either, and greeting the user
              over a dead pipeline is how this bug stayed invisible. */}
          <Show when={state().status === "failed"}>
            <div class="omp-chat__failed" data-testid="omp-chat-failed">
              Chat unavailable — the worker did not return this conversation.
            </div>
          </Show>
          <Show when={state().status !== "failed" && !started()}>
            <ChatWelcome sessionId={props.sessionId} focused={props.focused} />
          </Show>
          <For each={state().messages}>
            {(msg, i) => (
              <MessageRow msg={msg} sessionId={props.sessionId} expandThinking={expandThinking()}
                toolIndex={toolIndex()} streaming={state().streaming && i() === state().messages.length - 1} />
            )}
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
      {/* Anchored directly above the composer, where the terminal pins it — NOT
          inside .omp-chat__thread, which scrolls the HUD out of view. */}
      <Show when={todoHud()}>
        {(hud) => (
          <div class="omp-todo" data-testid="omp-chat-todo">
            <button type="button" class="omp-todo__header" data-testid="omp-chat-todo-toggle"
              aria-expanded={todoExpanded()} onClick={() => setTodoExpanded((v) => !v)}>
              <span class="omp-todo__title">Todos</span>
              <Show when={hud().phaseCount > 1}>
                <span class="omp-todo__count">{` · ${hud().activeIndex}/${hud().phaseCount}`}</span>
              </Show>
            </button>
            <div class="omp-todo__body">
              <For each={hud().phases}>
                {(p) => (
                  <>
                    <div class="omp-todo__phase" data-active={String(p.active)}>
                      <span class="omp-todo__phase-name">
                        {hud().phaseCount > 1 ? `${phaseRomanNumeral(p.index)}. ${p.name}` : p.name}
                      </span>
                      <span class="omp-todo__phase-progress">{` · ${p.done}/${p.total}`}</span>
                    </div>
                    <For each={p.tasks}>
                      {(t) => (
                        <div class="omp-todo__task" data-status={t.status}>
                          <span class="omp-todo__box">{t.status === "completed" ? "[x]" : "[ ]"}</span>
                          <span class="omp-todo__text">{t.content}</span>
                          <Show when={t.status === "blocked"}>
                            <span class="omp-todo__blocker">{t.blocker ? ` (blocked: ${t.blocker})` : " (blocked)"}</span>
                          </Show>
                        </div>
                      )}
                    </For>
                    <Show when={p.summary}>
                      <div class="omp-todo__more">{p.summary}</div>
                    </Show>
                  </>
                )}
              </For>
            </div>
          </div>
        )}
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

/** Blocks that already name themselves — a second generic `developer` chip
 *  above one of these cards is noise, and hiding the card's own identity behind
 *  it is exactly what made advisor / irc / async-result rows unreadable. */
const SELF_LABELLED: Partial<Record<ContentBlock["kind"], true>> = {
  custom: true, summary: true, exec: true, fileMention: true,
};

function MessageRow(props: { msg: ChatMessage; sessionId: string; expandThinking: boolean; toolIndex: Map<string, ToolMatch>; streaming: boolean }) {
  // The parity oracle's row projection, anchored to the block that paints each
  // row (-1 = this wrapper). Stamped as `data-tui-row` so the browser column of
  // `roost api chat-sbs` reads PAINTED rows, not stored ones — a row hidden by
  // CSS or eaten by the skeleton has no client rect and must not count.
  const stamps = createMemo(() => {
    const m = new Map<number, string>();
    for (const a of roostMessageRows(props.msg)) m.set(a.blockIndex, JSON.stringify(a.row));
    return m;
  });

  // Agent-attributed user input collapses, exactly as omp's
  // CollapsedSyntheticMessageComponent does (user-message.ts:84): an advisor
  // "Session update" replay is hundreds of KiB and buries every real turn.
  const head = () => {
    for (const b of props.msg.blocks) {
      if (b.kind !== "text") continue;
      for (const line of b.text.split("\n")) {
        const t = line.trim();
        if (t) return t.length > 120 ? `${t.slice(0, 120)}…` : t;
      }
    }
    return "synthetic message";
  };

  return (
    // data-streaming marks the ONE row the agent is still writing into, so the
    // reader (and the browser oracle) can watch growth on a stable node.
    <div class={`tr-row tr-row--${ROW_KIND[props.msg.role]}`} data-streaming={props.streaming ? "true" : undefined}
      data-tui-row={stamps().get(-1)}>
      <div class="tr-gutter" title={props.msg.ts}>{GUTTER_LABEL[props.msg.role]}</div>
      <div class="tr-body">
        <Show when={props.msg.role === "developer" && !props.msg.blocks.every((b) => SELF_LABELLED[b.kind])}>
          <span class="tr-chip">developer</span>
        </Show>
        <Show when={props.msg.role === "user" && props.msg.synthetic}
          fallback={<MessageBlocks msg={props.msg} sessionId={props.sessionId}
            expandThinking={props.expandThinking} toolIndex={props.toolIndex} stamps={stamps()} />}>
          <details class="tr-synthetic" data-testid="omp-chat-synthetic">
            <summary class="tr-synthetic-head">{head()} (expand)</summary>
            <MessageBlocks msg={props.msg} sessionId={props.sessionId}
              expandThinking={props.expandThinking} toolIndex={props.toolIndex} stamps={stamps()} />
          </details>
        </Show>
      </div>
    </div>
  );
}

/** The message's blocks in transcript order. Its own component only so the
 *  synthetic disclosure can wrap the loop without it existing twice.
 *  `stamps` maps a block index to the parity row that block paints. */
function MessageBlocks(props: { msg: ChatMessage; sessionId: string; expandThinking: boolean; toolIndex: Map<string, ToolMatch>; stamps: Map<number, string> }) {
  const grouped = createMemo(() => groupThinking(props.msg.blocks));
  return (
    // Index, not For: it keys by position, so a streaming message that
    // re-groups on every frame updates props in place instead of remounting
    // each run and dropping its local expand state.
    <Index each={grouped()}>
      {(item) => {
        const run = () => { const v = item(); return v.kind === "thinking" ? v : null; };
        const blk = () => { const v = item(); return v.kind === "block" ? v : null; };
        return (
          <Switch>
            <Match when={run()}>
              {(r) => (
                <ThinkingBlock sessionId={props.sessionId} messageId={props.msg.id}
                  parts={r().parts} expandAll={props.expandThinking}
                  dataTuiRow={props.stamps.get(r().index)} />
              )}
            </Match>
            <Match when={blk()}>
              {(b) => (
                <BlockView block={b().block} msg={props.msg} blockIndex={b().index}
                  sessionId={props.sessionId} toolIndex={props.toolIndex}
                  dataTuiRow={props.stamps.get(b().index)} />
              )}
            </Match>
          </Switch>
        );
      }}
    </Index>
  );
}

function BlockView(props: { block: ContentBlock; msg: ChatMessage; blockIndex: number; sessionId: string; toolIndex: Map<string, ToolMatch>; dataTuiRow?: string }) {
  const block = props.block;
  switch (block.kind) {
    case "text":
    case "image":
      // Inline images live on non-toolResult messages; toolResult images fold
      // into the tool card (rendered below), so skip them here.
      if (block.kind === "image" && props.msg.role === "toolResult") return null;
      return <ProseBlock block={block} sessionId={props.sessionId} dataTuiRow={props.dataTuiRow} />;
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
          dataTuiRow={props.dataTuiRow}
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
    case "notice":
      return <NoticeRow block={block} dataTuiRow={props.dataTuiRow} />;
    case "summary":
      return (
        <SummaryCard block={block} sessionId={props.sessionId}
          messageId={props.msg.id} blockIndex={props.blockIndex} dataTuiRow={props.dataTuiRow} />
      );
    case "custom":
      return (
        <CustomCard block={block} sessionId={props.sessionId}
          messageId={props.msg.id} blockIndex={props.blockIndex} dataTuiRow={props.dataTuiRow} />
      );
    case "exec":
      return (
        <ExecBlock block={block} sessionId={props.sessionId}
          messageId={props.msg.id} blockIndex={props.blockIndex} dataTuiRow={props.dataTuiRow} />
      );
    case "fileMention":
      return <FileMentionRow block={block} dataTuiRow={props.dataTuiRow} />;
  }
}
