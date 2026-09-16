// The /agent pane's render model: one ordered timeline of prompts, streamed
// assistant text, tool cards and permission asks, folded from Mecatl's own
// event union. Owned per selected session by agentSessionController.ts and read
// by AgentTranscript.tsx.
//
// This state is deliberately component-scoped: Roost persists nothing about
// agent conversations, so it must never reach rootStore, Sync, or localStorage.

import { createStore, produce } from "solid-js/store";
import type { Event, SessionTranscript } from "@stacklok-oss/mecatl-sdk";

export type AgentAskState = "pending" | "allow_once" | "allow_always" | "deny" | "retracted";
export type AgentNoticeTone = "info" | "error";

export interface AgentToolOutcome {
  content: string;
  isError: boolean;
}

export type AgentTimelineDraft =
  | { kind: "prompt"; text: string }
  | { kind: "assistant"; text: string; streaming: boolean }
  | { kind: "tool"; callId: string; name: string; args: string; result: AgentToolOutcome | null }
  | { kind: "ask"; askId: string; tool: string; args: string; reason: string; state: AgentAskState }
  | { kind: "notice"; tone: AgentNoticeTone; text: string };

/** One timeline row. `id` is the <For> key and is never reused within a load. */
export type AgentTimelineEntry = AgentTimelineDraft & { id: string };
export type AgentAskEntry = Extract<AgentTimelineEntry, { kind: "ask" }>;
export type AgentToolEntry = Extract<AgentTimelineEntry, { kind: "tool" }>;

export interface AgentTimeline {
  /** Reactive, render-ready timeline in arrival order. */
  readonly entries: readonly AgentTimelineEntry[];
  /** The ask still awaiting a verdict, or null. */
  pendingAsk(): AgentAskEntry | null;
  clear(): void;
  loadTranscript(transcript: SessionTranscript): void;
  appendPrompt(text: string): void;
  appendNotice(tone: AgentNoticeTone, text: string): void;
  /** Folds one Mecatl event. Lifecycle-only and unknown kinds are ignored. */
  applyEvent(event: Event): void;
  /** Records the verdict the pane sent so the card stops offering buttons. */
  resolveAsk(askId: string, state: AgentAskState): void;
  /** Closes a still-streaming assistant entry (run ended, failed, cancelled). */
  settleStreaming(): void;
}

