// omp RPC event frames → flat, seq-addressed transcript entries.
//
// This is a REDUCER, not a class: no I/O, no globals, no timers, no clock of
// its own (the caller passes `now`). Given the same (frame, state, now) it
// always produces the same ops and the same state advance. That is what makes
// it the unit-test target for the whole worker slice — tests/entry-projection.test.ts
// replays a recorded frame sequence against a fresh state.
//
// The one mutation it performs is on the caller-owned ProjectionState it is
// handed (seq counter + the open-block / tool / prompt indices). The controller
// owns that object; the projector never reaches outside it.
//
// Entries are RE-EMITTED with the same seq as they grow (streaming text, tool
// completion, prompt answered), so ops are append-or-update and the client
// upserts by seq. Frame shapes verified against omp v17.1.7 —
// see local://omp-rpc-contract.md.

import {
	AGENT_ENTRY_CAPS,
	clampText,
	type AgentEntry,
	type AgentPromptKind,
	type AgentPromptState,
	type AgentToolStatus,
} from "@roost/shared/wire/agent-entry";
import { isRpcRecord, type RpcFrame } from "./rpc-frame.ts";

/** A live omp UI request. `method` decides the REPLY shape — `confirm` wants
 *  `{confirmed:boolean}` while select/input/editor want `{value:string}` — so
 *  the controller has to remember it, not just the seq. */
export interface PromptRef {
	seq: number;
	method: string;
}

export interface ProjectionState {
	/** Next seq to hand out. Monotonic per session, starts at 1. */
	nextSeq: number;
	/** seq of the assistant entry currently being streamed, or null. */
	openTextSeq: number | null;
	openThinkingSeq: number | null;
	toolSeqByCallId: Map<string, number>;
	promptById: Map<string, PromptRef>;
}

export function newProjectionState(): ProjectionState {
	return {
		nextSeq: 1,
		openTextSeq: null,
		openThinkingSeq: null,
		toolSeqByCallId: new Map(),
		promptById: new Map(),
	};
}

/** Sparse patch against an entry already in the ring. `appendText` grows a
 *  streaming block; `text` replaces it outright (a `*_end` frame carries the
 *  authoritative full string, so we prefer it over the accumulated deltas). */
export interface EntryPatch {
	appendText?: string;
	text?: string;
	done?: boolean;
	status?: AgentToolStatus;
	details_json?: string;
	state?: AgentPromptState;
	answer?: string;
}

export type ProjectionOp =
	| { op: "append"; entry: AgentEntry }
	| { op: "update"; seq: number; patch: EntryPatch };

// Only these four are answerable dialogs. Everything else that arrives as an
// `extension_ui_request` is client chrome and MUST NOT be replied to — omp does
// not wait on them (verified: a turn ran to turn_end with an unanswered
// setWidget outstanding), and answering an id nothing is waiting on is a no-op
// at best. Matching on `type === "extension_ui_request"` alone is the trap.
const ANSWERABLE_METHODS: Record<string, true> = {
	select: true,
	confirm: true,
	editor: true,
	input: true,
};

// omp's convention for "let me type something instead" inside a select.
const FREE_TEXT_OPTION = "Other (type your own)";
// Every tool approval title starts with this; the tool name follows. The
// approval arrives as a plain formatted string over RPC (the typed
// ClientBridge shape is SDK-only), so the prefix is the classifier.
const APPROVAL_TITLE_PREFIX = "Allow tool: ";

/** Fold a patch into an entry in place, enforcing the wire caps at the single
 *  point where entry text is written. Shared by the controller and the tests so
 *  neither can drift from the other. */
export function applyEntryPatch(entry: AgentEntry, patch: EntryPatch): void {
	if (entry.kind === "tool") {
		if (patch.text !== undefined) entry.text = clampText(patch.text, AGENT_ENTRY_CAPS.toolText);
		if (patch.appendText !== undefined)
			entry.text = clampText(entry.text + patch.appendText, AGENT_ENTRY_CAPS.toolText);
		if (patch.status !== undefined) entry.status = patch.status;
		// details_json must stay parseable, so an over-cap payload is swapped for
		// a valid sentinel instead of clamped mid-literal (same rule as jsonOf).
		if (patch.details_json !== undefined)
			entry.details_json =
				patch.details_json.length <= AGENT_ENTRY_CAPS.toolDetails
					? patch.details_json
					: JSON.stringify({ _truncated: true, bytes: patch.details_json.length });
		return;
	}
	if (entry.kind === "prompt") {
		if (patch.state !== undefined) entry.state = patch.state;
		if (patch.answer !== undefined) entry.answer = clampText(patch.answer, AGENT_ENTRY_CAPS.text);
		return;
	}
	if (entry.kind === "notice") {
		if (patch.text !== undefined) entry.text = clampText(patch.text, AGENT_ENTRY_CAPS.text);
		return;
	}
	if (patch.text !== undefined) entry.text = clampText(patch.text, AGENT_ENTRY_CAPS.text);
	if (patch.appendText !== undefined)
		entry.text = clampText(entry.text + patch.appendText, AGENT_ENTRY_CAPS.text);
	if (patch.done !== undefined) entry.done = patch.done;
}

