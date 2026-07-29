// The scrolling body of an agent session: every AgentEntry in seq order.
//
// No virtualization yet: rows stream in place and can change height, which
// makes naive fixed-row virtualization corrupt scroll anchoring. Durable pages
// are loaded incrementally as the reader reaches the top.
//
// Scroll follows the cellRenderer idiom: the reader owns every non-bottom
// position, and a render only pins to the bottom when it STARTED at the literal
// bottom. Prepends (the "Load earlier" page) are left to native browser scroll
// anchoring, which keeps the inspected row under the cursor for free.
//
// Caller: components/agent/TranscriptDeck.tsx.

import {
  Index, Match, Show, Switch, createEffect, createMemo, createSignal, on,
  onCleanup, onMount, type Component,
} from "solid-js";
import { Button } from "../Settings/md/Button.tsx";
import { AgentNotice } from "./AgentNotice.tsx";
import { EntryPrompt } from "./EntryPrompt.tsx";
import { EntryText } from "./EntryText.tsx";
import { EntryTool } from "./EntryTool.tsx";
import { EntryImage } from "./EntryImage.tsx";
import { EntrySubagent } from "./EntrySubagent.tsx";
import { EntryTodo } from "./EntryTodo.tsx";
import {
  agentEntries, backfillEntries, hasBackfilled, hasEarlierEntries, isBackfilling,
} from "../../store/agentEntries.ts";
import type {
  AgentEntry, AgentImageEntry, AgentNoticeEntry, AgentPromptEntry,
  AgentSubagentEntry, AgentTextEntry, AgentTodoEntry, AgentToolEntry,
} from "@roost/shared/wire/agent-entry";
import type { Session } from "@roost/shared/wire";

