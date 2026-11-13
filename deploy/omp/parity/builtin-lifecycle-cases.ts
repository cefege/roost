import type { BuiltinCase } from "./scenario";
import { commandCase, dismissThenSend, effect, observed, send } from "./builtin-steps";

export const lifecycleCases: BuiltinCase[] = [
  commandCase("new", "/new", "New session started", [], [effect("builtin.session", 0, "/messageCount"), effect("builtin.session", false, "/sameSessionId")]),
  commandCase("fresh", "/fresh", "Fresh provider session started (0 provider states pruned).", [], [effect("builtin.session", 2, "/messageCount")]),
  commandCase("clear", "/clear", "Context reset — 2 messages dropped; session continues.", [], [effect("builtin.session", 0, "/messageCount"), effect("builtin.session", true, "/sameSessionId")]),
  commandCase("delete", "/delete", "Session deleted", [], [effect("builtin.session", false, "/originalFileExists")]),
  commandCase("compact-empty", "/compact", "Nothing to compact (no messages yet)"),
  commandCase("shake-empty", "/shake elide", "Nothing to shake.", [send("/shake images", "Nothing to shake."), send("/shake thinking", "Nothing to shake."), send("/shake invalid", 'Unknown /shake mode "invalid". Use elide, images, or thinking.')]),
  commandCase("handoff-empty", "/handoff", "Nothing to hand off (no messages yet)"),
  commandCase("resume-missing", "/resume parity-does-not-exist", 'Session "parity-does-not-exist" not found'),
  commandCase("pin", "/pin", "Session pinned to the top of the resume list.", [send("/pin", "Session unpinned.", [effect("builtin.session", false, "/pinned")])]),
  commandCase("btw-history", "/btw", "No side questions yet", [dismissThenSend("/retry", "Nothing to retry")]),
  commandCase("tan-empty", "/tan", "Usage: /tan <work>"),
  commandCase("omfg-empty", "/omfg", "Usage: /omfg <complaint>"),
  commandCase("cleanse-invalid", "/cleanse --parity-invalid", "Usage: /cleanse [request] [--all] [--tests] [-n <agents>] [-m <model>]"),
  commandCase("retry-empty", "/retry", "Nothing to retry"),
  (() => {
    const debugCase = commandCase("debug-cancel", "/debug", "Debug", [dismissThenSend("/retry", "Nothing to retry")]);
    return { ...debugCase, covers: [...debugCase.covers, "component:packages/coding-agent/src/debug/index.ts#DebugSelectorComponent"] };
  })(),
  commandCase("memory-empty", "/memory view", "Memory payload is empty (memory backend off, disabled, or no memory available).", [send("/memory clear", "Memory data cleared and system prompt refreshed."), send("/memory reset", "Memory data cleared and system prompt refreshed."), send("/memory enqueue", "Memory consolidation enqueued."), send("/memory rebuild", "Memory consolidation enqueued."), send("/memory sync", "Memory consolidation ran.")]),
  ...["list", "show", "refresh", "history", "seed", "delete", "reload", "remove"].map(verb => commandCase(`memory-mm-${verb}`, `/memory mm ${verb}`, "Hindsight backend is not active for this session.")),
  commandCase("rename", "/rename Parity title", 'Session renamed to "Parity title".', [send("/rename Second title", 'Session renamed to "Second title".', [effect("builtin.session", "Second title", "/persistedTitle")])]),
  commandCase("move-error", "/move absent/child", 'Cannot create "child": parent directory does not exist'),
  ...["wt", "worktree"].map(token => commandCase(`${token}-invalid`, `/${token} bad..branch`, "Worktree creation failed: Invalid branch name: bad..branch")),
  commandCase("add-dir-error", "/add-dir absent", "Directory does not exist:"),
  commandCase("remove-dir-root", "/remove-dir .", "Cannot remove the working directory; use /move to change it."),
  commandCase("dirs", "/dirs", "Workspace directories:"),
  commandCase("ssh-error", "/ssh add", "Usage: /ssh add <name> --host <host> [--user <user>] [--port <port>] [--key <keyPath>] [--desc <description>] [--compat] [--scope project|user]", [send("/ssh remove", "Host name required. Usage: /ssh remove <name> [--scope project|user]"), send("/ssh invalid", "Unknown subcommand: invalid. Type /ssh help for usage.")]),
  commandCase("seance-invalid", "/seance --invalid", "Unknown /seance option: --invalid", [send("/seance --model", "Usage: /seance [session id|path] [--model selector]"), send("/seance a b", "Usage: /seance [session id|path] [--model selector]")]),
  ...["exit", "quit", "q"].map((token): BuiltinCase => ({
    id: `builtin-${token}-exit`, covers: [`command:${token}`], command: "/rename Exit parity",
    checkpoints: [observed('Session renamed to "Exit parity".'), {
      ...send(`/${token}`, "Exit parity", [effect("builtin.process", 0, "/exitCode")]),
      comparisons: [
        { name: "Last rendered title", terminal: { type: "screen", pattern: '(Exit parity)', group: 1 }, browser: { type: "dom", selector: "text=\"Exit parity\"", read: "text" }, expected: "Exit parity" },
        effect("builtin.process", 0, "/exitCode"),
      ],
    }],
  })),
  commandCase("restart", "/rename Restart parity", 'Session renamed to "Restart parity".', [send("/restart", "Restart parity", [effect("builtin.process", 1, "/restartCount"), effect("builtin.session", "Restart parity", "/persistedTitle")])]),
];