export function str(v: unknown): string {
	return typeof v === "string" ? v : "";
}

/** Join the text blocks of an omp content array. Image blocks are dropped in
 *  v1 and replaced by a marker — a base64 screenshot would blow the frame cap
 *  on its own, and the transcript has no image surface yet. */
export function joinContent(content: unknown): string {
	if (!Array.isArray(content)) return "";
	let out = "";
	for (const block of content) {
		if (!isRpcRecord(block)) continue;
		if (block.type === "text") out += str(block.text);
		else if (block.type === "image") out += "[image]";
	}
	return out;
}

/** JSON for a value that came off the wire as JSON — it cannot be circular, but
 *  it CAN be undefined (absent field), which JSON.stringify returns undefined for.
 *  args_json / details_json are contract-typed as PARSEABLE json (EntryTool
 *  JSON.parses details_json for the edit diff), so an oversized payload is
 *  replaced by a valid sentinel object rather than clamped into a truncated
 *  object literal that every consumer would then fail to parse. */
export function jsonOf(v: unknown, cap: number): string {
	if (v === undefined) return "";
	const s = JSON.stringify(v);
	if (s === undefined) return "";
	return s.length <= cap ? s : JSON.stringify({ _truncated: true, bytes: s.length });
}

export function projectRpcFrame(
	frame: RpcFrame,
	state: ProjectionState,
	now: number = Date.now(),
): ProjectionOp[] {
	switch (frame.type) {
		case "message_update":
			return projectMessageUpdate(frame, state, now);
		case "tool_execution_start": {
			const callId = str(frame.toolCallId);
			if (!callId) return [];
			const seq = state.nextSeq++;
			state.toolSeqByCallId.set(callId, seq);
			return [
				{
					op: "append",
					entry: {
						kind: "tool",
						seq,
						ts: now,
						tool_call_id: callId,
						name: str(frame.toolName),
						// Capped with the details budget: a write/edit tool carries the
						// whole file body in args, which would otherwise dwarf the frame.
						args_json: jsonOf(frame.args, AGENT_ENTRY_CAPS.toolDetails),
						status: "running",
						text: "",
						details_json: "",
						intent: str(frame.intent),
					},
				},
			];
		}
		case "tool_execution_update": {
			const seq = state.toolSeqByCallId.get(str(frame.toolCallId));
			if (seq === undefined) return [];
			const partial = frame.partialResult;
			if (!isRpcRecord(partial)) return [];
			return [{ op: "update", seq, patch: { text: joinContent(partial.content) } }];
		}
		case "tool_execution_end": {
			const callId = str(frame.toolCallId);
			const seq = state.toolSeqByCallId.get(callId);
			if (seq === undefined) return [];
			state.toolSeqByCallId.delete(callId);
			const result = isRpcRecord(frame.result) ? frame.result : undefined;
			return [
				{
					op: "update",
					seq,
					patch: {
						status: frame.isError === true ? "error" : "ok",
						text: joinContent(result?.content),
						details_json: jsonOf(result?.details, AGENT_ENTRY_CAPS.toolDetails),
					},
				},
			];
		}
		case "extension_ui_request":
			return projectUiRequest(frame, state, now);
		case "command_output": {
			const text = str(frame.text);
			return text ? [notice(state, now, "info", text)] : [];
		}
		case "extension_error":
			return [
				notice(
					state,
					now,
					"error",
					`${str(frame.extensionPath) || "extension"}: ${str(frame.error) || "failed"}`,
				),
			];
		// agent_start / agent_end / turn_* / message_start / message_end /
		// prompt_result / available_commands_update / session_info_update /
		// config_update / auto_* / ttsr_triggered / todo_* / irc_message /
		// notice / subagent_* carry no transcript content — status, usage and
		// cost are read from get_state + message_end by the controller instead.
		// Unknown types land here too: omp adds frames across versions, and an
		// unknown frame is never an error.
		default:
			return [];
	}
}

