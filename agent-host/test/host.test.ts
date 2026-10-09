import { BACKGROUND_CONTEXT } from "@earendil-works/chord/context";
import { Harness, MemoryStorage } from "@earendil-works/pi-durable";
import { NodeExecutionEnv } from "@earendil-works/pi-durable/env/node";
import { createModels, fauxAssistantMessage, fauxProvider, fauxText, fauxToolCall } from "@earendil-works/pi-ai";
import { DatabaseSync } from "node:sqlite";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, expect, test } from "vitest";
import { createAgentHostServices } from "../src/host.ts";
import { SqliteCredentialStore } from "../src/host-db.ts";
import { createAgentRegistry } from "../src/registry.ts";

const cleanup: Array<() => Promise<void>> = [];
afterEach(async () => { await Promise.all(cleanup.splice(0).map(remove => remove())); });

test("serves authenticated durable agent events and real worker tools", async () => {
  const directory = await mkdtemp(join(tmpdir(), "roost-agent-host-"));
  cleanup.push(() => rm(directory, { recursive: true, force: true }));
  await writeFile(join(directory, "visible.txt"), "real file\n");
  const db = new DatabaseSync(join(directory, "host.sqlite"));
  db.exec("CREATE TABLE conversations(id TEXT PRIMARY KEY,title TEXT NOT NULL,created_ms INTEGER NOT NULL,updated_ms INTEGER NOT NULL); CREATE TABLE settings(key TEXT PRIMARY KEY,value TEXT NOT NULL); CREATE TABLE credentials(provider TEXT PRIMARY KEY,json TEXT NOT NULL);");
  const credentials = new SqliteCredentialStore(db);
  const faux = fauxProvider({ provider: "faux", models: [{ id: "faux-model", reasoning: false }] });
  faux.setResponses([
    fauxAssistantMessage([fauxText("Inspecting the directory."), fauxToolCall("bash", { command: "ls" }, { id: "bash-call" })], { stopReason: "toolUse" }),
    fauxAssistantMessage([fauxText("Directory inspected.")]),
  ]);
  const models = createModels({ credentials });
  models.setProvider(faux.provider);
  const harness = await Harness.open(new MemoryStorage(), { models, registry: createAgentRegistry(), env: () => new NodeExecutionEnv({ cwd: directory }) }, BACKGROUND_CONTEXT);
  harness.resume();
  const modelService = { models, catalog: async () => ({ models: [{ provider: "faux", model_id: "faux-model", name: "Faux", reasoning: false, available: true }], providers: [], thinking_levels: ["off"], default_model: null }), setApiKey: async () => undefined };
  const services = createAgentHostServices({ harness, db, config: { bind: "127.0.0.1:0", secret: "s".repeat(32), dataDir: directory, coordinatorInternalUrl: "ws://127.0.0.1:4113" }, models: modelService });
  cleanup.push(async () => { await services.http.close(); db.close(); });
  const address = await services.http.listen();
  const root = `http://127.0.0.1:${address.port}`;
  const unauthorized = await fetch(`${root}/v1/models`);
  expect(unauthorized.status).toBe(401);
  const catalogResponse = await fetch(`${root}/v1/models`, { headers: { authorization: `Bearer ${"s".repeat(32)}` } });
  expect((await catalogResponse.json() as { models: { model_id: string; available: boolean }[] }).models).toContainEqual(expect.objectContaining({ model_id: "faux-model", available: true }));
  const eventResponse = await fetch(`${root}/v1/events`, { headers: { authorization: `Bearer ${"s".repeat(32)}` } });
  expect(eventResponse.status).toBe(200);
  const reader = eventResponse.body!.getReader();
  cleanup.push(async () => { await reader.cancel(); });
  let remainder = "";
  const lines: Record<string, any>[] = [];
  const consume = async () => {
    while (lines.length < 3 || !lines.some(line => line.type === "chat" && line.events?.some((event: Record<string, any>) => event.type === "item" && event.item.kind === "tool" && !event.item.running))) {
      const result = await reader.read();
      if (result.done) break;
      remainder += new TextDecoder().decode(result.value, { stream: true });
      const complete = remainder.split("\n"); remainder = complete.pop() ?? "";
      for (const line of complete) if (line) lines.push(JSON.parse(line) as Record<string, any>);
    }
  };
  const conversationResponse = await fetch(`${root}/v1/conversations`, { method: "POST", headers: { authorization: `Bearer ${"s".repeat(32)}`, "content-type": "application/json" }, body: JSON.stringify({ worker_fp: "worker", worker_label: "test", worker_os: "linux", cwd: directory, model: { provider: "faux", model_id: "faux-model" } }) });
  const created = await conversationResponse.json() as { id?: string; error?: { message: string } };
  expect(conversationResponse.status, created.error?.message).toBe(201);
  const conversation = created as { id: string };
  await fetch(`${root}/v1/conversations/${conversation.id}/submit`, { method: "POST", headers: { authorization: `Bearer ${"s".repeat(32)}`, "content-type": "application/json" }, body: JSON.stringify({ text: "List this directory", request_id: "request-1" }) });
  await consume();
  const flattened = lines.flatMap(line => line.type === "chat" ? line.events.map((event: Record<string, any>) => ({ ...event, conversation_id: line.conversation_id })) : [line]);
  expect(lines[0]?.type).toBe("hello");
  expect(lines[1]?.type).toBe("conversations");
  const resetIndex = flattened.findIndex(event => event.type === "reset");
  const userIndex = flattened.findIndex(event => event.type === "item" && event.item.kind === "user");
  const deltaIndex = flattened.findIndex(event => event.type === "text_delta");
  const toolIndex = flattened.findIndex(event => event.type === "item" && event.item.kind === "tool" && !event.item.running);
  expect(resetIndex).toBeGreaterThan(-1);
  expect(userIndex).toBeGreaterThan(resetIndex);
  expect(deltaIndex).toBeGreaterThan(userIndex);
  expect(toolIndex).toBeGreaterThan(deltaIndex);
  expect(flattened[toolIndex]?.item.output).toContain("visible.txt");
  await reader.cancel();
});
