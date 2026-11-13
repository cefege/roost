import type { BuiltinCase, Checkpoint, Comparison } from "./scenario";
import { composer, effect, literalPattern } from "./builtin-steps";

export const assistant = '[data-omp-role="omp.assistant"]';
export const user = '[data-omp-role="omp.user"]';
export const chatComponents = [
  "component:packages/tui/src/chat/assistant-message.ts#AssistantMessageComponent",
  "component:packages/tui/src/chat/user-message.ts#UserMessageComponent",
];
export const toolComponent = "component:packages/tui/src/chat/tool-execution.ts#ToolExecutionComponent";
export const markdownText = "property:TspMarkdownProps.text";

export function literalContent(text: string, selector: string, entry: string): Comparison {
  return { ...content(text, selector), proves: [{ entry, effect: "literal", value: text }] };
}

export function markdownContent(text: string, owner = assistant): Comparison {
  return {
    ...literalContent(text, `${owner} [data-omp-node]:not(:has([data-omp-node])):has-text(${JSON.stringify(text)})`, markdownText),
    name: `Source Markdown: ${text}`,
  };
}

export function content(text: string, selector = assistant): Comparison {
  const pattern = `(${literalPattern(text)})`;
  return {
    name: text,
    terminal: { type: "screen", pattern, group: 1 },
    browser: { type: "dom", selector: selector === assistant ? `${assistant}:has-text(${JSON.stringify(text)})` : selector, read: "text", pattern, group: 1 },
    expected: text,
  };
}

export function settled(text: string, comparisons: Comparison[] = []): Checkpoint {
  return { name: text, comparisons: [content(text), effect("builtin.session", false, "/isStreaming"), ...comparisons] };
}

export function submit(prompt: string, text: string, comparisons: Comparison[] = []): Checkpoint {
  return {
    ...settled(text, comparisons),
    terminalInput: `\u001b[200~${prompt}\u001b[201~\r`,
    browserActions: [{ type: "fill", selector: composer, value: prompt }, { type: "press", selector: composer, key: "Enter" }],
  };
}

export function messageRows(role: "user" | "assistant", count: number): Comparison {
  return {
    name: `Exactly ${count} canonical ${role} rows`,
    terminal: { type: "effect", channel: "builtin.session", pointer: role === "user" ? "/userMessageCount" : "/assistantMessageCount" },
    browser: { type: "dom", selector: role === "user" ? user : assistant, read: "count" },
    expected: count,
  };
}

export function chatCase(id: string, prompt: string, checkpoints: Checkpoint[], covers: string[] = []): BuiltinCase {
  if (prompt.startsWith("/")) throw new Error(`Chat case ${id} must start through the actual normal composer`);
  return { id: `chat-${id}`, command: prompt, covers: [...chatComponents, markdownText, ...covers], checkpoints };
}
