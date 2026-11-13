import type { BuiltinCase } from "./scenario";
import { statsWorkflowCaptureSource, type StatsWorkflowProfile } from "./builtin-stats-workflow-requirements";
import { authWorkflowCaptureSource, authWorkflowServiceContract } from "./builtin-auth-workflow-requirements";
import { collabWorkflowCaptureSource, type CollabWorkflowProfile } from "./builtin-collab-workflow-requirements";

export interface BuiltinProviderTurn {
  match: { modelId?: string; textIncludes?: string; purpose?: "foreground" | "compaction" | "handoff" | "btw" | "background" | "rule" | "seance" };
  text?: string;
  thinking?: string;
  textChunks?: string[];
  chunkDelayMs?: number;
  toolCalls?: Array<{ id: string; name: string; arguments: Record<string, unknown>; argumentChunks?: string[] }>;
  failure?: { status: number; message: string };
  holdAfterText?: boolean;
}

export interface BuiltinRequirements {
  session: {
    storage: "persisted" | "memory";
    seed: "empty" | "transcript" | "code-and-link" | "heavy-blocks";
    title: string;
    userText: string;
    assistantText: string;
    additionalSavedSessions: Array<{ name: string; userText: string; assistantText: string }>;
  };
  models: {
    provider: "openai" | "openai-codex";
    api: "openai-completions";
    ids: string[];
    defaultId: string;
    reasoning: boolean;
    imageInput: boolean;
    roles: Record<string, string>;
    remoteNetwork: "deny";
    turns: BuiltinProviderTurn[];
  };
  host: {
    clipboard: "owned-memory";
    openUrl: "capture-without-navigation";
    externalEditor?: { target: "todo" | "local"; writes: string; exitCode: number };
    recordings: "owned-directory";
    audio: "silent-loopback-no-device" | "virtual-loopback-wave";
    oauth: "empty-owned-store" | "owned-live-fixture";
  };
  git?: { branch: string; commitFiles: Record<string, string>; dirtyFiles: Record<string, string>; worktreeRoot: "owned-run-directory" };
  localFiles?: Record<string, string>;
  directories?: string[];
  mcp?: { scope: "project" | "user"; name: string; command: string; enabled: boolean };
  statsWorkflow?: StatsWorkflowProfile;
  collabWorkflow?: CollabWorkflowProfile;
  services: Array<{
    kind: "skill-registry" | "share" | "collab-relay" | "mcp-stdio" | "mcp-http" | "smithery" | "live" | "hindsight";
    behavior: string;
    input: Record<string, unknown>;
  }>;
  lifecycle: { allowExit: boolean; superviseRealRestart: boolean; resumeSameSession: boolean };
  captures: Array<{ channel: string; pointer?: string; source: string }>;
  prerequisites: string[];
}

