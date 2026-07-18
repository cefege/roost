// pi-harness agent-status detection: the pi manifest (detect/pi-manifest.ts) is
// scraped alongside claude by detect/screen-detect.ts::detectAgentScreen. These
// drive REAL captured pi screen shapes (v0.80.3) through the multi-manifest
// dispatch and assert the arbitrated status, plus assert no cross-manifest
// regression (claude grids still resolve claude; plain shell stays unknown).
//
// pi identity = the footer fingerprint (`MCP: N/M servers` + `X%/Y (auto) … model`).
// Working = braille spinner + "Working..." in the body (pi's title is static π).

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

// pi footer, present in every pi frame — the identity gate.
const PI_FOOTER =
	"~/Code/idea (fix/enter-leaks-to-sidebar-nav)\r\n" +
	"0.0%/1.0M (auto)                       claude-opus-4-8 • thinking off\r\n" +
	"MCP: 0/1 servers LSP Inactive\r\n";
const HR = "─".repeat(100);

describe("pi manifest (detect-pi-manifest)", () => {
	test("pi working: braille spinner + Working... → running", async () => {
		const core = await coreWith(
			` say hello\r\n\r\n ⠼ Working...\r\n\r\n${HR}\r\n${HR}\r\n${PI_FOOTER}`,
		);
		const det = detectAgentScreen(core);
		expect(det.state).toBe("working");
		expect(screenStatus(det)).toBe("running");
	});

	test("pi idle: empty prompt box + footer, no spinner → idle", async () => {
		const core = await coreWith(
			` Hello!\r\n\r\n${HR}\r\n\r\n${HR}\r\n${PI_FOOTER}`,
		);
		const det = detectAgentScreen(core);
		expect(det.state).toBe("idle");
		expect(screenStatus(det)).toBe("idle");
	});

	test("pi approval dialog: Allow/Block + navigate → needs-input", async () => {
		const core = await coreWith(
			` Run bash command?\r\n\r\n ❯ Allow\r\n   Block\r\n ↑↓ to navigate · enter to submit\r\n${HR}\r\n${PI_FOOTER}`,
		);
		const det = detectAgentScreen(core);
		expect(det.state).toBe("blocked");
		expect(screenStatus(det)).toBe("needs-input");
	});

	test("working beats a stale prompt box in the same frame (priority)", async () => {
		// A frame that still shows the empty box above but the spinner is live.
		const core = await coreWith(
			`${HR}\r\n${HR}\r\n ⠴ Working...\r\n${PI_FOOTER}`,
		);
		expect(detectAgentScreen(core).state).toBe("working");
	});

	test("no cross-manifest regression: plain shell stays unknown (no pi footer)", async () => {
		// "Working..." printed by a plain command WITHOUT the pi footer must NOT match.
		const core = await coreWith(`$ ./build.sh\r\n ⠼ Working...\r\ndone\r\n$ `);
		const det = detectAgentScreen(core);
		expect(det.state).toBe("unknown");
		expect(screenStatus(det)).toBeNull();
	});

	test("no cross-manifest regression: claude idle grid still resolves via claude", async () => {
		const RULE = "─".repeat(25);
		const core = await coreWith(
			`⏺ Done.\r\n\r\n${RULE}\r\n ❯ type your message\r\n${RULE}\r\n  ? for shortcuts\r\n`,
		);
		const viaAll = detectAgentScreen(core);
		const viaClaude = detectClaudeScreen(core);
		expect(viaClaude.state).toBe("idle");
		expect(viaAll.state).toBe("idle");
		expect(viaAll.matchedRuleId).toBe(viaClaude.matchedRuleId); // claude rule owns it
	});
});
