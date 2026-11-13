import type { BuiltinCase, Comparison } from "./scenario";
import type { BuiltinProviderTurn } from "./builtin-requirements";
import { effect } from "./builtin-steps";
import { assistant, chatCase, content, markdownContent, messageRows, settled, user } from "./chat-steps";

const mermaidRow = "│ PARITY_ALPHA ├────►│ PARITY_BETA │";
const mermaidReply = ["PARITY_MERMAID_REPLY", "", "```mermaid", "flowchart LR", "  A[PARITY_ALPHA] --> B[PARITY_BETA]", "```", "", "PARITY_MERMAID_DONE"].join("\n");
const svgLabel = "PARITY_SVG_FIGURE";
const svgReply = ["PARITY_SVG_REPLY", "", "```svg", `<svg aria-label="${svgLabel}" viewBox="0 0 8 4"><rect width="8" height="4" fill="var(--c1)"/></svg>`, "```", "", "PARITY_SVG_DONE"].join("\n");
const reactionReply = "🎉 PARITY_REACTION_BODY";
const outReply = "PARITY_OUT_REPLY progress reached the user mid-turn.";

const mermaidSource = "flowchart LR\n  A[PARITY_ALPHA] --> B[PARITY_BETA]";
const mermaidArt = ["┌──────────────┐     ┌─────────────┐", "│              │     │             │", mermaidRow, "│              │     │             │", "└──────────────┘     └─────────────┘"].join("\n");
const svgSource = `<svg aria-label="${svgLabel}" viewBox="0 0 8 4"><rect width="8" height="4" fill="var(--c1)"/></svg>`;
const svgDocument = `<svg color="#e5e5e7" font-family="sans-serif" xmlns="http://www.w3.org/2000/svg" aria-label="${svgLabel}" viewBox="0 0 8 4"><rect width="8" height="4" fill="#00b4ff"/></svg>`;

const mermaidObservations: Comparison[] = [
  {
    ...content(mermaidRow, `${assistant} pre:has-text("PARITY_ALPHA")`),
    proves: [{ entry: "property:TspMarkdownProps.mermaid", effect: "literal", value: [{ src: mermaidSource, ascii: mermaidArt }] }],
  },
  markdownContent("PARITY_MERMAID_DONE"),
];
const svgObservations: Comparison[] = [
  {
    name: "svg figure keeps its own description",
    terminal: { type: "screen", pattern: `aria-label="(${svgLabel})"`, group: 1 },
    browser: { type: "dom", selector: `${assistant} [data-svg-figure] img`, read: "attribute", attribute: "alt" },
    expected: svgLabel,
    proves: [{ entry: "property:TspMarkdownProps.svg", effect: "literal", value: [{ src: svgSource, doc: svgDocument }] }],
  },
  markdownContent("PARITY_SVG_DONE"),
];

export const builtinCases: BuiltinCase[] = [
  chatCase("figure-mermaid", "Parity draw the mermaid flow.", [
    settled("PARITY_MERMAID_DONE", [...mermaidObservations, messageRows("user", 1), messageRows("assistant", 1)]),
  ], ["kind:md", "property:TspMarkdownProps.mermaid"]),
  chatCase("figure-svg", "Parity draw the svg figure.", [
    settled("PARITY_SVG_DONE", [...svgObservations, messageRows("user", 1), messageRows("assistant", 1)]),
  ], ["kind:md", "property:TspMarkdownProps.svg"]),
  chatCase("reaction-badge", "Parity react to this message.", [
    settled("PARITY_REACTION_BODY", [
      {
        name: "reaction badge sits on the user message",
        terminal: { type: "screen", pattern: "(🎉)", group: 1 },
        browser: { type: "dom", selector: `${user} [data-omp-role="omp.reaction"]`, read: "text" },
        expected: "🎉",
      },
      markdownContent("PARITY_REACTION_BODY"),
      messageRows("user", 1),
    ]),
  ], ["kind:badge"]),
  chatCase("out-reply", "Parity reply through out.", [
    settled("PARITY_OUT_DONE", [content(outReply, `:is(${assistant}, [data-omp-role="omp.tool.write"]):has-text(${JSON.stringify(outReply)})`), markdownContent("PARITY_OUT_DONE"), effect("builtin.session", "write", "/messages/2/toolName")]),
  ], ["tool:write"]),
];

export const providerTurns: Record<string, BuiltinProviderTurn[]> = {
  "chat-figure-mermaid": [{ match: { textIncludes: "Parity draw the mermaid flow." }, text: mermaidReply }],
  "chat-figure-svg": [{ match: { textIncludes: "Parity draw the svg figure." }, text: svgReply }],
  "chat-reaction-badge": [{ match: { textIncludes: "Parity react to this message." }, text: reactionReply }],
  "chat-out-reply": [
    { match: { textIncludes: "Parity reply through out." }, toolCalls: [{ id: "chat-out-reply-write", name: "write", arguments: { path: "out://reply", content: outReply } }] },
    { match: {}, text: "PARITY_OUT_DONE" },
  ],
};
