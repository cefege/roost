import type { AgentEvent, SnapshotEvent } from "@earendil-works/pi-durable";

type JsonRecord = Record<string, any>;
export type ChatEvent = JsonRecord & { type: string };
type ToolReference = { name: string; args_json: string };

function contentBlocks(message: JsonRecord): JsonRecord[] {
  const parts = Array.isArray(message.content) ? message.content as JsonRecord[] : [];
  const blocks: JsonRecord[] = [];
  for (const part of parts) {
    if (part.type === "text" || part.type === "thinking") blocks.push({ type: part.type, text: part.text ?? "" });
    else if (part.type === "toolCall") blocks.push({ type: "tool_call", call_id: part.id, tool_name: part.name, args_json: JSON.stringify(part.arguments ?? {}) });
  }
  return blocks;
}
function messageText(message: JsonRecord): string {
  if (typeof message.content === "string") return message.content;
  return Array.isArray(message.content) ? message.content.map((part: JsonRecord | string) => typeof part === "string" ? part : part.text ?? "").join("") : "";
}
function assistantItem(id: string, message: JsonRecord, streaming: boolean): JsonRecord {
  return { id, kind: "assistant", blocks: contentBlocks(message), streaming, error: message.errorMessage ?? null };
}
function toolReferences(entries: JsonRecord[]): Record<string, ToolReference> {
  const references: Record<string, ToolReference> = {};
  for (const entry of entries) {
    for (const message of entry.model ?? []) {
      if (message.role !== "assistant" || !Array.isArray(message.content)) continue;
      for (const block of message.content) {
        if (block.type === "toolCall") references[block.id] = { name: block.name, args_json: JSON.stringify(block.arguments ?? {}) };
      }
    }
  }
  return references;
}
function entryItem(entry: JsonRecord, references: Record<string, ToolReference>): JsonRecord | undefined {
  const message = entry.model?.[0] as JsonRecord | undefined;
  if (!message) return undefined;
  if (message.role === "user") return { id: String(entry.id), kind: "user", text: messageText(message) };
  if (message.role === "assistant") return assistantItem(String(entry.id), message, false);
  if (message.role === "toolResult") {
    const reference = references[message.toolCallId];
    return { id: String(entry.id), kind: "tool", call_id: message.toolCallId, tool_name: reference?.name ?? message.toolName, args_json: reference?.args_json ?? "{}", output: messageText(message), is_error: Boolean(message.isError), running: false };
  }
  return undefined;
}

export class ChatTranslator {
  runState = "idle";
  error: string | null = null;
  model: { provider: string; model_id: string } | null = null;
  thinkingLevel: string | null = null;
  private liveIndex = 0;
  private assistantId: string | undefined;
  private readonly blockLengths: Record<number, number> = {};
  private readonly toolArguments: Record<string, ToolReference> = {};

  snapshot(value: SnapshotEvent): ChatEvent[] {
    const snapshot = value as JsonRecord;
    this.runState = snapshot.run ? "running" : "idle";
    this.error = null;
    this.model = snapshot.agent.model ? { provider: snapshot.agent.model.provider, model_id: snapshot.agent.model.modelId } : null;
    this.thinkingLevel = snapshot.agent.thinkingLevel ?? null;
    for (const key of Object.keys(this.toolArguments)) delete this.toolArguments[key];
    Object.assign(this.toolArguments, toolReferences(snapshot.entries));
    const items: JsonRecord[] = snapshot.entries.map((entry: JsonRecord) => entryItem(entry, this.toolArguments)).filter((item: JsonRecord | undefined): item is JsonRecord => item !== undefined);
    if (snapshot.generation?.message) items.push(assistantItem(`generation-${snapshot.generation.attempt}`, snapshot.generation.message, true));
    for (const slot of snapshot.tools) {
      const reference = this.toolArguments[slot.callId];
      items.push({ id: `tool-${slot.callId}`, kind: "tool", call_id: slot.callId, tool_name: reference?.name ?? slot.name, args_json: reference?.args_json ?? "{}", output: slot.output ?? "", is_error: false, running: slot.status === "running" });
    }
    return [{ type: "reset", transcript: { items, run_state: this.runState, error: this.error, model: this.model, thinking_level: this.thinkingLevel, usage: this.usageTotals(snapshot.usage) } }];
  }

  private usageTotals(state: JsonRecord): JsonRecord {
    const totals = { input_tokens: 0, output_tokens: 0, cost_usd: 0 };
    for (const group of [state.models ?? {}, state.tools ?? {}]) {
      for (const usage of Object.values(group) as JsonRecord[]) {
        totals.input_tokens += Number(usage.input ?? 0);
        totals.output_tokens += Number(usage.output ?? 0);
        totals.cost_usd += Number(usage.cost?.total ?? 0);
      }
    }
    return totals;
  }