export const Transcript: Component<{ session: Session }> = (props) => {
  let el: HTMLDivElement | undefined;
  let earlierSentinel: HTMLDivElement | undefined;
  // Not a signal: nothing renders it, and a scroll handler that writes a signal
  // on every wheel tick would re-run the whole reactive graph at 60 Hz.
  let atBottom = true;
  const sid = () => props.session.id;
  const entries = () => agentEntries(sid());

  // Length alone misses the streaming case: the tail entry is re-emitted under
  // the same seq with a longer body, which is most of what a live turn looks
  // like. Fold the tail's mutable field in so the pin still fires.
  const tailTick = createMemo(() => {
    const list = entries();
    const last = list[list.length - 1];
    return `${list.length}\u0000${last ? tailSignature(last) : ""}`;
  });

  // Mount AND session switch both need the newest durable page: the live
  // firehose does not replay. `tried` gates the failure state so the first
  // paint (before this effect runs) reads "Loading…", not "Couldn't load".
  const [tried, setTried] = createSignal(false);
  createEffect(on(sid, (id) => {
    atBottom = true;
    setTried(true);
    if (!hasBackfilled(id)) void backfillEntries(id);
  }));
  const failedFirstPage = () => tried() && !hasBackfilled(sid()) && !isBackfilling(sid());

  createEffect(on(tailTick, () => {
    if (!atBottom || !el) return;
    const bottom = Math.max(0, el.scrollHeight - el.clientHeight);
    if (el.scrollTop !== bottom) el.scrollTop = bottom;
  }));

  onMount(() => {
    if (!el || !earlierSentinel || typeof IntersectionObserver === "undefined") return;
    const observer = new IntersectionObserver((observations) => {
      if (!observations.some((observation) => observation.isIntersecting)) return;
      const id = sid();
      if (hasEarlierEntries(id) && !isBackfilling(id)) void backfillEntries(id);
    }, { root: el });
    observer.observe(earlierSentinel);
    onCleanup(() => observer.disconnect());
  });

  return (
    <div
      ref={el}
      data-testid="agent-transcript"
      onScroll={() => {
        if (!el) return;
        // 1px slack: fractional zoom leaves scrollTop a hair under the maximum.
        atBottom = el.scrollTop >= Math.max(0, el.scrollHeight - el.clientHeight) - 1;
      }}
      style={{
        flex: "1",
        "min-height": "0",
        overflow: "auto",
        display: "flex",
        "flex-direction": "column",
        gap: "var(--md-space-3)",
        padding: "var(--md-space-4)",
      }}
    >
      <div
        ref={earlierSentinel}
        aria-hidden="true"
        data-testid="agent-transcript-earlier-sentinel"
        style={{ height: "1px", "flex-shrink": 0 }}
      />
      <Show when={hasEarlierEntries(sid())}>
        <div style={{ display: "flex", "justify-content": "center" }}>
          <Button
            variant="text"
            disabled={isBackfilling(sid())}
            data-testid="agent-load-earlier"
            onClick={() => void backfillEntries(sid())}
          >
            {isBackfilling(sid()) ? "Loading…" : "Load earlier"}
          </Button>
        </div>
      </Show>

      <Show when={entries().length === 0}>
        <div
          data-testid="agent-transcript-empty"
          style={{
            margin: "auto",
            display: "flex",
            "flex-direction": "column",
            "align-items": "center",
            gap: "var(--md-space-2)",
            color: "var(--md-on-surface-dim)",
            "font-size": "var(--md-body-m-size)",
            "line-height": "var(--md-body-m-line)",
          }}
        >
          {/* A failed FIRST page is the one dead end: nothing is stored, so
              hasEarlier never flips and no "Load earlier" button appears —
              without this the pane would read "Loading transcript…" forever. */}
          <Show when={failedFirstPage()} fallback={hasBackfilled(sid()) ? "No messages yet." : "Loading transcript…"}>
            <span>Couldn't load the transcript.</span>
            <Button variant="text" data-testid="agent-transcript-retry" onClick={() => void backfillEntries(sid())}>
              Retry
            </Button>
          </Show>
        </div>
      </Show>

      {/* Index, not For: entries are replaced IN PLACE as they stream, and
          index keying updates the row's accessor instead of tearing down and
          rebuilding its DOM on every 50 ms flush. */}
      <Index each={entries()}>
        {(entry) => (
          <Switch>
            <Match when={asText(entry())}>{(e) => <EntryText entry={e()} />}</Match>
            <Match when={asTool(entry())}>{(e) => <EntryTool entry={e()} />}</Match>
            <Match when={asPrompt(entry())}>
              {(e) => <EntryPrompt sessionId={sid()} entry={e()} />}
            </Match>
            <Match when={asNotice(entry())}>{(e) => <AgentNotice entry={e()} />}</Match>
            <Match when={asTodo(entry())}>{(e) => <EntryTodo entry={e()} />}</Match>
            <Match when={asSubagent(entry())}>{(e) => <EntrySubagent entry={e()} />}</Match>
            <Match when={asImage(entry())}>{(e) => <EntryImage entry={e()} />}</Match>
          </Switch>
        )}
      </Index>
    </div>
  );
};


// Narrowing helpers, not renames: <Match when={...}> hands its callback the
// truthy value, so returning the entry (or undefined) is what lets each row
// component take a precise variant with no cast.
function asText(e: AgentEntry): AgentTextEntry | undefined {
  return e.kind === "user" || e.kind === "assistant" || e.kind === "thinking" ? e : undefined;
}
function asTool(e: AgentEntry): AgentToolEntry | undefined {
  return e.kind === "tool" ? e : undefined;
}
function asPrompt(e: AgentEntry): AgentPromptEntry | undefined {
  return e.kind === "prompt" ? e : undefined;
}
function asNotice(e: AgentEntry): AgentNoticeEntry | undefined {
  return e.kind === "notice" ? e : undefined;
}
function asTodo(e: AgentEntry): AgentTodoEntry | undefined {
  return e.kind === "todo" ? e : undefined;
}
function asSubagent(e: AgentEntry): AgentSubagentEntry | undefined {
  return e.kind === "subagent" ? e : undefined;
}
function asImage(e: AgentEntry): AgentImageEntry | undefined {
  return e.kind === "image" ? e : undefined;
}

// The one field of the tail entry that grows in place.
function tailSignature(e: AgentEntry): string {
  switch (e.kind) {
    case "prompt":
      return `${e.state}\u0000${e.answer.length}`;
    case "todo":
      return String(e.phases_json.length);
    case "subagent":
      return `${e.state}\u0000${e.text.length}`;
    case "image":
      return String(e.data_b64.length);
    default:
      return String(e.text.length);
  }
}
