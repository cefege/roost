import type { BuiltinCase } from "./scenario";
import { commandCase, dismissThenSend, effect, escape, observed, send } from "./builtin-steps";

export const mutationCases: BuiltinCase[] = [
  { ...commandCase("plan-lifecycle", "/plan", "Plan mode enabled. Plan file:", [send("/plan", "Plan mode paused.", [effect("builtin.session", "paused", "/planState")]), send("/plan", "Plan mode disabled.", [effect("builtin.session", "none", "/mode")])], [effect("builtin.session", "plan", "/mode")]), settings: { "plan.enabled": true } },
  { ...commandCase("goal-lifecycle", "/goal set Parity objective", "PARITY_GOAL_STARTED", [
    send("/goal budget 10000", "Goal budget set to 10000.", [effect("builtin.goal", 10000, "/tokenBudget")]),
    send("/goal budget off", "Goal budget cleared."),
    send("/goal budget invalid", "Goal budget must be a positive integer or `off`."),
    send("/goal pause", "Goal mode paused.", [effect("builtin.goal", "paused", "/status")]),
    send("/goal resume", "Goal mode resumed.", [effect("builtin.goal", "active", "/status")]),
    send("/goal drop", "Drop goal?"),
    { ...observed("Goal mode disabled.", [effect("builtin.goal", null)]), terminalInput: "\r", browserActions: [{ type: "click", selector: "text=\"Yes\"" }] },
  ], [effect("builtin.goal", "Parity objective", "/objective")]), settings: { "goal.enabled": true } },
  { ...commandCase("guided-goal-interview", "/guided-goal Parity objective", "PARITY_GUIDED_QUESTION", [], [effect("builtin.provider", true, "/guidedGoalPromptIncluded")]), settings: { "goal.enabled": true } },
  commandCase("switch-local", "/switch openai/parity-coder:high", "Model set to openai/parity-coder.", [send("/switch @smol", "Model set to openai/parity-small.", [effect("builtin.session", "parity-small", "/model/id")]), send("/switch openai/parity-coder", "Model set to openai/parity-coder.")], [effect("builtin.session", "high", "/thinkingLevel")]),
  commandCase("preset-lifecycle", "/modelpreset save parity", 'Saved model preset "parity"', [
    send("/modelpreset list", "Model presets: parity", [effect("builtin.persisted-settings", true, "/modelPresets/parity/exists")]),
    send("/modelpreset switch parity", 'Switched to preset "parity"', [effect("builtin.session", "parity-coder", "/model/id")]),
    send("/modelpreset delete parity", 'Deleted model preset "parity"', [effect("builtin.persisted-settings", false, "/modelPresets/parity/exists")]),
  ]),
  commandCase("compact-provider", "/compact Parity compaction instruction", "compacted", [{ ...observed("PARITY_COMPACTION_SUMMARY"), terminalInput: "\u000f", browserActions: [{ type: "click", selector: "[data-omp-role='omp.compaction']" }] }], [effect("builtin.session", "PARITY_COMPACTION_SUMMARY", "/latestCompaction/summary"), effect("builtin.provider", true, "/compactionInstructionIncluded")]),
  commandCase("compact-cancel", "/compact Parity blocked compaction", "Compacting", [{ ...observed("Compaction cancelled", [effect("builtin.session", false, "/isCompacting"), effect("builtin.provider", true, "/cancelled")]), terminalInput: "\u001b", browserActions: escape }]),
  commandCase("handoff-provider", "/handoff Parity handoff instruction", "Context handed off and compacted in place", [], [effect("builtin.session", true, "/handoffDocumentExists"), effect("builtin.session", true, "/handoffCommitted"), effect("builtin.session", "# PARITY_HANDOFF_DOCUMENT\n\nContinue the parity scratch task.", "/handoffDocument")]),
  commandCase("btw-provider", "/btw Parity side question", "PARITY_SIDE_ANSWER", [], [effect("builtin.btw", "Parity side question", "/0/question"), effect("builtin.btw", "PARITY_SIDE_ANSWER", "/0/answer")]),
  commandCase("tan-provider", "/tan Parity background work", "PARITY_BACKGROUND_DONE", [], [effect("builtin.background", "completed", "/status"), effect("builtin.provider", true, "/backgroundWorkIncluded")]),
  commandCase("retry-provider", "/queue Parity retry prompt", "parity deterministic provider failure", [send("/retry", "PARITY_RETRY_COMPLETE", [effect("builtin.provider", 2, "/requestCount")])]),
  commandCase("seance-provider", "/seance parity-seance-session --model openai/parity-coder", "Seance completed", [], [effect("builtin.provider", true, "/seanceHistoryIncluded")]),
  commandCase("fork-persisted", "/fork", "Session forked", [], [effect("builtin.session", false, "/sameSessionId"), effect("builtin.session", true, "/originalFileExists"), effect("builtin.session", 2, "/messageCount")]),
  commandCase("local-editor", "/local local://parity.txt", "Closed local://parity.txt in editor.", [], [effect("builtin.local-file", "Parity editor saved\n", "/parity.txt/content")]),
  commandCase("todo-editor", "/todo edit", "Todos updated from editor: 1 phase(s), 1 task(s).", [send("/todo", "Parity editor task", [effect("builtin.todos", "Parity editor task", "/0/tasks/0/content")])]),
  commandCase("todo-expand-collapse", "/todo append parity", "Appended to Todos: Parity", [send("/todo expand", "Parity", [effect("builtin.session", true, "/todoExpanded")]), send("/todo collapse", "Parity", [effect("builtin.session", false, "/todoExpanded")])]),
  commandCase("rename-persist-restart", "/rename Persisted parity", 'Session renamed to "Persisted parity".', [send("/restart", "Persisted parity", [effect("builtin.session", "Persisted parity", "/persistedTitle"), effect("builtin.process", 1, "/restartCount")]), send("/rename After restart", 'Session renamed to "After restart".')]),
  commandCase("model-picker-commit", "/model", "Models", [{
    ...observed("parity-small", [effect("builtin.session", "parity-small", "/model/id")]),
    terminalInput: "parity-small\r", browserActions: [{ type: "fill", selector: "[data-omp-node='picker'] input, [role='dialog'] input", value: "parity-small" }, { type: "press", key: "Enter" }],
  }]),
  commandCase("omfg-cancel", "/omfg Never print parity marker", "Save TTSR rule where?", [dismissThenSend("/retry", "Nothing to retry", [effect("builtin.rules", [])])]),
  commandCase("omfg-save", "/omfg Never print parity marker", "Save TTSR rule where?", [{ ...observed("Saved", [effect("builtin.workspace-file", true, "/.omp/rules/parity-rule.md/exists")]), terminalInput: "\r", browserActions: [{ type: "click", selector: 'text="This project (.omp/rules)"' }] }]),
];
