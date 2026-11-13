import type { BuiltinCase } from "./scenario";
import type { BuiltinProviderTurn } from "./builtin-requirements";
import { effect } from "./builtin-steps";
import { chatCase, literalContent, settled, toolComponent } from "./chat-steps";

const judgeHud = '[data-omp-role="omp.hud.judge"]';
const intent = "Parity judge batch";
const evalCode = `const batch = await judgeBatch({ item: "Parity judged state" }, { verdict: { type: "choice", instructions: "Parity verdict question", criteria: { yes: null, no: null } } }, { intent: ${JSON.stringify(intent)}, concurrency: 1 }); const settled = await batch.drain({ timeout: 30000 }); return JSON.stringify(settled.map(([key, item]) => [key, item.error ?? "answered"]));`;

export const builtinCases: BuiltinCase[] = [{
  ...chatCase("judgment-batch-progress", "Parity run the real judge batch through the eval tool.", [
    {
      name: "Real judge_batch run shows the live judgment progress HUD while its provider call is held",
      terminalResize: { columns: 120, rows: 48 },
      browserActions: [{ type: "resize", width: 1200, height: 960 }],
      comparisons: [
        literalContent(intent, `${judgeHud}:has-text(${JSON.stringify(intent)})`, "property:TspSpan.t"),
        effect("builtin.provider", 2, "/requestCount"),
      ],
    },
    {
      ...settled("PARITY_JUDGE_BATCH_DONE", [effect("builtin.provider", 3, "/requestCount")]),
      releaseProviderTurns: [1],
    },
  ], [toolComponent, "component:packages/coding-agent/src/modes/progress-hud.ts#JudgmentBatchProgressHud", "tool:eval"]),
  settings: { "tools.approvalMode": "yolo" },
}];

export const providerTurns: Record<string, BuiltinProviderTurn[]> = {
  "chat-judgment-batch-progress": [
    { match: { textIncludes: "Parity run the real judge batch" }, toolCalls: [{ id: "chat-judge-batch-eval", name: "eval", arguments: { language: "js", code: evalCode } }] },
    { match: { purpose: "background" }, text: "yes", holdAfterText: true },
    { match: {}, text: "PARITY_JUDGE_BATCH_DONE" },
  ],
};
