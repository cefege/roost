// Adapted from Herdr src/detect/manifest.rs at commit
// eacea2daf0b72973173b728936b27478374f2cd2 (Apache-2.0).

import type { AgentRuntimeState } from "@roost/shared/wire";

export interface ManifestGate {
  contains?: readonly string[];
  regex?: readonly string[];
  line_regex?: readonly string[];
  all?: readonly ManifestGate[];
  any?: readonly ManifestGate[];
  not?: readonly ManifestGate[];
}

export interface ManifestRule extends ManifestGate {
  id: string;
  state: AgentRuntimeState | "unknown";
  priority: number;
  region?: string;
  visible_blocker?: boolean;
  visible_working?: boolean;
  visible_idle?: boolean;
  skip_state_update?: boolean;
}

export interface AgentManifest {
  id: string;
  version: string;
  aliases?: readonly string[];
  rules: readonly ManifestRule[];
}

export interface DetectionInput {
  screen: string;
  oscTitle?: string;
  oscProgress?: string;
}

export interface ManifestDetection {
  state: AgentRuntimeState | "unknown";
  visibleIdle: boolean;
  visibleBlocker: boolean;
  visibleWorking: boolean;
  skipStateUpdate: boolean;
  matchedRuleId: string | null;
}

interface CompiledGate {
  contains: readonly string[];
  regex: readonly RegExp[];
  lineRegex: readonly RegExp[];
  all: readonly CompiledGate[];
  any: readonly CompiledGate[];
  not: readonly CompiledGate[];
}

const compiledGates = new WeakMap<object, CompiledGate>();

/** Translate the Rust-regex syntax present in the pinned Herdr manifests. */
export function compileHerdrRegex(pattern: string): RegExp {
  let source = pattern;
  let flags = "u";
  if (source.startsWith("(?i)")) {
    source = source.slice(4);
    flags += "i";
  }
  source = source.replace(/\\x\{([0-9a-fA-F]+)\}/g, "\\u{$1}");
  return new RegExp(source, flags);
}

function compileGate(gate: ManifestGate): CompiledGate {
  const cached = compiledGates.get(gate);
  if (cached) return cached;
  const compiled: CompiledGate = {
    contains: (gate.contains ?? []).map((needle) => needle.toLowerCase()),
    regex: (gate.regex ?? []).map(compileHerdrRegex),
    lineRegex: (gate.line_regex ?? []).map(compileHerdrRegex),
    all: (gate.all ?? []).map(compileGate),
    any: (gate.any ?? []).map(compileGate),
    not: (gate.not ?? []).map(compileGate),
  };
  compiledGates.set(gate, compiled);
  return compiled;
}

function gateMatches(gate: CompiledGate, text: string, lowerText: string): boolean {
  if (!gate.contains.every((needle) => lowerText.includes(needle))) return false;
  if (!gate.regex.every((regex) => regex.test(text))) return false;
  const lines = text.split("\n");
  if (!gate.lineRegex.every((regex) => lines.some((line) => regex.test(line)))) return false;
  if (!gate.all.every((nested) => gateMatches(nested, text, lowerText))) return false;
  if (gate.any.length > 0 && !gate.any.some((nested) => gateMatches(nested, text, lowerText))) return false;
  if (gate.not.some((nested) => gateMatches(nested, text, lowerText))) return false;
  return true;
}

function lineOffset(lines: readonly string[], index: number): number {
  let offset = 0;
  for (let i = 0; i < Math.min(index, lines.length); i++) offset += lines[i]!.length + 1;
  return offset;
}

function bottomLines(content: string, count: number): string {
  const lines = content.split("\n");
  return lines.slice(Math.max(0, lines.length - count)).join("\n");
}

function bottomNonEmptyLines(content: string, count: number): string {
  const lines = content.split("\n");
  let remaining = count;
  let start = lines.length;
  for (let index = lines.length - 1; index >= 0; index--) {
    if (lines[index]!.trim()) {
      remaining--;
      start = index;
      if (remaining === 0) break;
    }
  }
  return start === lines.length ? "" : lines.slice(start).join("\n");
}

function topNonEmptyLines(content: string, count: number): string {
  const lines = content.split("\n");
  let remaining = count;
  let end = -1;
  for (let index = 0; index < lines.length; index++) {
    if (lines[index]!.trim()) {
      remaining--;
      end = index;
      if (remaining === 0) break;
    }
  }
  return end < 0 ? "" : lines.slice(0, end + 1).join("\n");
}

function codexPromptLine(line: string): boolean {
  return line === "›" || line.startsWith("› ");
}

