// Owns runtime builtin checkpoints; each claimed mutation pairs real UI with durable/host evidence.
// The corresponding requirements supply actual tools/providers and strict owned host prerequisites.
import type { BehaviorClaim, BuiltinCase, Checkpoint, Comparison } from "./scenario";
import { commandCase, dismissThenSend, effect, escape, observed, send } from "./builtin-steps";
import { runtimeRatchetFiles, runtimeRatchetTunedPrompt, runtimeShakeSource } from "./builtin-runtime-workflow-requirements";

const session = (pointer: string, expected: unknown): Comparison => effect("builtin.session", expected, `/runtime/${pointer}`);
const savedFile = (path: string, expected: string): Comparison => ({ name: `Owned file ${path}`, terminal: { type: "file", path }, browser: { type: "file", path }, expected });
const sameSession = (): Comparison => effect("builtin.session", true, "/sameSessionId");
const restarted = (): Comparison => effect("builtin.process", 1, "/restartCount");

function prove(workflow: BuiltinCase, entries: string[][], behaviors: string[]): BuiltinCase {
  if (entries.length !== workflow.checkpoints.length) throw new Error(`Missing runtime checkpoint entries: ${workflow.id}`);
  workflow.behaviors = behaviors;
  workflow.checkpoints.forEach((checkpoint, index) => {
    const evidence = checkpoint.comparisons.map(comparison => ({ checkpoint: checkpoint.name, comparison: comparison.name }));
    if (evidence.length < 2) throw new Error(`Runtime checkpoint lacks UI plus state: ${workflow.id}/${checkpoint.name}`);
    for (const entry of entries[index]) {
      if (!workflow.covers.includes(entry)) workflow.covers.push(entry);
      for (const comparison of checkpoint.comparisons.slice(1)) {
        const claim: BehaviorClaim = { entry, effect: "host", value: comparison.expected, assertionGroups: [{ comparisons: evidence }] };
        (comparison.proves ??= []).push(claim);
      }
    }
  });
  return workflow;
}

const jobs = prove({ ...commandCase("runtime-jobs-kill-owned", "/queue Start parity owned background jobs", "PARITY_JOBS_STARTED", [
  send("/jobs full", "Running: 3", [session("jobs/runningCount", 3), session("jobs/ownedLiveProcessCount", 3)]),
  dismissThenSend("/jobs kill bg_1", "Cancelled background job bg_1.", [session("jobs/runningCount", 2), session("jobs/bg_1/status", "cancelled"), session("jobs/bg_2/status", "running"), session("jobs/bg_3/status", "running"), session("jobs/ownedLiveProcessCount", 2)]),
  send("/jobs kill all", "Cancelled 2 background jobs.", [session("jobs/runningCount", 0), session("jobs/bg_1/status", "cancelled"), session("jobs/bg_2/status", "cancelled"), session("jobs/bg_3/status", "cancelled"), session("jobs/ownedLiveProcessCount", 0)]),
  send("/jobs kill all", "No running background jobs to cancel.", [session("jobs/runningCount", 0), session("jobs/ownedLiveProcessCount", 0)]),
], [session("jobs/runningCount", 3), session("jobs/ownedLiveProcessCount", 3)]), settings: { "async.enabled": true, "bash.autoBackground.enabled": false, "eval.autoBackground.enabled": false } }, [
  ["command:queue"], ["command:jobs", "subcommand:jobs full"], ["command:jobs", "subcommand:jobs kill"], ["command:jobs", "subcommand:jobs kill"], ["command:jobs", "subcommand:jobs kill"],
], ["owned-real-background-bash", "selective-cancel-preserves-siblings", "kill-all-reaps-owned-processes", "repeat-cancel-does-not-recreate-jobs"]);

const cleanse = prove(commandCase("runtime-cleanse-repair-persist", "/cleanse --all -n 1 -m openai/parity-small", "Clean: all detected diagnostics are resolved.", [
  dismissThenSend("/restart", "PARITY_CLEANSE_ORIGIN_ANSWER", [sameSession(), restarted(), session("cleanse/status", "clean"), session("cleanse/workers", 1), session("cleanse/remaining", 0), session("cleanse/workerWritePersisted", true), savedFile("cleanse-value.ts", "export const parityValue: number = 7;\n")]),
], [sameSession(), session("cleanse/status", "clean"), session("cleanse/workers", 1), session("cleanse/remaining", 0), session("cleanse/dispatchedFiles", ["cleanse-value.ts"]), session("cleanse/workerWritePersisted", true), savedFile("cleanse-value.ts", "export const parityValue: number = 7;\n")]), [
  ["command:cleanse"], ["command:cleanse", "command:restart"],
], ["real-typescript-diagnostic", "real-sonic-tool-fixup", "real-checker-verifies-repair", "independent-cleanse-journals-survive-restart"]);

