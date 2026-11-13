import type { BrowserAction, BuiltinCase, Checkpoint, Comparison } from "./scenario";

export const composer = "[data-omp-dock] [data-omp-role='omp.composer.line'] textarea, [data-omp-dock] [data-omp-role='omp.composer.line'] [contenteditable='true']";
export const escape: BrowserAction[] = [{ type: "press", key: "Escape" }];
// OMP's stdin parser reads a bare ESC followed by more bytes in one PTY write
// as an Alt chord; the CSI-u form is Escape with no timing hold.
export const unambiguousEscape = "\u001b[27u";

export function literalPattern(text: string): string {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

export function textComparison(text: string): Comparison {
  return {
    name: text,
    terminal: { type: "screen", pattern: `(${literalPattern(text)})`, group: 1 },
    browser: { type: "dom", selector: `text=/${literalPattern(text)}/`, read: "text", pattern: `(${literalPattern(text)})`, group: 1 },
    expected: text,
    normalize: "whitespace",
  };
}

export function effect(channel: string, expected: unknown, pointer?: string): Comparison {
  return {
    name: channel + (pointer ?? ""),
    terminal: { type: "effect", channel, pointer },
    browser: { type: "effect", channel, pointer },
    expected,
  };
}

export function observed(text: string, comparisons: Comparison[] = []): Checkpoint {
  return { name: text, comparisons: [textComparison(text), ...comparisons] };
}

export function send(command: string, text: string, comparisons: Comparison[] = []): Checkpoint {
  return {
    ...observed(text, comparisons),
    terminalInput: `${command}\r`,
    browserActions: [
      { type: "fill", selector: composer, value: command },
      { type: "press", selector: composer, key: "Enter" },
    ],
  };
}

export function dismissThenSend(command: string, text: string, comparisons: Comparison[] = []): Checkpoint {
  const step = send(command, text, comparisons);
  return { ...step, terminalInput: `${unambiguousEscape}${step.terminalInput}`, browserActions: [...escape, ...(step.browserActions ?? [])] };
}

export function pathStatus(command: string, prefix: string, comparisons: Comparison[] = []): Checkpoint {
  const step = send(command, prefix, comparisons);
  step.comparisons[0] = {
    name: prefix,
    terminal: { type: "screen", pattern: `(${literalPattern(prefix)}[^\\r\\n]+)`, group: 1 },
    browser: { type: "dom", selector: `text=/${literalPattern(prefix)}/`, read: "text", pattern: `(${literalPattern(prefix)}[^\\r\\n]+)`, group: 1 },
    normalize: "fixture-path",
  };
  return step;
}

export function commandCase(id: string, command: string, text: string, checkpoints: Checkpoint[] = [], comparisons: Comparison[] = []): BuiltinCase {
  const tokens = new Set<string>();
  const record = (submitted: string): void => {
    const token = submitted.match(/^\/([^\s]+)/)?.[1];
    if (token) tokens.add(token.startsWith("force:") ? "force:" : token);
  };
  record(command);
  for (const checkpoint of checkpoints) {
    if (!checkpoint.browserActions?.some(action => action.type === "press" && action.key === "Enter")) continue;
    for (const action of checkpoint.browserActions) if (action.type === "fill") record(action.value);
  }
  if (tokens.size === 0) throw new Error(`Builtin case ${id} needs a slash token`);
  const steps = [observed(text, comparisons), ...checkpoints].map((checkpoint, index) => ({
    ...checkpoint, name: `${index + 1}. ${checkpoint.name}`,
  }));
  return { id: `builtin-${id}`, covers: [...tokens].map(token => `command:${token}`), command, checkpoints: steps };
}

export function completion(command: string, prefix: string, completed: string, text: string): BuiltinCase {
  const token = command.slice(1).split(/\s/, 1)[0];
  return {
    id: `builtin-completion-${token}-${completed.replace(/[^a-z0-9]+/gi, "-")}`,
    covers: [`command:${token}`],
    command,
    checkpoints: [
      {
        name: `Complete ${prefix}`,
        terminalInput: `${prefix}\t`,
        browserActions: [{ type: "fill", selector: composer, value: prefix }, { type: "press", selector: composer, key: "Tab" }],
        comparisons: [{ name: "Accepted completion", terminal: { type: "screen", pattern: `(${literalPattern(completed)})`, group: 1 }, browser: { type: "dom", selector: composer, read: "value" }, expected: completed }],
      },
      { ...observed(text), terminalInput: "\r", browserActions: [{ type: "press", selector: composer, key: "Enter" }] },
    ],
  };
}

