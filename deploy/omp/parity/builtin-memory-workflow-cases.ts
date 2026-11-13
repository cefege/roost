// Drives the real /memory dispatcher and production HindsightSessionState.
// Every checkpoint reads an actual terminal screen, HTML DOM and observed HTTP/store effects.
// External fixture content proves UI/harness transport parity, never Hindsight synthesis.
import type { BuiltinCase, Checkpoint, Comparison } from "./scenario";
import { dismissThenSend, effect, literalPattern, observed, send, textComparison } from "./builtin-steps";
import { memoryWorkflowData as memory } from "./builtin-memory-workflow-requirements";

function checkpoint(verb: string | null, command: string | null, text: string, requestCount: number, comparisons: Comparison[] = [], closePanel = false): Checkpoint {
  const effects = [
    effect("builtin.memory-http", "external-service-fixture", "/evidenceClass"),
    effect("builtin.memory-http", requestCount, "/requestCount"),
    effect("builtin.memory-http", [], "/violations"),
    effect("builtin.provider", 0, "/requestCount"),
    effect("builtin.provider", 0, "/remoteRequests"),
    ...comparisons.map(comparison => structuredClone(comparison)),
  ];
  const result = command === null ? observed(text, effects) : closePanel ? dismissThenSend(command, text, effects) : send(command, text, effects);
  const entries = verb === null ? ["command:memory"] : ["command:memory", `subcommand:memory ${verb}`];
  for (const comparison of result.comparisons) comparison.proves = entries.map(entry => ({ entry, effect: comparison.terminal.type === "effect" ? "host" : "interaction", value: comparison.expected }));
  return result;
}
function workflow(id: string, command: string, steps: Checkpoint[]): BuiltinCase {
  const covers = new Set<string>(["command:memory"]);
  for (const step of steps) for (const comparison of step.comparisons) for (const proof of comparison.proves ?? []) covers.add(proof.entry);
  return { id: `builtin-memory-hindsight-${id}`, command, covers: [...covers], settings: { ...memory.settings }, checkpoints: steps.map((step, index) => ({ ...step, name: `${index + 1}. ${step.name}` })) };
}

const original = effect("builtin.memory", true, "/snippetIncludes/original");
const refreshed = effect("builtin.memory", true, "/snippetIncludes/refreshed");
const seeded = effect("builtin.memory", true, "/snippetIncludes/seeded");
const active = effect("builtin.memory", true, "/active");
const persisted = [
  effect("builtin.memory", "hindsight", "/persistedSettings/backend"),
  effect("builtin.memory", memory.bankId, "/persistedSettings/bankId"),
  effect("builtin.memory", "global", "/persistedSettings/scoping"),
  effect("builtin.memory", true, "/persistedSettings/mentalModelsEnabled"),
  effect("builtin.memory", false, "/persistedSettings/autoSeed"),
];
const modelIds = [memory.autoId, memory.curatedId].sort();
const root = `/v1/default/banks/${memory.bankId}/mental-models`;

const dismissOriginal: Checkpoint = {
  name: "Escape closes the actual mental-model report without HTTP mutation",
  terminalInput: "\u001b",
  browserActions: [{ type: "press", key: "Escape" }],
  comparisons: ([
    { name: "Original report is closed", terminal: { type: "screen", pattern: literalPattern(memory.original), read: "visible" }, browser: { type: "dom", selector: `text=${memory.original}`, read: "visible" }, expected: false },
    effect("builtin.memory-http", 4, "/requestCount"), original, active,
  ] satisfies Comparison[]).map((comparison): Comparison => ({ ...comparison, proves: [{ entry: "command:memory", effect: comparison.terminal.type === "effect" ? "host" : "interaction", value: comparison.expected }] })),
};

