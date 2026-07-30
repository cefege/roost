import { describe, expect, it } from "bun:test";
import { type AssistantMessage, applyAssistantDelta } from "../src";

function base(content: AssistantMessage["content"] = []): AssistantMessage {
	return {
		role: "assistant",
		content,
		model: "test/model",
		usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { total: 0 } },
		stopReason: "stop",
		timestamp: 1,
	};
}

describe("applyAssistantDelta", () => {
	it("opens and grows a text block", () => {
		let message = applyAssistantDelta(base(), { d: "text_start", i: 0 });
		expect(message.content).toEqual([{ type: "text", text: "" }]);
		message = applyAssistantDelta(message, { d: "text_delta", i: 0, delta: "He" });
		message = applyAssistantDelta(message, { d: "text_delta", i: 0, delta: "llo" });
		expect(message.content).toEqual([{ type: "text", text: "Hello" }]);
	});

	it("opens and grows a thinking block", () => {
		let message = applyAssistantDelta(base(), { d: "thinking_start", i: 0 });
		expect(message.content).toEqual([{ type: "thinking", thinking: "" }]);
		message = applyAssistantDelta(message, { d: "thinking_delta", i: 0, delta: "hmm" });
		message = applyAssistantDelta(message, { d: "thinking_delta", i: 0, delta: "..." });
		expect(message.content).toEqual([{ type: "thinking", thinking: "hmm..." }]);
	});

	it("opens a tool call with empty arguments and replaces it wholesale at the end", () => {
		let message = applyAssistantDelta(base(), { d: "toolcall_start", i: 0, id: "tc1", name: "bash" });
		expect(message.content).toEqual([{ type: "toolCall", id: "tc1", name: "bash", arguments: {} }]);

		message = applyAssistantDelta(message, {
			d: "toolcall_end",
			i: 0,
			toolCall: { type: "toolCall", id: "tc1", name: "bash", arguments: { command: "ls" }, intent: "Listing" },
		});
		expect(message.content).toEqual([
			{ type: "toolCall", id: "tc1", name: "bash", arguments: { command: "ls" }, intent: "Listing" },
		]);
	});

	it("carries intent on toolcall_start only when the host sent one", () => {
		const withIntent = applyAssistantDelta(base(), {
			d: "toolcall_start",
			i: 0,
			id: "tc1",
			name: "bash",
			intent: "Listing",
		});
		expect(withIntent.content[0]).toEqual({
			type: "toolCall",
			id: "tc1",
			name: "bash",
			arguments: {},
			intent: "Listing",
		});
		const without = applyAssistantDelta(base(), { d: "toolcall_start", i: 0, id: "tc1", name: "bash" });
		expect(Object.hasOwn(without.content[0] ?? {}, "intent")).toBe(false);
	});

	it("is pure: new message, new content array, new block, untouched blocks shared", () => {
		const before = base([{ type: "text", text: "kept" }]);
		const kept = before.content[0];
		const after = applyAssistantDelta(before, { d: "text_delta", i: 1, delta: "new" });

		expect(after).not.toBe(before);
		expect(after.content).not.toBe(before.content);
		expect(before.content).toEqual([{ type: "text", text: "kept" }]);
		// Untouched blocks are shared by reference — the fold must not deep-clone
		// the whole message per token.
		expect(after.content[0]).toBe(kept);
	});

	it("extends past the end, filling the gap with empty text blocks", () => {
		const message = applyAssistantDelta(base(), { d: "text_delta", i: 2, delta: "late" });
		expect(message.content).toEqual([
			{ type: "text", text: "" },
			{ type: "text", text: "" },
			{ type: "text", text: "late" },
		]);
	});

	it("replaces a mismatched block instead of throwing", () => {
		const message = applyAssistantDelta(base([{ type: "thinking", thinking: "wrong kind" }]), {
			d: "text_delta",
			i: 0,
			delta: "text",
		});
		expect(message.content).toEqual([{ type: "text", text: "text" }]);
	});
});
