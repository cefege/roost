import type { AgentEvent, SnapshotEvent } from "@earendil-works/pi-durable";

type JsonRecord = Record<string, any>;
export type ChatEvent = JsonRecord & { type: string };

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
function entryItem(entry: JsonRecord): JsonRecord | undefined {
  const message = entry.model?.[0] as JsonRecord | undefined;
  if (!message) return undefined;
  if (message.role === "user") return { id: String(entry.id), kind: "user", text: messageText(message) };
  if (message.role === "assistant") return assistantItem(String(entry.id), message, false);
  if (message.role === "toolResult") return { id: String(entry.id), kind: "tool", call_id: message.toolCallId, tool_name: message.toolName, args_json: "{}", output: Array.isArray(message.content) ? message.content.map((part: JsonRecord) => part.text ?? "").join("") : "", is_error: Boolean(message.isError), running: false };
  return undefined;
}

export class ChatTranslator {
  runState = "idle";
  private liveIndex = 0;
  private assistantId: string | undefined;
  private readonly blockLengths: Record<number, number> = {};
  snapshot(value: SnapshotEvent): ChatEvent[] {
    const snapshot = value as JsonRecord;
    this.runState = snapshot.run ? "running" : "idle";
    const items: JsonRecord[] = snapshot.entries.map((entry: JsonRecord) => entryItem(entry)).filter((item: JsonRecord | undefined): item is JsonRecord => item !== undefined);
    if (snapshot.generation?.message) items.push(assistantItem(`generation-${snapshot.generation.attempt}`, snapshot.generation.message, true));
    for (const slot of snapshot.tools) items.push({ id: `tool-${slot.callId}`, kind: "tool", call_id: slot.callId, tool_name: slot.name, args_json: "{}", output: slot.output ?? "", is_error: false, running: slot.status === "running" });
    return [{ type: "reset", transcript: { items, run_state: this.runState, error: null, model: snapshot.agent.model ? { provider: snapshot.agent.model.provider, model_id: snapshot.agent.model.modelId } : null, thinking_level: snapshot.agent.thinkingLevel ?? null, usage: this.usageTotals(snapshot.usage) } }];
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
            for (const [blockIndex, block] of (change.message as JsonRecord).content.entries()) {
              if (block.type === "text" || block.type === "thinking") {
                const priorLength = this.blockLengths[blockIndex] ?? 0;
                const text = String(block.text ?? "");
                if (text.length > priorLength) updates.push({ type: block.type === "text" ? "text_delta" : "thinking_delta", item_id: id, block: blockIndex, delta: text.slice(priorLength) });
                this.blockLengths[blockIndex] = text.length;
              } else if (block.type === "toolCall") {
                updates.push({ type: "block_set", item_id: id, block: blockIndex, value: { type: "tool_call", call_id: block.id, tool_name: block.name, args_json: JSON.stringify(block.arguments ?? {}) } });
              }
            }
          } else if ("block" in change) {
            const block = change.block as JsonRecord;
            const blockValue = block.type === "toolCall" ? { type: "tool_call", call_id: block.id, tool_name: block.name, args_json: JSON.stringify(block.arguments ?? {}) } : block;
            updates.push({ type: "block_set", item_id: id, block: change.contentIndex, value: blockValue });
            if (block.type === "text" || block.type === "thinking") this.blockLengths[change.contentIndex] = String(block.text ?? "").length;
          }
        }
        return updates;
      }
      case "message_end": {
        const item = entryItem(event.entry);
        if (!item || !this.assistantId) return [];
        const result = { ...item, id: this.assistantId, streaming: false };
        this.assistantId = undefined;
        return [{ type: "item", item: result }];
      }
      case "tool_execution_start": return [{ type: "item", item: { id: `tool-${event.toolCallId}`, kind: "tool", call_id: event.toolCallId, tool_name: event.toolName, args_json: JSON.stringify(event.args), output: "", is_error: false, running: true } }];
      case "tool_execution_update": {
        const output = event.output as JsonRecord | undefined;
        return [{ type: "tool_output", item_id: `tool-${event.toolCallId}`, ...(output && "set" in output ? { set: output.set } : { trim_start: output?.trimStart ?? null, append: output?.append ?? null }) }];
      }
      case "tool_execution_end": {
        const result = event.entry ? entryItem(event.entry) : undefined;
        const content = event.entry?.model?.[0]?.content;
        const output = Array.isArray(content) ? content.map((part: JsonRecord) => part.text ?? "").join("") : "";
        return [{ type: "item", item: { id: `tool-${event.toolCallId}`, kind: "tool", call_id: event.toolCallId, tool_name: event.toolName, args_json: "{}", output: result?.output ?? output, is_error: Boolean(event.entry?.model?.[0]?.isError), running: false } }];
      }
      case "run_start": this.runState = "running"; return [{ type: "run_state", run_state: "running", error: null }];
      case "run_end": this.runState = "idle"; return [{ type: "run_state", run_state: "idle", error: null }];
      case "task_failed": this.runState = "failed"; return [{ type: "run_state", run_state: "failed", error: event.message }];
      case "auto_retry_start": return [{ type: "run_state", run_state: "running", error: `Retrying (attempt ${event.attempt}): ${event.errorMessage}` }];
      case "auto_retry_end": return [{ type: "run_state", run_state: "running", error: null }];
      case "agent_changed": return [{ type: "agent", model: event.agent.model ?? null, thinking_level: event.agent.thinkingLevel ?? null }];
      case "usage_changed": return [{ type: "usage", usage: this.usageTotals(event.usage) }];
      default: return [];
    }
  }
}