  translate(value: AgentEvent): ChatEvent[] {
    const event = value as JsonRecord;
    const itemId = `live-${++this.liveIndex}`;
    switch (event.type) {
      case "snapshot": return this.snapshot(value as SnapshotEvent);
      case "message_start": {
        const message = event.message as JsonRecord;
        if (message.role === "assistant") {
          this.assistantId = itemId;
          for (const key of Object.keys(this.blockLengths)) delete this.blockLengths[Number(key)];
          const blocks = contentBlocks(message).map(block => block.type === "text" || block.type === "thinking" ? { ...block, text: "" } : block);
          const updates: ChatEvent[] = [{ type: "item", item: { id: itemId, kind: "assistant", blocks, streaming: true, error: null } }];
          const parts = Array.isArray(message.content) ? message.content as JsonRecord[] : [];
          for (const [blockIndex, block] of parts.entries()) {
            if ((block.type === "text" || block.type === "thinking") && block.text) {
              updates.push({ type: block.type === "text" ? "text_delta" : "thinking_delta", item_id: itemId, block: blockIndex, delta: block.text });
              this.blockLengths[blockIndex] = String(block.text).length;
            }
            if (block.type === "toolCall") this.toolArguments[block.id] = { name: block.name, args_json: JSON.stringify(block.arguments ?? {}) };
          }
          return updates;
        }
        if (message.role === "user") return [{ type: "item", item: { id: itemId, kind: "user", text: messageText(message) } }];
        return [];
      }
      case "message_update": {
        const updates: ChatEvent[] = [];
        for (const change of event.changes as JsonRecord[]) {
          const id = this.assistantId;
          if (!id) continue;
          if (change.type === "text_delta" || change.type === "thinking_delta") {
            updates.push({ type: change.type, item_id: id, block: change.contentIndex, delta: change.delta });
            this.blockLengths[change.contentIndex] = (this.blockLengths[change.contentIndex] ?? 0) + change.delta.length;
          } else if (change.type === "message") {
            const message = change.message as JsonRecord;
            for (const [blockIndex, block] of message.content.entries()) {
              if (block.type === "text" || block.type === "thinking") this.blockLengths[blockIndex] = String(block.text ?? "").length;
              else if (block.type === "toolCall") this.toolArguments[block.id] = { name: block.name, args_json: JSON.stringify(block.arguments ?? {}) };
            }
            updates.push({ type: "item", item: assistantItem(id, message, true) });
          } else if ("block" in change) {
            const block = change.block as JsonRecord;
            if (block.type === "toolCall") this.toolArguments[block.id] = { name: block.name, args_json: JSON.stringify(block.arguments ?? {}) };
            const blockValue = block.type === "toolCall" ? { type: "tool_call", call_id: block.id, tool_name: block.name, args_json: this.toolArguments[block.id]!.args_json } : block;
            updates.push({ type: "block_set", item_id: id, block: change.contentIndex, value: blockValue });
            if (block.type === "text" || block.type === "thinking") this.blockLengths[change.contentIndex] = String(block.text ?? "").length;
          }
        }
        return updates;
      }
      case "message_end": {
        const item = entryItem(event.entry, this.toolArguments);
        if (!item || !this.assistantId) return [];
        const result = { ...item, id: this.assistantId, streaming: false };
        this.assistantId = undefined;
        return [{ type: "item", item: result }];
      }
      case "tool_execution_start": {
        const args = JSON.stringify(event.args);
        this.toolArguments[event.toolCallId] = { name: event.toolName, args_json: args };
        return [{ type: "item", item: { id: `tool-${event.toolCallId}`, kind: "tool", call_id: event.toolCallId, tool_name: event.toolName, args_json: args, output: "", is_error: false, running: true } }];
      }
      case "tool_execution_update": {
        const output = event.output as JsonRecord | undefined;
        return [{ type: "tool_output", item_id: `tool-${event.toolCallId}`, ...(output && "set" in output ? { set: output.set } : { trim_start: output?.trimStart ?? null, append: output?.append ?? null }) }];
      }
      case "tool_execution_end": {
        const result = event.entry ? entryItem(event.entry, this.toolArguments) : undefined;
        const content = event.entry?.model?.[0]?.content;
        const output = Array.isArray(content) ? content.map((part: JsonRecord) => part.text ?? "").join("") : "";
        const reference = this.toolArguments[event.toolCallId];
        return [{ type: "item", item: { id: `tool-${event.toolCallId}`, kind: "tool", call_id: event.toolCallId, tool_name: reference?.name ?? event.toolName, args_json: reference?.args_json ?? "{}", output: result?.output ?? output, is_error: Boolean(event.entry?.model?.[0]?.isError), running: false } }];
      }
      case "run_start": this.runState = "running"; this.error = null; return [{ type: "run_state", run_state: this.runState, error: this.error }];
      case "run_end": this.runState = "idle"; this.error = null; return [{ type: "run_state", run_state: this.runState, error: this.error }];
      case "task_failed": this.runState = "failed"; this.error = event.message; return [{ type: "run_state", run_state: this.runState, error: this.error }];
      case "auto_retry_start": this.runState = "running"; this.error = `Retrying (attempt ${event.attempt}): ${event.errorMessage}`; return [{ type: "run_state", run_state: this.runState, error: this.error }];
      case "auto_retry_end": this.runState = "running"; this.error = null; return [{ type: "run_state", run_state: this.runState, error: this.error }];
      case "agent_changed":
        this.model = event.agent.model ? { provider: event.agent.model.provider, model_id: event.agent.model.modelId } : null;
        this.thinkingLevel = event.agent.thinkingLevel ?? null;
        return [{ type: "agent", model: this.model, thinking_level: this.thinkingLevel }];
      case "usage_changed": return [{ type: "usage", usage: this.usageTotals(event.usage) }];
      default: return [];
    }
  }
}
