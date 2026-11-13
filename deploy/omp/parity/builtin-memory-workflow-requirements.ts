// Owns strict external HTTP tapes and the registered Settings for memory workflows.
// The parent starts the service before the real session; no memory runtime is injected.
// Seed request payloads derive from the pinned production seed resolver.
import { resolveSeedsForScope } from "@omp-source/packages/coding-agent/src/hindsight/mental-models";
import type { BuiltinRequirements } from "./builtin-requirements";
import type { MemoryHttpExpectedRequest, MemoryHttpModel, MemoryHttpServiceInput, MemoryHttpStage } from "./builtin-memory-http-service";

export const memoryWorkflowData = {
  bankId: "parity-memory",
  autoId: "parity-auto",
  curatedId: "parity-curated",
  original: "PARITY_MEMORY_ORIGINAL",
  refreshed: "PARITY_MEMORY_REFRESHED",
  curated: "PARITY_MEMORY_CURATED",
  seeded: "PARITY_MEMORY_SEEDED",
  settings: {
    "memory.backend": "hindsight",
    "hindsight.bankId": "parity-memory",
    "hindsight.scoping": "global",
    "hindsight.mentalModelsEnabled": true,
    "hindsight.mentalModelAutoSeed": false,
    "hindsight.autoRecall": false,
    "hindsight.autoRetain": false,
  },
};

export const memoryWorkflowHooks = {
  evidenceClass: "external-service-fixture",
  configuration: "Start one fresh loopback service per leg before session creation; set hindsight.apiUrl and hindsight.apiToken via owned Settings using service.url/input.apiToken. Scrub inherited HINDSIGHT_* overrides. Never replace canonical HindsightApi, backend, HindsightSessionState, controller, or dispatcher.",
  initialSettlement: "Await actual session.settleMemoryBackend() and session.getHindsightSessionState()?.mentalModelsLoadPromise before submitting the initial slash command. Missing primary state is a hard prerequisite failure.",
  sessionCapture: "builtin.memory reads actual primary Hindsight state, snippet, and flushed owned Settings; active is real state existence, bankId/config are real fields; snippetIncludes original/refreshed/seeded/curated are actual substring checks against the declared PARITY_MEMORY markers. persistedSettings reads backend,bankId,scoping,mentalModelsEnabled,autoSeed from actual owned settings file.",
  httpCapture: "builtin.memory-http is the latest snapshot emitted by the real service callback, including exact requests with bodies/query/status, bank existence, models/history/operations and tape positions. No observer recreates this state from requirements or expected comparisons.",
  lifecycle: "Keep service and storage alive throughout all checkpoints in one leg. Every request must match the next tape stage (concurrent history GETs may arrive in either order). assertConsumed/stop fail on extra traffic, unconsumed declared requests, violations, or pending operations. All writes remain in owned fixture API storage; no claim about vendor Hindsight synthesis correctness.",
};

const root = `/v1/default/banks/${memoryWorkflowData.bankId}`;
const modelsPath = `${root}/mental-models`;
const autoPath = `${modelsPath}/${memoryWorkflowData.autoId}`;
const curatedPath = `${modelsPath}/${memoryWorkflowData.curatedId}`;

function list(detail: "metadata" | "content", completeOperations = false): MemoryHttpExpectedRequest {
  return { method: "GET", path: modelsPath, query: { detail, limit: "1000", offset: "0" }, body: null, ...(completeOperations ? { completeOperations } : {}) };
}
function model(path: string): MemoryHttpExpectedRequest {
  return { method: "GET", path, query: { detail: "content" }, body: null };
}
function stage(name: string, ...requests: MemoryHttpExpectedRequest[]): MemoryHttpStage {
  return { name, requests };
}
function refresh(path: string): MemoryHttpExpectedRequest {
  return { method: "POST", path: `${path}/refresh`, query: {}, body: null };
}
function failed(request: MemoryHttpExpectedRequest): MemoryHttpExpectedRequest {
  return { ...request, failure: { status: 503, detail: "PARITY_MEMORY_SERVICE_UNAVAILABLE" } };
}

const initialModels: MemoryHttpModel[] = [
  { id: memoryWorkflowData.autoId, bank_id: memoryWorkflowData.bankId, name: "Parity Automatic", content: memoryWorkflowData.original, refreshContent: memoryWorkflowData.refreshed, source_query: "Parity automatic source query", trigger: { mode: "delta", refresh_after_consolidation: true } },
  { id: memoryWorkflowData.curatedId, bank_id: memoryWorkflowData.bankId, name: "Parity Curated", content: memoryWorkflowData.curated, refreshContent: "PARITY_MEMORY_CURATED_REFRESHED", source_query: "Parity curated source query", trigger: { mode: "full", refresh_after_consolidation: false } },
];
const seeds = resolveSeedsForScope({ bankId: memoryWorkflowData.bankId }, "global");
if (seeds.length !== 1 || seeds[0]?.id !== "user-preferences") throw new Error("Memory workflow requires the canonical global user-preferences seed");
const createSeeds = seeds.map((seed): MemoryHttpExpectedRequest => ({
  method: "POST", path: modelsPath, query: {}, body: {
    id: seed.id, name: seed.name, source_query: seed.sourceQuery,
    ...(seed.tags.length > 0 ? { tags: seed.tags } : {}),
    ...(seed.maxTokens === undefined ? {} : { max_tokens: seed.maxTokens }),
    ...(seed.trigger === undefined ? {} : { trigger: seed.trigger }),
  },
}));
const bootstrap = (): MemoryHttpStage[] => [stage("bootstrap-bank", { method: "PUT", path: root, query: {}, body: {} }), stage("bootstrap-cache", list("content"))];
const history = (path: string): MemoryHttpExpectedRequest => ({ method: "GET", path: `${path}/history`, query: {}, body: null });
const remove = (path: string): MemoryHttpExpectedRequest => ({ method: "DELETE", path, query: {}, body: null });

