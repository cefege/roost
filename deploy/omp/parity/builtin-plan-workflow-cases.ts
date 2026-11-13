import type { BuiltinCase, Checkpoint, Comparison } from "./scenario";
import { commandCase, composer, effect, observed, send, textComparison } from "./builtin-steps";

export const parityPlan = "# Parity workflow\n\n## Implement\nWrite parity-result.txt with the approved result.\n\n## Review\nKeep workspace files unchanged until approval.\n";
export const refinedParityPlan = "# Parity workflow\n\n## Implement\nWrite parity-result.txt with the refined result.\n\n## Review\nKeep workspace files unchanged until approval.\n";
const review = "Plan mode - next step";
const planFile = "/parity-workflow-plan.md/content";
const planSettings = { "plan.enabled": true, "plan.defaultOnStartup": false, "plan.autosave": true, "plan.autosaveDir": "parity-plans", "compaction.keepRecentTokens": 1 };

function pick(label: string, keys: string, text: string, comparisons: Comparison[] = []): Checkpoint {
  const index = label === "Approve and execute" ? 0 : label === "Approve and compact context" ? 1 : label === "Refine plan" ? 3 : 2;
  return {
    ...observed(text, comparisons), terminalInput: keys,
    browserActions: [{ type: "doubleClick", selector: `[data-omp-role='omp.plan.options'] [data-omp-node$='o${index}']` }],
  };
}

function approvedState(content: string, sameSession: boolean): Comparison[] {
  return [
    effect("builtin.session", "none", "/mode"),
    effect("builtin.session", null, "/planModeState"),
    effect("builtin.session", "local://parity-workflow-plan.md", "/planReferencePath"),
    effect("builtin.session", sameSession, "/sameSessionId"),
    effect("builtin.local-file", content, planFile),
    effect("builtin.workspace-file", content, "/parity-plans/PARITY_WORKFLOW_PLAN.md/content"),
    effect("builtin.workspace-file", content === refinedParityPlan ? "refined parity\n" : "approved parity\n", "/parity-result.txt/content"),
    effect("builtin.workspace-file", "workspace seed\n", "/parity-protected.txt/content"),
    effect("builtin.provider", 0, "/remoteRequests"),
  ];
}

function approvalCase(id: string, label: string, keys: string, sameSession: boolean): BuiltinCase {
  return {
    ...commandCase(id, "/plan Draft parity workflow", review, [
      pick(label, keys, "PARITY_PLAN_EXECUTED", [...approvedState(parityPlan, sameSession), ...(id.endsWith("-compact") ? [effect("builtin.session", "PARITY_PLAN_COMPACTION_SUMMARY", "/latestCompaction/summary")] : [])]),
      send("/plan-review", "Plan mode is not active.", [effect("builtin.local-file", parityPlan, planFile)]),
    ], [effect("builtin.local-file", parityPlan, planFile), effect("builtin.session", "plan", "/mode"), effect("builtin.workspace-file", false, "/parity-result.txt/exists")]),
    settings: planSettings, files: { "parity-protected.txt": "workspace seed\n" },
  };
}

