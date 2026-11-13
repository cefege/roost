// Owns real runtime-tool tapes, owned subprocess prerequisites and read-only observations.
// Consumed by builtin aggregation and the actual terminal/native host fixture owners.
import type { BuiltinProviderTurn, BuiltinRequirements } from "./builtin-requirements";

export interface RuntimeWorkflowProfile {
  tools: Array<"bash" | "eval" | "task" | "read" | "write">;
  sourceRootEnvironment?: { OMP_PARITY_SOURCE_ROOT: "canonical-read-only-source-root" };
  authoritySettings?: { "task.perCallModel": "on"; "task.perCallModelAllow": ["openai/parity-small"] };
  checker?: { executable: "tsc"; implementation: "real-typescript-compiler"; workspaceConfig: "tsconfig.json" };
  remoteCompaction?: {
    setting: "compaction.remoteEndpoint";
    origin: "owned-provider-origin";
    path: "/v1/chat/completions";
    requestStream: false;
    responses: Array<{ status: 200; summary: string } | { status: 503; message: string }>;
  };
  multiplexer?: {
    adapter: "terminal-only-attached-client";
    kind: "tmux";
    socket: "private-owned-unix-socket";
    parent: "actual-host-process-in-owned-pane";
    shell: "/bin/sh";
    child: "canonical-source-cli-not-fixture-executable";
    placement: "pane" | "window";
    cleanup: "reap-owned-children-and-private-server-only";
  };
  nativeFork?: {
    adapter: "canonical-visible-nativeHTML-fork";
    placement: "pane" | "window";
    parentSelector: "[data-omp-fork-parent]";
    childSelector: "[data-omp-fork-child]";
    pageRouting: "actual-parent-and-child-pages-not-hidden-copies";
    focus: "child-after-launch";
  };
}

declare module "./builtin-requirements" {
  interface BuiltinRequirements { runtimeWorkflow?: RuntimeWorkflowProfile }
}

export const runtimeWorkflowTraceEnvironment = "OMP_PARITY_RUNTIME_TRACE";
export type RuntimeWorkflowBoundary =
  | { channel: "runtime.provider.request"; value: { requestId: string; modelId: string; url: string; body: unknown; stream: boolean } }
  | { channel: "runtime.provider.abort"; value: { requestId: string; completed: false } }
  | { channel: "runtime.job.process"; value: { jobId: string; pid: number; pidFile: string; phase: "spawned" | "exited"; exitCode?: number | null; signal?: string | null } }
  | { channel: "runtime.job.status"; value: { sessionId: string; jobId: string; status: "running" | "completed" | "failed" | "cancelled" } }
  | { channel: "runtime.fork.child"; value: {
      adapter: "terminal" | "browser"; pid: number; sessionPath: string; sessionId: string;
      parentSessionPath: string; parentSessionId: string; placement: "pane" | "window";
      parentPaneId?: string; childPaneId?: string; parentWindowId?: string; childWindowId?: string;
      parentPageUrl?: string; childPageUrl?: string; childRootSelector?: "[data-omp-fork-child]";
    } };

