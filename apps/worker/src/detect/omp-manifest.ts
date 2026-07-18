// @oh-my-pi (omp) screen-detection rules. omp (`@oh-my-pi/pi-coding-agent`,
// v16.4.x) is a diverged fork of pi; matched by manifest-engine.ts::evaluate,
// registered in screen-detect.ts alongside claude + pi.
//
// MODEL — herdr's identify-then-classify (herdr src/pane/agent_detection.rs +
// src/detect/manifests/pi.toml): herdr identifies the agent at LAUNCH
// (autodetect), then the agent's manifest carries ONLY active-state rules
// (pi.toml is literally one rule: working = body "Working..."); a KNOWN agent
// with no active signal falls back to IDLE (never "hold previous"). Roost has no
// launch-time autodetect — identity comes from the SCRAPE — so we anchor identity
// on omp's STABLE OSC title `π: <summary>` (U+03C0 then ":") and self-gate every
// rule on it:
//   working:  omp title + a body braille-spinner line with an "esc" cancel hint.
//   approval: omp title + a body tool-approval / ↑↓ select prompt.
//   idle:     omp title alone (LOWEST priority) = the known-agent→idle fallback;
//             wins only when no working/approval signal is on screen.
// A plain shell has no "π:" title, so NONE match → it stays unknown (never
// mis-badged as a working agent — the regression detect-omp-manifest guards).
//
// WHY title-anchored (2026-07-11): omp v16.4.x dropped the "<pct>%/<ctx>" footer
// token the old body rules gated on, so working + idle stopped matching →
// screenStatus() went null → the arbiter FROZE the prior status forever (badge
// stuck "Working" while idle AND stuck "Idle" while working — a bidirectional
// freeze). The OSC title survives version churn; the braille-spinner + "esc" line
// is the documented invariant for a live omp turn:
//   "⠇ Working… ⟨esc⟩" (agent turn) OR "⠼ Running… (esc to cancel)" (bash/compact).

import type { Rule } from "./manifest-engine.ts";

// omp identity — the OSC title omp emits: `π: <summary>` (π = U+03C0, then ":").
// Static across states (carries no status) but omp-specific: a plain shell never
// sets a "π:" window title, and pi's title is "π - <dir>" (space-dash, no colon)
// so this never steals a pi pane. The worker feeds detection the raw-stream title
// (UTF-8 intact — session-manager.ts lastOscTitle), so the π survives. `region` is
// pinned to osc_title so this gate works INSIDE a whole_recent (body) rule.
const OMP_TITLE: Rule["all"] = [
	{ region: "osc_title", regex: ["^\\x{03C0}:"] },
];

// omp working spinner line: a braille frame (U+2800–U+28FF) followed on the SAME
// line by an "esc" cancel hint. Covers "⠇ Working… ⟨esc⟩" (agent turn) and
// "⠼ Running… (esc to cancel)" (bash/compact). Same-line braille+esc ignores a
// stray braille glyph elsewhere in the transcript.
const OMP_SPINNER: Rule["all"] = [
	{ line_regex: ["(?i)[\\x{2800}-\\x{28FF}].*esc"] },
];

// omp approval/select tokens (body). A tool approval ("Allow tool: <name>") or any
// ↑↓ select menu ("…navigate … enter select …", tolerant of an optional "to").
const OMP_APPROVAL: Rule["any"] = [
	{ contains: ["allow tool:"] },
	{
		all: [
			{ line_regex: ["(?i)navigate"] },
			{ line_regex: ["(?i)enter\\s+(?:to\\s+)?select"] },
		],
	},
];

export const OMP_RULES: Rule[] = [
	{
		// omp working: omp title + a live braille spinner+esc in the body. Highest
		// priority so a live spinner beats the idle-identity fallback.
		id: "omp_working_spinner",
		state: "working",
		priority: 1100,
		region: "whole_recent",
		visible_working: true,
		all: [...OMP_TITLE, ...OMP_SPINNER],
		// a tool approval also shows a spinner — let the approval rule own it.
		not: [{ contains: ["allow tool:"] }],
	},
	{
		// omp needs-input: omp title + a tool approval or ↑↓ select menu.
		id: "omp_approval_prompt",
		state: "blocked",
		priority: 900,
		region: "whole_recent",
		visible_blocker: true,
		all: [...OMP_TITLE],
		any: OMP_APPROVAL,
	},
	{
		// omp idle (herdr known-agent→idle fallback): the omp OSC title is present
		// and no working/approval signal fired. LOWEST priority so any live signal
		// wins first; when none does, this commits idle instead of freezing the
		// previous status.
		id: "omp_idle_title",
		state: "idle",
		priority: 250,
		region: "osc_title",
		visible_idle: true,
		regex: ["^\\x{03C0}:"],
	},
];