const cleanseCancel = prove(commandCase("runtime-cleanse-cancel", "/cleanse --all -n 1 -m openai/parity-small", "CleanseA1", [
  { ...observed("Cleanse cancelled.", [sameSession(), session("cleanse/status", "cancelled"), session("cleanse/workerStreamCancelled", true), savedFile("cleanse-value.ts", 'export const parityValue: number = "broken";\n')]), terminalInput: "\u001b", browserActions: escape },
], [sameSession(), session("cleanse/status", "running"), savedFile("cleanse-value.ts", 'export const parityValue: number = "broken";\n')]), [
  ["command:cleanse"], ["command:cleanse"],
], ["cancel-real-cleanse-provider-stream", "cancel-does-not-apply-fixup", "cancel-recorded-in-real-cleanse-journal"]);

function compact(mode: "soft" | "remote" | "snapcompact" | "remote-fallback"): BuiltinCase {
  const commandMode = mode === "remote-fallback" ? "remote" : mode;
  const method = mode === "remote-fallback" ? "soft" : mode;
  const entry = `subcommand:compact ${commandMode}`;
  const summary = mode === "remote" ? "PARITY_REMOTE_COMPACTION_SUMMARY" : mode === "remote-fallback" ? "PARITY_REMOTE_FALLBACK_SUMMARY" : "PARITY_SOFT_COMPACTION_SUMMARY";
  const durable = (): Comparison[] => [sameSession(), session("compact/count", 1), session("compact/method", method), session("compact/rawHistoryRetained", true), session("compact/activeHasOriginalAssistant", false), ...(mode === "snapcompact" ? [session("compact/archiveSourceRetained", true)] : [session("compact/summary", summary)])];
  const command = `/compact ${commandMode}${commandMode === "snapcompact" ? "" : " Preserve parity remote evidence"}`;
  const compactEvidence = (): Comparison[] => [...durable(), ...(mode === "snapcompact" ? [session("compact/archiveHasFrames", true)] : []), ...(mode.startsWith("remote") ? [session("compact/remoteRequestCount", 1), session("compact/remoteFocusIncluded", true)] : [])];
  const workflow = commandCase(`runtime-compact-${mode}`, "/queue Add parity retained tail", "PARITY_COMPACT_TAIL", [
    send(command, "compacted", compactEvidence()),
    send("/restart", "compacted", [...compactEvidence(), restarted()]),
    send("/queue Continue after parity compaction", "PARITY_COMPACT_CONTINUED", [...compactEvidence(), ...(mode === "snapcompact" ? [session("compact/providerImageReplay", true)] : [])]),
  ], [sameSession(), session("compact/count", 0), session("compact/activeHasOriginalAssistant", true)]);
  return prove({ ...workflow, settings: { "compaction.enabled": false, "compaction.keepRecentTokens": 1 } }, [
    ["command:queue"], ["command:compact", entry], ["command:compact", entry, "command:restart"], ["command:compact", entry, "command:queue"],
  ], ["manual-mode-uses-real-source-maintenance", "persisted-raw-history-not-deleted", "active-context-rebuilt-from-compaction", "compaction-reloaded-by-real-restart", ...(mode === "snapcompact" ? ["actual-rendered-bitmap-archive", "actual-image-provider-replay"] : []), ...(mode === "remote-fallback" ? ["actual-http-failure-before-real-soft-fallback"] : [])]);
}