export const runtimeWorkflowCapturePointers: Readonly<Record<string, string>> = {
  "/runtime/jobs/runningCount": "Read session.getAsyncJobTree({recentLimit:Number.MAX_SAFE_INTEGER}).running.length without settleAsyncWork (live sleep jobs must remain running). Do not await whole-job completion merely to capture a snapshot.",
  "/runtime/jobs/bg_1/status": "Read the actual bg_1 async job status through inspectAsyncJob/getAsyncJobTree, retaining the real completed/cancelled row even after automatic result delivery. A missing row fails, never means cancelled.",
  "/runtime/jobs/bg_2/status": "As for bg_1, read actual bg_2 status, retaining its actual terminal transition in an observation ledger before eviction.",
  "/runtime/jobs/bg_3/status": "As for bg_1, read actual bg_3 status, retaining its actual terminal transition in an observation ledger before eviction.",
  "/runtime/jobs/ownedLiveProcessCount": "Read each real bash job's process inspection and the pid files parity-job-{a,b,c}.pid; OS-probe only PIDs proved owned by this run. Count live sleep descendants, not the persistent executor shell. Await actual spawn before the initial running checkpoint, and actual process exit/reaping after cancellation. Never infer OS termination from a cancellation notice.",
  "/runtime/cleanse/status": "Read the latest custom entry customType=cleanse in the independently persisted production Cleanse parent journal (identified by actual name/header, cwd and run-created path, not the foreground journal). Return data.status only after its real close/flush. Keep original run identity through foreground /restart.",
  "/runtime/cleanse/workers": "Read latest actual persisted cleanse custom entry data.workers. Do not count provider tapes or file writes as workers.",
  "/runtime/cleanse/remaining": "Read latest actual persisted cleanse custom entry data.remaining. Zero requires the real compiler verification pass to have finished, not just a repair write.",
  "/runtime/cleanse/dispatchedFiles": "Flatten actual cleanse_dispatch custom-entry data.files in that same real Cleanse journal, preserving order; no synthetic dispatch or replacement checker/controller.",
  "/runtime/cleanse/workerWritePersisted": "Follow the real Cleanse parent's persisted spawn record to the real child journal and confirm an actual write call for cleanse-value.ts plus its non-error result is durable. Parent dispatch and final text alone are insufficient.",
  "/runtime/cleanse/workerStreamCancelled": "Observe the actual local SSE stream for the Cleanse worker ending through request abort before a terminal completion frame; no fixture-driven normal release or fabricated cancellation.",
  "/runtime/compact/method": "After manager.flush(), read the newest real CompactionEntry.method from the CURRENT session FILE using the production parser, not agent context or submitted mode.",
  "/runtime/compact/summary": "Read that persisted CompactionEntry.summary verbatim; no supplied provider marker fallback.",
  "/runtime/compact/count": "Count persisted CompactionEntry rows in the current session file after flush. Preserve the same original manifest/session identity over restart.",
  "/runtime/compact/rawHistoryRetained": "Parse the current journal and require the original seeded user and complete original seeded assistant still exist as message entries before the compaction, while the active context uses the real summary. Do not reconstruct deleted history from manifest values.",
  "/runtime/compact/activeHasOriginalAssistant": "Compare live session.messages assistant text to the complete original seeded assistant text. True only for a complete raw original message; archive edges and compaction summaries do not count.",
  "/runtime/compact/archiveHasFrames": "Read the persisted snapcompact archive with production getPreservedArchive, resolve each frame using sessionManager.resolveFrameData, validate actual PNG signatures and nonempty decoded bytes, and return whether frame count is greater than zero. Missing, text-only, or undecodable archive fails the positive imaged-archive prerequisite.",
  "/runtime/compact/archiveSourceRetained": "Read production archiveSourceText from the actual persisted archive and require both original user and original assistant marker in its actual text. Never return expected seed text as archive evidence.",
  "/runtime/compact/providerImageReplay": "Inspect the actual post-snapcompact local-provider HTTP request: require an image_url data:image/png;base64 block whose decoded bytes equal a resolved persisted archive frame. Do not merely count image-capable catalog entries.",
  "/runtime/compact/remoteRequestCount": "Count actual HTTP POSTs to the owned remoteCompaction path with stream=false, valid model and nonempty system/user messages. Preserve actual request/response bodies and count failures too; foreground SSE calls are separate.",
  "/runtime/compact/remoteFocusIncluded": "Return whether the actual stream=false compaction HTTP body contains the exact submitted focus 'Preserve parity remote evidence'; never derive it from command text alone.",
  "/runtime/shake/prunedReadResults": "Count actual read ToolResultMessage entries with toolCallId=parity-shake-read and numeric prunedAt in current persisted journal after rewriteEntries/flush. Inspect the file, not only live state.",
  "/runtime/shake/originalInLiveToolResult": "Read the actual live parity-shake-read tool result and test for PARITY_SHAKE_ORIGINAL_START plus PARITY_SHAKE_ORIGINAL_END. A missing tool result fails rather than meaning false.",
  "/runtime/shake/originalInDiskToolResult": "As for originalInLiveToolResult, but load actual rewritten ToolResultMessage from current saved journal through production parser; a missing row fails.",
  "/runtime/shake/recoveryArtifactContainsOriginal": "Parse the actual elided tool-result replacement to locate its real shake artifact. Read the owned artifact file and require the entire original shake-source.txt bytes, not just markers. Require the referenced section to exist; no independently fabricated recovery file.",
  "/runtime/shake/recoveryPointerOnWire": "Inspect the actual provider request after /shake: require its original read result was replaced by the actual artifact-backed elision pointer and no complete original text remains. Do not modify the request to supply the pointer.",
  "/runtime/ratchet/enabled": "Read registered cfgRatchetEnabled.get(session.settings), including session-local override; /restart removes the override because only /ratchet armed it.",
  "/runtime/ratchet/inputsApproval": "Load actual flow via loadState and production approvalStatus(cwd,state). Return inputs status, computing current hashes, never treating presence alone as current.",
  "/runtime/ratchet/graderApproval": "As inputsApproval for grader; cancellation leaves it missing, not approved.",
  "/runtime/ratchet/planApproval": "As inputsApproval for plan.",
  "/runtime/ratchet/roundCount": "Read actual .omp/ratchet/parity-flow/_state.json via production loadState and return rounds.length; never seed or hand-edit this state.",
  "/runtime/ratchet/baselineDecision": "Read actual persisted rounds[0].decision; missing round fails this pointer.",
  "/runtime/ratchet/lastDecision": "Read actual last persisted RatchetRound.decision.",
  "/runtime/ratchet/bestVariant": "Read actual persisted state.best.variant; do not infer keep from tuned prompt bytes.",
  "/runtime/ratchet/baselineRows": "Parse actual baseline/results.jsonl and count rows after the real runner exits; no expected-row substitution or fixture-written result files.",
  "/runtime/ratchet/variantRows": "Parse actual v1/results.jsonl and count rows after the real runner exits.",
  "/runtime/ratchet/testTraceCount": "Load actual frozen test_ids and enumerate baseline/traces and v1/traces. Count matching held-out <id>_rep<rep>.json files. Do not delete a leak in the observer.",
  "/runtime/ratchet/analyzerCompleted": "Observe the actual task result and child journal for parity-ratchet-analyzer, requiring its real read of parity-train-view.json and normal successful completion. Never invoke the analyzer directly or manufacture a task result.",
  "/runtime/fork/launchedChildCount": "Read actual runtime.fork.child boundary records and independently validate their source-owned session paths/headers. Terminal requires actual private-tmux topology and canonical omp --fork argv; browser requires the actual mounted nativeHTML child/real ACP managed session, not a hidden CLI or phantom window. Count live fork children only. A status string alone is not a fork.",
  "/runtime/fork/placement": "Terminal: resolve actual child/parent tmux panes/windows. Browser: resolve actual visible nativeHTML parent/child topology through the real placement adapter (pane versus actual managed new window). Return pane/window from real topology; tab must produce window. Never derive this from submitted command or inject TMUX into the ACP/browser process.",
  "/runtime/fork/childHasDifferentSessionId": "Read child-owned session JSONL and compare its actual header id with immutable original manifest.sessionId. Missing child journal fails, not true.",
  "/runtime/fork/childParentIsOriginal": "Read actual child header parentSession and compare id/path with immutable original manifest. Do not infer parentage from different IDs.",
  "/runtime/fork/childUserTexts": "Read the fork child's persisted active branch via production parser/context fold; return user text strings in order. Locate the journal from actual child process/spawn effects, never by creating one in the fixture.",
  "/runtime/fork/childAssistantTexts": "Read the same real child active branch assistant text strings, excluding tool-call-only empty messages.",
  "/runtime/fork/childWritePersisted": "Require real child journal contains write(fork-child.txt,PARITY_FORK_CHILD_FILE\\n) and matching non-error tool result, after child CLI flush; workspace file alone cannot establish child dispatch.",
};