export const planWorkflowCases: BuiltinCase[] = [
  approvalCase("plan-workflow-execute", "Approve and execute", "\r", false),
  approvalCase("plan-workflow-keep", "Approve and keep context", "\u001b[B\u001b[B\r", true),
  approvalCase("plan-workflow-compact", "Approve and compact context", "\u001b[B\r", true),
  {
    ...commandCase("plan-workflow-refine-reopen", "/plan Draft parity workflow", review, [
      { ...observed(review, [effect("builtin.local-file", "# Parity workflow\n\n## Review\nKeep workspace files unchanged until approval.\n", planFile), effect("builtin.workspace-file", false, "/parity-result.txt/exists")]), terminalInput: "\t\u001b[Bd", browserActions: [{ type: "click", selector: "[data-omp-role='omp.plan.toc'] [data-omp-node$='h1']" }, { type: "press", key: "d" }] },
      { ...observed(review, [effect("builtin.local-file", parityPlan, planFile)]), terminalInput: "u\u001b[Z", browserActions: [{ type: "press", key: "u" }] },
      { ...observed(review, [effect("builtin.clipboard", parityPlan)]), terminalInput: "c", browserActions: [{ type: "click", selector: "[data-omp-role='omp.plan.tools'] button:has-text('Copy')" }] },
      pick("Refine plan", "\u001b[B\u001b[B\u001b[B\r", "Refine plan: enter a follow-up prompt.", [effect("builtin.session", "plan", "/mode"), effect("builtin.workspace-file", false, "/parity-result.txt/exists")]),
      send("Refine parity workflow to use the refined result", review, [effect("builtin.local-file", refinedParityPlan, planFile)]),
      { ...observed("PARITY_REFINED_REVIEW_READY", [effect("builtin.session", "plan", "/mode"), effect("builtin.local-file", refinedParityPlan, planFile)]), terminalInput: "\u001b", browserActions: [{ type: "click", selector: "[data-omp-role='omp.plan.tools'] button:has-text('Cancel')" }] },
      send("/plan-review", review, [effect("builtin.local-file", refinedParityPlan, planFile), effect("builtin.session", "local://parity-workflow-plan.md", "/planModeState/planFilePath")]),
      pick("Approve and keep context", "\u001b[B\u001b[B\r", "PARITY_PLAN_EXECUTED", approvedState(refinedParityPlan, true)),
    ], [effect("builtin.local-file", parityPlan, planFile)]),
    settings: planSettings, files: { "parity-protected.txt": "workspace seed\n" },
  },
  {
    ...commandCase("plan-workflow-readonly", "/plan Exercise parity read-only guard", "PARITY_READONLY_FAILURE_SEEN", [
      { ...observed(review, [effect("builtin.local-file", parityPlan, planFile), effect("builtin.workspace-file", "workspace seed\n", "/parity-protected.txt/content")]), releaseProviderTurns: [1] },
      { ...observed("PARITY_READONLY_REVIEW_READY", [effect("builtin.session", "plan", "/mode"), effect("builtin.workspace-file", "workspace seed\n", "/parity-protected.txt/content")]), terminalInput: "\u001b", browserActions: [{ type: "click", selector: "[data-omp-role='omp.plan.tools'] button:has-text('Cancel')" }] },
      send("/plan", "Exit plan mode?", [effect("builtin.session", "plan", "/mode")]),
      { ...observed("PARITY_READONLY_REVIEW_READY", [effect("builtin.session", "plan", "/mode"), effect("builtin.workspace-file", "workspace seed\n", "/parity-protected.txt/content")]), terminalInput: "\u001b", browserActions: [{ type: "press", key: "Escape" }] },
      send("/plan", "Exit plan mode?"),
      { ...observed("Plan mode paused.", [effect("builtin.session", "plan_paused", "/persistedMode"), effect("builtin.session", null, "/planModeState"), effect("builtin.local-file", parityPlan, planFile)]), terminalInput: "\r", browserActions: [{ type: "click", selector: 'text="Yes"' }] },
      send("/plan", "Plan mode disabled.", [effect("builtin.session", "none", "/persistedMode"), effect("builtin.local-file", parityPlan, planFile)]),
      send("Write parity-protected.txt after leaving plan mode", "PARITY_WRITABLE_AFTER_PLAN", [effect("builtin.workspace-file", "written after exit\n", "/parity-protected.txt/content")]),
    ], [textComparison("Plan mode: the working tree is read-only."), effect("builtin.workspace-file", "workspace seed\n", "/parity-protected.txt/content"), effect("builtin.session", "plan", "/persistedMode")]),
    settings: planSettings, files: { "parity-protected.txt": "workspace seed\n" },
  },
  {
    ...commandCase("plan-workflow-compact-cancel", "/plan Draft parity workflow", review, [
      pick("Approve and compact context", "\u001b[B\r", "Compacting", [effect("builtin.session", true, "/isCompacting"), effect("builtin.provider", 3, "/requestCount")]),
      { ...observed("Plan approved, but compaction was cancelled — execution not dispatched. Submit a turn to continue.", [effect("builtin.session", false, "/isCompacting"), effect("builtin.session", "none", "/mode"), effect("builtin.session", "local://parity-workflow-plan.md", "/planReferencePath"), effect("builtin.local-file", parityPlan, planFile), effect("builtin.workspace-file", false, "/parity-result.txt/exists"), effect("builtin.provider", true, "/cancelled")]), terminalInput: "\u001b", browserActions: [{ type: "press", key: "Escape" }] },
      send("Continue the approved parity workflow", "PARITY_PLAN_EXECUTED", approvedState(parityPlan, true)),
    ]), settings: planSettings, files: { "parity-protected.txt": "workspace seed\n" },
  },
  {
    ...commandCase("goal-workflow-complete", "/goal set Produce parity goal artifact", "PARITY_GOAL_COMPLETE", [
      send("/goal show", "No goal set.", [effect("builtin.goal", null), effect("builtin.session", "Produce parity goal artifact", "/completedGoal/objective"), effect("builtin.session", "none", "/persistedMode")]),
    ], [effect("builtin.workspace-file", "goal complete\n", "/parity-goal.txt/content"), effect("builtin.goal", null), effect("builtin.session", "Produce parity goal artifact", "/completedGoal/objective"), effect("builtin.session", "none", "/persistedMode")]), settings: { "goal.enabled": true },
  },
  {
    ...commandCase("guided-goal-workflow-complete", "/guided-goal Produce parity guided artifact", "Which parity artifact should I create?", [
      send("Create parity-guided.txt with guided complete", "PARITY_GUIDED_COMPLETE", [effect("builtin.workspace-file", "guided complete\n", "/parity-guided.txt/content"), effect("builtin.goal", null), effect("builtin.session", "Produce parity guided artifact", "/completedGoal/objective"), effect("builtin.session", 10000, "/completedGoal/tokenBudget")]),
      send("/goal show", "No goal set.", [effect("builtin.session", "none", "/persistedMode")]),
    ]), settings: { "goal.enabled": true },
  },
  {
    ...commandCase("advisor-workflow-advice", "/advisor on", "Advisor enabled.", [
      send("Review parity advisor artifact", "PARITY_ADVISOR_NOTE: Add the missing newline to parity-advised.txt.", [effect("builtin.session", true, "/advisor/stats/active"), effect("builtin.session", 3, "/advisor/stats/messages/assistant"), effect("builtin.session", "PARITY_ADVISOR_NOTE: Add the missing newline to parity-advised.txt.", "/advisor/notes/0/note"), effect("builtin.session", "nit", "/advisor/notes/0/severity"), effect("builtin.session", "PARITY_ADVISOR_NOTE: Add the missing newline to parity-advised.txt.", "/advisor/persistedNotes/0/note"), effect("builtin.workspace-file", "missing newline", "/parity-advised.txt/content")]),
      send("/advisor dump raw", "Advisor history copied to clipboard", [effect("builtin.session", true, "/advisor/clipboardMatchesHistory")]),
      send("/advisor off", "Advisor disabled.", [effect("builtin.session", false, "/advisor/stats/active")]),
      send("Apply parity advisor note", "PARITY_ADVICE_APPLIED", [effect("builtin.workspace-file", "missing newline\n", "/parity-advised.txt/content"), effect("builtin.session", "PARITY_ADVISOR_NOTE: Add the missing newline to parity-advised.txt.", "/advisor/persistedNotes/0/note"), effect("builtin.provider", 0, "/remoteRequests")]),
    ], [effect("builtin.session", true, "/advisor/stats/active"), effect("builtin.workspace-file", "missing newline", "/parity-advised.txt/content")]), settings: { "advisor.enabled": false, "advisor.reviewMode": "agent-end", "advisor.continueSeverity": "concern" },
    files: {
      "parity-advised.txt": "missing newline",
      "WATCHDOG.yml": "advisors:\n  - name: Parity reviewer\n    model: openai/parity-small\n    tools: [read]\n    reviewMode: agent-end\n    enabled: true\n    instructions: Inspect parity-advised.txt and report a concrete missing-newline issue through advise.\n",
    },
  },
  {
    ...commandCase("vibe-workflow-worker", "/vibe Direct the parity worker", "PARITY_DIRECTOR_FIRST_COMPLETE", [
      send("Continue parity worker with second artifact", "PARITY_DIRECTOR_SECOND_COMPLETE", [effect("builtin.workspace-file", "worker second\n", "/parity-worker-second.txt/content"), effect("builtin.session", 2, "/vibe/workers/parity-worker/turns"), effect("builtin.session", true, "/vibe/childFiles/parity-worker/exists")]),
      send("Kill the parity worker", "PARITY_DIRECTOR_KILLED", [effect("builtin.session", "explicit-kill", "/vibe/tombstones/parity-worker/reason"), effect("builtin.session", true, "/vibe/workers/parity-worker/killed"), effect("builtin.session", true, "/vibe/childFiles/parity-worker/exists")]),
      send("/vibe", "Vibe mode disabled.", [effect("builtin.session", "none", "/persistedMode"), effect("builtin.workspace-file", "worker first\n", "/parity-worker-first.txt/content"), effect("builtin.workspace-file", "worker second\n", "/parity-worker-second.txt/content"), effect("builtin.provider", 0, "/remoteRequests")]),
    ], [effect("builtin.workspace-file", "worker first\n", "/parity-worker-first.txt/content"), effect("builtin.session", 1, "/vibe/workers/parity-worker/turns"), effect("builtin.session", "vibe", "/mode")]),
    settings: { "task.enableLsp": false },
  },
  {
    ...commandCase("vibe-workflow-exit-cancel", "/vibe Start a cancellable parity worker", "PARITY_VIBE_WORKER_STREAM", [
      send("/vibe", "Vibe mode disabled. Killed 1 worker session.", [effect("builtin.session", "none", "/persistedMode"), effect("builtin.session", "mode-exit", "/vibe/tombstones/parity-cancel-worker/reason"), effect("builtin.session", true, "/vibe/childFiles/parity-cancel-worker/exists"), effect("builtin.provider", true, "/cancelled"), effect("builtin.provider", 0, "/remoteRequests")]),
    ], [effect("builtin.session", "vibe", "/mode"), effect("builtin.session", true, "/vibe/childFiles/parity-cancel-worker/exists")]),
    settings: { "task.enableLsp": false },
  },
];

for (const scenario of planWorkflowCases) {
  const canonical = scenario.command.slice(1).split(" ", 1)[0]!;
  for (const checkpoint of scenario.checkpoints) {
    const submitted = checkpoint.browserActions?.find(action => action.type === "fill" && action.selector === composer);
    const command = submitted?.type === "fill" ? submitted.value : scenario.command;
    const tokens = command.match(/^\/(goal|advisor)\s+(set|show|drop|on|off|dump)\b/);
    const entry = tokens ? `subcommand:${tokens[1]} ${tokens[2]}` : command.startsWith("/plan-review") ? "command:plan-review" : `command:${canonical}`;
    if (!scenario.covers.includes(entry)) scenario.covers.push(entry);
    for (const comparison of checkpoint.comparisons) {
      const effectKind = comparison.terminal.type === "effect" ? "host" : "interaction";
      comparison.proves = [{ entry, effect: effectKind }];
      if (entry !== `command:${canonical}`) comparison.proves.push({ entry: `command:${canonical}`, effect: effectKind });
    }
  }
}
