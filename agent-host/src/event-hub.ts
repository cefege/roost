import { watchEvents, type Harness, type AgentEventStream } from "@earendil-works/pi-durable";
import { BACKGROUND_CONTEXT } from "@earendil-works/chord/context";
import type { DatabaseSync } from "node:sqlite";
import type { Conversations } from "./conversations.ts";
import { ChatTranslator } from "./translate.ts";

export type HostLine = Record<string, unknown>;
type Subscriber = {
  write(line: HostLine): void;
  streams: Map<string, AgentEventStream>;
  translators: Map<string, ChatTranslator>;
  summaries: Map<string, string>;
  initializing: boolean;
  pending: HostLine[];
};

export class EventHub {
  private readonly harness: Harness;
  private readonly db: DatabaseSync;
  private readonly subscribers = new Set<Subscriber>();
  private conversations?: Conversations;
  constructor(harness: Harness, db: DatabaseSync) { this.harness = harness; this.db = db; }
  setConversations(conversations: Conversations): void { this.conversations = conversations; }
  emit(line: HostLine): void { for (const subscriber of this.subscribers) subscriber.write(line); }

  async connect(output: (line: HostLine) => void): Promise<() => void> {
    const subscriber: Subscriber = {
      write: output,
      streams: new Map(),
      translators: new Map(),
      summaries: new Map(),
      initializing: true,
      pending: [],
    };
    subscriber.write = line => { if (subscriber.initializing) subscriber.pending.push(line); else output(line); };
    this.subscribers.add(subscriber);
    const rows = this.db.prepare("SELECT id FROM conversations ORDER BY created_ms").all() as { id: string }[];
    const attached = await Promise.all(rows.map(async row => {
      const stream = await watchEvents(this.harness, Number(row.id) as never, BACKGROUND_CONTEXT);
      const translator = new ChatTranslator();
      translator.snapshot(stream.snapshot);
      return { id: row.id, stream, translator, summary: await this.conversations!.summary(row.id, translator.runState, translator.error) };
    }));
    output({ type: "hello", protocol: 1 });
    output({ type: "conversations", conversations: attached.map(item => item.summary) });
    for (const item of attached) await this.attachStream(subscriber, item.id, item.stream, item.translator, item.summary, false);
    subscriber.initializing = false;
    for (const line of subscriber.pending.splice(0)) output(line);
    return () => {
      this.subscribers.delete(subscriber);
      for (const stream of subscriber.streams.values()) void stream.stop();
    };
  }

  async watch(id: string): Promise<void> {
    if (!this.conversations) return;
    const row = this.db.prepare("SELECT id FROM conversations WHERE id=?").get(id) as { id: string } | undefined;
    if (!row) return;
    for (const subscriber of this.subscribers) {
      if (subscriber.streams.has(id)) continue;
      const stream = await watchEvents(this.harness, Number(id) as never, BACKGROUND_CONTEXT);
      const translator = new ChatTranslator();
      translator.snapshot(stream.snapshot);
      const summary = await this.conversations.summary(id, translator.runState, translator.error);
      await this.attachStream(subscriber, id, stream, translator, summary, true);
    }
  }

  async conversationChanged(id: string): Promise<void> {
    for (const subscriber of this.subscribers) {
      const translator = subscriber.translators.get(id);
      if (translator) await this.publishSummaryIfChanged(subscriber, id, translator);
    }
  }

  unwatch(id: string): void {
    for (const subscriber of this.subscribers) {
      const stream = subscriber.streams.get(id);
      if (stream) { void stream.stop(); subscriber.streams.delete(id); }
      subscriber.translators.delete(id);
      subscriber.summaries.delete(id);
    }
  }

  private async attachStream(subscriber: Subscriber, id: string, stream: AgentEventStream, translator: ChatTranslator, summary: Record<string, unknown>, emitSummary: boolean): Promise<void> {
    subscriber.streams.set(id, stream);
    subscriber.translators.set(id, translator);
    subscriber.summaries.set(id, this.summaryKey(summary));
    if (emitSummary) subscriber.write({ type: "conversation", conversation: summary });
    subscriber.write({ type: "chat", conversation_id: id, events: translator.snapshot(stream.snapshot) });
    stream.start(async batch => {
      const events = batch.flatMap(event => translator.translate(event));
      if (events.length) subscriber.write({ type: "chat", conversation_id: id, events });
      await this.publishSummaryIfChanged(subscriber, id, translator);
    });
  }

  private async publishSummaryIfChanged(subscriber: Subscriber, id: string, translator: ChatTranslator): Promise<void> {
    const current = await this.conversations!.summary(id, translator.runState, translator.error);
    const key = this.summaryKey(current);
    if (subscriber.summaries.get(id) === key) return;
    this.db.prepare("UPDATE conversations SET updated_ms=? WHERE id=?").run(Date.now(), id);
    const updated = await this.conversations!.summary(id, translator.runState, translator.error);
    subscriber.summaries.set(id, key);
    subscriber.write({ type: "conversation", conversation: updated });
  }

  private summaryKey(summary: Record<string, unknown>): string {
    return JSON.stringify([summary.title, summary.run_state, summary.error, summary.model, summary.thinking_level]);
  }
}
