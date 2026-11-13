import type { GalleryFixture } from "@omp-source/packages/coding-agent/src/cli/gallery-fixtures/types";
import type { VibeOp, VibeToolDetails } from "@omp-source/packages/tui/src/tools/vibe";
import type { FindToolDetails } from "@omp-source/packages/tui/src/tools/find";

export const toolKeys = [
  "ask", "ast_grep", "ast_edit", "bash", "debug", "eval", "edit", "apply_patch", "find", "glob", "grep", "lsp",
  "wait", "read", "resolve", "reject", "retain", "recall", "reflect", "task", "think", "todo", "github", "goal", "web_search",
  "vibe_spawn", "vibe_send", "vibe_wait", "vibe_kill", "vibe_list", "write", "yield",
] as const;

export const callTokens: Record<typeof toolKeys[number], string> = {
  ask: "Which database", ast_grep: "useState", ast_edit: "countEditFiles", bash: "git status", debug: "stack trace",
  eval: "load config", edit: "read.ts", apply_patch: "renderer.ts", find: "session", glob: "coding-agent", grep: "useState",
  lsp: "auth.ts", wait: "Wait", read: "glob", resolve: "rename", reject: "patch", retain: "Bun", recall: "runtime",
  reflect: "Reflect", task: "AuthLoader", think: "retry loop", todo: "init", github: "GitHub",
  goal: "auth hardening", web_search: "Bun vs Node", vibe_spawn: "parity-worker", vibe_send: "parity-worker",
  vibe_wait: "wait on", vibe_kill: "parity-worker", vibe_list: "sessions", write: "parse-sel.test.ts", yield: "races",
};

export const successTokens: Record<typeof toolKeys[number], string> = {
  ...callTokens,
  ask: "Postgres", ast_grep: "SearchBox.tsx", ast_edit: "renderer.ts", bash: "gallery-cli.ts",
  eval: "37 dependencies", find: "scratch.ts", grep: "SearchBox.tsx", lsp: "auth.ts", wait: "bun test",
  recall: "Bun", todo: "Scaffold crate", web_search: "Bun", vibe_wait: "parity-worker", vibe_list: "parity-worker",
};

export function dedicatedVibeFixture(op: VibeOp): GalleryFixture {
  const details: VibeToolDetails = {
    op,
    screens: [{ id: "parity-worker", cli: "fast", state: "idle", turns: 1, queued: 0, trace: ["read scratch.ts"], outputTail: ["Parity worker completed"], lastActivityAt: 0 }],
    ...(op === "spawn" ? { spawned: { id: "parity-worker", cli: "fast", jobId: "parity-job" } } : {}),
    ...(op === "send" ? { send: { id: "parity-worker", mode: "turn", jobId: "parity-job" } } : {}),
    ...(op === "wait" ? { wait: { settled: [{ id: "parity-worker", jobId: "parity-job", status: "completed" }], stillRunning: [], timedOut: false } } : {}),
    ...(op === "kill" ? { killed: { id: "parity-worker", cancelledTurn: true } } : {}),
  };
  return {
    args: { cli: "fast", name: "parity-worker", session: "parity-worker", sessions: ["parity-worker"], prompt: "Read scratch.ts", message: "Read scratch.ts" },
    streamingArgs: { cli: "fast", name: "parity-worker", session: "parity-worker", message: "Read scratch" },
    result: { content: [{ type: "text", text: "Parity worker completed" }], details },
    errorResult: { content: [{ type: "text", text: `Parity vibe ${op} failed` }], isError: true },
  };
}

export const imageFixture: GalleryFixture = {
  args: { path: "parity-pixel.png" },
  result: {
    content: [
      { type: "text", text: "parity-pixel.png" },
      { type: "image", mimeType: "image/png", data: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=" },
    ],
    details: { kind: "file", contentType: "image/png" },
  },
  errorResult: { content: [{ type: "text", text: "Parity image decode failed" }], isError: true },
};

export function dedicatedFindFixture(): GalleryFixture {
  const details: FindToolDetails = {
    query: "session admission", keywords: ["session"], threshold: 0.4, coverage: "complete", elapsedMs: 12, cwd: "/fixture",
    hits: [{ rel: "scratch.ts", contentScore: 0.98, ranges: [{ start: 1, end: 1, p: 0.98, snippet: "session admission" }], linesSeen: 1, truncated: false }],
    stats: { listed: 1, requests: 1, failed: 0, unanswered: 0, judged: 1, filesRead: 1, fileBytes: 18, inputTokens: 10, outputTokens: 4, cost: 0, apiMs: 12, windowsJudged: 1, windowsPruned: 0, mapCards: 0, failures: [] },
  };
  return {
    args: { query: "session admission", grep_keywords: ["session"] },
    streamingArgs: { query: "session" },
    result: { content: [{ type: "text", text: "scratch.ts:1 session admission" }], details },
  };
}
