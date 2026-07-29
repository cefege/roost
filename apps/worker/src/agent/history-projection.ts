// Resume-seed projection: omp session history (`get_messages_page`) → the same
// transcript entries the live event stream produces. Split from
// entry-projection.ts to stay under the 400-line cap; it shares that module's
// ProjectionState and helpers so a seeded transcript and a streamed one are
// indistinguishable to the client.

import { AGENT_ENTRY_CAPS, clampText } from "@roost/shared/wire/agent-entry";
import { isRpcRecord } from "./rpc-frame.ts";
import {
	splitContent,
	jsonOf,
	projectAssistantTermination,
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
	if ((message.role === "user" && message.steering !== true) || message.role === "developer") {
		const content =
			typeof message.content === "string"
				? { text: message.content, images: [] }
				: splitContent(message.content);
		const ops: ProjectionOp[] = [];
		if (content.text) {
			ops.push({
				op: "append",
				entry: {
					kind: "user",
					seq: state.nextSeq++,
					ts,
					text: clampText(content.text, AGENT_ENTRY_CAPS.text),
					done: true,
				},
			});
		}
		for (const image of content.images) {
			if (image.data_b64.length > AGENT_ENTRY_CAPS.imageBytes) continue;
			ops.push({
				op: "append",
				entry: {
					kind: "image",
					seq: state.nextSeq++,
					ts,
					media_type: image.media_type,
					data_b64: image.data_b64,
					alt: "user image",
				},
			});
		}
		return ops;
	}
	if (message.role === "bashExecution" || message.role === "pythonExecution") {
		const input = message.role === "bashExecution" ? str(message.command) : str(message.code);
		const output = str(message.output);
		const label = message.role === "bashExecution" ? "Bash execution" : "Python execution";
		const suffix =
			message.cancelled === true
				? "\n(cancelled)"
				: typeof message.exitCode === "number" && message.exitCode !== 0
					? `\n(exit ${message.exitCode})`
					: "";
		return [{
			op: "append",
			entry: {
				kind: "notice",
				seq: state.nextSeq++,
				ts,
				level: "info",
				text: clampText(`${label}: ${input}${output ? `\n${output}` : ""}${suffix}`, AGENT_ENTRY_CAPS.text),
				details_json: "",
			},
		}];
	}
	if (message.role === "custom" || message.role === "hookMessage") {
		if (message.display !== true) return [];
		const content =
			typeof message.content === "string"
				? { text: message.content, images: [] }
				: splitContent(message.content);
		const ops: ProjectionOp[] = [];
		if (content.text) {
			ops.push({
				op: "append",
				entry: {
					kind: "notice",
					seq: state.nextSeq++,
					ts,
					level: "info",
					text: clampText(`${str(message.customType) || "custom"}: ${content.text}`, AGENT_ENTRY_CAPS.text),
					details_json: jsonOf(message.details, AGENT_ENTRY_CAPS.toolDetails),
				},
			});
		}
		for (const image of content.images) {
			if (image.data_b64.length > AGENT_ENTRY_CAPS.imageBytes) continue;
			ops.push({
				op: "append",
				entry: {
					kind: "image",
					seq: state.nextSeq++,
					ts,
					media_type: image.media_type,
					data_b64: image.data_b64,
					alt: `${str(message.customType) || "custom"} image`,
				},
			});
		}
		return ops;
	}
	if (message.role === "branchSummary" || message.role === "compactionSummary") {
		const label = message.role === "branchSummary" ? "Branch summary" : "Compaction summary";
		const ops: ProjectionOp[] = [{
			op: "append",
			entry: {
				kind: "notice",
				seq: state.nextSeq++,
				ts,
				level: str(message.warning) ? "warn" : "info",
				text: clampText(`${label}: ${str(message.summary)}${str(message.warning) ? `\n${str(message.warning)}` : ""}`, AGENT_ENTRY_CAPS.text),
				details_json: "",
			},
		}];
		if (message.role === "compactionSummary") {
			const content = splitContent([
				...(Array.isArray(message.blocks) ? message.blocks : []),
				...(Array.isArray(message.images) ? message.images : []),
			]);
			for (const image of content.images) {
				if (image.data_b64.length > AGENT_ENTRY_CAPS.imageBytes) continue;
				ops.push({
					op: "append",
					entry: {
						kind: "image",
						seq: state.nextSeq++,
						ts,
						media_type: image.media_type,
						data_b64: image.data_b64,
						alt: "compaction image",
					},
				});
			}
		}
		return ops;
	}
	if (message.role === "fileMention" && Array.isArray(message.files)) {
		const paths: string[] = [];
		const images: Array<{ path: string; data: string; mediaType: string }> = [];
		for (const file of message.files) {
			if (!isRpcRecord(file)) continue;
			const path = str(file.path);
			if (path) paths.push(path);
			const image = isRpcRecord(file.image) ? file.image : undefined;
			const data = str(image?.data);
			if (!data || data.length > AGENT_ENTRY_CAPS.imageBytes) continue;
			images.push({ path, data, mediaType: str(image?.mimeType) || "image/png" });
		}
		const ops: ProjectionOp[] = [];
		if (paths.length > 0) {
			ops.push({
				op: "append",
				entry: {
					kind: "notice",
					seq: state.nextSeq++,
					ts,
					level: "info",
					text: clampText(`Files: ${paths.join(", ")}`, AGENT_ENTRY_CAPS.text),
					details_json: "",
				},
			});
		}
		for (const image of images) {
			ops.push({
				op: "append",
				entry: {
					kind: "image",
					seq: state.nextSeq++,
					ts,
					media_type: image.mediaType,
					data_b64: image.data,
					alt: image.path ? `file ${image.path}` : "file image",
				},
			});
		}
		return ops;
	}
	if (message.role === "toolResult") {
		const seq = state.toolSeqByCallId.get(str(message.toolCallId));
		if (seq === undefined) return [];
		state.toolSeqByCallId.delete(str(message.toolCallId));
		const content = splitContent(message.content);
		const ops: ProjectionOp[] = [
			{
				op: "update",
				seq,
				patch: {
					status: message.isError === true ? "error" : "ok",
					text: content.text,
					details_json: jsonOf(message.details, AGENT_ENTRY_CAPS.toolDetails),
				},
			},
		];
		for (const image of content.images) {
			if (image.data_b64.length > AGENT_ENTRY_CAPS.imageBytes) continue;
			ops.push({
				op: "append",
				entry: {
					kind: "image",
					seq: state.nextSeq++,
					ts,
					media_type: image.media_type,
					data_b64: image.data_b64,
					alt: "tool result image",
				},
			});
		}
		return ops;
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
		} else if (block.type === "image" && str(block.data)) {
			const data = str(block.data);
			if (data.length > AGENT_ENTRY_CAPS.imageBytes) continue;
			ops.push({
				op: "append",
				entry: {
					kind: "image",
					seq: state.nextSeq++,
					ts,
					media_type: str(block.mimeType) || str(block.media_type) || "image/png",
					data_b64: data,
					alt: "assistant image",
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
					intent:
						str(block.intent) ||
						(isRpcRecord(block.arguments) ? str(block.arguments.i) : ""),
				},
			});
		}
	}
	ops.push(...projectAssistantTermination(message, state, ts));
	return ops;
}
