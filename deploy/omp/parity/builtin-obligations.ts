import { BUILTIN_SLASH_COMMAND_DEFS } from "@omp-source/packages/coding-agent/src/slash-commands/builtin-registry";
import type { BehaviorClaim, BuiltinCase, Checkpoint, Comparison } from "./scenario";
import { composer, literalPattern } from "./builtin-steps";

export interface BuiltinAliasVariant { scenario: BuiltinCase; originalId: string }
const editorComponent = "component:packages/tui/src/prompt/custom-editor.ts#CustomEditor";

function submittedEntries(text: string): string[] {
  const match = /^\/([^\s]+)(?:\s+([\s\S]*))?$/.exec(text.trim());
  if (!match) return [];
  const token = match[1].startsWith("force:") ? "force:" : match[1];
  const definition = BUILTIN_SLASH_COMMAND_DEFS.find(command => command.name === token || command.aliases?.includes(token));
  if (!definition) return [];
  const args = (match[2] ?? "").trim();
  const subcommand = [...(definition.subcommands ?? [])].sort((left, right) => right.name.length - left.name.length)
    .find(candidate => args === candidate.name || args.startsWith(`${candidate.name} `));
  return [`command:${token}`, ...(subcommand ? [`subcommand:${token} ${subcommand.name}`] : [])];
}

function rewrittenSubmission(text: string, canonical: string, alias: string): string {
  return text.replace(new RegExp(`(^|[\\r\\u001b])/${literalPattern(canonical)}(?=\\s|$)`, "g"), (_match, separator: string) => `${separator}/${alias}`);
}

export function deriveBuiltinAliasCases(cases: readonly BuiltinCase[]): BuiltinAliasVariant[] {
  const variants: BuiltinAliasVariant[] = [];
  for (const definition of BUILTIN_SLASH_COMMAND_DEFS) for (const alias of definition.aliases ?? []) {
    for (const original of cases) {
      if (original.id.startsWith("builtin-completion-")) continue;
      const clone = structuredClone(original);
      let changed = false;
      const replace = (text: string): string => {
        const rewritten = rewrittenSubmission(text, definition.name, alias);
        changed ||= rewritten !== text;
        return rewritten;
      };
      clone.command = replace(clone.command);
      for (const checkpoint of clone.checkpoints) {
        if (checkpoint.terminalInput !== undefined) checkpoint.terminalInput = replace(checkpoint.terminalInput);
        for (const action of checkpoint.browserActions ?? []) if (action.type === "fill" && action.selector === composer) action.value = replace(action.value);
      }
      if (!changed) continue;
      clone.id = `${original.id}--alias-${alias.replace(":", "-colon")}`;
      clone.covers = original.covers.filter(entry => !entry.startsWith("command:") && !entry.startsWith("subcommand:"));
      variants.push({ scenario: clone, originalId: original.id });
    }
  }
  return variants;
}

function observationClaim(entry: string, comparison: Comparison): BehaviorClaim | undefined {
  if (comparison.expected === undefined || (typeof comparison.expected === "object" && comparison.expected !== null)) return undefined;
  const host = comparison.terminal.type === "file" || (comparison.terminal.type === "effect" && /(?:file|clipboard|open-url|process|recording|git|http|live)/.test(comparison.terminal.channel));
  return { entry, effect: host ? "host" : comparison.terminal.type === "effect" ? "interaction" : "literal", value: comparison.expected };
}

function enteredText(checkpoint: Checkpoint, previous: string): { entered?: string; pending: string; completed: boolean } {
  let pending = previous;
  let entered: string | undefined;
  let completed = false;
  for (const action of checkpoint.browserActions ?? []) {
    if (action.type === "fill" && action.selector === composer) pending = action.value;
    if (action.type === "press" && action.key === "Escape") pending = "";
    if (action.type === "press" && action.key === "Tab") {
      const acceptance = checkpoint.comparisons.find(comparison => comparison.browser.type === "dom" && comparison.browser.read === "value" && typeof comparison.expected === "string");
      if (acceptance && typeof acceptance.expected === "string") { pending = acceptance.expected; completed = true; }
    }
    if (action.type === "press" && action.key === "Enter" && pending) { entered = pending; pending = ""; }
  }
  return { entered, pending, completed };
}

export function annotateBuiltinExecution(scenario: BuiltinCase): BuiltinCase {
  const completionCase = scenario.id.startsWith("builtin-completion-");
  let active = completionCase ? [] : submittedEntries(scenario.command);
  if (completionCase) scenario.behaviors = [...new Set([...(scenario.behaviors ?? []), "autocomplete-input"])];
  let pending = "";
  const proven = new Set<string>();
  for (const checkpoint of scenario.checkpoints) {
    const submission = enteredText(checkpoint, pending);
    pending = submission.pending;
    if (submission.entered) active = submittedEntries(submission.entered);
    const entries = submission.completed && !submission.entered ? [editorComponent] : active;
    for (const comparison of checkpoint.comparisons) {
      const claims = entries.map(entry => observationClaim(entry, comparison)).filter((claim): claim is BehaviorClaim => claim !== undefined);
      for (const claim of claims) claim.assertionGroups = [{ comparisons: checkpoint.comparisons.map(observation => ({ checkpoint: checkpoint.name, comparison: observation.name })) }];
      if (claims.length > 0) comparison.proves = [...(comparison.proves ?? []), ...claims];
      for (const claim of comparison.proves ?? []) proven.add(claim.entry);
    }
  }
  scenario.covers = [...new Set([...scenario.covers.filter(entry => !entry.startsWith("command:") && !entry.startsWith("subcommand:")), ...proven])];
  return scenario;
}