export function createAgentTimeline(): AgentTimeline {
  const [entries, setEntries] = createStore<AgentTimelineEntry[]>([]);
  let nextId = 0;
  // A monotonic per-load counter, not the Mecatl ids: notices and streamed
  // assistant bubbles have no server identity, and <For> needs stable keys.
  function mintId(): string {
    nextId += 1;
    return `e${nextId}`;
  }

  function append(draft: AgentTimelineDraft): void {
    setEntries(produce((list) => {
      list.push({ ...draft, id: mintId() });
    }));
  }

  // Deltas arrive token-by-token, so they extend the trailing streaming entry
  // in place rather than appending one bubble per token.
  function appendAssistantDelta(delta: string): void {
    if (delta === "") return;
    setEntries(produce((list) => {
      const tail = list[list.length - 1];
      if (tail !== undefined && tail.kind === "assistant" && tail.streaming) {
        tail.text += delta;
        return;
      }
      list.push({ id: mintId(), kind: "assistant", text: delta, streaming: true });
    }));
  }

  function foldToolResult(callId: string, outcome: AgentToolOutcome): void {
    setEntries(produce((list) => {
      const target = lastToolEntry(list, callId);
      if (target !== null) {
        target.result = outcome;
        return;
      }
      list.push({ id: mintId(), kind: "tool", callId, name: callId, args: "", result: outcome });
    }));
  }

  function setAskState(askId: string, state: AgentAskState): void {
    setEntries(produce((list) => {
      for (let idx = list.length - 1; idx >= 0; idx -= 1) {
        const entry = list[idx];
        if (entry === undefined || entry.kind !== "ask" || entry.askId !== askId) continue;
        if (entry.state === "pending") entry.state = state;
        return;
      }
    }));
  }

  // A run can terminate without ever emitting a delta (a cached answer, or a
  // hard error), so the terminal event is the last chance to show its text.
  function foldResult(text: string, error: string, stop: string): void {
    setEntries(produce((list) => {
      const tail = list[list.length - 1];
      const streamed = tail !== undefined && tail.kind === "assistant" && tail.streaming;
      if (tail !== undefined && tail.kind === "assistant") tail.streaming = false;
      if (text !== "" && !streamed) {
        list.push({ id: mintId(), kind: "assistant", text, streaming: false });
      }
      if (error !== "") {
        list.push({ id: mintId(), kind: "notice", tone: "error", text: error });
        return;
      }
      if (stop !== "" && stop !== "end_turn" && stop !== "stop") {
        list.push({ id: mintId(), kind: "notice", tone: "info", text: `Run ended: ${stop}` });
      }
    }));
  }

  function loadTranscript(transcript: SessionTranscript): void {
    setEntries(produce((list) => {
      list.length = 0;
      nextId = 0;
      for (const message of transcript.messages) {
        if (message.text !== "") {
          list.push(message.role === "user"
            ? { id: mintId(), kind: "prompt", text: message.text }
            : { id: mintId(), kind: "assistant", text: message.text, streaming: false });
        }
        for (const call of message.toolCalls) {
          list.push({
            id: mintId(),
            kind: "tool",
            callId: call.id,
            name: call.name,
            args: call.args,
            result: null,
          });
        }
        const result = message.toolResult;
        if (result === undefined) continue;
        const outcome: AgentToolOutcome = { content: result.content, isError: result.isError };
        const target = lastToolEntry(list, result.callId);
        if (target === null) {
          list.push({
            id: mintId(),
            kind: "tool",
            callId: result.callId,
            name: result.callId,
            args: "",
            result: outcome,
          });
          continue;
        }
        target.result = outcome;
      }
    }));
    if (!transcript.complete) {
      append({ kind: "notice", tone: "info", text: "Earlier history was compacted by Mecatl." });
    }
  }

  function applyEvent(event: Event): void {
    switch (event.kind) {
      case "message.delta":
        appendAssistantDelta(event.text);
        return;
      case "tool.call":
        append({
          kind: "tool",
          callId: event.payload.id,
          name: event.payload.name,
          args: event.payload.args,
          result: null,
        });
        return;
      case "tool.result":
        foldToolResult(event.payload.callId, {
          content: event.payload.content,
          isError: event.payload.isError,
        });
        return;
      case "permission.ask":
        append({
          kind: "ask",
          askId: event.payload.askId,
          tool: event.payload.tool,
          args: event.payload.args,
          reason: event.payload.reason,
          state: "pending",
        });
        return;
      case "permission.retract":
        setAskState(event.payload.askId, "retracted");
        return;
      case "approval":
        setAskState(
          event.payload.askId,
          event.payload.verdict === "deny"
            ? "deny"
            : event.payload.allowAlways ? "allow_always" : "allow_once",
        );
        return;
      case "user_prompt":
        append({ kind: "prompt", text: event.payload.text });
        return;
      case "steer":
        append({ kind: "notice", tone: "info", text: `Steered: ${event.payload.text}` });
        return;
      case "compaction":
        append({ kind: "notice", tone: "info", text: "Mecatl compacted this conversation." });
        return;
      case "no_progress":
        append({ kind: "notice", tone: "info", text: "The model stopped making progress." });
        return;
      case "result":
        foldResult(event.payload.text, event.payload.error, event.payload.stop);
        return;
      default:
        return;
    }
  }

  return {
    entries,
    pendingAsk(): AgentAskEntry | null {
      for (let idx = entries.length - 1; idx >= 0; idx -= 1) {
        const entry = entries[idx];
        if (entry !== undefined && entry.kind === "ask" && entry.state === "pending") return entry;
      }
      return null;
    },
    clear(): void {
      setEntries([]);
      nextId = 0;
    },
    loadTranscript,
    appendPrompt(text: string): void {
      append({ kind: "prompt", text });
    },
    appendNotice(tone: AgentNoticeTone, text: string): void {
      append({ kind: "notice", tone, text });
    },
    applyEvent,
    resolveAsk: setAskState,
    settleStreaming(): void {
      setEntries(produce((list) => {
        const tail = list[list.length - 1];
        if (tail !== undefined && tail.kind === "assistant") tail.streaming = false;
      }));
    },
  };
}

function lastToolEntry(list: AgentTimelineEntry[], callId: string): AgentToolEntry | null {
  for (let idx = list.length - 1; idx >= 0; idx -= 1) {
    const entry = list[idx];
    if (entry !== undefined && entry.kind === "tool" && entry.callId === callId) return entry;
  }
  return null;
}