const lifecycle = workflow("lifecycle", "/memory mm list", [
  checkpoint("mm list", null, "parity-auto: Parity Automatic", 3, [active, original, ...persisted, effect("builtin.memory-http", true, "/bank/exists"), effect("builtin.memory-http", modelIds, "/modelIds"), effect("builtin.memory-http", "metadata", "/requests/2/query/detail")]),
  checkpoint("mm show", `/memory mm show ${memory.autoId}`, memory.original, 4, [effect("builtin.memory-http", `${root}/${memory.autoId}`, "/requests/3/path"), effect("builtin.memory-http", "content", "/requests/3/query/detail")], true),
  dismissOriginal,
  checkpoint("view", "/memory view", memory.original, 4, [original, effect("builtin.memory", true, "/snippetIncludes/curated")]),
  checkpoint("mm refresh", `/memory mm refresh ${memory.autoId}`, `Refresh queued for mental model ${memory.autoId}.`, 6, [refreshed, effect("builtin.memory", false, "/snippetIncludes/original"), effect("builtin.memory-http", "completed", "/operations/0/status"), effect("builtin.memory-http", "refresh", "/operations/0/kind"), effect("builtin.memory-http", memory.refreshed, `/models/${memory.autoId}/content`), effect("builtin.memory-http", memory.original, `/history/${memory.autoId}/0/previous_content`)], true),
  checkpoint("mm history", `/memory mm history ${memory.autoId}`, `+ ${memory.refreshed}`, 8, [textComparison(`- ${memory.original}`), effect("builtin.memory-http", memory.original, `/history/${memory.autoId}/0/previous_content`), effect("builtin.memory-http", memory.refreshed, `/models/${memory.autoId}/content`)]),
  checkpoint("mm refresh", "/memory mm refresh", "Refresh queued for 1/1 auto-refresh model(s); skipped 1 curated model(s).", 11, [effect("builtin.memory-http", "content", "/requests/8/query/detail"), effect("builtin.memory-http", memory.autoId, "/operations/1/modelId"), effect("builtin.memory-http", "completed", "/operations/1/status"), effect("builtin.memory-http", memory.curated, `/models/${memory.curatedId}/content`)], true),
  checkpoint("mm seed", "/memory mm seed", "Seeded 1 new mental model(s); 0 already present.", 13, [effect("builtin.memory-http", "POST", "/requests/12/method"), effect("builtin.memory-http", "user-preferences", "/requests/12/body/id"), effect("builtin.memory-http", "pending", "/operations/2/status"), effect("builtin.memory-http", "create", "/operations/2/kind"), effect("builtin.memory-http", "", "/models/user-preferences/content")]),
  checkpoint("mm seed", "/memory mm seed", "Seeded 0 new mental model(s); 1 already present.", 14, [effect("builtin.memory-http", "GET", "/requests/13/method"), effect("builtin.memory-http", "pending", "/operations/2/status")]),
  checkpoint("mm reload", "/memory mm reload", "Mental-model cache reloaded.", 15, [seeded, refreshed, effect("builtin.memory-http", "completed", "/operations/2/status"), effect("builtin.memory-http", memory.seeded, "/models/user-preferences/content")]),
  checkpoint("view", "/memory view", memory.seeded, 15, [seeded, refreshed]),
  checkpoint("mm delete", `/memory mm delete ${memory.autoId}`, `Deleted mental model ${memory.autoId} from bank ${memory.bankId}.`, 17, [effect("builtin.memory-http", "DELETE", "/requests/15/method"), effect("builtin.memory-http", [memory.curatedId, "user-preferences"].sort(), "/modelIds"), effect("builtin.memory", false, "/snippetIncludes/refreshed"), seeded], true),
  checkpoint("mm show", `/memory mm show ${memory.autoId}`, `Mental model not found: ${memory.autoId}`, 18, [effect("builtin.memory-http", 404, "/requests/17/status")]),
  checkpoint("mm delete", `/memory mm delete ${memory.autoId}`, `Mental model not found: ${memory.autoId}`, 19, [effect("builtin.memory-http", 404, "/requests/18/status")]),
  checkpoint("mm list", "/memory mm list", "user-preferences: User Preferences", 20, [effect("builtin.memory-http", [memory.curatedId, "user-preferences"].sort(), "/modelIds")]),
  checkpoint("mm reload", "/memory mm reload", "Mental-model cache reloaded.", 21, [seeded, effect("builtin.memory", true, "/snippetIncludes/curated"), ...persisted], true),
  checkpoint(null, "/memory mm remove user-preferences", `Deleted mental model user-preferences from bank ${memory.bankId}.`, 23, [effect("builtin.memory-http", "DELETE", "/requests/21/method"), effect("builtin.memory-http", [memory.curatedId], "/modelIds"), effect("builtin.memory", false, "/snippetIncludes/seeded"), effect("builtin.memory", true, "/snippetIncludes/curated"), effect("builtin.memory-http", [], "/remainingStages")]),
  checkpoint("clear", "/memory clear", "Memory data cleared and system prompt refreshed.", 23, [effect("builtin.memory", false, "/active"), effect("builtin.memory-http", [memory.curatedId], "/modelIds"), ...persisted]),
  checkpoint("mm list", "/memory mm list", "Hindsight backend is not active for this session.", 23, [effect("builtin.memory", false, "/active"), effect("builtin.memory-http", [], "/remainingStages")]),
]);