const tapes: Record<string, MemoryHttpServiceInput> = {
  "builtin-memory-hindsight-lifecycle": {
    bankId: memoryWorkflowData.bankId, apiToken: "parity-hindsight-key", initialModels,
    createdContent: { "user-preferences": memoryWorkflowData.seeded },
    tape: [...bootstrap(),
      stage("list-inventory", list("metadata")), stage("show-original", model(autoPath)),
      stage("refresh-explicit", refresh(autoPath)), stage("refresh-explicit-cache", list("content", true)),
      stage("history-after-refresh", model(autoPath), history(autoPath)),
      stage("bulk-select-targets", list("content")), stage("bulk-refresh-auto-only", refresh(autoPath)), stage("bulk-refresh-cache", list("content", true)),
      stage("seed-inventory", list("metadata")), stage("seed-create", ...createSeeds), stage("seed-idempotent", list("metadata")),
      stage("reload-seeded", list("content", true)), stage("delete-auto", remove(autoPath)), stage("delete-cache", list("content")),
      stage("show-missing", model(autoPath)), stage("delete-missing", remove(autoPath)), stage("list-after-delete", list("metadata")), stage("reload-persisted", list("content")),
      stage("remove-alias-seed", remove(`${modelsPath}/user-preferences`)), stage("remove-alias-cache", list("content")),
    ],
  },
  "builtin-memory-hindsight-errors": {
    bankId: memoryWorkflowData.bankId, apiToken: "parity-hindsight-key", initialModels, createdContent: {},
    tape: [...bootstrap(), stage("show-missing", model(`${modelsPath}/parity-missing`)),
      stage("list-failure", failed(list("metadata"))), stage("show-failure", failed(model(autoPath))),
      stage("refresh-failure", failed(refresh(autoPath))), stage("history-failure", model(autoPath), failed(history(autoPath))),
      stage("seed-failure", failed(list("metadata"))), stage("reload-failure-preserves-cache", failed(list("content"))),
    ],
  },
  "builtin-memory-hindsight-empty": {
    bankId: memoryWorkflowData.bankId, apiToken: "parity-hindsight-key", initialModels: [], createdContent: { "user-preferences": memoryWorkflowData.seeded },
    tape: [...bootstrap(), stage("empty-list", list("metadata")), stage("empty-refresh", list("content")),
      stage("empty-seed-inventory", list("metadata")), stage("empty-seed-create", ...createSeeds), stage("empty-seed-reload", list("content", true)),
    ],
  },
  "builtin-memory-hindsight-curated": {
    bankId: memoryWorkflowData.bankId, apiToken: "parity-hindsight-key", initialModels: initialModels.filter(model => model.id === memoryWorkflowData.curatedId), createdContent: {},
    tape: [...bootstrap(), stage("curated-bulk-no-targets", list("content")), stage("curated-no-history", model(curatedPath), history(curatedPath)),
      stage("curated-explicit-refresh", refresh(curatedPath)), stage("curated-refresh-cache", list("content", true)), stage("curated-show-refreshed", model(curatedPath)),
    ],
  },
  "builtin-memory-hindsight-reset": {
    bankId: memoryWorkflowData.bankId, apiToken: "parity-hindsight-key", initialModels, createdContent: {}, tape: bootstrap(),
  },
};

export function applyMemoryWorkflowRequirements(requirements: Record<string, BuiltinRequirements>): void {
  for (const [id, input] of Object.entries(tapes)) {
    const requirement = requirements[id];
    if (!requirement) throw new Error(`Missing explicit memory workflow ${id}`);
    requirement.services.push({ kind: "hindsight", behavior: memoryWorkflowHooks.lifecycle, input: { ...structuredClone(input) } });
    requirement.models.turns = [];
    requirement.prerequisites.push(memoryWorkflowHooks.configuration, memoryWorkflowHooks.initialSettlement, "Fresh owned HOME, agent directory and workspace per adapter; only deliberately nonsecret parity-hindsight-key; deny every non-loopback request and never read operator credentials.");
    for (const capture of requirement.captures) {
      if (capture.channel === "builtin.memory") capture.source = memoryWorkflowHooks.sessionCapture;
      if (capture.channel === "builtin.memory-http") capture.source = memoryWorkflowHooks.httpCapture;
    }
  }
}
