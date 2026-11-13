import { CLI_THINKING_LEVELS } from "@omp-source/packages/tui/src/thinking";
import type { BuiltinCase } from "./scenario";
import { commandCase, composer, dismissThenSend, effect, observed, pathStatus, send } from "./builtin-steps";

const withCovers = (item: BuiltinCase, ...ids: string[]): BuiltinCase => ({ ...item, covers: [...item.covers, ...ids] });

export const executionCases: BuiltinCase[] = [
  commandCase("fast-ultra", "/fast ultra", "Ultrafast mode enabled.", [send("/fast status", "Fast mode is ultra."), send("/fast ultrafast", "Ultrafast mode enabled."), send("/fast toggle", "Fast mode disabled."), send("/fast off", "Fast mode disabled.")]),
  commandCase("computer-status", "/computer status", "Computer use: disabled"),
  ...CLI_THINKING_LEVELS.map(level => commandCase(`effort-${level}`, `/effort ${level}`, `Thinking set to ${level}.`, [], [effect("builtin.session", level, "/thinkingLevel")])),
  commandCase("advisor-status", "/advisor status", "Advisor is disabled."),
  commandCase("session-delete", "/session delete", "Session deleted", [], [effect("builtin.session", false, "/originalFileExists")]),
  commandCase("jobs-full", "/jobs full", "No async jobs yet.", [dismissThenSend("/retry", "Nothing to retry")]),
  commandCase("login-cancel", "/login", "Anthropic", [dismissThenSend("/retry", "Nothing to retry")]),
  commandCase("usage-show", "/usage show", "Usage", [dismissThenSend("/retry", "Nothing to retry")]),
  commandCase("changelog-full", "/changelog full", "Full Changelog", [dismissThenSend("/changelog last 1", "Last Release"), dismissThenSend("/changelog last invalid", "Usage: /changelog [full|last [N]]")]),
  commandCase("compact-modes-empty", "/compact soft", "Nothing to compact (no messages yet)", [send("/compact remote", "Nothing to compact (no messages yet)"), send("/compact snapcompact", "Nothing to compact (no messages yet)"), send("/compact snapcompact focus", "/compact snapcompact does not take focus instructions (it archives history without an LLM summary).")]),
  commandCase("memory-reports", "/memory stats", "Memory backend is off — there is nothing to show.", [send("/memory diagnose", "Memory backend is off — there is nothing to show."), send("/memory queue", "Memory queue is not available for the off backend.")]),
  ...["pane", "window", "tab"].map(placement => commandCase(`fork-placement-${placement}`, `/fork ${placement}`, "No terminal multiplexer was detected.", [], [effect("builtin.session", true, "/sameSessionId")])),
  commandCase("workspace-roots", "/add-dir parity-extra", "Workspace directories:", [
    send("/dirs", "parity-extra", [effect("builtin.session", 1, "/additionalDirectoryCount")]),
    pathStatus("/add-dir parity-extra", "Already in the workspace:"),
    pathStatus("/remove-dir parity-extra", "Removed", [effect("builtin.session", 0, "/additionalDirectoryCount")]),
    pathStatus("/remove-dir parity-extra", "Not a workspace directory:"),
  ], [effect("builtin.session", 1, "/additionalDirectoryCount")]),
  ...["wt", "worktree"].map(token => commandCase(`${token}-commit`, `/${token} parity-branch`, "Moved to worktree", [], [effect("builtin.git", "parity-branch", "/branch"), effect("builtin.session", true, "/cwdMatchesGitRoot")])),
  { ...commandCase("tree-commit", "/tree", "Parity seed question", [{
    ...observed("Navigated to selected point", [effect("builtin.session", true, "/sameSessionId"), effect("builtin.session", 0, "/messageCount")]),
    terminalInput: "\u001b[H\r", browserActions: [{ type: "doubleClick", selector: "text=/Parity seed question/" }],
  }]), settings: { "branchSummary.enabled": false } },
  ...["branch", "rewind"].map(token => commandCase(`${token}-commit`, `/${token}`, "Parity seed question", [{
    ...observed("Rewound to selected point", [effect("builtin.session", true, "/sameSessionId"), effect("builtin.session", 0, "/messageCount")]),
    terminalInput: "\u001b[H\r", browserActions: [{ type: "doubleClick", selector: "text=/Parity seed question/" }],
  }, {
    name: "Restored exact user draft", comparisons: [{ name: "Draft after real rewind", terminal: { type: "screen", pattern: "(Parity seed question)", group: 1 }, browser: { type: "dom", selector: composer, read: "value" }, expected: "Parity seed question" }],
  }])),
  commandCase("git-stage-commit", "/git", "parity.txt", [{
    ...observed("Staged", [effect("builtin.git", ["parity.txt"], "/stagedFiles")]), terminalInput: "s", browserActions: [{ type: "click", selector: 'text="Stage file"' }],
  }, {
    ...observed("Created commit", [effect("builtin.git", true, "/clean"), effect("builtin.git", "test: parity commit", "/headSubject")]),
    terminalInput: "ctest: parity commit\u001b[B\u001b[B\r",
    browserActions: [{ type: "click", selector: "[data-omp-role='omp.app.git.summary']" }, { type: "fill", selector: "input[placeholder='Summary']", value: "test: parity commit" }, { type: "click", selector: 'text="Commit"' }],
  }, dismissThenSend("/retry", "Nothing to retry")]),
  commandCase("prewalk-lifecycle", "/prewalk openai/parity-small", "Prewalk on: switching to openai/parity-small at the next edit/write (todo-gated).", [
    send("/prewalk off", "Prewalk off: canceled this session's pending handoff; keeping the active model.", [effect("builtin.session", false, "/prewalkArmed")]),
    send("/prewalk restart openai/parity-small openai/parity-coder", "Prewalk restarted: using openai/parity-small (openai/parity-small) for planning, then switching to openai/parity-coder (openai/parity-coder) at the next edit/write (todo-gated).", [effect("builtin.session", "parity-small", "/model/id"), effect("builtin.session", true, "/prewalkArmed")]),
    send("/prewalk off", "Prewalk off: canceled this session's pending handoff; keeping the active model."),
  ], [effect("builtin.session", true, "/prewalkArmed")]),
  commandCase("extended-context-persist-restart", "/extended-context on", "Extended context enabled.", [send("/restart", "Parity session", [effect("builtin.process", 1, "/restartCount")]), send("/extended-context status", "Extended context is on.", [effect("builtin.persisted-settings", true, "/extendedContext")])]),
  withCovers(commandCase("shake-content", "/shake images", "Dropped 1 image from this session.", [send("/shake thinking", "Dropped 1 thinking block from this session", [effect("builtin.session", 0, "/thinkingBlocks")])], [effect("builtin.session", 0, "/imageBlocks")]), "component:packages/coding-agent/src/modes/utils/ui-helpers.ts#UiHelpers"),
  commandCase("share-local", "/share", "Share URL:", [], [effect("builtin.http", 1, "/share/requests"), effect("builtin.http", true, "/share/bodyIsBinary")]),
];

for (const scenario of executionCases) scenario.settings = { "memory.backend": "off", "computer.enabled": false, "advisor.enabled": false, ...scenario.settings };
