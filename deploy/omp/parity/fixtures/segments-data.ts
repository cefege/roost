import * as os from "node:os";
import type { SegmentContext, StatusLineSegmentId } from "@omp-source/packages/tui/src/status-line/types";

/** A state past `expected` that drives a segment through a branch the base states never reach. */
export type ExtraVariant =
  | { kind: "visible"; expected: string; spanValue?: string; projectDirectory?: boolean; configure(context: SegmentContext): void }
  | { kind: "live"; pattern: string; configure(context: SegmentContext): void }
  | { kind: "hidden"; configure(context: SegmentContext): void };

export interface SegmentSpec {
  id: StatusLineSegmentId;
  expected: readonly string[];
  spanValues?: readonly string[];
  configure(context: SegmentContext, index: number): void;
  extra?: readonly ExtraVariant[];
}

const hiddenWith = (configure: (context: SegmentContext) => void): ExtraVariant => ({ kind: "hidden", configure });
const noStatus = { staged: 0, unstaged: 0, untracked: 0 };
const modeStates: ReadonlyArray<(context: SegmentContext) => void> = [
  context => { context.planMode = null; context.goalMode = null; context.loopMode = { state: "paused", limit: { kind: "iterations", initial: 10, remaining: 4 } }; },
  context => { context.planMode = { enabled: true, paused: true }; context.goalMode = null; context.loopMode = null; },
  context => { context.planMode = null; context.goalMode = { enabled: true, paused: false }; context.loopMode = null; },
  context => { context.planMode = null; context.goalMode = null; context.loopMode = { state: "running", condition: { command: "bun test", until: true } }; },
];
const hostLabel = os.hostname().split(".")[0];