const captureSources: Readonly<Record<string, string>> = {
  "builtin.session": "Observe actual ctx.session/ctx.sessionManager before command and after settlement: model, configuredThinkingLevel(), getTodoPhases(), plan/loop/paused flags, getMessages()/agent.state.messages, getSessionId(), getSessionName(), original session file existence; never return expected literals.",
  "builtin.persisted-settings": "await ctx.settings.flush(); read the isolated settings config, resolve settings registered IDs to stored paths; /modelPresets/<name>/exists is actual own-property presence.",
  "builtin.todos": "ctx.session.getTodoPhases() (or latest todo entry on sessionManager.getBranch()); preserve phase/tasks order and raw status values.",
  "builtin.goal": "ctx.session.getGoalModeState()?.goal; null only after actual drop; read objective/status/tokenBudget from persisted mode entry.",
  "builtin.provider": "Captured real loopback HTTP adapter requests and completed/cancelled streams, decoded model/tool messages; userPrompts filters actual foreground user prompts, toolResults is actual next-round tool output, remoteRequests counts requests escaping the loopback fixture (must remain zero), deniedModelDiscovery counts OMP's own startup model-catalog probes the guard refused (identified by their model-registry call stack).",
  "builtin.btw": "Read the real session-local BTW store after its write promise settles; retain actual question/answer order.",
  "builtin.background": "Observe actual async job tree and task result settlement; background tool execution remains real.",
  "builtin.workspace-file": "Read only owned workspace files; expose content, parsed JSON, exists and containsSeedTranscript (exact supplied seed user+assistant strings must both occur).",
  "builtin.local-file": "Read files under sessionManager.getArtifactsDir() local:// root, not workspace files.",
  "builtin.clipboard": "Capture actual terminal clipboard helper/OSC52 and actual ACP host clipboard response text. Return latest completed clipboard write bytes.",
  "builtin.host-open-url": "Capture actual OS/browser host open request without navigating external sites; return requested URL.",
  "builtin.plugins": "Read actual MarketplaceManager.listInstalledPlugins() and installed entries after reload; key by id; installed is presence, enabled is actual entry.enabled !== false.",
  "builtin.marketplaces": "Read actual MarketplaceManager.listMarketplaces() after local mutation.",
  "builtin.plugin-reloads": "Count completed real reloadTuiPluginState calls observed at extension/plugin lifecycle boundary; do not intercept command handler.",
  "builtin.skills": "Read actual skills.json lock entry and installed skill store SKILL.md; key by @scope/name with version/content.",
  "builtin.security": "Read actual SecurityStore for owned repository, including imported bundle findings and mutable dispositions; do not replace coordinator/store methods.",
  "builtin.ssh": "Read actual project/user SSH config files through readSSHConfigFile; add exists fields only from own-property checks.",
  "builtin.mcp": "Read actual MCP config and real manager connection/tool registry; tool names remove only actual server namespace prefix; existence from config files.",
  "builtin.smithery": "Read presence only from isolated Smithery credential store, never return key bytes.",
  "builtin.http": "Count actual local boundary requests by service name.",
  "builtin.live": "Read actual owned live-service WebSocket events and RTP/RTCPeer media statistics: userTranscript, assistantTranscript, receivedAudio (real received packets/frames > 0), ended (transport/peer closed after cleanup), failed (actual protocol/media failure). Never infer success from fixture input.",
  "builtin.memory-http": "Read strict owned Hindsight HTTP service snapshot of actual requests, stateful model CRUD/history/refresh operations, and unexpected/unconsumed operations; external-service-fixture evidence only, not Hindsight engine semantics.",
  "builtin.memory": "Read actual canonical session Hindsight/Mnemopi state, resolved owned settings, live cached injection snippets, and real owned SQLite database state; never install a replacement backend or return expected values.",
  "builtin.stats": statsWorkflowCaptureSource,
  "builtin.auth": authWorkflowCaptureSource,
  "builtin.auth-http": authWorkflowServiceContract,
  "builtin.collab": collabWorkflowCaptureSource,
  "builtin.process": "Actual child exit status and real restart process generation; a synthetic shutdown callback is not an exit.",
  "builtin.recording": "Observe actual SessionRecorder active state and saved replay file frames in owned recording directory.",
  "builtin.approvals": "Read approval grants from the owned agent directory after real clearApprovalGrants.",
  "builtin.git": "Read actual owned git index/status/HEAD commit via pi-natives/vcs (or git binary): stagedFiles, clean, headSubject. No commands execute through this observer.",
  "builtin.rules": "Discover actual .omp/rules files after rule-generation command; no invented rule persistence callbacks.",
};