const shake = prove({ ...commandCase("runtime-shake-elide-persist", "/queue Read the owned shake source", "PARITY_SHAKE_READY", [
  send("/shake elide", "Shook 1 tool result", [sameSession(), session("shake/prunedReadResults", 1), session("shake/originalInLiveToolResult", false), session("shake/originalInDiskToolResult", false), session("shake/recoveryArtifactContainsOriginal", true)]),
  send("/shake elide", "Nothing to shake.", [session("shake/prunedReadResults", 1), session("shake/recoveryArtifactContainsOriginal", true)]),
  send("/restart", "PARITY_SHAKE_READY", [sameSession(), restarted(), session("shake/prunedReadResults", 1), session("shake/originalInLiveToolResult", false), session("shake/originalInDiskToolResult", false), session("shake/recoveryArtifactContainsOriginal", true)]),
  send("/queue Continue after parity shake", "PARITY_SHAKE_CONTINUED", [session("shake/prunedReadResults", 1), session("shake/recoveryPointerOnWire", true), session("shake/recoveryArtifactContainsOriginal", true)]),
], [session("shake/prunedReadResults", 0), session("shake/originalInLiveToolResult", true), session("shake/originalInDiskToolResult", true)]), files: { "shake-source.txt": runtimeShakeSource }, settings: { "compaction.enabled": false } }, [
  ["command:queue"], ["command:shake", "subcommand:shake elide"], ["command:shake", "subcommand:shake elide"], ["command:shake", "subcommand:shake elide", "command:restart"], ["command:shake", "subcommand:shake elide", "command:queue"],
], ["elide-actual-read-tool-result", "real-artifact-recovers-entire-original", "rewrite-both-live-and-journal-context", "idempotent-elide", "restart-and-provider-replay-use-actual-elision"]);

function approve(question: string, comparisons: Comparison[]): Checkpoint {
  return { ...observed(question, comparisons), terminalInput: "\r", browserActions: [{ type: "click", selector: "[data-omp-role='omp.ask.option']:has-text('Approve')" }] };
}
const ratchetFinal = (): Comparison[] => [session("ratchet/roundCount", 2), session("ratchet/baselineDecision", "baseline"), session("ratchet/lastDecision", "keep"), session("ratchet/bestVariant", "v1"), session("ratchet/baselineRows", 20), session("ratchet/variantRows", 20), session("ratchet/testTraceCount", 0), session("ratchet/inputsApproval", "current"), session("ratchet/graderApproval", "current"), session("ratchet/planApproval", "current"), session("ratchet/analyzerCompleted", true), savedFile("parity-prompt.txt", runtimeRatchetTunedPrompt)];
const ratchet = prove({ ...commandCase("runtime-ratchet-approve-keep-persist", "/ratchet Correct the owned signal flow once", "PARITY_RATCHET_INPUTS_REVIEW", [
  { ...observed("PARITY_RATCHET_DECLINED", [session("ratchet/enabled", true), session("ratchet/roundCount", 0), session("ratchet/inputsApproval", "missing"), session("ratchet/graderApproval", "missing"), session("ratchet/planApproval", "missing")]), terminalInput: "\u001b", browserActions: escape },
  send("/ratchet Continue parity ratchet approvals", "PARITY_RATCHET_INPUTS_REVIEW", [session("ratchet/roundCount", 0), session("ratchet/inputsApproval", "missing")]),
  approve("PARITY_RATCHET_GRADER_REVIEW", [session("ratchet/inputsApproval", "current"), session("ratchet/graderApproval", "missing"), session("ratchet/roundCount", 0)]),
  approve("PARITY_RATCHET_PLAN_REVIEW", [session("ratchet/inputsApproval", "current"), session("ratchet/graderApproval", "current"), session("ratchet/planApproval", "missing"), session("ratchet/roundCount", 0)]),
  approve("PARITY_RATCHET_KEPT", [session("ratchet/enabled", true), ...ratchetFinal()]),
  send("/restart", "PARITY_RATCHET_KEPT", [sameSession(), restarted(), session("ratchet/enabled", false), ...ratchetFinal()]),
  send("/ratchet Verify the wrong next variant is refused", "PARITY_RATCHET_SEQUENCE_REFUSED", [session("ratchet/enabled", true), ...ratchetFinal()]),
], [session("ratchet/enabled", true), session("ratchet/inputsApproval", "missing"), session("ratchet/roundCount", 0)]), files: runtimeRatchetFiles, settings: { "ratchet.enabled": false, "eval.js": true, "eval.autoBackground.enabled": false, "bash.autoBackground.enabled": false, "task.structured": false, "task.perCallModel": "on", "task.perCallModelAllow": ["openai/parity-small"], "compaction.enabled": false } }, [
  ["command:ratchet"], ["command:ratchet"], ["command:ratchet"], ["command:ratchet"], ["command:ratchet"], ["command:ratchet"], ["command:ratchet", "command:restart"], ["command:ratchet"],
], ["real-eval-prelude-and-tools", "cancelled-approval-is-not-recorded", "content-hashed-input-grader-plan-approvals", "actual-app-entrypoint-provider-baseline-and-variant", "train-only-real-task-analyzer", "actual-sequence-guard", "actual-paired-gate-keeps-improvement", "durable-rounds-with-session-local-arming"]);