export const runtimeRatchetBaselinePrompt = "PARITY_RATCHET_BASELINE\nReturn ALERT for signal=off and IGNORE for signal=on. Return only the label.\n";
export const runtimeRatchetTunedPrompt = "PARITY_RATCHET_TUNED\nReturn ALERT for signal=on and IGNORE for signal=off. Return only the label.\n";
export const runtimeShakeSource = `PARITY_SHAKE_ORIGINAL_START\n${Array.from({ length: 180 }, (_, index) => `source-row-${index}: owned diagnostic payload ${index * 37}`).join("\n")}\nPARITY_SHAKE_ORIGINAL_END\n`;
export const runtimeShakeTail = `PARITY_SHAKE_READ_COMPLETE\n${Array.from({ length: 1800 }, (_, index) => `tail-${index}: keep the latest conversational explanation outside the elided tool result.`).join("\n")}\nPARITY_SHAKE_READY`;
const ratchetCases = Array.from({ length: 20 }, (_, index) => ({
  id: `case-${String(index).padStart(2, "0")}`, tag: index % 2 === 0 ? "positive" : "negative",
  input: `PARITY_RATCHET_CASE_${String(index).padStart(2, "0")} signal=${index % 2 === 0 ? "on" : "off"}`,
  expected: index % 2 === 0 ? "ALERT" : "IGNORE",
}));

