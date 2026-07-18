// Declarative screen-scrape detection engine. Ported from herdr's
// src/detect/manifest.rs: a list of prioritized rules matched against
// regions of the rendered terminal screen → one AgentDetection.
// Owns: region slicing, gate matching (contains/regex/line_regex/any/all/not),
// first-highest-priority-wins resolution.
// Consumed by: detect/screen-detect.ts (builds DetectionInput from wtermCore).
// The rule DATA lives per-agent in detect/<agent>-manifest.ts.

type DetectState = "working" | "blocked" | "idle" | "unknown";

// A gate is an AND of its present matchers. `any` = OR of sub-gates,
// `all` = AND of sub-gates, `not` = NONE of sub-gates. Mirrors
// manifest.rs::compiled_gate_matches.
interface Gate {
  contains?: string[];   // every needle present (case-insensitive substring)
  regex?: string[];      // every pattern matches the whole region text
  line_regex?: string[]; // every pattern matches AT LEAST ONE line
  // Override the region THIS gate (and its sub-gates, unless they set their
  // own) matches against; absent = inherit the enclosing rule's region. This is
  // an Roost adaptation, NOT a herdr port: herdr's gates are single-region and it
  // identifies the agent at LAUNCH (autodetect), so a plain shell is never a
  // "known agent" and its stray output can't false-positive. Roost identifies
  // agents from the SCRAPE, so a state rule must self-gate on identity — this
  // lets one rule AND an osc_title identity gate with a whole_recent state gate.
  region?: string;
  any?: Gate[];
  all?: Gate[];
  not?: Gate[];
}

export interface Rule extends Gate {
  id: string;
  state: DetectState;
  priority: number;
  region: string;
  visible_blocker?: boolean;
  visible_idle?: boolean;
  visible_working?: boolean;
  skip_state_update?: boolean;
}

export interface DetectionInput {
  screen: string;       // visible grid as plain text, one row per line
  oscTitle: string;     // core.getTitle() — carries claude's spinner/idle glyph
  oscProgress: string;  // OSC 9;4 progress payload; "" when not tracked
}

export interface Detection {
  state: DetectState;
  matchedRuleId: string | null;
  visibleBlocker: boolean;
  visibleIdle: boolean;
  visibleWorking: boolean;
  skipStateUpdate: boolean;
}