function fork(placement: "pane" | "window" | "tab"): BuiltinCase {
  const actualPlacement = placement === "pane" ? "pane" : "window";
  const childComposer = "[data-omp-fork-child] [data-omp-role='omp.composer.line'] textarea, [data-omp-fork-child] [data-omp-role='omp.composer.line'] [contenteditable='true']";
  const parentComposer = "[data-omp-fork-parent] [data-omp-role='omp.composer.line'] textarea, [data-omp-fork-parent] [data-omp-role='omp.composer.line'] [contenteditable='true']";
  const forked = (): Comparison[] => [sameSession(), session("fork/launchedChildCount", 1), session("fork/placement", actualPlacement), session("fork/childHasDifferentSessionId", true), session("fork/childParentIsOriginal", true)];
  const childMutated = (): Comparison[] => [...forked(), session("fork/childWritePersisted", true), session("fork/childUserTexts", ["PARITY_FORK_MUX_ORIGIN_USER", "Parity mutate the owned fork child"]), session("fork/childAssistantTexts", ["PARITY_FORK_MUX_ORIGIN_ANSWER", "PARITY_FORK_MUX_CHILD_ANSWER"]), savedFile("fork-child.txt", "PARITY_FORK_CHILD_FILE\n")];
  const childInput: Checkpoint = { ...observed("PARITY_FORK_MUX_CHILD_ANSWER", childMutated()), terminalInput: "Parity mutate the owned fork child\r", browserActions: [{ type: "fill", selector: childComposer, value: "Parity mutate the owned fork child" }, { type: "press", selector: childComposer, key: "Enter" }] };
  const parentInput = send("/queue Continue parent after fork", "PARITY_PARENT_CHILD_DRIVEN", [...childMutated(), effect("builtin.session", ["PARITY_FORK_MUX_ORIGIN_USER", "Continue parent after fork"], "/userTexts")]);
  parentInput.terminalInput = `${placement === "pane" ? "\u0002o" : "\u0002p"}${parentInput.terminalInput}`;
  parentInput.browserActions = [{ type: "fill", selector: parentComposer, value: "/queue Continue parent after fork" }, { type: "press", selector: parentComposer, key: "Enter" }];
  const restart = send("/restart", "PARITY_PARENT_CHILD_DRIVEN", [...childMutated(), restarted(), effect("builtin.session", ["PARITY_FORK_MUX_ORIGIN_USER", "Continue parent after fork"], "/userTexts")]);
  restart.browserActions = [{ type: "fill", selector: parentComposer, value: "/restart" }, { type: "press", selector: parentComposer, key: "Enter" }];
  return prove(commandCase(`runtime-fork-${placement}`, `/fork ${placement}`, "PARITY_FORK_MUX_ORIGIN_ANSWER", [childInput, parentInput, restart], [...forked(), session("fork/childUserTexts", ["PARITY_FORK_MUX_ORIGIN_USER"])]), [
    ["command:fork", `subcommand:fork ${placement}`], ["command:fork", `subcommand:fork ${placement}`], ["command:fork", `subcommand:fork ${placement}`, "command:queue"], ["command:fork", `subcommand:fork ${placement}`, "command:restart"],
  ], ["actual-owned-tmux-or-visible-native-host-placement", "fork-child-real-editor-provider-and-write", "parent-and-child-have-independent-durable-history", "tab-is-window-not-an-unrelated-new-session", "parent-restart-retains-child-and-original-journals"]);
}

export const runtimeWorkflowCases: BuiltinCase[] = [jobs, cleanse, cleanseCancel, compact("soft"), compact("remote"), compact("snapcompact"), compact("remote-fallback"), shake, ratchet, fork("pane"), fork("window"), fork("tab")];