export const segmentSpecs: readonly SegmentSpec[] = [
  { id: "pi", expected: ["ParityScout", "ParityReviewer"], configure(context, index) { context.focusedAgentId = index ? "ParityReviewer" : "ParityScout"; } },
  { id: "status", expected: ["Indexer ready", "Tests passing"], configure(context, index) { context.hookStatuses = [index ? "Tests passing" : "Indexer ready"]; } },
  { id: "model", expected: ["Sonnet 4.5", "Parity Model"], configure(context, index) { if (index && context.session.state.model) context.session.state.model.name = "Parity Model"; } },
  { id: "mode", expected: ["Plan", "Loop paused 4/10"], configure(context, index) { if (index) modeStates[0](context); },
    extra: [
      { kind: "visible", expected: "Plan (paused)", configure: modeStates[1] },
      { kind: "visible", expected: "Goal", configure: modeStates[2] },
      { kind: "visible", expected: "Loop running until: bun test", configure: modeStates[3] },
    ] },
  { id: "path", expected: ["/workspace/oh-my-pi", "oh-my-pi/parity-worktree"], configure(context, index) { if (index) { context.worktree = { projectName: "oh-my-pi", worktreeName: "parity-worktree" }; context.options.path = { stripWorkPrefix: true }; } },
    extra: [{ kind: "visible", expected: "parity-segment-path", projectDirectory: true, configure(context) { context.activeRepo = null; context.worktree = null; context.options.path = { stripWorkPrefix: true }; } }] },
  { id: "git", expected: ["gallery/reference", "parity-clean"], configure(context, index) { if (index) context.git = { branch: "parity-clean", status: { staged: 0, unstaged: 0, untracked: 0 }, pr: null }; },
    extra: [
      hiddenWith(context => { context.git = { branch: null, status: null, pr: null }; }),
      hiddenWith(context => { context.git = { branch: null, status: { ...noStatus }, pr: null }; }),
    ] },
  { id: "pr", expected: ["#1842", "#42"], configure(context, index) { if (index) context.git.pr = { number: 42, url: "https://example.invalid/pull/42" }; },
    extra: [hiddenWith(context => { context.git.pr = null; })] },
  { id: "subagents", expected: ["3", "7"], configure(context, index) { context.subagentCount = index ? 7 : 3; },
    extra: [hiddenWith(context => { context.subagentCount = 0; })] },
  { id: "token_in", expected: ["12", "24"], configure(context, index) { context.usageStats.input = index ? 24 : 12; },
    extra: [hiddenWith(context => { context.usageStats.input = 0; })] },
  { id: "token_out", expected: ["3", "6"], configure(context, index) { context.usageStats.output = index ? 6 : 3; } },
  { id: "token_total", expected: ["16", "32"], configure(context, index) { if (index) { context.usageStats.input = 24; context.usageStats.output = 6; context.usageStats.cacheWrite = 2; } },
    extra: [hiddenWith(context => { Object.assign(context.usageStats, { input: 0, output: 0, cacheWrite: 0, orchestrationInput: 0, orchestrationOutput: 0 }); })] },
  { id: "token_rate", expected: ["87.3 tok/s", "42.0 tok/s"], configure(context, index) { context.usageStats.tokensPerSecond = index ? 42 : 87.3; },
    extra: [hiddenWith(context => { context.usageStats.tokensPerSecond = 0; })] },
  { id: "cost", expected: ["$0.42", "$0.84"], spanValues: ["$0.42 +", "$0.84 +"], configure(context, index) { context.usageStats.cost = index ? 0.84 : 0.42; } },
  { id: "context_pct", expected: ["22.0%/100", "72.0%/100", "96.0%/100"], configure(context, index) { context.contextWindow = 100; context.contextPercent = [22, 72, 96][index]; context.contextTokens = context.contextPercent; },
    extra: [
      { kind: "visible", expected: "100", configure(context) { context.contextPercent = null; context.contextTokens = 0; context.contextWindow = 100; } },
      { kind: "visible", expected: "22.0%/100", configure(context) { context.contextPercent = 22; context.contextTokens = 22; context.contextWindow = 100; context.compactionSpeculation = "running"; context.speculationBlinkOn = true; } },
    ] },
  { id: "context_total", expected: ["100", "200"], configure(context, index) { context.contextWindow = index ? 200 : 100; } },
  { id: "time_spent", expected: ["6.0s", "12.0s"], configure(context, index) { context.activeMs = index ? 12_000 : 6_000; context.turnElapsedMs = null; },
    extra: [
      { kind: "live", pattern: "(\\d+(?:\\.\\d+)?)s", configure(context) { context.activeMs = 4_000; context.turnElapsedMs = 1_000; } },
      hiddenWith(context => { context.activeMs = 500; context.turnElapsedMs = null; }),
    ] },
  { id: "time", expected: ["14:07:09", "2:07pm"], configure(context, index) { if (index) context.options.time = { format: "12h", showSeconds: false }; } },
  { id: "session", expected: ["gallery-", "parity-s"], configure(context, index) { if (index) context.session.sessionManager.getSessionId = () => "parity-session-id"; } },
  { id: "hostname", expected: ["gallery-host", "parity-host"], configure(context, index) { if (index) context.hostname = "parity-host"; },
    extra: [{ kind: "visible", expected: hostLabel, configure(context) { context.hostname = undefined; } }] },
  { id: "cache_read", expected: ["48", "96"], configure(context, index) { context.usageStats.cacheRead = index ? 96 : 48; } },
  { id: "cache_write", expected: ["1", "2"], configure(context, index) { context.usageStats.cacheWrite = index ? 2 : 1; } },
  { id: "cache_hit", expected: ["78.69%", "60.00%"], configure(context, index) { if (index) { context.usageStats.cacheRead = 18; context.usageStats.cacheWrite = 0; context.usageStats.input = 12; } },
    extra: [hiddenWith(context => { context.usageStats.cacheRead = 0; })] },
  { id: "session_name", expected: ["gallery", "parity renamed"], configure(context, index) { if (index) context.session.sessionManager.getSessionName = () => "parity renamed"; } },
  { id: "usage", expected: ["24%", "65%", "92%"], configure(context, index) { context.usage = { tier: "Pro", fiveHour: { percent: [24, 65, 92][index], resetMinutes: 75 } }; },
    extra: [
      { kind: "visible", expected: "✦ 3 (2 usable) ▲ 1 exp 1d 6h", configure(context) { context.usage = { tier: "Pro", resetCredits: { bankedCount: 3, redeemableCount: 2, expiryHours: 30, expiring: { tier: "soon", count: 1, expiresAtMs: 0 } } }; } },
      hiddenWith(context => { context.usage = null; }),
    ] },
  { id: "collab", expected: ["collab:3", "collab guest:4"], configure(context, index) { if (index) context.collab = { role: "guest", participantCount: 4 }; } },
  { id: "stream", expected: ["LIVE 2", "REC"], configure(context, index) { context.stream = index ? null : { viewers: 2 }; context.recording = index > 0; } },
  { id: "vim", expected: ["NORMAL", "V-LINE 4L 2d"], spanValues: ["NORMAL", "V-LINE 4L"], configure(context, index) { context.vim = index ? { mode: "visual-line", pending: "2d", selectedLines: 4, display: "text" } : { mode: "normal", pending: "", selectedLines: 0, display: "text" }; },
    extra: [
      { kind: "visible", expected: "4L", configure(context) { context.vim = { mode: "visual-line", pending: "", selectedLines: 4, display: "icon" }; } },
      hiddenWith(context => { context.vim = null; }),
    ] },
];