function projectMessageUpdate(frame: RpcFrame, state: ProjectionState, now: number): ProjectionOp[] {
	const ev = frame.assistantMessageEvent;
	if (!isRpcRecord(ev)) return [];
	const kind = ev.type === "thinking_start" || ev.type === "thinking_delta" || ev.type === "thinking_end"
		? "thinking"
		: "assistant";
	const openSeq = kind === "thinking" ? state.openThinkingSeq : state.openTextSeq;
	switch (ev.type) {
		case "text_start":
		case "thinking_start": {
			const seq = state.nextSeq++;
			if (kind === "thinking") state.openThinkingSeq = seq;
			else state.openTextSeq = seq;
			return [{ op: "append", entry: { kind, seq, ts: now, text: "", done: false } }];
		}
		case "text_delta":
		case "thinking_delta": {
			// A delta with no open block means we joined mid-stream (resume, or a
			// dropped chunk sequence). Nothing to grow; the *_end carries the full
			// text and will land as an update only if a start was seen.
			if (openSeq === null) return [];
			const delta = str(ev.delta);
			return delta ? [{ op: "update", seq: openSeq, patch: { appendText: delta } }] : [];
		}
		case "text_end":
		case "thinking_end": {
			if (openSeq === null) return [];
			if (kind === "thinking") state.openThinkingSeq = null;
			else state.openTextSeq = null;
			// `content` is the authoritative full string; prefer it over the sum of
			// the deltas so a dropped delta self-heals at the end of the block.
			const patch: EntryPatch = { done: true };
			if (typeof ev.content === "string") patch.text = ev.content;
			return [{ op: "update", seq: openSeq, patch }];
		}
		// toolcall_* / image_end / start / done / error deltas are ignored here:
		// tool state comes from the tool_execution_* frames, which are
		// authoritative and carry parsed args.
		default:
			return [];
	}
}

function projectUiRequest(frame: RpcFrame, state: ProjectionState, now: number): ProjectionOp[] {
	const id = str(frame.id);
	const method = str(frame.method);
	if (!id || !method) return [];

	// omp withdrew a dialog it had raised (timeout upstream, tool aborted). The
	// card must stop asking for an answer or the session reads as needs-input
	// forever.
	if (method === "cancel") {
		const target = state.promptById.get(str(frame.targetId));
		if (!target) return [];
		state.promptById.delete(str(frame.targetId));
		return [{ op: "update", seq: target.seq, patch: { state: "cancelled" } }];
	}

	if (!ANSWERABLE_METHODS[method]) return chromeNotice(frame, method, state, now);

	const title = str(frame.title);
	const rawOptions = frame.options;
	const options =
		method === "confirm"
			? ["Yes", "No"]
			: Array.isArray(rawOptions)
				? rawOptions.filter((o): o is string => typeof o === "string")
				: [];
	const isApproval =
		method === "select" &&
		options.length === 2 &&
		options[0] === "Approve" &&
		options[1] === "Deny" &&
		title.startsWith(APPROVAL_TITLE_PREFIX);
	const promptKind: AgentPromptKind = isApproval
		? "approval"
		: method === "input" || method === "editor"
			? "input"
			: "question";
	const seq = state.nextSeq++;
	state.promptById.set(id, { seq, method });
	return [
		{
			op: "append",
			entry: {
				kind: "prompt",
				seq,
				ts: now,
				prompt_id: id,
				prompt_kind: promptKind,
				// confirm splits its text across title + message; the card shows one
				// string, so fold them here rather than in every renderer.
				title: clampText(
					method === "confirm" && str(frame.message) ? `${title}\n${str(frame.message)}` : title,
					AGENT_ENTRY_CAPS.text,
				),
				options,
				allow_free_text: promptKind === "input" || options.includes(FREE_TEXT_OPTION),
				state: "pending",
				answer: "",
			},
		},
	];
}

/** Non-answerable `extension_ui_request` methods.
 *
 *  DEVIATION from the plan's mapping table, taken deliberately: the table maps
 *  all five chrome methods to a notice, but setStatus/setWidget/setTitle/
 *  set_editor_text are client-chrome repaints that fire many times per turn and
 *  carry no message a reader wants — turning each into an entry would flood the
 *  2000-entry ring and push real content out of the transcript. Only the two
 *  that carry agent-authored, user-facing text become notices. */
function chromeNotice(
	frame: RpcFrame,
	method: string,
	state: ProjectionState,
	now: number,
): ProjectionOp[] {
	if (method === "notify") {
		const text = str(frame.message);
		if (!text) return [];
		return [notice(state, now, frame.notifyType === "error" ? "error" : "info", text)];
	}
	if (method === "open_url") {
		const url = str(frame.url);
		if (!url) return [];
		const instructions = str(frame.instructions);
		return [notice(state, now, "info", instructions ? `${url}\n${instructions}` : url)];
	}
	return [];
}

function notice(
	state: ProjectionState,
	now: number,
	level: "info" | "error",
	text: string,
): ProjectionOp {
	return {
		op: "append",
		entry: {
			kind: "notice",
			seq: state.nextSeq++,
			ts: now,
			level,
			text: clampText(text, AGENT_ENTRY_CAPS.text),
		},
	};
}
