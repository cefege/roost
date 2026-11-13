import type { BuiltinCase } from "./scenario";
import type { BuiltinProviderTurn } from "./builtin-requirements";
import { effect } from "./builtin-steps";
import { chatCase, content, markdownContent, submit } from "./chat-steps";

const replyText = ["PARITY_REPLY_HEAD", "", ...Array.from({ length: 6 }, (_, index) => `parity reply paragraph ${index + 1}\n`), "PARITY_REPLY_TAIL"].join("\n");

export const builtinCases: BuiltinCase[] = [
  chatCase("reply-write", "Parity show the mid-task reply.", [
    submit("Parity show the mid-task reply.", "PARITY_REPLY_DONE", [content("PARITY_REPLY_HEAD", 'pre [data-line]:has-text("PARITY_REPLY_HEAD")'), markdownContent("PARITY_REPLY_DONE")]),
    {
      name: "Both renderers survive a transcript viewport smaller than its blocks",
      terminalResize: { columns: 60, rows: 7 },
      browserActions: [{ type: "resize", width: 480, height: 420 }],
      comparisons: [{ ...effect("builtin.session", false, "/isStreaming"), name: "builtin.session/isStreaming while narrowed" }],
    },
    {
      name: "Both renderers return to a full viewport",
      terminalResize: { columns: 120, rows: 48 },
      browserActions: [{ type: "resize", width: 1200, height: 960 }],
      comparisons: [{ ...content("PARITY_REPLY_DONE"), name: "Reply survives the restored viewport" }],
    },
  ], [
    "component:packages/tui/src/chat/reply-write.ts#ReplyWriteComponent",
    "component:packages/tui/src/chrome/transcript-container.ts#TranscriptContainer",
  ]),
  chatCase("tool-default-card", "Parity checkpoint the investigation.", [
    submit("Parity checkpoint the investigation.", "PARITY_CHECKPOINT_DONE", [markdownContent("PARITY_CHECKPOINT_DONE")]),
  ], [
    "component:packages/tui/src/render/width-aware-text.ts#WidthAwareText",
  ]),
];

export const providerTurns: Record<string, BuiltinProviderTurn[]> = {
  "chat-reply-write": [
    { match: { textIncludes: "Parity show the mid-task reply." }, toolCalls: [{ id: "chat-reply", name: "write", arguments: { path: "out://", content: replyText } }] },
    { match: {}, text: "PARITY_REPLY_DONE" },
  ],
  "chat-tool-default-card": [
    { match: { textIncludes: "Parity checkpoint the investigation." }, toolCalls: [{ id: "chat-checkpoint", name: "checkpoint", arguments: { goal: "parity default card" } }] },
    { match: {}, text: "PARITY_CHECKPOINT_DONE" },
  ],
};