const errors = workflow("errors", "/memory mm show parity-missing", [
  checkpoint("mm show", null, "Mental model not found: parity-missing", 3, [original, effect("builtin.memory-http", 404, "/requests/2/status")]),
  checkpoint("mm list", "/memory mm list", "mm list failed: listMentalModels failed: PARITY_MEMORY_SERVICE_UNAVAILABLE", 4, [effect("builtin.memory-http", 503, "/requests/3/status"), original]),
  checkpoint("mm show", `/memory mm show ${memory.autoId}`, "mm show failed: getMentalModel failed: PARITY_MEMORY_SERVICE_UNAVAILABLE", 5, [effect("builtin.memory-http", 503, "/requests/4/status"), original]),
  checkpoint("mm refresh", `/memory mm refresh ${memory.autoId}`, "mm refresh failed: refreshMentalModel failed: PARITY_MEMORY_SERVICE_UNAVAILABLE", 6, [effect("builtin.memory-http", [], "/operations"), original]),
  checkpoint("mm history", `/memory mm history ${memory.autoId}`, "mm history failed: getMentalModelHistory failed: PARITY_MEMORY_SERVICE_UNAVAILABLE", 8, [effect("builtin.memory-http", [], `/history/${memory.autoId}`), original]),
  checkpoint("mm seed", "/memory mm seed", "mm seed failed: listMentalModels failed: PARITY_MEMORY_SERVICE_UNAVAILABLE", 9, [effect("builtin.memory-http", modelIds, "/modelIds")]),
  checkpoint("mm reload", "/memory mm reload", "Mental-model cache reloaded.", 10, [original, effect("builtin.memory-http", 503, "/requests/9/status"), effect("builtin.memory-http", [], "/remainingStages")]),
  checkpoint("view", "/memory view", memory.original, 10, [original, active]),
]);
const empty = workflow("empty", "/memory mm list", [
  checkpoint("mm list", null, `No mental models on bank ${memory.bankId}.`, 3, [effect("builtin.memory-http", [], "/modelIds"), effect("builtin.memory", null, "/snippet")]),
  checkpoint("mm refresh", "/memory mm refresh", `No mental models on bank ${memory.bankId}.`, 4, [effect("builtin.memory-http", [], "/operations")]),
  checkpoint("mm seed", "/memory mm seed", "Seeded 1 new mental model(s); 0 already present.", 6, [effect("builtin.memory-http", ["user-preferences"], "/modelIds"), effect("builtin.memory-http", "pending", "/operations/0/status")]),
  checkpoint("mm reload", "/memory mm reload", "Mental-model cache reloaded.", 7, [seeded, effect("builtin.memory-http", "completed", "/operations/0/status"), effect("builtin.memory-http", [], "/remainingStages")]),
  checkpoint("view", "/memory view", memory.seeded, 7, [seeded]),
]);
const curated = workflow("curated", "/memory mm refresh", [
  checkpoint("mm refresh", null, "No mental models opted into auto-refresh; 1 curated model(s) left untouched.", 3, [effect("builtin.memory-http", [], "/operations"), effect("builtin.memory-http", memory.curated, `/models/${memory.curatedId}/content`)]),
  checkpoint("mm history", `/memory mm history ${memory.curatedId}`, `No history recorded for ${memory.curatedId}.`, 5, [effect("builtin.memory-http", [], `/history/${memory.curatedId}`)]),
  checkpoint("mm refresh", `/memory mm refresh ${memory.curatedId}`, `Refresh queued for mental model ${memory.curatedId}.`, 7, [effect("builtin.memory-http", memory.curatedId, "/operations/0/modelId"), effect("builtin.memory-http", "completed", "/operations/0/status"), effect("builtin.memory-http", memory.curated, `/history/${memory.curatedId}/0/previous_content`)]),
  checkpoint("mm show", `/memory mm show ${memory.curatedId}`, "PARITY_MEMORY_CURATED_REFRESHED", 8, [effect("builtin.memory-http", "PARITY_MEMORY_CURATED_REFRESHED", `/models/${memory.curatedId}/content`), effect("builtin.memory-http", [], "/remainingStages")]),
]);
const reset = workflow("reset", "/memory reset", [
  checkpoint("reset", null, "Memory data cleared and system prompt refreshed.", 2, [effect("builtin.memory", false, "/active"), effect("builtin.memory-http", modelIds, "/modelIds"), ...persisted, effect("builtin.memory-http", [], "/remainingStages")]),
  checkpoint("mm show", `/memory mm show ${memory.autoId}`, "Hindsight backend is not active for this session.", 2, [effect("builtin.memory", false, "/active"), effect("builtin.memory-http", memory.original, `/models/${memory.autoId}/content`)]),
]);

export const memoryWorkflowCases: BuiltinCase[] = [lifecycle, errors, empty, curated, reset];