// This owned runner calls the canonical provider implementation and the app entry point.
// Neither it nor the fixture writes ratchet verdicts/approvals; those belong to the real prelude.
const ratchetApplication = `import type { Model } from "@omp-source/packages/ai/src/types";
import type * as CanonicalStream from "@omp-source/packages/ai/src/stream";
export async function classifySignal(input: string, instruction: string, model: Model, apiKey: string) {
  const source = process.env.OMP_PARITY_SOURCE_ROOT;
  if (!source) throw new Error("Missing canonical read-only source root");
  // The read-only source checkout is runtime-selected by the isolated host, not a workspace dependency.
  const ai: typeof CanonicalStream = await import(source + "/packages/ai/src/stream.ts");
  const messages = [{ role: "user" as const, content: [{ type: "text" as const, text: input }], timestamp: Date.now() }];
  const result = await ai.streamSimple(model, { systemPrompt: instruction, messages }, { apiKey, maxTokens: 64 }).result();
  if (result.stopReason === "error" || result.stopReason === "aborted") throw new Error(result.errorMessage ?? result.stopReason);
  return { result, messages: [{ role: "system", content: instruction }, ...messages, { role: "assistant", content: result.content }] };
}
`;
const ratchetRunner = `import { mkdir, readFile, writeFile, appendFile } from "node:fs/promises";
import { join } from "node:path";
import { classifySignal } from "./parity-flow";
import type * as CanonicalSettings from "@omp-source/packages/coding-agent/src/config/settings";
import type * as CanonicalSdk from "@omp-source/packages/coding-agent/src/sdk";
import type * as CanonicalRegistry from "@omp-source/packages/coding-agent/src/config/model-registry";
interface SignalCase { id: string; tag: string; input: string; expected: string }
const source = process.env.OMP_PARITY_SOURCE_ROOT;
if (!source) throw new Error("Missing canonical read-only source root");
// The isolated host selects the canonical checkout at runtime; do not resolve operator-installed modules.
const config: typeof CanonicalSettings = await import(source + "/packages/coding-agent/src/config/settings.ts");
const sdk: typeof CanonicalSdk = await import(source + "/packages/coding-agent/src/sdk.ts");
const registryModule: typeof CanonicalRegistry = await import(source + "/packages/coding-agent/src/config/model-registry.ts");
const settings = await config.Settings.init({ cwd: process.cwd() });
const auth = await sdk.discoverAuthStorage(undefined, { settings });
const registry = new registryModule.ModelRegistry(auth);
await registry.refresh();
const model = registry.getAll().find(candidate => candidate.provider === "openai" && candidate.id === "parity-small");
if (!model) throw new Error("Missing actual owned parity-small registry row");
const apiKey = await registry.getApiKey(model);
if (!apiKey) throw new Error("Missing owned local provider credential");
const variant = process.argv[process.argv.indexOf("--variant") + 1];
const out = process.argv[process.argv.indexOf("--out") + 1];
if (!/^(baseline|v1)$/.test(variant ?? "") || !out?.startsWith(".omp/ratchet/parity-flow/")) throw new Error("Invalid owned runner output");
const state: { train_ids: string[] } = JSON.parse(await readFile(".omp/ratchet/parity-flow/_state.json", "utf8"));
const cases: SignalCase[] = JSON.parse(await readFile("parity-cases.json", "utf8"));
const instruction = await readFile("parity-prompt.txt", "utf8");
await mkdir(join(out, "traces"), { recursive: true });
for (const example of cases) {
  const rowPath = join(out, "results.jsonl");
  let existing = "";
  try { existing = await readFile(rowPath, "utf8"); } catch (error) { if (!(error instanceof Error && "code" in error && error.code === "ENOENT")) throw error; }
  if (existing.split("\\n").filter(Boolean).some(line => JSON.parse(line).prompt_id === example.id)) continue;
  const started = performance.now();
  try {
    const call = await classifySignal(example.input, instruction, model, apiKey);
    const answer = call.result.content.flatMap(block => block.type === "text" ? [block.text] : []).join("\\n").trim();
    const row = { prompt_id: example.id, rep: 0, prompt: example.input, tags: [example.tag], grade: answer === example.expected, status: "ok", model: call.result.model, usage: call.result.usage, latency_s: (performance.now() - started) / 1000 };
    await appendFile(rowPath, JSON.stringify(row) + "\\n");
    if (state.train_ids.includes(example.id)) await writeFile(join(out, "traces", example.id + "_rep0.json"), JSON.stringify(call.messages));
  } catch (error) {
    await appendFile(join(out, "errors.jsonl"), JSON.stringify({ prompt_id: example.id, rep: 0, error: String(error), class: "provider-or-runner" }) + "\\n");
    throw error;
  }
}
`;

export const runtimeRatchetFiles: Record<string, string> = {
  "parity-flow.ts": ratchetApplication, "parity-runner.ts": ratchetRunner,
  "parity-cases.json": JSON.stringify(ratchetCases), "parity-prompt.txt": runtimeRatchetBaselinePrompt,
};

