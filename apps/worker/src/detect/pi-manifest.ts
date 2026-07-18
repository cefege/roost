// pi-harness (pi-coding-agent) screen-detection rules. Same shape as
// claude-manifest.ts, matched by detect/manifest-engine.ts::evaluate against the
// rendered grid. pi's OSC title is STATIC (`π - idea` — identity, not status), so
// unlike claude there is NO osc_title_working/idle rule; status comes from the
// SCREEN body. Every pi TUI frame carries a footer fingerprint (`MCP: N/M servers`
// + `X%/Y (auto) … model • thinking`) — the pi_* rules AND that fingerprint so a
// plain shell that happens to print "Working..." never lights a pi chip.
// This is data, not logic. Verified 2026-07-09 against pi v0.80.3 rendered grids.

import type { Rule } from "./manifest-engine.ts";

// pi's footer fingerprint — present in EVERY pi frame (idle/working/prompt).
// Required as an `all` gate on the body rules so non-pi screens can't match.
const PI_FOOTER: Rule["all"] = [
	{
		any: [
			{ line_regex: ["MCP:\\s*\\d+/\\d+\\s*servers"] }, // MCP: 0/1 servers LSP ...
			{ line_regex: ["\\d+(?:\\.\\d+)?%/\\S+\\s*\\(auto\\)"] }, // 0.0%/1.0M (auto) … model
		],
	},
];

export const PI_RULES: Rule[] = [
	{
		// pi working: braille spinner (U+2800–U+28FF) + literal "Working..." in the
		// body. Highest priority — a live spinner beats an idle/blocked read of a
		// frame that still shows a stale prompt box above it.
		id: "pi_working_spinner",
		state: "working",
		priority: 1100,
		region: "whole_recent",
		visible_working: true,
		line_regex: ["[\\x{2800}-\\x{28FF}]\\s+Working\\.\\.\\."],
		all: PI_FOOTER,
	},
	{
		// pi needs-input: an approval/select dialog. pi surfaces the RPC extension-UI
		// select/confirm as Allow/Block choices with ↑↓ navigation + a submit hint.
		id: "pi_approval_prompt",
		state: "blocked",
		priority: 900,
		region: "whole_recent",
		visible_blocker: true,
		all: [
			...PI_FOOTER,
			{
				any: [
					{ contains: ["allow", "block"] },
					{
						line_regex: ["(?i)\\ballow\\b"],
						any: [{ contains: ["to navigate"] }, { contains: ["↑↓"] }],
					},
					{
						contains: ["to submit"],
						any: [
							{ contains: ["allow"] },
							{ contains: ["deny"] },
							{ contains: ["block"] },
						],
					},
				],
			},
		],
		not: [{ line_regex: ["[\\x{2800}-\\x{28FF}]\\s+Working\\.\\.\\."] }],
	},
	{
		// pi idle: the pi footer is present, no spinner is working, no approval dialog
		// is up → pi is parked at the empty prompt box waiting for input.
		id: "pi_idle_prompt",
		state: "idle",
		priority: 250,
		region: "whole_recent",
		visible_idle: true,
		all: PI_FOOTER,
		not: [
			{ line_regex: ["[\\x{2800}-\\x{28FF}]\\s+Working\\.\\.\\."] },
			{ contains: ["allow", "block"] },
			{ contains: ["to submit"] },
		],
	},
];
