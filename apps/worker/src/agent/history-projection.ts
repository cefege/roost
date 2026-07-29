// Resume-seed projection: omp session history (`get_messages_page`) → the same
// transcript entries the live event stream produces. Split from
// entry-projection.ts to stay under the 400-line cap; it shares that module's
// ProjectionState and helpers so a seeded transcript and a streamed one are
// indistinguishable to the client.

import { AGENT_ENTRY_CAPS, clampText } from "@roost/shared/wire/agent-entry";
import { isRpcRecord } from "./rpc-frame.ts";
import {
	joinContent,
	jsonOf,
	str,
	type ProjectionOp,
	type ProjectionState,
} from "./entry-projection.ts";

/** Seed projection for `get_messages_page` history on `--resume`.
 *
 *  Deliberately shape-tolerant and lossy: a stored session message is a
 *  provider-shaped record, not one of the event frames above, and the resumed
 *  transcript only needs to read back — it is never re-sent to the model. Text
 *  and thinking blocks become entries, tool calls become a completed tool card,
 *  and everything else (developer messages, redacted thinking, provider
 *  fallbacks) is skipped rather than guessed at.
 *
 *  UNVERIFIED against a live resumed session: the only capture we have returned
 *  `messages: []`. Every field is read defensively for that reason. */
export function projectSessionMessage(
	message: unknown,
	state: ProjectionState,
	now: number = Date.now(),
): ProjectionOp[] {
	if (!isRpcRecord(message)) return [];
	const ts = typeof message.timestamp === "number" ? message.timestamp : now;
	if (message.role === "user") {
		const text = typeof message.content === "string" ? message.content : joinContent(message.content);
		if (!text) return [];
		return [
			{
				op: "append",
				entry: {
					kind: "user",
					seq: state.nextSeq++,
					ts,
					text: clampText(text, AGENT_ENTRY_CAPS.text),
					done: true,
				},
			},
		];
	}
	if (message.role === "toolResult") {
		const seq = state.toolSeqByCallId.get(str(message.toolCallId));
		if (seq === undefined) return [];
		state.toolSeqByCallId.delete(str(message.toolCallId));
		return [
			{
				op: "update",
				seq,
				patch: {
					status: message.isError === true ? "error" : "ok",
					text: joinContent(message.content),
					details_json: jsonOf(message.details, AGENT_ENTRY_CAPS.toolDetails),
				},
			},
		];
	}
	if (message.role !== "assistant" || !Array.isArray(message.content)) return [];
	const ops: ProjectionOp[] = [];
	for (const block of message.content) {
		if (!isRpcRecord(block)) continue;
		if (block.type === "text" && str(block.text)) {
			ops.push({
				op: "append",
				entry: {
					kind: "assistant",
					seq: state.nextSeq++,
					ts,
					text: clampText(str(block.text), AGENT_ENTRY_CAPS.text),
					done: true,
				},
			});
		} else if (block.type === "thinking" && str(block.thinking)) {
			ops.push({
				op: "append",
				entry: {
					kind: "thinking",
					seq: state.nextSeq++,
					ts,
					text: clampText(str(block.thinking), AGENT_ENTRY_CAPS.text),
					done: true,
				},
			});
		} else if (block.type === "toolCall") {
			const callId = str(block.id);
			const seq = state.nextSeq++;
			// A matching toolResult message later in the page completes it; if the
			// page boundary split them, the card stays "running", which reads
			// correctly for a turn that was interrupted.
			if (callId) state.toolSeqByCallId.set(callId, seq);
			ops.push({
				op: "append",
				entry: {
					kind: "tool",
					seq,
					ts,
					tool_call_id: callId,
					name: str(block.name),
					args_json: jsonOf(block.arguments, AGENT_ENTRY_CAPS.toolDetails),
					status: "running",
					text: "",
					details_json: "",
					intent: str(block.intent),
				},
			});
		}
	}
	return ops;
}
