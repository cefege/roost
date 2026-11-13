import type { BuiltinCase, Comparison } from "./scenario";
import type { BuiltinProviderTurn } from "./builtin-requirements";
import { composer, effect } from "./builtin-steps";
import { assistant, chatCase, content, markdownContent, messageRows, settled, submit, user } from "./chat-steps";

const unicodePrompt = "Parity Unicode: こんにちは 👩🏽‍💻 café é — ∑λ\nSecond prompt line remains one user message.";
const unicodeReply = "PARITY_UNICODE_REPLY\n\nこんにちは 👩🏽‍💻 café é — ∑λ\n\n**Parity bold** and _Parity emphasis_.\n\n| Mode | Count |\n| --- | --- |\n| alpha | 7 |\n\n```typescript\nconst parity = 42;\n```\n\n[Parity reference](https://example.invalid/parity)\n\nhttps://example.invalid/parity\n\nPARITY_MARKDOWN_COMPLETE";
const thinking = "PARITY_REASONING: preserve Unicode, then verify the table and code.";

const unicodeObservations: Comparison[] = [
  content("こんにちは 👩🏽‍💻 café é — ∑λ"),
  content("Second prompt line remains one user message.", user),
  content("Parity bold", `${assistant} strong`),
  content("Parity emphasis", `${assistant} em`),
  content("alpha", `${assistant} td:has-text("alpha")`),
  content("7", `${assistant} td:text-is("7")`),
  content("const parity = 42;", `${assistant} pre code`),
  content("Parity reference", `${assistant} a:text-is("Parity reference")`),
  {
    name: "Markdown link keeps its intended destination",
    terminal: { type: "screen", pattern: "(https://example\\.invalid/parity)", group: 1 },
    browser: { type: "dom", selector: `${assistant} a:text-is("Parity reference")`, read: "attribute", attribute: "href" },
    expected: "https://example.invalid/parity",
  },
];

export const builtinCases: BuiltinCase[] = [
  chatCase("unicode-markdown-stream", unicodePrompt, [
    {
      name: "Actual adapter has streamed Markdown and reasoning without settling",
      comparisons: [content("PARITY_MARKDOWN_COMPLETE"), markdownContent(thinking, '[data-omp-role="omp.thinking"]'), ...unicodeObservations,
        effect("builtin.session", true, "/isStreaming"), effect("builtin.session", 1, "/userMessageCount"),
        effect("builtin.provider", 1, "/requestCount"), effect("builtin.provider", 0, "/remoteRequests")],
    },
    { ...settled("PARITY_MARKDOWN_COMPLETE", [...unicodeObservations, messageRows("user", 1), messageRows("assistant", 1), effect("builtin.session", 1, "/thinkingBlocks")]), releaseProviderTurns: [0] },
    {
      name: "Reasoning visibility uses the real global control",
      terminalInput: "\u0014",
      browserActions: [{ type: "press", selector: composer, key: "Control+t" }],
      comparisons: [
        {
          name: "Reasoning is absent while the actual answer remains",
          terminal: { type: "screen", pattern: "^(?![\\s\\S]*PARITY_REASONING)[\\s\\S]*?(PARITY_MARKDOWN_COMPLETE)", group: 1 },
          browser: { type: "dom", selector: assistant, read: "text", pattern: "^(?![\\s\\S]*PARITY_REASONING)[\\s\\S]*?(PARITY_MARKDOWN_COMPLETE)", group: 1 },
          expected: "PARITY_MARKDOWN_COMPLETE",
        },
        effect("builtin.persisted-settings", true, "/hideThinkingBlock"), effect("builtin.session", 1, "/thinkingBlocks"),
      ],
    },
    {
      name: "Reasoning restores without mutating canonical content",
      terminalInput: "\u0014",
      browserActions: [{ type: "press", selector: composer, key: "Control+t" }],
      comparisons: [markdownContent(thinking, '[data-omp-role="omp.thinking"]'), effect("builtin.persisted-settings", false, "/hideThinkingBlock"), messageRows("assistant", 1)],
    },
  ], ["kind:md", "kind:section"]),
  chatCase("live-reload-resize", "Parity keep this real streamed turn alive across native reload.", [
    { name: "Real provider stream remains open", comparisons: [markdownContent("PARITY_RECONNECT_STREAM"), effect("builtin.session", true, "/isStreaming"), effect("builtin.provider", 1, "/requestCount")] },
    {
      name: "Actual native socket reload retains the same harness and partial text",
      browserActions: [{ type: "reload" }],
      comparisons: [markdownContent("PARITY_RECONNECT_STREAM"), effect("builtin.session", true, "/isStreaming"), effect("builtin.session", true, "/sameSessionId"), effect("builtin.provider", 1, "/requestCount")],
    },
    {
      name: "Both production renderers resize during the same live turn",
      terminalResize: { columns: 50, rows: 45 },
      browserActions: [{ type: "resize", width: 390, height: 900 }, { type: "transportDelay", milliseconds: 250, direction: "incoming" }],
      comparisons: [markdownContent("PARITY_RECONNECT_STREAM"), effect("builtin.session", true, "/isStreaming"), effect("builtin.provider", 1, "/requestCount")],
    },
    { ...settled("PARITY_RECONNECT_STREAM", [messageRows("user", 1), messageRows("assistant", 1)]), releaseProviderTurns: [0] },
    {
      name: "Completed native reconnect does not replay raw ACP transcript rows",
      terminalResize: { columns: 120, rows: 48 },
      browserActions: [{ type: "resize", width: 1200, height: 960 }, { type: "reload" }],
      comparisons: [content("PARITY_RECONNECT_STREAM"), messageRows("user", 1), messageRows("assistant", 1), effect("builtin.provider", 1, "/requestCount")],
    },
    submit("Parity continue after reload.", "PARITY_RECONNECT_FOLLOWUP", [
      markdownContent("Parity continue after reload.", user), markdownContent("PARITY_RECONNECT_FOLLOWUP"),
      messageRows("user", 2), messageRows("assistant", 2), effect("builtin.session", true, "/sameSessionId"),
    ]),
  ]),
];

export const providerTurns: Record<string, BuiltinProviderTurn[]> = {
  "chat-unicode-markdown-stream": [{ match: { textIncludes: "Parity Unicode:" }, thinking, textChunks: ["PARITY_UNICODE_REPLY\n\n", unicodeReply.slice("PARITY_UNICODE_REPLY\n\n".length)], chunkDelayMs: 150, holdAfterText: true }],
  "chat-live-reload-resize": [
    { match: { textIncludes: "Parity keep this real streamed turn alive" }, textChunks: ["PARITY_RECONNECT_", "STREAM"], chunkDelayMs: 150, holdAfterText: true },
    { match: { textIncludes: "Parity continue after reload." }, text: "PARITY_RECONNECT_FOLLOWUP" },
  ],
};
