// Reads the rendered grid + window title out of a headless wterm core and runs
// the agent manifest over it. Consumed by coord's claude-status-hub, which feeds
// one core per session from the relayed byte stream (globalBytesBus). The core
// is typed structurally (just the read methods) so this file needs no
// @wterm/core dependency — coord's createWtermCore return satisfies it.

import type { AgentStatus } from "@roost/shared";
import { evaluate, type Detection, type Rule } from "./manifest-engine.ts";
import { CLAUDE_RULES } from "./claude-manifest.ts";
import { PI_RULES } from "./pi-manifest.ts";
import { OMP_RULES } from "./omp-manifest.ts";

/** The minimal read surface the scraper needs from a wterm core. */
export interface DetectableCore {
	getCols(): number;
	getRows(): number;
	getCell(row: number, col: number): { char: number };
	getTitle(): string | null;
}

function cellChar(cp: number): string {
	return cp === 0 ? " " : String.fromCodePoint(cp);
}

/** Visible grid → plain text, one row per line, trailing blanks trimmed. */
export function readScreenText(core: DetectableCore): string {
	const cols = core.getCols();
	const rows = core.getRows();
	const lines: string[] = [];
	for (let row = 0; row < rows; row++) {
		let line = "";
		for (let col = 0; col < cols; col++)
			line += cellChar(core.getCell(row, col).char);
		lines.push(line.replace(/\s+$/u, ""));
	}
	return lines.join("\n");
}

/** Run one manifest against the core's current screen + title.
 *  oscTitle = the raw-stream title (braille intact); omit to read the core's
 *  lossy title. */
function detectWithRules(
	rules: Rule[],
	core: DetectableCore,
	oscTitle?: string,
): Detection {
	return evaluate(rules, {
		// oscTitle OVERRIDE: the wterm core's OSC parser is ASCII-only — it strips
		// the braille spinner claude puts in its title, so core.getTitle() can never
		// satisfy osc_title_working. The caller passes the title parsed from the raw
		// byte stream (UTF-8 intact); fall back to the core only when omitted.
		screen: readScreenText(core),
		oscTitle: oscTitle ?? core.getTitle() ?? "",
		oscProgress: "", // OSC 9;4 progress not tracked; osc_progress_idle is a no-op
	});
}

/** Run the claude manifest only. Retained for tests that target claude directly. */
export function detectClaudeScreen(
	core: DetectableCore,
	oscTitle?: string,
): Detection {
	return detectWithRules(CLAUDE_RULES, core, oscTitle);
}

// Manifest registry: every agent kind whose screens we recognize (claude, pi,
// omp). Claude FIRST so it wins an exact-priority tie (preserves pre-pi
// behavior). Additive — a new agent = one more manifest entry, no
// engine/dispatch change.
const AGENT_MANIFESTS: Rule[][] = [CLAUDE_RULES, PI_RULES, OMP_RULES];

// Priority of the rule a Detection matched — looked up by id across the registry
// so the cross-manifest winner is the higher-priority screen signal.
function rulePriority(det: Detection): number {
	if (det.matchedRuleId === null) return -1;
	for (const rules of AGENT_MANIFESTS) {
		const r = rules.find((x) => x.id === det.matchedRuleId);
		if (r) return r.priority;
	}
	return -1;
}

/** Scrape the grid against ALL agent manifests; the non-unknown verdict with the
 *  highest matched rule priority wins (Claude on exact tie via registry order).
 *  All-unknown → unknown (silent, no chip). Consumed by session-manager._runDetect
 *  so a session running claude OR pi (or any registered agent) lights its chip.
 *  A skip_state_update verdict (transcript/menu) is respected only from the
 *  manifest that owns the live screen — it still returns unknown+skip so the
 *  caller's screenStatus() freezes, matching single-manifest behavior. */
export function detectAgentScreen(
	core: DetectableCore,
	oscTitle?: string,
): Detection {
	let best: Detection | null = null;
	let bestSkip: Detection | null = null;
	for (const rules of AGENT_MANIFESTS) {
		const det = detectWithRules(rules, core, oscTitle);
		if (det.skipStateUpdate) {
			if (bestSkip === null) bestSkip = det;
			continue;
		}
		if (det.state === "unknown") continue;
		if (best === null || rulePriority(det) > rulePriority(best)) best = det;
	}
	// A real state verdict wins over a skip-freeze; skip wins over bare unknown.
	return (
		best ??
		bestSkip ?? {
			state: "unknown",
			matchedRuleId: null,
			visibleBlocker: false,
			visibleIdle: false,
			visibleWorking: false,
			skipStateUpdate: false,
		}
	);
}

/** Map the screen verdict onto our wire AgentStatus, or null = "no opinion".
 *  skip_state_update (transcript/model-picker screens) is also null so the
 *  status freezes while the user scrolls history or opens a menu. */
export function screenStatus(
	det: Detection,
): Exclude<AgentStatus, "done"> | null {
	if (det.skipStateUpdate) return null;
	switch (det.state) {
		case "blocked":
			return "needs-input";
		case "working":
			return "running";
		case "idle":
			return "idle";
		default:
			return null; // unknown → defer to hooks
	}
}
