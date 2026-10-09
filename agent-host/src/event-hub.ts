import { watchEvents, type Harness, type AgentEventStream } from "@earendil-works/pi-durable";
import { BACKGROUND_CONTEXT } from "@earendil-works/chord/context";
import type { DatabaseSync } from "node:sqlite";
import type { Conversations } from "./conversations.ts";
import { ChatTranslator } from "./translate.ts";

export type HostLine = Record<string, unknown>;
type Subscriber = { write(line: HostLine): void; streams: Map<string, AgentEventStream>; initializing: boolean; pending: HostLine[] };

export class EventHub {
  private readonly harness: Harness;
  private readonly db: DatabaseSync;
  private readonly subscribers = new Set<Subscriber>();
  private conversations?: Conversations;
  constructor(harness: Harness, db: DatabaseSync) { this.harness = harness; this.db = db; }
  setConversations(conversations: Conversations): void { this.conversations = conversations; }
  emit(line: HostLine): void { for (const subscriber of this.subscribers) subscriber.write(line); }
  async connect(output: (line: HostLine) => void): Promise<() => void> {
    const subscriber: Subscriber = { write: output, streams: new Map(), initializing: true, pending: [] };
    subscriber.write = line => { if (subscriber.initializing) subscriber.pending.push(line); else output(line); };
    this.subscribers.add(subscriber);
    const rows = this.db.prepare("SELECT id FROM conversations ORDER BY created_ms").all() as { id: string }[];
    const summaries = await Promise.all(rows.map(async row => {
      const stream = await watchEvents(this.harness, Number(row.id) as never, BACKGROUND_CONTEXT);
      subscriber.streams.set(row.id, stream);
      return { summary: await this.conversations!.summary(row.id, stream.snapshot.run ? "running" : "idle"), stream, translator: new ChatTranslator() };
    }));
    output({ type: "hello", protocol: 1 });
    output({ type: "conversations", conversations: summaries.map(item => item.summary) });
    for (const { summary, stream, translator } of summaries) {
      const id = String(summary.id);
      output({ type: "chat", conversation_id: id, events: translator.snapshot(stream.snapshot) });
      stream.start(async events => {
        const translated = events.flatMap(event => translator.translate(event));
        if (translated.length) subscriber.write({ type: "chat", conversation_id: id, events: translated });
        this.db.prepare("UPDATE conversations SET updated_ms=? WHERE id=?").run(Date.now(), id);
        const updated = await this.conversations!.summary(id, translator.runState);
        subscriber.write({ type: "conversation", conversation: updated });
      });
    }
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
      subscriber.streams.set(id, stream);
      subscriber.write({ type: "conversation", conversation: await this.conversations.summary(id) });
      subscriber.write({ type: "chat", conversation_id: id, events: translator.snapshot(stream.snapshot) });
      stream.start(async batch => {
        const translated = batch.flatMap(event => translator.translate(event));
        if (translated.length) subscriber.write({ type: "chat", conversation_id: id, events: translated });
        this.db.prepare("UPDATE conversations SET updated_ms=? WHERE id=?").run(Date.now(), id);
        subscriber.write({ type: "conversation", conversation: await this.conversations!.summary(id, translator.runState) });
      });
    }
  }
  unwatch(id: string): void {
    for (const subscriber of this.subscribers) {
      const stream = subscriber.streams.get(id);
      if (stream) { void stream.stop(); subscriber.streams.delete(id); }
    }
  }
}