function tool(id: string, name: string, arguments_: Record<string, unknown>, modelId = "parity-coder", textIncludes?: string): BuiltinProviderTurn {
  return { match: { modelId, ...(textIncludes ? { textIncludes } : {}) }, chunkDelayMs: 0, toolCalls: [{ id, name, arguments: arguments_ }] };
}
function evalTurn(id: string, code: string): BuiltinProviderTurn { return tool(id, "eval", { language: "js", code, title: id }); }
function reply(text: string, modelId = "parity-coder"): BuiltinProviderTurn { return { match: { modelId }, text, chunkDelayMs: 0 }; }
function requirementFor(requirements: Record<string, BuiltinRequirements>, id: string, profile: RuntimeWorkflowProfile): BuiltinRequirements {
  const requirement = requirements[`builtin-${id}`];
  if (!requirement) throw new Error(`Missing runtime workflow builtin-${id}`);
  requirement.runtimeWorkflow = profile;
  requirement.models.roles.smol = "openai/parity-coder";
  for (const capture of requirement.captures) {
    if (capture.channel !== "builtin.session" || !capture.pointer?.startsWith("/runtime/")) continue;
    const source = runtimeWorkflowCapturePointers[capture.pointer];
    if (!source) throw new Error(`Undeclared runtime observation ${capture.pointer}`);
    capture.source = source;
  }
  requirement.prerequisites.push("RuntimeWorkflowProfile is REQUIRED real input: activate the listed canonical tools, preserve source controller/dispatcher and local provider loops, and implement its exact owned host inputs. Missing tools, compiler, tmux, source-root propagation, or capture fields are failing prerequisites, never skips, fake callbacks, replaced handlers, or normalized success. Fresh process, owned HOME/auth/settings/workspace only; deny all remote traffic and reap every owned descendant on failure and success.");
  requirement.prerequisites.push("Expose genuine host/provider boundary JSONL under owned OMP_PARITY_RUNTIME_TRACE using exported RuntimeWorkflowBoundary. Record raw actual provider requests and aborts by requestId, actual owned job PID lifecycle events, and actual fork child session/topology identities. Captures join those records to actual production journals/process/DOM observations; the trace must never be populated from expected checkpoint values. Source async manager/controller remains unmodified; subscriptions and OS probes are observation only.");
  return requirement;
}
function seeded(requirement: BuiltinRequirements, title: string, user: string, assistant: string): void {
  Object.assign(requirement.session, { storage: "persisted", seed: "transcript", title, userText: user, assistantText: assistant });
}

