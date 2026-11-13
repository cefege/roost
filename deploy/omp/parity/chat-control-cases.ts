import type { BuiltinCase, Checkpoint } from "./scenario";
import type { BuiltinProviderTurn } from "./builtin-requirements";
import { composer, effect } from "./builtin-steps";
import { chatCase, content, literalContent, markdownContent, messageRows, settled, submit } from "./chat-steps";

const queueComponent = "component:packages/tui/src/prompt/queued-messages.ts#QueuedMessagesBand";
const textProperty = "property:TspTextProps.text";

function queued(prompt: string, behavior: "steer" | "followUp"): Checkpoint {
  return {
    name: `${behavior} through the real running composer`,
    terminalInput: `\u001b[200~${prompt}\u001b[201~${behavior === "steer" ? "\r" : "\u0011"}`,
    browserActions: [{ type: "fill", selector: composer, value: prompt }, { type: "press", selector: composer, key: behavior === "steer" ? "Enter" : "Control+q" }],
    comparisons: [
      literalContent(prompt, `[data-omp-role="omp.queue.item"] [data-omp-node$="text"]:text-is(${JSON.stringify(prompt)})`, textProperty),
      effect("builtin.session", [prompt], behavior === "steer" ? "/steeringQueue" : "/followUpQueue"),
      effect("builtin.session", true, "/isStreaming"),
    ],
  };
}

const continueAfterQueueAbort = submit("Parity explicit input after withdrawing the queue.", "PARITY_AFTER_QUEUE_ABORT", [effect("builtin.session", [], "/followUpQueue"), effect("builtin.session", [], "/steeringQueue"), effect("builtin.provider", 2, "/requestCount")]);
continueAfterQueueAbort.terminalInput = `\u0015${continueAfterQueueAbort.terminalInput}`;
const switchModelRole = submit("/switch @smol", "parity-small", [effect("builtin.session", "parity-small", "/model/id")]);
switchModelRole.comparisons[0] = literalContent("parity-small", '[data-omp-role="omp.composer.model"] [data-omp-node$="name"]', "property:TspSpan.t");
const switchHighEffort = submit("/switch openai/parity-coder:high", "high", [effect("builtin.session", "high", "/thinkingLevel")]);
switchHighEffort.comparisons[0] = literalContent("high", '[data-omp-role="omp.composer.effort"] [data-omp-node$="level"]', textProperty);

