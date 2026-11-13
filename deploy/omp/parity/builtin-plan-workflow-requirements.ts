import type { BuiltinProviderTurn, BuiltinRequirements } from "./builtin-requirements";
import { parityPlan, refinedParityPlan } from "./builtin-plan-workflow-cases";

function toolTurn(id: string, name: string, arguments_: Record<string, unknown>, textIncludes?: string, modelId?: string): BuiltinProviderTurn {
  return { match: { ...(textIncludes ? { textIncludes } : {}), ...(modelId ? { modelId } : {}) }, chunkDelayMs: 0, toolCalls: [{ id, name, arguments: arguments_ }] };
}

function planDraft(content = parityPlan, marker = "PARITY_PLAN_REVIEW_READY"): BuiltinProviderTurn[] {
  return [
    toolTurn(`${marker}-draft`, "write", { path: "local://parity-workflow-plan.md", content }),
    { ...toolTurn(`${marker}-proposal`, "write", { path: "xd://propose", content: "parity-workflow" }), text: marker },
  ];
}

function execution(textIncludes = "Plan approved.", content = "approved parity\n"): BuiltinProviderTurn[] {
  return [
    toolTurn("parity-plan-execution", "write", { path: "parity-result.txt", content }, textIncludes),
    { match: {}, text: "PARITY_PLAN_EXECUTED", chunkDelayMs: 0 },
  ];
}

const sessionCaptureSource = [
  "Plan APIs: actual session.getPlanModeState() ?? null, getPlanReferencePath(), and persisted sessionManager.buildSessionContext().mode as persistedMode; never infer approval from UI text.",
  "Goal completion: read latest custom entry customType goal-completed from the real journal into completedGoal (interactive-mode.ts:6137-6145); completed goals are removed from live mode, not retained as active goals.",
  "Advisor: stats=session.getAdvisorStats(); notes flatten actual custom advisor cards' details.notes in session.getMessages(); persistedNotes flatten the same actual persisted message entries; transcript=session.formatAdvisorHistoryAsText({compact:false}); clipboardMatchesHistory compares actual last host clipboard bytes to that current raw transcript, never a supplied marker.",
  "Vibe: workers are actual VibeSessionRegistry.global().screens(real ToolSession), keyed by id, preserving turns/state/killed; tombstones are actual latest vibe-session-lifecycle custom entry data action=tombstone keyed by id; childFiles[id].exists comes from stat of the actual spawn childSessionFile relative to parent session artifact directory. Preserve lifecycle data and read actual child journals; do not synthesize workers from fixture names.",
].join(" ");

function requireCase(requirements: Record<string, BuiltinRequirements>, id: string): BuiltinRequirements {
  const requirement = requirements[`builtin-${id}`];
  if (!requirement) throw new Error(`Missing plan workflow scenario builtin-${id}`);
  for (const capture of requirement.captures) if (capture.channel === "builtin.session") capture.source += ` ${sessionCaptureSource}`;
  return requirement;
}

