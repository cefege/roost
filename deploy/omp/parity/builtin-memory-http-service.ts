// Owns an isolated external Hindsight HTTP fixture, not OMP memory state or synthesis.
// The canonical source HindsightApi calls this loopback server without substitution.
// Exact request stages reject undeclared traffic; snapshots expose observed storage.
import { isDeepStrictEqual } from "node:util";
import type { MentalModelHistoryEntry, MentalModelSummary } from "@omp-source/packages/coding-agent/src/hindsight/client";
import { objectValue } from "./runtime";

export interface MemoryHttpModel extends MentalModelSummary {
  refreshContent: string;
}
export interface MemoryHttpExpectedRequest {
  method: "GET" | "PUT" | "POST" | "DELETE";
  path: string;
  query: Record<string, string>;
  body: Record<string, unknown> | null;
  failure?: { status: number; detail: string };
  completeOperations?: boolean;
}
export interface MemoryHttpStage {
  name: string;
  requests: MemoryHttpExpectedRequest[];
}
export interface MemoryHttpServiceInput {
  bankId: string;
  apiToken: string;
  initialModels: MemoryHttpModel[];
  createdContent: Record<string, string>;
  tape: MemoryHttpStage[];
}
export interface MemoryHttpRequestCapture {
  method: string;
  path: string;
  query: Record<string, string>;
  body: unknown;
  status: number;
  stage: string;
}
export interface MemoryHttpOperation {
  id: string;
  kind: "create" | "refresh";
  modelId: string;
  status: "pending" | "completed";
  content: string;
}
export interface MemoryHttpSnapshot {
  evidenceClass: "external-service-fixture";
  requests: MemoryHttpRequestCapture[];
  requestCount: number;
  modelIds: string[];
  models: Record<string, MentalModelSummary>;
  history: Record<string, MentalModelHistoryEntry[]>;
  operations: MemoryHttpOperation[];
  bank: { id: string; exists: boolean; reflectMission?: string; retainMission?: string };
  consumedStages: string[];
  remainingStages: string[];
  violations: string[];
}
export interface MemoryHttpService {
  url: string;
  snapshot(): MemoryHttpSnapshot;
  assertConsumed(): void;
  stop(): Promise<void>;
}

export function parseMemoryHttpServiceInput(input: Record<string, unknown>): MemoryHttpServiceInput {
  const readString = (value: unknown, label: string): string => {
    if (typeof value !== "string") throw new Error(`Hindsight fixture ${label} must be a string`);
    return value;
  };
  const readTrigger = (value: unknown): NonNullable<MentalModelSummary["trigger"]> => {
    const trigger = objectValue(value, "Hindsight trigger");
    if (trigger.mode !== undefined && trigger.mode !== "full" && trigger.mode !== "delta") throw new Error("Invalid Hindsight trigger mode");
    if (trigger.refresh_after_consolidation !== undefined && typeof trigger.refresh_after_consolidation !== "boolean") throw new Error("Invalid Hindsight auto-refresh trigger");
    return { ...(trigger.mode === undefined ? {} : { mode: trigger.mode }), ...(trigger.refresh_after_consolidation === undefined ? {} : { refresh_after_consolidation: trigger.refresh_after_consolidation }) };
  };
  if (!Array.isArray(input.initialModels) || !Array.isArray(input.tape)) throw new Error("Hindsight fixture requires initialModels and tape arrays");
  const initialModels = input.initialModels.map((value: unknown): MemoryHttpModel => {
    const model = objectValue(value, "Hindsight model");
    const parsed: MemoryHttpModel = { id: readString(model.id, "model id"), bank_id: readString(model.bank_id, "bank_id"), name: readString(model.name, "name"), refreshContent: readString(model.refreshContent, "refreshContent") };
    if (model.content !== undefined) parsed.content = readString(model.content, "content");
    if (model.source_query !== undefined) parsed.source_query = readString(model.source_query, "source_query");
    if (model.tags !== undefined) {
      if (!Array.isArray(model.tags)) throw new Error("Hindsight model tags must be an array");
      parsed.tags = model.tags.map((tag: unknown) => readString(tag, "tag"));
    }
    if (model.trigger !== undefined) parsed.trigger = readTrigger(model.trigger);
    if (model.max_tokens !== undefined) {
      if (typeof model.max_tokens !== "number" || !Number.isFinite(model.max_tokens)) throw new Error("Invalid Hindsight max_tokens");
      parsed.max_tokens = model.max_tokens;
    }
    return parsed;
  });
  const createdContent = Object.fromEntries(Object.entries(objectValue(input.createdContent, "Hindsight createdContent")).map(([id, content]) => [id, readString(content, `createdContent.${id}`)]));
  const tape = input.tape.map((value: unknown): MemoryHttpStage => {
    const stage = objectValue(value, "Hindsight stage");
    if (!Array.isArray(stage.requests) || stage.requests.length === 0) throw new Error("Hindsight stage requires requests");
    const requests = stage.requests.map((value: unknown): MemoryHttpExpectedRequest => {
      const request = objectValue(value, "Hindsight request");
      const method = request.method;
      if (method !== "GET" && method !== "PUT" && method !== "POST" && method !== "DELETE") throw new Error("Invalid Hindsight method");
      const query = Object.fromEntries(Object.entries(objectValue(request.query, "Hindsight query")).map(([key, value]) => [key, readString(value, `query.${key}`)]));
      const parsed: MemoryHttpExpectedRequest = { method, path: readString(request.path, "path"), query, body: request.body === null ? null : objectValue(request.body, "Hindsight request body") };
      if (request.failure !== undefined) {
        const failure = objectValue(request.failure, "Hindsight failure");
        if (typeof failure.status !== "number" || !Number.isInteger(failure.status) || failure.status < 400 || failure.status > 599) throw new Error("Invalid Hindsight failure status");
        parsed.failure = { status: failure.status, detail: readString(failure.detail, "failure detail") };
      }
      if (request.completeOperations !== undefined) {
        if (typeof request.completeOperations !== "boolean") throw new Error("Invalid Hindsight completion directive");
        parsed.completeOperations = request.completeOperations;
      }
      return parsed;
    });
    return { name: readString(stage.name, "stage name"), requests };
  });
  return { bankId: readString(input.bankId, "bankId"), apiToken: readString(input.apiToken, "apiToken"), initialModels, createdContent, tape };
}