function codexBlockLine(line: string): boolean {
  return ["•", "■", "✗", "✓"].some((marker) => line.startsWith(marker));
}

function currentCodexPromptIndex(lines: readonly string[]): number {
  const prompt = lines.findLastIndex(codexPromptLine);
  if (prompt < 0 || lines.slice(prompt + 1).some(codexBlockLine)) return -1;
  return prompt;
}

function isHorizontalRule(line: string): boolean {
  const trimmed = line.trim();
  if (!trimmed.startsWith("─")) return false;
  let count = 0;
  while (trimmed[count] === "─") count++;
  return trimmed.slice(count).trimStart() === "" || count >= 3;
}

function promptBoxTop(lines: readonly string[]): number {
  let borders = 0;
  for (let index = lines.length - 1; index >= 0; index--) {
    if (isHorizontalRule(lines[index]!)) {
      borders++;
      if (borders === 2) return index;
    }
  }
  return -1;
}

function selectRegion(input: DetectionInput, spec = "whole_recent"): string {
  if (spec === "osc_title") return input.oscTitle ?? "";
  if (spec === "osc_progress") return input.oscProgress ?? "";
  const content = input.screen;
  if (spec === "whole_recent") return content;
  const lines = content.split("\n");
  if (spec === "after_last_prompt_marker") {
    const index = lines.findLastIndex(codexPromptLine);
    return index < 0 ? content : content.slice(Math.min(content.length, lineOffset(lines, index + 1)));
  }
  if (spec === "before_current_prompt_marker") {
    const index = currentCodexPromptIndex(lines);
    return index < 0 ? content : content.slice(0, lineOffset(lines, index));
  }
  if (spec === "whole_recent_without_current_prompt_marker") {
    return currentCodexPromptIndex(lines) < 0 ? content : "";
  }
  if (spec === "current_prompt_block_marker") {
    const prompt = currentCodexPromptIndex(lines);
    return prompt < 0 ? "" : (lines.slice(0, prompt).findLast(codexBlockLine) ?? "");
  }
  if (spec === "after_current_prompt_block_marker") {
    const prompt = currentCodexPromptIndex(lines);
    if (prompt < 0) return "";
    const block = lines.slice(0, prompt).findLastIndex(codexBlockLine);
    return block < 0 ? "" : lines.slice(block).join("\n");
  }
  const top = promptBoxTop(lines);
  if (spec === "prompt_box_body") {
    if (top < 0) return "";
    const relativeEnd = lines.slice(top + 1).findIndex(isHorizontalRule);
    const end = relativeEnd < 0 ? lines.length : top + 1 + relativeEnd;
    return lines.slice(top + 1, end).join("\n");
  }
  const abovePrompt = top < 0 ? content : lines.slice(0, top).join("\n");
  if (spec === "above_prompt_box") return abovePrompt;
  if (spec === "last_non_empty_above_prompt_box") {
    return abovePrompt.split("\n").findLast((line) => line.trim().length > 0) ?? "";
  }
  if (spec === "after_last_horizontal_rule") {
    const last = lines.findLastIndex(isHorizontalRule);
    return last < 0 ? content : lines.slice(last + 1).join("\n");
  }
  const regionMatch = /^(bottom_lines|bottom_non_empty_lines|top_non_empty_lines)\(([1-9]\d*)\)$/.exec(spec);
  if (!regionMatch) return "";
  const count = Number(regionMatch[2]);
  if (regionMatch[1] === "bottom_lines") return bottomLines(content, count);
  if (regionMatch[1] === "bottom_non_empty_lines") return bottomNonEmptyLines(content, count);
  return topNonEmptyLines(content, count);
}

export function evaluateManifest(manifest: AgentManifest, input: DetectionInput): ManifestDetection {
  let matched: ManifestRule | null = null;
  for (const rule of manifest.rules) {
    const text = selectRegion(input, rule.region);
    if (!gateMatches(compileGate(rule), text, text.toLowerCase())) continue;
    if (!matched || matched.priority < rule.priority) matched = rule;
  }
  if (!matched) {
    return {
      state: "idle", visibleIdle: false, visibleBlocker: false,
      visibleWorking: false, skipStateUpdate: false, matchedRuleId: null,
    };
  }
  return {
    state: matched.state,
    visibleIdle: matched.visible_idle === true && matched.state === "idle",
    visibleBlocker: matched.visible_blocker === true && matched.state === "blocked",
    visibleWorking: matched.visible_working === true && matched.state === "working",
    skipStateUpdate: matched.skip_state_update === true,
    matchedRuleId: matched.id,
  };
}