// ─── regex compile (Rust→JS) ──────────────────────────────────────────────
// herdr patterns use Rust syntax: \x{NNNN} unicode escapes + leading inline
// flags (?i)/(?m). JS needs \u{NNNN} + the `u` flag, and inline flags must
// move to the RegExp flags arg.
const _regexCache = new Map<string, RegExp>();
function compileRegex(pattern: string): RegExp {
  const cached = _regexCache.get(pattern);
  if (cached) return cached;
  let flags = "u";
  let body = pattern;
  const inline = body.match(/^\(\?([a-z]+)\)/);
  if (inline) {
    for (const ch of inline[1]) if ("ims".includes(ch) && !flags.includes(ch)) flags += ch;
    body = body.slice(inline[0].length);
  }
  body = body.replace(/\\x\{/g, "\\u{");
  const re = new RegExp(body, flags);
  _regexCache.set(pattern, re);
  return re;
}

function gateMatches(gate: Gate, input: DetectionInput, region: string): boolean {
  const r = gate.region ?? region;              // gate's own region or inherited
  const text = regionText(input, r);
  const lowerText = text.toLowerCase();
  if (gate.contains && !gate.contains.every((n) => lowerText.includes(n.toLowerCase()))) return false;
  if (gate.regex && !gate.regex.every((p) => compileRegex(p).test(text))) return false;
  if (gate.line_regex) {
    const lines = text.split("\n");
    if (!gate.line_regex.every((p) => { const re = compileRegex(p); return lines.some((l) => re.test(l)); })) return false;
  }
  if (gate.all && !gate.all.every((g) => gateMatches(g, input, r))) return false;
  if (gate.any && gate.any.length > 0 && !gate.any.some((g) => gateMatches(g, input, r))) return false;
  if (gate.not && gate.not.some((g) => gateMatches(g, input, r))) return false;
  return true;
}

// ─── region slicing (manifest.rs::region + helpers) ───────────────────────
function isHorizontalRule(line: string): boolean {
  const t = line.trim();
  if (!t) return false;
  let ruleChars = 0;
  while (ruleChars < t.length && t[ruleChars] === "─") ruleChars++;
  if (ruleChars === 0) return false;
  const suffix = t.slice(ruleChars).trimStart();
  return suffix === "" || ruleChars >= 3;
}

// Top border of claude's prompt box = the SECOND horizontal rule from the
// bottom (box has a top + bottom border). manifest.rs::prompt_box_top_border_index.
function promptBoxTopBorderIndex(lines: string[]): number | null {
  let count = 0;
  for (let i = lines.length - 1; i >= 0; i--) {
    if (isHorizontalRule(lines[i])) { count++; if (count === 2) return i; }
  }
  return null;
}

function bottomNonEmptyLines(lines: string[], count: number): string {
  const idxs: number[] = [];
  for (let i = lines.length - 1; i >= 0 && idxs.length < count; i--) {
    if (lines[i].trim() !== "") idxs.push(i);
  }
  if (idxs.length === 0) return "";
  return lines.slice(idxs[idxs.length - 1]).join("\n");
}

function afterLastHorizontalRule(lines: string[]): string {
  let last = -1;
  for (let i = 0; i < lines.length; i++) if (isHorizontalRule(lines[i])) last = i;
  return last < 0 ? lines.join("\n") : lines.slice(last + 1).join("\n");
}

function promptBoxBody(lines: string[]): string {
  const top = promptBoxTopBorderIndex(lines);
  if (top === null) return "";
  let end = lines.length;
  for (let i = top + 1; i < lines.length; i++) if (isHorizontalRule(lines[i])) { end = i; break; }
  return lines.slice(top + 1, end).join("\n");
}

function regionText(input: DetectionInput, spec: string): string {
  const s = spec.trim();
  if (s === "osc_title") return input.oscTitle;
  if (s === "osc_progress") return input.oscProgress;
  if (s === "whole_recent") return input.screen;
  const lines = input.screen.split("\n");
  if (s === "after_last_horizontal_rule") return afterLastHorizontalRule(lines);
  if (s === "prompt_box_body") return promptBoxBody(lines);
  const m = s.match(/^bottom_non_empty_lines\((\d+)\)$/);
  if (m) return bottomNonEmptyLines(lines, Number(m[1]));
  return ""; // unsupported region → never matches (rule is a no-op)
}

// First match at the HIGHEST priority wins; ties keep the earlier rule
// (strictly-greater replace), matching manifest.rs::evaluate_loaded_manifest.
export function evaluate(rules: Rule[], input: DetectionInput): Detection {
  let best: Rule | null = null;
  for (const rule of rules) {
    if (!gateMatches(rule, input, rule.region)) continue;
    if (!best || rule.priority > best.priority) best = rule;
  }
  if (!best) {
    // No-match: stay silent (unknown). Unlike herdr's known-agent→idle
    // fallback, we do NOT emit idle here — this layer rides ALONGSIDE the
    // claude hooks, and a pattern miss must not flip a working agent to idle.
    return { state: "unknown", matchedRuleId: null, visibleBlocker: false, visibleIdle: false, visibleWorking: false, skipStateUpdate: false };
  }
  return {
    state: best.state,
    matchedRuleId: best.id,
    visibleBlocker: !!best.visible_blocker && best.state === "blocked",
    visibleIdle: !!best.visible_idle && best.state === "idle",
    visibleWorking: !!best.visible_working && best.state === "working",
    skipStateUpdate: !!best.skip_state_update,
  };
}
