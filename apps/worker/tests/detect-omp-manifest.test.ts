// omp-harness agent-status detection (detect/omp-manifest.ts), scraped alongside
// claude + pi by detect/screen-detect.ts::detectAgentScreen. omp v16.4.x is
// IDENTITY-ANCHORED on its OSC title `π: <summary>` (herdr's identify-then-
// classify, adapted for Roost's scrape-based identity): working = title + a body
// braille-spinner+esc line; idle = title alone (herdr's known-agent→idle
// fallback); approval = title + a body approval/select prompt. A plain shell
// (no "π:" title) matches nothing → unknown, so it is never mis-badged.
//
// Regression (2026-07-11): omp v16.4.x dropped the "<pct>%/<ctx>" footer the old
// body rules gated on; working AND idle then went unmatched → screenStatus()
// null → the arbiter froze the previous status (stuck "Working" while idle AND
// stuck "Idle" while working). Title-anchoring fixes both directions.

import { describe, test, expect } from "bun:test";
import { WasmBridge } from "@wterm/core";
import {
	detectAgentScreen,
	screenStatus,
	detectClaudeScreen,
} from "../src/detect/screen-detect.ts";

async function coreWith(text: string): Promise<WasmBridge> {
	const core = await WasmBridge.load();
	core.init(100, 30);
	core.writeRaw(new TextEncoder().encode(text));
	return core;
}

// The omp OSC title the worker parses from the raw byte stream (UTF-8, π intact)
// and passes to detectAgentScreen. Real shape captured live: "π: <task summary>".
const OMP_TITLE = "π: Fix stuck agent status detector";
const HR = "─".repeat(80);
// omp's real approval screen: Allow tool + Approve/Deny + the BARE-WORD select
// hint (no "to"), a spinner, and NO footer.
const OMP_APPROVAL_BODY =
	` I'll run that command for you.\r\n${HR}\r\n` +
	` ⠸ Running approval probe ⟨esc⟩\r\n${HR}\r\n` +
	` Allow tool: bash\r\n Command: echo hi\r\n  Approve\r\n   Deny\r\n` +
	` up/down navigate  enter select  esc cancel\r\n${HR}\r\n`;

describe("omp manifest (detect-omp-manifest)", () => {
	test("working (agent turn): π: title + ⠇ Working… ⟨esc⟩ → running", async () => {
		const core = await coreWith(` do a thing\r\n\r\n ⠇ Working… ⟨esc⟩\r\n`);
		const det = detectAgentScreen(core, OMP_TITLE);
		expect(det.state).toBe("working");
		expect(screenStatus(det)).toBe("running");
		expect(det.matchedRuleId).toBe("omp_working_spinner");
	});

	test("working (bash !): π: title + ⠼ Running… (esc to cancel) → running", async () => {
		const core = await coreWith(` $ sleep 5\r\n\r\n ⠼ Running… (esc to cancel)\r\n`);
		const det = detectAgentScreen(core, OMP_TITLE);
		expect(det.state).toBe("working");
		expect(det.matchedRuleId).toBe("omp_working_spinner");
	});

	test("idle: π: title, no spinner/approval → idle (known-agent→idle fallback)", async () => {
		const core = await coreWith(` Hello! How can I help?\r\n`);
		const det = detectAgentScreen(core, OMP_TITLE);
		expect(det.state).toBe("idle");
		expect(screenStatus(det)).toBe("idle");
		expect(det.matchedRuleId).toBe("omp_idle_title");
	});

	test("idle needs NO footer (the v16.4.x fix): bare body + π: title → idle", async () => {
		// No "<pct>%/<ctx>" footer token anywhere; idle resolves purely from the
		// omp title. This is the direct fix for the frozen-status regression.
		const core = await coreWith(` some finished output, nothing special\r\n`);
		const det = detectAgentScreen(core, OMP_TITLE);
		expect(det.state).toBe("idle");
		expect(det.matchedRuleId).toBe("omp_idle_title");
	});

	test("approval (tool): π: title + Allow tool + select hint → needs-input", async () => {
		const core = await coreWith(OMP_APPROVAL_BODY);
		const det = detectAgentScreen(core, OMP_TITLE);
		expect(det.state).toBe("blocked");
		expect(screenStatus(det)).toBe("needs-input");
		expect(det.matchedRuleId).toBe("omp_approval_prompt");
	});

	test("approval (menu-only): π: title + ↑↓ select without Allow tool → needs-input", async () => {
		const core = await coreWith(
			` Pick a model\r\n${HR}\r\n  Opus 4.8\r\n   Sonnet\r\n` +
				` up/down navigate  enter select  esc cancel\r\n${HR}\r\n`,
		);
		const det = detectAgentScreen(core, OMP_TITLE);
		expect(det.state).toBe("blocked");
		expect(det.matchedRuleId).toBe("omp_approval_prompt");
	});

	test("working beats idle in the same frame (priority)", async () => {
		const core = await coreWith(` ⠴ Working… ⟨esc⟩\r\n`);
		const det = detectAgentScreen(core, OMP_TITLE);
		expect(det.state).toBe("working");
		expect(det.matchedRuleId).toBe("omp_working_spinner");
	});

	test("identity gate: plain shell braille+esc line but NO π: title → unknown", async () => {
		// Load-bearing guard: a plain command printing a braille spinner + "(esc to
		// cancel)" must NOT be mistaken for a working omp agent. Without the omp OSC
		// title, every omp rule's identity gate fails → unknown.
		const core = await coreWith(`$ ./build.sh\r\n ⠼ Running… (esc to cancel)\r\ndone\r\n$ `);
		const det = detectAgentScreen(core); // no title
		expect(det.state).toBe("unknown");
		expect(screenStatus(det)).toBeNull();
	});

	test("identity gate: a non-π: title (e.g. bash) → unknown even with a spinner", async () => {
		const core = await coreWith(` ⠼ Running… (esc to cancel)\r\n`);
		const det = detectAgentScreen(core, "bash");
		expect(det.state).toBe("unknown");
	});

	test("no cross-manifest regression: claude idle → claude, pi idle → pi", async () => {
		const RULE = "─".repeat(25);
		const claude = await coreWith(
			`⏺ Done.\r\n\r\n${RULE}\r\n ❯ type your message\r\n${RULE}\r\n  ? for shortcuts\r\n`,
		);
		const viaAll = detectAgentScreen(claude);
		const viaClaude = detectClaudeScreen(claude);
		expect(viaClaude.state).toBe("idle");
		expect(viaAll.state).toBe("idle");
		expect(viaAll.matchedRuleId).toBe(viaClaude.matchedRuleId); // claude rule owns it

		const HR100 = "─".repeat(100);
		const PI_FOOTER =
			"~/Code/idea (main)\r\n" +
			"0.0%/1.0M (auto)                       claude-opus-4-8 • thinking off\r\n" +
			"MCP: 0/1 servers LSP Inactive\r\n";
		const pi = await coreWith(
			` Hello!\r\n\r\n${HR100}\r\n\r\n${HR100}\r\n${PI_FOOTER}`,
		);
		const piDet = detectAgentScreen(pi);
		expect(piDet.state).toBe("idle");
		expect(screenStatus(piDet)).toBe("idle");
		expect(piDet.matchedRuleId).toBe("pi_idle_prompt");
	});
});