export const builtinCases: BuiltinCase[] = [
  chatCase("queue-steer-delivery", "Parity start queue ordering A.", [
    { name: "Original foreground request stays open", comparisons: [markdownContent("PARITY_QUEUE_A_STARTED"), effect("builtin.session", true, "/isStreaming"), effect("builtin.provider", 1, "/requestCount")] },
    queued("Parity queued B.", "followUp"),
    queued("Parity steer C.", "steer"),
    {
      ...settled("PARITY_FOLLOWUP_B_COMPLETE", [
        effect("builtin.session", ["Parity start queue ordering A.", "Parity steer C.", "Parity queued B."], "/userTexts"),
        effect("builtin.session", [], "/steeringQueue"), effect("builtin.session", [], "/followUpQueue"), effect("builtin.provider", 3, "/requestCount"),
        messageRows("user", 3),
      ]),
      releaseProviderTurns: [0],
    },
  ], [queueComponent, textProperty]),
  chatCase("queue-abort-withdraw", "Parity start cancellable queue A.", [
    { name: "Actual adapter begins a cancellable foreground stream", comparisons: [markdownContent("PARITY_QUEUE_ABORT_STARTED"), effect("builtin.session", true, "/isStreaming")] },
    queued("Parity queued but never executed B.", "followUp"),
    {
      name: "Escape cancels the actual stream and restores the withdrawn draft",
      terminalInput: "\u001b", browserActions: [{ type: "press", selector: composer, key: "Escape" }],
      comparisons: [
        content("PARITY_QUEUE_ABORT_STARTED"),
        { name: "Queued input is returned to the actual editor", terminal: { type: "screen", pattern: "(Parity queued but never executed B\\.)", group: 1 }, browser: { type: "dom", selector: composer, read: "value" }, expected: "Parity queued but never executed B.",
          proves: [{ entry: "property:TspEditorProps.text", effect: "literal", value: "Parity queued but never executed B." }] },
        effect("builtin.session", [], "/followUpQueue"), effect("builtin.session", false, "/isStreaming"), effect("builtin.provider", true, "/cancelled"), effect("builtin.provider", 1, "/requestCount"),
      ],
    },
    continueAfterQueueAbort,
  ], [queueComponent, textProperty, "property:TspEditorProps.text"]),
  chatCase("provider-error-retry", "Parity normal chat failure then retry.", [
    {
      name: "Real provider failure produces the production actionable error card",
      comparisons: [
        content("parity normal chat failure", '[data-omp-role="omp.error.message"]'),
        content("F5", '[data-omp-role="omp.error.action"][title="Retry the failed turn"]'),
        effect("builtin.session", false, "/isStreaming"), effect("builtin.provider", 1, "/requestCount"),
      ],
    },
    {
      ...settled("PARITY_NORMAL_RETRY_COMPLETE", [markdownContent("PARITY_NORMAL_RETRY_COMPLETE"), effect("builtin.provider", 2, "/requestCount"), effect("builtin.session", 1, "/userMessageCount")]),
      terminalInput: "\u001b[15~",
      browserActions: [{ type: "click", selector: '[data-omp-role="omp.error.action"][title="Retry the failed turn"]' }],
    },
  ], ["kind:kbd"]),
  chatCase("model-role-switch", "Parity reply with the initially pinned model.", [
    settled("PARITY_DEFAULT_MODEL_REPLY", [markdownContent("PARITY_DEFAULT_MODEL_REPLY"), effect("builtin.session", "parity-coder", "/model/id")]),
    switchHighEffort,
    switchModelRole,
    submit("Parity reply after resolving the smol model role.", "PARITY_SMALL_MODEL_REPLY", [effect("builtin.session", "parity-small", "/model/id"), effect("builtin.provider", 2, "/requestCount"), messageRows("user", 2)]),
  ], ["command:switch", "kind:effort", "component:packages/tui/src/prompt/custom-editor.ts#CustomEditor", textProperty, "property:TspSpan.t"]),
  chatCase("model-cycle-track", "Parity reply before cycling the model role.", [
    settled("PARITY_BEFORE_ROLE_CYCLE", [markdownContent("PARITY_BEFORE_ROLE_CYCLE"), effect("builtin.session", "parity-coder", "/model/id")]),
    {
      name: "Ctrl+P cycles the role model and shows the chip track",
      terminalInput: "\u0010",
      browserActions: [{ type: "press", selector: composer, key: "Control+p" }],
      comparisons: [
        literalContent("smol", '[data-omp-role="omp.track.active"]', "property:TspSpan.t"),
        effect("builtin.session", "parity-small", "/model/id"),
      ],
    },
  ], ["component:packages/coding-agent/src/modes/interactive-mode.ts#DescribedComponent", "property:TspSpan.t"]),
];

export const providerTurns: Record<string, BuiltinProviderTurn[]> = {
  "chat-queue-steer-delivery": [
    { match: { textIncludes: "Parity start queue ordering A." }, text: "PARITY_QUEUE_A_STARTED", holdAfterText: true },
    { match: { textIncludes: "Parity steer C." }, text: "PARITY_STEER_C_COMPLETE" },
    { match: { textIncludes: "Parity queued B." }, text: "PARITY_FOLLOWUP_B_COMPLETE" },
  ],
  "chat-queue-abort-withdraw": [
    { match: { textIncludes: "Parity start cancellable queue A." }, text: "PARITY_QUEUE_ABORT_STARTED", holdAfterText: true },
    { match: { textIncludes: "Parity explicit input after withdrawing the queue." }, text: "PARITY_AFTER_QUEUE_ABORT" },
  ],
  "chat-provider-error-retry": [
    { match: { textIncludes: "Parity normal chat failure then retry." }, failure: { status: 400, message: "parity normal chat failure" } },
    { match: { textIncludes: "Parity normal chat failure then retry." }, text: "PARITY_NORMAL_RETRY_COMPLETE" },
  ],
  "chat-model-cycle-track": [
    { match: { textIncludes: "Parity reply before cycling the model role." }, text: "PARITY_BEFORE_ROLE_CYCLE" },
  ],
  "chat-model-role-switch": [
    { match: { textIncludes: "Parity reply with the initially pinned model." }, text: "PARITY_DEFAULT_MODEL_REPLY" },
    { match: { textIncludes: "Parity reply after resolving the smol model role." }, text: "PARITY_SMALL_MODEL_REPLY" },
  ],
};