export function defaultBuiltinRequirements(scenario: BuiltinCase): BuiltinRequirements {
  const captures: BuiltinRequirements["captures"] = [];
  for (const checkpoint of scenario.checkpoints) for (const comparison of checkpoint.comparisons) {
    const observation = comparison.terminal;
    if (observation.type !== "effect") continue;
    const source = captureSources[observation.channel];
    if (!source) throw new Error(`Missing real capture source for ${observation.channel}`);
    if (!captures.some(capture => capture.channel === observation.channel && capture.pointer === observation.pointer)) captures.push({ channel: observation.channel, pointer: observation.pointer, source });
  }
  return {
    session: { storage: "persisted", seed: "empty", title: "Parity session", userText: "Parity seed question", assistantText: "Parity seed answer", additionalSavedSessions: [] },
    models: { provider: "openai", api: "openai-completions", ids: ["parity-coder", "parity-small"], defaultId: "parity-coder", reasoning: true, imageInput: true, roles: { default: "openai/parity-coder", smol: "openai/parity-small" }, remoteNetwork: "deny", turns: [] },
    host: { clipboard: "owned-memory", openUrl: "capture-without-navigation", recordings: "owned-directory", audio: "silent-loopback-no-device", oauth: "empty-owned-store" },
    services: [], lifecycle: { allowExit: /-exit$/.test(scenario.id), superviseRealRestart: scenario.id.includes("restart"), resumeSameSession: true }, captures, prerequisites: [],
  };
}