export async function startBuiltinMemoryHttpService(input: MemoryHttpServiceInput, onSnapshot?: (snapshot: MemoryHttpSnapshot) => void): Promise<MemoryHttpService> {
  const owned = structuredClone(input);
  const models = new Map<string, MentalModelSummary>();
  const refreshContent = new Map<string, string>();
  const history: Record<string, MentalModelHistoryEntry[]> = {};
  for (const model of owned.initialModels) {
    if (model.bank_id !== owned.bankId || models.has(model.id)) throw new Error(`Invalid initial Hindsight model ${model.id}`);
    const { refreshContent: nextContent, ...wire } = model;
    models.set(model.id, wire);
    refreshContent.set(model.id, nextContent);
    history[model.id] = [];
  }
  const requests: MemoryHttpRequestCapture[] = [];
  const operations: MemoryHttpOperation[] = [];
  const violations: string[] = [];
  const consumedStages: string[] = [];
  const bank: MemoryHttpSnapshot["bank"] = { id: owned.bankId, exists: false };
  let stageIndex = 0;
  let stopped = false;
  const root = `/v1/default/banks/${encodeURIComponent(owned.bankId)}`;
  const modelRoot = `${root}/mental-models`;
  const timestamp = (): string => new Date(Date.UTC(2026, 0, 1, 0, 0, operations.length)).toISOString();
  const snapshot = (): MemoryHttpSnapshot => structuredClone({ evidenceClass: "external-service-fixture", requests, requestCount: requests.length, modelIds: [...models.keys()].sort(), models: Object.fromEntries(models), history, operations, bank, consumedStages, remainingStages: owned.tape.slice(stageIndex).map(stage => stage.name), violations });

  function completeOperations(): void {
    for (const operation of operations) {
      if (operation.status !== "pending") continue;
      const model = models.get(operation.modelId);
      if (!model) throw new Error(`Pending operation lost model ${operation.modelId}`);
      if (operation.kind === "refresh") history[model.id]!.unshift({ previous_content: model.content ?? null, changed_at: timestamp() });
      model.content = operation.content;
      model.last_refreshed_at = timestamp();
      operation.status = "completed";
    }
  }
  function enqueue(kind: MemoryHttpOperation["kind"], modelId: string, content: string): Response {
    const id = `parity-memory-operation-${operations.length + 1}`;
    operations.push({ id, kind, modelId, status: "pending", content });
    return Response.json({ operation_id: id });
  }
  function wireModel(model: MentalModelSummary, detail: string | undefined): MentalModelSummary {
    if (detail !== "metadata") return structuredClone(model);
    const { content: _content, source_query: _query, trigger: _trigger, max_tokens: _tokens, ...metadata } = model;
    return metadata;
  }
  function dispatch(expected: MemoryHttpExpectedRequest): Response {
    const { method, path, query, body } = expected;
    if (expected.failure) return Response.json({ detail: expected.failure.detail }, { status: expected.failure.status });
    if (expected.completeOperations) completeOperations();
    if (method === "PUT" && path === root && body) {
      bank.exists = true;
      if (typeof body.reflect_mission === "string") bank.reflectMission = body.reflect_mission;
      if (typeof body.retain_mission === "string") bank.retainMission = body.retain_mission;
      return Response.json({ bank_id: owned.bankId });
    }
    if (!bank.exists) throw new Error("Hindsight bank must be PUT before model access");
    if (method === "GET" && path === modelRoot) {
      const items = [...models.values()].map(model => wireModel(model, query.detail));
      return Response.json({ items, total: items.length });
    }
    if (method === "POST" && path === modelRoot && body) {
      const { id, name, source_query: sourceQuery, tags, max_tokens: maxTokens, trigger } = body;
      if (typeof id !== "string" || typeof name !== "string" || typeof sourceQuery !== "string" || models.has(id)) throw new Error("Invalid mental-model create payload");
      const content = owned.createdContent[id];
      if (typeof content !== "string") throw new Error(`Undeclared external create content for ${id}`);
      const model: MentalModelSummary = { id, bank_id: owned.bankId, name, source_query: sourceQuery, content: "", created_at: timestamp() };
      if (Array.isArray(tags) && tags.every(tag => typeof tag === "string")) model.tags = tags;
      if (typeof maxTokens === "number") model.max_tokens = maxTokens;
      if (trigger !== null && typeof trigger === "object" && !Array.isArray(trigger)) {
        const mode = "mode" in trigger ? trigger.mode : undefined;
        const auto = "refresh_after_consolidation" in trigger ? trigger.refresh_after_consolidation : undefined;
        if (mode !== undefined && mode !== "full" && mode !== "delta") throw new Error("Invalid mental-model trigger mode");
        if (auto !== undefined && typeof auto !== "boolean") throw new Error("Invalid mental-model auto-refresh trigger");
        model.trigger = { ...(mode === undefined ? {} : { mode }), ...(auto === undefined ? {} : { refresh_after_consolidation: auto }) };
      }
      models.set(id, model);
      history[id] = [];
      refreshContent.set(id, content);
      return enqueue("create", id, content);
    }
    const escapedModelRoot = modelRoot.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    const match = path.match(new RegExp(`^${escapedModelRoot}/([^/]+)(/(refresh|history))?$`));
    if (!match?.[1]) throw new Error(`Unsupported Hindsight route ${method} ${path}`);
    const id = decodeURIComponent(match[1]);
    const model = models.get(id);
    if (!model) return Response.json({ detail: "Mental model not found" }, { status: 404 });
    if (method === "GET" && match[3] === "history") return Response.json({ items: history[id] });
    if (method === "GET" && !match[3]) return Response.json(wireModel(model, query.detail));
    if (method === "DELETE" && !match[3]) {
      if (operations.some(operation => operation.modelId === id && operation.status === "pending")) throw new Error(`Cannot delete pending fixture model ${id}`);
      models.delete(id);
      delete history[id];
      refreshContent.delete(id);
      return Response.json({ deleted: true });
    }
    if (method === "POST" && match[3] === "refresh") {
      const content = refreshContent.get(id);
      if (content === undefined) throw new Error(`Undeclared refresh content for ${id}`);
      return enqueue("refresh", id, content);
    }
    throw new Error(`Unsupported Hindsight operation ${method} ${path}`);
  }
  const server = Bun.serve({
    hostname: "127.0.0.1", port: 0,
    async fetch(request) {
      const url = new URL(request.url);
      const query = Object.fromEntries(url.searchParams);
      const stage = owned.tape[stageIndex];
      const capture: MemoryHttpRequestCapture = { method: request.method, path: url.pathname, query, body: null, status: 500, stage: stage?.name ?? "undeclared" };
      requests.push(capture);
      try {
        if (request.headers.get("authorization") !== `Bearer ${owned.apiToken}`) throw new Error("Unexpected Hindsight authorization");
        if (request.headers.get("content-type") !== "application/json") throw new Error("Unexpected Hindsight content type");
        if ([...url.searchParams.keys()].length !== Object.keys(query).length) throw new Error("Duplicate Hindsight query parameter");
        const text = await request.text();
        capture.body = text ? JSON.parse(text) : null;
        const expectedIndex = stage?.requests.findIndex(expected => expected.method === capture.method && expected.path === capture.path && isDeepStrictEqual(expected.query, capture.query) && isDeepStrictEqual(expected.body, capture.body)) ?? -1;
        if (!stage || expectedIndex < 0) throw new Error(`Unexpected Hindsight request at stage ${capture.stage}: ${JSON.stringify(capture)}`);
        const expected = stage.requests.splice(expectedIndex, 1)[0];
        if (!expected) throw new Error("Hindsight tape request disappeared");
        const response = dispatch(expected);
        capture.status = response.status;
        if (stage.requests.length === 0) { consumedStages.push(stage.name); stageIndex++; }
        return response;
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        violations.push(message);
        return Response.json({ detail: message }, { status: 500 });
      } finally {
        onSnapshot?.(snapshot());
      }
    },
  });
  const assertConsumed = (): void => {
    if (violations.length) throw new Error(`Hindsight boundary violations: ${violations.join("; ")}`);
    if (stageIndex !== owned.tape.length) throw new Error(`Unconsumed Hindsight tape: ${owned.tape.slice(stageIndex).map(stage => stage.name).join(", ")}`);
    if (operations.some(operation => operation.status === "pending")) throw new Error("Unsettled Hindsight fixture operation");
  };
  onSnapshot?.(snapshot());
  return {
    url: server.url.origin,
    snapshot,
    assertConsumed,
    async stop() {
      if (stopped) return;
      stopped = true;
      await server.stop(true);
      assertConsumed();
    },
  };
}