export function applyRuntimeWorkflowRequirements(requirements: Record<string, BuiltinRequirements>): void {
  const jobs = requirementFor(requirements, "runtime-jobs-kill-owned", { tools: ["bash"] });
  jobs.models.turns = [
    ...["a", "b", "c"].map((name, index) => tool(`parity-background-${name}`, "bash", { command: `sh -c 'printf "%s\\n" "$$" > parity-job-${name}.pid; exec sleep 300'`, async: true, timeout: 0 }, "parity-coder", index === 0 ? "Start parity owned background jobs" : undefined)),
    reply("PARITY_JOBS_STARTED"),
  ];
  jobs.prerequisites.push("A fresh actual AsyncJobManager naturally allocates bg_1, bg_2 and bg_3 from real SSE bash calls. Disable auto-backgrounding of unrelated foreground commands; no reserve-id, job registration, sleeper or cancellation injection. Preserve real process inspection and retained terminal rows even if normal delivery consumes a result; OS liveness must independently prove selective kill and kill all.");

  for (const cancel of [false, true]) {
    const requirement = requirementFor(requirements, `runtime-cleanse-${cancel ? "cancel" : "repair-persist"}`, { tools: ["write"], checker: { executable: "tsc", implementation: "real-typescript-compiler", workspaceConfig: "tsconfig.json" } });
    seeded(requirement, "Parity cleanse foreground", "PARITY_CLEANSE_ORIGIN_USER", "PARITY_CLEANSE_ORIGIN_ANSWER");
    requirement.git = { branch: "parity-cleanse", commitFiles: { "tsconfig.json": '{"compilerOptions":{"strict":true,"noEmit":true,"skipLibCheck":true,"types":[]},"files":["cleanse-value.ts"]}\n', "cleanse-value.ts": "export const parityValue: number = 7;\n" }, dirtyFiles: { "cleanse-value.ts": 'export const parityValue: number = "broken";\n' }, worktreeRoot: "owned-run-directory" };
    requirement.models.turns = cancel
      ? [{ match: { modelId: "parity-small" }, text: "PARITY_CLEANSE_WORKER_HELD", holdAfterText: true, chunkDelayMs: 0 }]
      : [tool("parity-cleanse-fix", "write", { path: "cleanse-value.ts", content: "export const parityValue: number = 7;\n" }, "parity-small"), reply("PARITY_CLEANSE_REPAIRED", "parity-small")];
    requirement.lifecycle.superviseRealRestart = !cancel;
    requirement.prerequisites.push("Put the REAL TypeScript compiler tsc on owned PATH; do not supply a fake checker executable or intercepted diagnostic stream. Canonical discoverCleanseDiagnosticSuite finds tsconfig.json, actual tsc reports the dirty string-to-number assignment, createCleanseAgentRuntime discovers the owned model/auth from disk and real sonic subagent writes only cleanse-value.ts. Run --all -n 1 -m openai/parity-small. Final clean requires canonical compiler verification, durable separate Cleanse parent and child journals; Escape cancellation must abort the held worker's actual HTTP request and persist status cancelled with no fixup.");
  }

  for (const mode of ["soft", "remote", "snapcompact", "remote-fallback"] as const) {
    const remote = mode.startsWith("remote");
    const summary = mode === "remote" ? "PARITY_REMOTE_COMPACTION_SUMMARY" : mode === "remote-fallback" ? "PARITY_REMOTE_FALLBACK_SUMMARY" : "PARITY_SOFT_COMPACTION_SUMMARY";
    const requirement = requirementFor(requirements, `runtime-compact-${mode}`, {
      tools: ["read"],
      ...(remote ? { remoteCompaction: { setting: "compaction.remoteEndpoint", origin: "owned-provider-origin", path: "/v1/chat/completions", requestStream: false, responses: mode === "remote" ? [{ status: 200, summary }] : [{ status: 503, message: "PARITY_REMOTE_COMPACTION_REFUSED" }] } satisfies RuntimeWorkflowProfile["remoteCompaction"] } : {}),
    });
    seeded(requirement, `Parity compact ${mode}`, "PARITY_COMPACT_ORIGINAL_USER", `PARITY_COMPACT_ORIGINAL_ANSWER\n${Array.from({ length: 1400 }, (_, index) => `archive-row-${index}: owned source context must remain recoverable after compaction ${index * 43}.`).join("\n")}`);
    requirement.lifecycle.superviseRealRestart = true;
    requirement.models.ids = ["parity-coder"];
    requirement.models.turns = [reply("PARITY_COMPACT_TAIL"), ...(mode === "soft" || mode === "remote-fallback" ? [{ match: { modelId: "parity-coder", purpose: "compaction" }, text: summary, chunkDelayMs: 0 } satisfies BuiltinProviderTurn] : []), reply("PARITY_COMPACT_CONTINUED")];
    requirement.prerequisites.push("Keep compaction.keepRecentTokens=1 and compaction.enabled=false for automatic maintenance only; explicit manual compact remains the actual source path. Add a real recent user/assistant tail through /queue before compaction; no injected CompactionEntry. The 1400-line ASCII original makes a real bitmap archive economical; snapcompact must render/decode actual PNG frames with canonical fonts/native dependencies, retain archive source in the saved journal and replay a matching real image in the next provider request. A frameless/no-reduction outcome is a failing positive prerequisite, not normalized success. Source journal retains raw original history while active context drops it; verify both before and after actual /restart.");
    if (remote) requirement.prerequisites.push("Bind remoteCompaction.setting to the ACTUAL owned provider origin plus /v1/chat/completions before source session construction. Handle stream=false using canonical requestRemoteCompaction's real OpenAI-compatible JSON shape, not SSE. Consume exactly the declared remote response once, record its raw body/status/focus, and allow only the declared real SSE fallback after HTTP 503. Never count portable soft fallback as positive remote success; persisted method must distinguish remote and soft.");
  }

  const shake = requirementFor(requirements, "runtime-shake-elide-persist", { tools: ["read"] });
  shake.models.turns = [tool("parity-shake-read", "read", { path: "shake-source.txt" }), reply(runtimeShakeTail), reply("PARITY_SHAKE_CONTINUED")];
  shake.lifecycle.superviseRealRestart = true;
  shake.prerequisites.push("The original payload is produced by the actual read tool from owned shake-source.txt. A real long final assistant explanation supplies >4000 tokenizer tokens AFTER the result, satisfying AGGRESSIVE_SHAKE_CONFIG.protectTokens without changing source settings or seeding fake tool results. /shake elide must call canonical collect/applyShakeRegions, save the real recovery artifact, rewrite the saved tool result with prunedAt, rebuild live context, and retain that exact rewrite/artifact through real /restart. No pre-pruning or direct session mutation.");

  const ratchet = requirementFor(requirements, "runtime-ratchet-approve-keep-persist", { tools: ["eval", "task", "bash", "read", "write"], sourceRootEnvironment: { OMP_PARITY_SOURCE_ROOT: "canonical-read-only-source-root" }, authoritySettings: { "task.perCallModel": "on", "task.perCallModelAllow": ["openai/parity-small"] } });
  ratchet.models.roles.smol = "openai/parity-small";
  ratchet.lifecycle.superviseRealRestart = true;
  const setup = `const parityRatchet = ratchet("parity-flow"); await parityRatchet.init({cases:["parity-cases.json"],harness:["parity-runner.ts","parity-flow.ts"],change:["parity-prompt.txt"],command:"bun parity-runner.ts --variant {variant} --out {flow_dir}/{variant}"}); await parityRatchet.split(${JSON.stringify(Object.fromEntries(ratchetCases.map(example => [example.id, example.tag])))},{testFraction:0.5,seed:17}); await parityRatchet.plan({goal:{target:"score",direction:"higher",hold:[]},reps:1,stop:{plateau:2,rounds:1},prices:{"parity-small":{in:1,out:1}}}); print(await parityRatchet.approve("inputs",{question:"PARITY_RATCHET_INPUTS_REVIEW",preview:"20 owned signal-on/off cases; exact ALERT/IGNORE labels; no operator data."}));`;
  ratchet.models.turns = [
    evalTurn("parity-ratchet-setup", setup), reply("PARITY_RATCHET_DECLINED"),
    evalTurn("parity-ratchet-approve-inputs", 'print(await ratchet("parity-flow").approve("inputs",{question:"PARITY_RATCHET_INPUTS_REVIEW",preview:"20 owned signal-on/off cases; exact ALERT/IGNORE labels; no operator data."}));'),
    evalTurn("parity-ratchet-approve-grader", 'print(await ratchet("parity-flow").approve("grader",{question:"PARITY_RATCHET_GRADER_REVIEW",preview:"Actual canonical streamSimple app; exact-label grader; actual served model/usage/latency; train-only traces."}));'),
    evalTurn("parity-ratchet-approve-plan", 'print(await ratchet("parity-flow").approve("plan",{question:"PARITY_RATCHET_PLAN_REVIEW",preview:"20 cases x 1 rep, frozen 10/10 split; one prompt change; one round; loopback only."}));'),
    evalTurn("parity-ratchet-check-baseline", 'print(await ratchet("parity-flow").check("baseline"));'),
    tool("parity-ratchet-baseline-run", "bash", { command: "bun parity-runner.ts --variant baseline --out .omp/ratchet/parity-flow/baseline", timeout: 120 }),
    evalTurn("parity-ratchet-baseline-gate", 'print(await ratchet("parity-flow").gate("baseline")); const view=await ratchet("parity-flow").train("baseline"); await write("parity-train-view.json",JSON.stringify(view));'),
    tool("parity-ratchet-analyze", "task", { name: "parity-ratchet-analyzer", agent: "task", task: "PARITY_RATCHET_ANALYZE_TRAIN: Read parity-train-view.json and parity-prompt.txt. Identify the single inverted signal mapping. Do not read held-out cases or edit files.", solutionSpace: "Read-only diagnosis of the owned training view; recommend one mapping correction.", model: "openai/parity-small", tools: ["read"] }),
    tool("parity-ratchet-analyzer-read", "read", { path: "parity-train-view.json" }, "parity-small", "PARITY_RATCHET_ANALYZE_TRAIN"),
    { match: { modelId: "parity-small", textIncludes: "PARITY_RATCHET_ANALYZE_TRAIN" }, text: "PARITY_RATCHET_ANALYSIS: The prompt inverts signal on/off. Swap the two label rules, without inserting case content.", chunkDelayMs: 0 },
    tool("parity-ratchet-apply", "write", { path: "parity-prompt.txt", content: runtimeRatchetTunedPrompt }),
    evalTurn("parity-ratchet-check-v1", 'print(await ratchet("parity-flow").check("v1"));'),
    tool("parity-ratchet-v1-run", "bash", { command: "bun parity-runner.ts --variant v1 --out .omp/ratchet/parity-flow/v1", timeout: 120 }),
    evalTurn("parity-ratchet-v1-gate", 'print(await ratchet("parity-flow").gate("v1",{change:"Correct the inverted signal-to-label mapping"}));'),
    reply("PARITY_RATCHET_KEPT"),
    evalTurn("parity-ratchet-invalid-sequence", 'try { await ratchet("parity-flow").check("v9"); throw new Error("Sequence guard accepted v9"); } catch(error) { if (!(error instanceof Error) || !error.message.includes("Next variant is v2; got v9")) throw error; print(error.message); }'),
    reply("PARITY_RATCHET_SEQUENCE_REFUSED"),
    ...ratchetCases.flatMap((example, index): BuiltinProviderTurn[] => [
      { match: { modelId: "parity-small", textIncludes: example.input }, text: index % 5 === 0 ? example.expected : example.expected === "ALERT" ? "IGNORE" : "ALERT", chunkDelayMs: 0 },
      { match: { modelId: "parity-small", textIncludes: example.input }, text: example.expected, chunkDelayMs: 0 },
    ]),
  ];
  ratchet.prerequisites.push("Propagate OMP_PARITY_SOURCE_ROOT as the actual canonical read-only source checkout into the real eval kernel, bash runner and task subprocess. Real Bun/JS eval backend and canonical eval/task/bash/read/write must be enabled; task.structured=false permits the actual analyzer's plain final answer. /ratchet alone enables its real session prelude. No ratchet API/controller/approval/gate substitution, direct state seeding, fixture-written results or approvals. The owned app imports canonical streamSimple and the owned runner discovers the actual local registry/AuthStorage; it records real model/usage/latency/result text. Tape matching MUST select modelId+textIncludes before consumption across primary, analyzer and app requests; preserve original held turn indexes. Baseline has four deliberate correct cases and sixteen inverted cases; tuned mapping wins on actual held-out calls. Approve through real ask-dialog native/TUI input, first Escape without recording an approval, then complete all three content-hashed approvals. Actual prelude owns frozen split, sequence rejection, baseline/keep verdicts and status.md; actual /restart retains flow/results and clears only the nonpersisted ratchet.enabled override.");

  for (const placement of ["pane", "window", "tab"] as const) {
    const actualPlacement = placement === "pane" ? "pane" : "window";
    const requirement = requirementFor(requirements, `runtime-fork-${placement}`, { tools: ["write"], multiplexer: { adapter: "terminal-only-attached-client", kind: "tmux", socket: "private-owned-unix-socket", parent: "actual-host-process-in-owned-pane", shell: "/bin/sh", child: "canonical-source-cli-not-fixture-executable", placement: actualPlacement, cleanup: "reap-owned-children-and-private-server-only" }, nativeFork: { adapter: "canonical-visible-nativeHTML-fork", placement: actualPlacement, parentSelector: "[data-omp-fork-parent]", childSelector: "[data-omp-fork-child]", pageRouting: "actual-parent-and-child-pages-not-hidden-copies", focus: "child-after-launch" } });
    seeded(requirement, "Parity multiplexer fork", "PARITY_FORK_MUX_ORIGIN_USER", "PARITY_FORK_MUX_ORIGIN_ANSWER");
    requirement.lifecycle.superviseRealRestart = true;
    requirement.models.turns = [
      { match: { modelId: "parity-coder", textIncludes: "Continue parent after fork" }, text: "PARITY_PARENT_CHILD_DRIVEN", chunkDelayMs: 0 },
      tool("parity-fork-child-write", "write", { path: "fork-child.txt", content: "PARITY_FORK_CHILD_FILE\n" }, "parity-coder", "Parity mutate the owned fork child"),
      { match: { modelId: "parity-coder", textIncludes: "Parity mutate the owned fork child" }, text: "PARITY_FORK_MUX_CHILD_ANSWER", chunkDelayMs: 0 },
    ];
    requirement.prerequisites.push("TERMINAL ONLY: launch the real source TUI in the sole initial pane of a genuine private owned tmux session, with an attached outer tmux client as the terminal input/screen target and /bin/sh default shell. Source canonical mux classification/restartArgv/launchTerminal launches actual source CLI --fork against the saved original. Source tmux split/new-window focuses the child; subsequent ordinary terminal input therefore reaches its actual editor. Ctrl-b o (pane) or Ctrl-b p (window/tab) returns to the parent. No environment-only TMUX, ForkTerminalDependencies replacement, fake runCli or hidden capture executable. BROWSER ONLY: actual native fork transport must run before terminal mux classification, use real canonical harness/session forking with original unchanged, and mount the child nativeHTML visibly in the requested pane or actual managed window (tab aliases window). Mark actual native parent/child roots with nativeFork selectors; route those selectors to their actual Pages if a new browser window is used, never clone a hidden DOM. Browser remains outside tmux. Both hosts' actual child editors receive the same plain child prompt; real child SSE writes fork-child.txt and flushes a separate journal. Then the parent independently continues and restarts without rewriting either saved branch. No old retired AgentChatFork RPC/button. Inherit only owned HOME/settings/auth/local provider, never --api-key; cleanup only the private owned server/children. Missing visible native placement or tmux/real CLI launch fails the prerequisite and must not reuse no-multiplexer errors or journal-only parity.");
  }
}