export function buildBuiltinRequirements(cases: readonly BuiltinCase[]): Record<string, BuiltinRequirements> {
  const result: Record<string, BuiltinRequirements> = {};
  for (const scenario of cases) result[scenario.id] = defaultBuiltinRequirements(scenario);
  for (const id of ["new", "fresh", "clear", "delete", "session-delete", "export-file", "share-local", "fork-persisted", "compact-provider", "compact-cancel", "handoff-provider", "tree", "branch", "rewind"]) {
    const requirement = result[`builtin-${id}`];
    if (requirement) requirement.session.seed = "transcript";
  }
  for (const id of ["copy-populated", "open-link"]) {
    const requirement = result[`builtin-${id}`];
    if (!requirement) throw new Error(`Missing explicit ${id} scenario`);
    requirement.session.seed = "code-and-link";
    requirement.session.assistantText = "```typescript\nconst parity = 42;\n```\n\nhttps://example.invalid/parity";
  }
  for (const id of ["trace-empty"]) {
    const requirement = result[`builtin-${id}`];
    if (requirement) requirement.session.storage = "memory";
  }
  const providerTurns: Record<string, BuiltinProviderTurn[]> = {
    "force-prompt": [{ match: { textIncludes: "Inspect parity.txt" }, toolCalls: [{ id: "parity-read-call", name: "read", arguments: { path: "parity.txt" } }] }, { match: {}, text: "PARITY_READ_COMPLETE" }],
    "queue-dispatch": [{ match: { textIncludes: "Parity queued prompt" }, text: "PARITY_QUEUE_COMPLETE" }],
    "queue-running": [{ match: { textIncludes: "Parity blocked prompt" }, text: "PARITY_STREAM_STARTED", holdAfterText: true }],
    "goal-lifecycle": [{ match: { textIncludes: "Parity objective" }, text: "PARITY_GOAL_STARTED", holdAfterText: true }],
    "guided-goal-interview": [{ match: { purpose: "foreground" }, text: "PARITY_GUIDED_QUESTION" }],
    "compact-provider": [{ match: { purpose: "compaction" }, text: "PARITY_COMPACTION_SUMMARY" }],
    "compact-cancel": [{ match: { purpose: "compaction" }, text: "PARITY_BLOCKED_COMPACTION", holdAfterText: true }],
    "handoff-provider": [{ match: { purpose: "handoff" }, text: "# PARITY_HANDOFF_DOCUMENT\n\nContinue the parity scratch task." }],
    "btw-provider": [{ match: { textIncludes: "Parity side question", purpose: "btw" }, text: "PARITY_SIDE_ANSWER" }],
    "tan-provider": [{ match: { textIncludes: "Parity background work", purpose: "background" }, text: "PARITY_BACKGROUND_DONE" }],
    "skills-invoke-provider": [{ match: { modelId: "parity-small", textIncludes: "Compress into one routing hint" }, text: "Use for parity echo requests" }, { match: { modelId: "parity-coder", textIncludes: "Parity skill body" }, text: "PARITY_SKILL_RESULT" }],
    "retry-provider": [{ match: { textIncludes: "Parity retry prompt" }, failure: { status: 400, message: "parity deterministic provider failure" } }, { match: { textIncludes: "Parity retry prompt" }, text: "PARITY_RETRY_COMPLETE" }],
    "seance-provider": [{ match: { purpose: "seance" }, text: "PARITY_SEANCE_ANSWER" }],
  };
  for (const [id, turns] of Object.entries(providerTurns)) {
    const requirement = result[`builtin-${id}`];
    if (!requirement) throw new Error(`Missing explicit provider scenario ${id}`);
    requirement.models.turns = turns;
  }
  const seance = result["builtin-seance-provider"];
  if (seance) seance.session.additionalSavedSessions = [{ name: "parity-seance-session", userText: "Parity historical question", assistantText: "Parity historical answer" }];
  const force = cases.find(scenario => scenario.id === "builtin-force-prompt");
  if (force) force.files = { ...force.files, "parity.txt": "parity\n" };
  const local = result["builtin-local-editor"];
  if (local) { local.localFiles = { "parity.txt": "Parity before editor\n" }; local.host.externalEditor = { target: "local", writes: "Parity editor saved\n", exitCode: 0 }; }
  const todo = result["builtin-todo-editor"];
  if (todo) todo.host.externalEditor = { target: "todo", writes: "## Todos\n- [ ] Parity editor task\n", exitCode: 0 };
  for (const [id, requirement] of Object.entries(result)) {
    if (id.includes("worktree") || id.startsWith("builtin-wt-") || id.startsWith("builtin-git") || id.includes("security")) requirement.git = { branch: "parity-main", commitFiles: { "parity.txt": "parity base\n" }, dirtyFiles: { "parity.txt": "parity dirty\n" }, worktreeRoot: "owned-run-directory" };
    if (id.includes("security-disposition")) requirement.prerequisites.push("Seed SecurityStore through its real putBundle API with secscan_parity/finding_parity, report 'Parity report\\n', SARIF version 2.1.0 and producer parity; no provider requests.");
    if (id === "builtin-mcp-local" || id.includes("completion-runtime-mcp-")) requirement.services.push({ kind: "mcp-stdio", behavior: "Real JSON-RPC stdio server on PATH as parity-mcp: initialize serverInfo parity/1.0.0; tools/list echo; resources/list resource parity://resource titled Parity resource; prompts/list parity titled Parity prompt; resources/subscribe supported. No network.", input: { name: "parity", version: "1.0.0", tool: "echo", resource: "parity://resource", prompt: "parity" } });
    if (id === "builtin-skills-local" || id === "builtin-skills-invoke-provider") requirement.services.push({ kind: "skill-registry", behavior: "Set skills.registryUrl to loopback. Implement actual pi-wire skillshare search, packument, version and tarball routes for @parity/echo@1.0.0 with valid sha512 and SKILL.md frontmatter name echo, body 'Parity skill body\\n'; no scripts. Keep lockfile/store and loader real.", input: { scope: "parity", name: "echo", version: "1.0.0", body: "Parity skill body\n" } });
    if (id === "builtin-marketplace-local") requirement.prerequisites.push("Create real local git repository ./parity-marketplace with .claude-plugin/marketplace.json name parity-market and plugin parity-plugin version 1.0.0, source ./parity-plugin; plugin has valid .claude-plugin/plugin.json and no executable hooks. Install/reload/uninstall use actual managers.");
    if (id.includes("mcp-smithery-login")) requirement.services.push({ kind: "smithery", behavior: "At HTTP transport boundary only, make createSmitheryCliAuthSession fail with HTTP 503 so real controller falls back to its manual API-key input; cancel the real input with Escape. Deny all other external traffic.", input: { status: 503 } });
    if (id === "builtin-mcp-smithery-logout") requirement.prerequisites.push("Seed only the owned Smithery credential store with deliberately nonsecret parity-local-key using real saveSmitheryApiKey.");
    if (id.startsWith("builtin-omfg-") && id !== "builtin-omfg-empty") {
      requirement.session.seed = "transcript";
      requirement.session.assistantText = "PARITY_OFFENDING_MARKER";
      requirement.models.turns = [{ match: { purpose: "rule", textIncludes: "Never print parity marker" }, text: JSON.stringify({ name: "parity-rule", description: "Never print the parity marker", condition: ["PARITY_OFFENDING_MARKER"], scope: ["text"], body: "Do not print PARITY_OFFENDING_MARKER." }) }];
    }
    if (id === "builtin-ratchet-unavailable") requirement.prerequisites.push("Launch actual session without eval/task tools, preserving builtin dispatcher.");
    if (id === "builtin-live-no-auth") requirement.prerequisites.push("Empty owned Codex auth; provide silent loopback peer SDP before real withOAuthAccess check; no physical microphone and no Codex endpoint request.");
    if (id === "builtin-live-conversation" || id === "builtin-live-failure") {
      requirement.host.audio = "virtual-loopback-wave";
      requirement.host.oauth = "owned-live-fixture";
      requirement.services.push({ kind: "live", behavior: "Run canonical live controller against owned realtime HTTP/WebSocket/WebRTC fixture; native ALSA loopback and browser getUserMedia consume actual controlled WAV audio; no controller/session substitution.", input: { mode: id.endsWith("failure") ? "failure" : "conversation", userTranscript: "PARITY_LIVE_USER", assistantTranscript: "PARITY_LIVE_REPLY", failureMessage: "PARITY_LIVE_FAILURE" } });
      requirement.prerequisites.push("Ubuntu native ALSA controlled loopback device and real browser virtual-WAV microphone; owned fresh deliberately nonsecret Codex OAuth fixture credential accepted only by owned local live service; never operator auth or remote requests.");
    }
    if (id === "builtin-share-error") requirement.services.push({ kind: "share", behavior: "Set share.serverUrl to owned loopback, share.store=blob; real encrypted POST is refused with HTTP 503 and exact plain response body.", input: { status: 503, body: "parity share boundary refused" } });
    if (id === "builtin-share-local") requirement.services.push({ kind: "share", behavior: "Set share.serverUrl to owned loopback, share.store=blob; accept real encrypted POST with octet-stream bytes and reply with a valid real share identifier.", input: { status: 200, body: "{\"id\":\"parity-share-0001\"}" } });
    if (id.startsWith("builtin-collab-") && id.endsWith("refused")) requirement.services.push({ kind: "collab-relay", behavior: "Accept real WebSocket then close 4029; actual CollabSocket maps it to fatal 'room is full' without retries.", input: { closeCode: 4029, reason: "parity refusal" } });
    if (id.includes("completion-runtime-mcp-")) requirement.mcp = { scope: id.endsWith("remove-scope") ? "user" : "project", name: "parity", command: "parity-mcp", enabled: true };
    if (id.includes("directory") || id.includes("directories") || id === "builtin-workspace-roots") requirement.directories = ["parity-destination", "parity-extra"];
    if (id === "builtin-completion-runtime-local-url") { requirement.localFiles = { "parity.txt": "Parity before editor\n" }; requirement.host.externalEditor = { target: "local", writes: "Parity editor saved\n", exitCode: 0 }; }
    if (id.startsWith("builtin-fork-placement-")) requirement.prerequisites.push("Scrub multiplexer identification environment in both actual hosts; classifyTerminalMultiplexer must naturally return null. Do not stub the classifier.");
    if (id.includes("branch-commit") || id.includes("rewind-commit") || id.includes("tree-commit")) requirement.session.seed = "transcript";
    if (id === "builtin-shake-content") requirement.session.seed = "heavy-blocks";
  }
  return result;
}
