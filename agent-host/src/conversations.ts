import type { DatabaseSync } from "node:sqlite";
import { BACKGROUND_CONTEXT } from "@earendil-works/chord/context";
import type { Harness, Conversation } from "@earendil-works/pi-durable";
import { RoostTarget, type RoostTargetState } from "./target-doc.ts";
import type { EventHub } from "./event-hub.ts";
import type { ModelsService } from "./models.ts";

export class Conversations {
  private readonly harness: Harness;
  private readonly db: DatabaseSync;
  private readonly models: ModelsService;
  private readonly hub: EventHub;
  constructor(harness: Harness, db: DatabaseSync, models: ModelsService, hub: EventHub) {
    this.harness = harness; this.db = db; this.models = models; this.hub = hub;
  }
  private async get(id: string): Promise<Conversation> {
    const conversation = await this.harness.conversation(Number(id) as never, BACKGROUND_CONTEXT);
    if (!conversation) throw Object.assign(new Error("conversation not found"), { status: 404, code: "not_found" });
    return conversation;
  }
  async summary(id: string, runState = "idle", error: string | null = null): Promise<Record<string, unknown>> {
    const conversation = await this.get(id);
    const target = await this.harness.snapshot(RoostTarget, Number(id) as never, BACKGROUND_CONTEXT);
    const index = this.db.prepare("SELECT title,created_ms,updated_ms FROM conversations WHERE id=?").get(id) as { title: string; created_ms: number; updated_ms: number } | undefined;
    const agent = await conversation.agent(BACKGROUND_CONTEXT);
    return { id, title: index?.title ?? "New conversation", worker_fp: target?.worker_fp ?? "", worker_label: target?.worker_label ?? "", cwd: target?.cwd ?? "", model: agent.model ? { provider: agent.model.provider, model_id: agent.model.modelId } : null, thinking_level: agent.thinkingLevel ?? null, run_state: runState, error, created_ms: index?.created_ms ?? Date.now(), updated_ms: index?.updated_ms ?? Date.now() };
  }
  async list(): Promise<Record<string, unknown>[]> {
    const rows = this.db.prepare("SELECT id FROM conversations ORDER BY updated_ms DESC").all() as { id: string }[];
    return Promise.all(rows.map(row => this.summary(row.id)));
  }
  async create(body: { worker_fp: string; worker_label: string; worker_os: string; cwd: string; model?: { provider: string; model_id: string } | null }): Promise<Record<string, unknown>> {
    const saved = this.db.prepare("SELECT value FROM settings WHERE key='default_model'").get() as { value: string } | undefined;
    const available = await this.models.models.getAvailable();
    const chosen = body.model ?? (saved ? JSON.parse(saved.value) as { provider: string; model_id: string } : available[0] ? { provider: available[0].provider, model_id: available[0].id } : undefined);
    if (!chosen) throw Object.assign(new Error("no model is signed in"), { status: 400, code: "invalid" });
    const target: RoostTargetState = { worker_fp: body.worker_fp, worker_label: body.worker_label, worker_os: body.worker_os, cwd: body.cwd };
    const conversation = await this.harness.createConversation({ ownership: { kind: "ownerless" }, agent: { model: { provider: chosen.provider, modelId: chosen.model_id } }, init: async (tx, id) => { Object.assign(await tx.doc(RoostTarget, id), target); } }, BACKGROUND_CONTEXT);
    const id = String(conversation.id);
    const now = Date.now();
    this.db.prepare("INSERT INTO conversations(id,title,created_ms,updated_ms) VALUES(?,?,?,?)").run(id, "New conversation", now, now);
    this.db.prepare("INSERT INTO settings(key,value) VALUES('default_model',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value").run(JSON.stringify(chosen));
    await this.hub.watch(id);
    return this.summary(id);
  }
  async submit(id: string, text: string, requestId: string): Promise<void> {
    const conversation = await this.get(id);
    const row = this.db.prepare("SELECT title FROM conversations WHERE id=?").get(id) as { title: string } | undefined;
    if (row?.title === "New conversation") {
      this.db.prepare("UPDATE conversations SET title=?,updated_ms=? WHERE id=?").run(text.replace(/\s+/g, " ").slice(0, 60), Date.now(), id);
      await this.hub.conversationChanged(id);
    }
    await conversation.submit({ type: "input", content: text, requestId, whenBusy: "steer" }, BACKGROUND_CONTEXT);
  }
  async configure(id: string, change: Record<string, unknown>): Promise<Record<string, unknown>> {
    const conversation = await this.get(id);
    const model = change.model as { provider: string; model_id: string } | undefined;
    if (model || change.thinking_level) {
      await conversation.configure({ ...(model ? { model: { provider: model.provider, modelId: model.model_id } } : {}), ...(change.thinking_level ? { thinkingLevel: change.thinking_level as never } : {}) }, BACKGROUND_CONTEXT);
      if (model) this.db.prepare("INSERT INTO settings(key,value) VALUES('default_model',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value").run(JSON.stringify(model));
    }
    const fields: Partial<RoostTargetState> = {};
    for (const key of ["worker_fp", "worker_label", "worker_os", "cwd"] as const) if (typeof change[key] === "string") fields[key] = change[key] as string;
    if (Object.keys(fields).length) await conversation.commit(async tx => { Object.assign(await tx.doc(RoostTarget, Number(id) as never), fields); }, BACKGROUND_CONTEXT);
    if (typeof change.title === "string") this.db.prepare("UPDATE conversations SET title=?,updated_ms=? WHERE id=?").run(change.title, Date.now(), id);
    return this.summary(id);
  }
  async abort(id: string): Promise<void> { await (await this.get(id)).abort(BACKGROUND_CONTEXT); }
  async delete(id: string): Promise<void> {
    const conversation = await this.get(id);
    await conversation.abort(BACKGROUND_CONTEXT);
    this.db.prepare("DELETE FROM conversations WHERE id=?").run(id);
    this.hub.unwatch(id);
    this.hub.emit({ type: "conversation_removed", id });
  }
}