export function applyPlanWorkflowRequirements(requirements: Record<string, BuiltinRequirements>): void {
  for (const suffix of ["execute", "keep", "compact"]) {
    const requirement = requireCase(requirements, `plan-workflow-${suffix}`);
    if (suffix === "compact") requirement.session.seed = "transcript";
    requirement.models.turns = [
      ...planDraft(),
      ...(suffix === "compact" ? [{ match: { purpose: "compaction" }, text: "PARITY_PLAN_COMPACTION_SUMMARY", chunkDelayMs: 0 } satisfies BuiltinProviderTurn] : []),
      ...execution(),
    ];
    requirement.prerequisites.push("Use the actual built-in write/read/edit tools and real xd://propose dispatch. Keep the plan approval controller and both native PlanReviewOverlay descriptions and TUI input routing real. Initial session is persisted; the compact variant includes a real seeded prior transcript so compaction has an older turn to summarize. Default plan role must not resolve outside the loopback model catalog. Autosave runs through plan-autosave.ts and creates parity-plans/PARITY_WORKFLOW_PLAN.md in the owned cwd.");
  }

  const refine = requireCase(requirements, "plan-workflow-refine-reopen");
  refine.models.turns = [...planDraft(), ...planDraft(refinedParityPlan, "PARITY_REFINED_REVIEW_READY"), ...execution("Plan approved.", "refined parity\n")];
  refine.prerequisites.push("Use the real plan overlay with a terminal at least 64 columns wide so Contents is available. Tab from actions enters Contents; Down selects Implement; d deletes that section and mirrors disk; u restores exact source bytes. Empty Refine plan returns to normal composer; the subsequent real user prompt writes the refined plan and submits xd://propose again. Cancel only dismisses review; /plan-review rescans actual artifact mtimes and reopens the latest draft.");

  const readonly = requireCase(requirements, "plan-workflow-readonly");
  readonly.models.turns = [
    toolTurn("parity-plan-forbidden-write", "write", { path: "parity-protected.txt", content: "must never be written\n" }, "Exercise parity read-only guard"),
    { ...toolTurn("parity-plan-recovery-draft", "write", { path: "local://parity-workflow-plan.md", content: parityPlan }), text: "PARITY_READONLY_FAILURE_SEEN", holdAfterText: true },
    { ...toolTurn("parity-plan-recovery-proposal", "write", { path: "xd://propose", content: "parity-workflow" }), text: "PARITY_READONLY_REVIEW_READY" },
    toolTurn("parity-after-plan-write", "write", { path: "parity-protected.txt", content: "written after exit\n" }, "Write parity-protected.txt after leaving plan mode"),
    { match: {}, text: "PARITY_WRITABLE_AFTER_PLAN", chunkDelayMs: 0 },
  ];
  readonly.prerequisites.push("Held provider turn index 1 has a genuine pending write tool call, not an executed callback. First checkpoint observes the real enforcePlanModeWrite ToolError and unchanged workspace; releaseProviderTurns [1] completes SSE so the real model loop executes only the local plan write. Exit confirm cancellation retains plan mode; Yes journals plan_paused, next toggle journals none, and only then the same built-in write can mutate the workspace.");

  const compactCancel = requireCase(requirements, "plan-workflow-compact-cancel");
  compactCancel.session.seed = "transcript";
  compactCancel.models.turns = [
    ...planDraft(),
    { match: { purpose: "compaction" }, text: "PARITY_PLAN_COMPACTION_HELD", holdAfterText: true },
    ...execution("Continue the approved parity workflow"),
  ];
  compactCancel.prerequisites.push("Run actual plan approval compact path. Escape aborts the real local SSE compaction request; the controller retains the durable approved reference and autosave but does not dispatch execution. The next explicit user prompt dispatches real tools with the injected reference. No synthetic cancellation or compaction result.");

  const goal = requireCase(requirements, "goal-workflow-complete");
  goal.models.turns = [
    toolTurn("parity-goal-write", "write", { path: "parity-goal.txt", content: "goal complete\n" }, "Produce parity goal artifact"),
    toolTurn("parity-goal-get", "goal", { op: "get" }),
    toolTurn("parity-goal-complete", "goal", { op: "complete" }),
    { match: {}, text: "PARITY_GOAL_COMPLETE", chunkDelayMs: 0 },
  ];
  goal.prerequisites.push("Goal commands and goal tool use actual GoalRuntime; wait for real agent_end so completion appends goal-completed and mode none. Capture objective from that actual durable completion record, not getGoalModeState after completion (which is cleared).");

  const guided = requireCase(requirements, "guided-goal-workflow-complete");
  guided.models.turns = [
    { match: { purpose: "foreground" }, text: "Which parity artifact should I create?", chunkDelayMs: 0 },
    toolTurn("parity-guided-create", "goal", { op: "create", objective: "Produce parity guided artifact", token_budget: 10000 }, "Create parity-guided.txt with guided complete"),
    toolTurn("parity-guided-write", "write", { path: "parity-guided.txt", content: "guided complete\n" }),
    toolTurn("parity-guided-complete", "goal", { op: "complete" }),
    { match: {}, text: "PARITY_GUIDED_COMPLETE", chunkDelayMs: 0 },
  ];
  guided.prerequisites.push("Guided goal interview is the real synthetic developer kickoff and ordinary composer answer. The goal tool becomes active through handleGuidedGoalCommand, creates the goal through GoalRuntime, and completes it normally; do not seed or directly set goal state.");

  const advisor = requireCase(requirements, "advisor-workflow-advice");
  advisor.models.roles.advisor = "openai/parity-small";
  advisor.git = { branch: "parity-advisor", commitFiles: { "parity-advised.txt": "missing newline" }, dirtyFiles: {}, worktreeRoot: "owned-run-directory" };
  advisor.prerequisites.push("Place WATCHDOG.yml through real workspace files before session construction; project discovery must find its roster, not stub SessionAdvisors. Start disabled and activate through /advisor on. agent-end cadence reviews only final primary yield; a nit remains an actual visible/persisted card without waking the primary. Do not fabricate advisor cards, stats, transcript, or clipboard response. The later explicit user turn invokes write with advisor disabled.");
  advisor.models.turns = [
    { match: { textIncludes: "Review parity advisor artifact", modelId: "parity-coder" }, text: "PARITY_PRIMARY_DRAFT", chunkDelayMs: 0 },
    toolTurn("parity-advisor-read", "read", { path: "parity-advised.txt" }, undefined, "parity-small"),
    toolTurn("parity-advisor-advise", "advise", { note: "PARITY_ADVISOR_NOTE: Add the missing newline to parity-advised.txt.", severity: "nit" }, undefined, "parity-small"),
    { match: { modelId: "parity-small" }, text: "PARITY_ADVISOR_REVIEW_COMPLETE", chunkDelayMs: 0 },
    toolTurn("parity-advice-apply", "write", { path: "parity-advised.txt", content: "missing newline\n" }, "PARITY_ADVISOR_NOTE: Add the missing newline to parity-advised.txt.", "parity-coder"),
    { match: { modelId: "parity-coder" }, text: "PARITY_ADVICE_APPLIED", chunkDelayMs: 0 },
  ];

  const vibe = requireCase(requirements, "vibe-workflow-worker");
  vibe.models.roles.smol = "openai/parity-small";
  vibe.models.roles.task = "openai/parity-small";
  vibe.prerequisites.push("Vibe workers are real runSubprocess keepAlive task-executor sessions and real follow-up turns, never fake fast/good executables. All child processes inherit the owned model registry and loopback provider URL, deliberately nonsecret local fixture credentials, and isolated HOME. Route tape match.modelId before consumption because worker and director requests interleave; preserve original turn indexes. vibe_wait names parity-worker explicitly and consumes real async job results to prevent duplicate automatic follow-ups (vibe/runtime.ts:928-1015). Delay the worker final SSE frame until the real director has installed its wait, without replacing tool/model loops; worker chunkDelayMs provides the local boundary delay. Keep real child JSONL and lifecycle spawn/turn/tombstone entries.");
  vibe.models.turns = [
    toolTurn("parity-vibe-spawn", "vibe_spawn", { cli: "fast", name: "parity-worker", prompt: "Create the first parity worker artifact" }, "Direct the parity worker", "parity-coder"),
    toolTurn("parity-vibe-wait-first", "vibe_wait", { sessions: ["parity-worker"], timeout: 30 }, undefined, "parity-coder"),
    toolTurn("parity-vibe-worker-write-first", "write", { path: "parity-worker-first.txt", content: "worker first\n" }, "Create the first parity worker artifact", "parity-small"),
    { match: { modelId: "parity-small" }, textChunks: ["PARITY_WORKER_FIRST_COMPLETE"], chunkDelayMs: 1000 },
    { match: { modelId: "parity-coder" }, text: "PARITY_DIRECTOR_FIRST_COMPLETE", chunkDelayMs: 0 },
    toolTurn("parity-vibe-send", "vibe_send", { session: "parity-worker", message: "Create the second parity worker artifact" }, "Continue parity worker with second artifact", "parity-coder"),
    toolTurn("parity-vibe-wait-second", "vibe_wait", { sessions: ["parity-worker"], timeout: 30 }, undefined, "parity-coder"),
    toolTurn("parity-vibe-worker-write-second", "write", { path: "parity-worker-second.txt", content: "worker second\n" }, "Create the second parity worker artifact", "parity-small"),
    { match: { modelId: "parity-small" }, textChunks: ["PARITY_WORKER_SECOND_COMPLETE"], chunkDelayMs: 1000 },
    { match: { modelId: "parity-coder" }, text: "PARITY_DIRECTOR_SECOND_COMPLETE", chunkDelayMs: 0 },
    toolTurn("parity-vibe-kill", "vibe_kill", { session: "parity-worker" }, "Kill the parity worker", "parity-coder"),
    toolTurn("parity-vibe-list", "vibe_list", {}, undefined, "parity-coder"),
    { match: { modelId: "parity-coder" }, text: "PARITY_DIRECTOR_KILLED", chunkDelayMs: 0 },
  ];

  const vibeCancel = requireCase(requirements, "vibe-workflow-exit-cancel");
  vibeCancel.models.roles.smol = "openai/parity-small";
  vibeCancel.models.roles.task = "openai/parity-small";
  vibeCancel.prerequisites.push("Use the same real worker/provider boundary as vibe-workflow-worker. Worker SSE text remains genuinely open at original tape index 2; parent vibe_wait renders actual outputTail in TUI and native HTML (packages/tui/src/tools/vibe.ts:252-255,388-389). /vibe executes real runModeExitTeardown, aborts parent wait and worker HTTP stream, persists the mode-exit tombstone before teardown, retains the child's journal, and suppresses pending async result delivery. Do not release the held worker or synthesize cancellation.");
  vibeCancel.models.turns = [
    toolTurn("parity-vibe-cancel-spawn", "vibe_spawn", { cli: "fast", name: "parity-cancel-worker", prompt: "Wait for parity cancellation" }, "Start a cancellable parity worker", "parity-coder"),
    toolTurn("parity-vibe-cancel-wait", "vibe_wait", { sessions: ["parity-cancel-worker"], timeout: 30 }, undefined, "parity-coder"),
    { match: { modelId: "parity-small", textIncludes: "Wait for parity cancellation" }, text: "PARITY_VIBE_WORKER_STREAM", chunkDelayMs: 0, holdAfterText: true },
  ];
}
