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
	type AgentSubagentEntry,
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
	backgroundToolCallIds: Set<string>;
	promptById: Map<string, PromptRef>;
	subagentSeqById: Map<string, number>;
	renderedCustomMessageSignatures: Set<string>;
}

export function newProjectionState(): ProjectionState {
	return {
		nextSeq: 1,
		openTextSeq: null,
		openThinkingSeq: null,
		toolSeqByCallId: new Map(),
		backgroundToolCallIds: new Set(),
		promptById: new Map(),
		subagentSeqById: new Map(),
		renderedCustomMessageSignatures: new Set(),
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
	subagent_state?: AgentSubagentEntry["state"];
	phases_json?: string;
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

const ERROR_FLAG_SILENT_ABORT = 0x0200_0000;
const ERROR_FLAG_USER_INTERRUPT = 0x0400_0000;
const ERROR_FLAG_ABORT = 0x0800_0000;

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
	if (entry.kind === "subagent") {
		if (patch.text !== undefined) entry.text = clampText(patch.text, AGENT_ENTRY_CAPS.text);
		if (patch.appendText !== undefined)
			entry.text = clampText(entry.text + patch.appendText, AGENT_ENTRY_CAPS.text);
		if (patch.subagent_state !== undefined) entry.state = patch.subagent_state;
		return;
	}
	if (entry.kind === "todo") {
		if (patch.phases_json !== undefined) entry.phases_json = patch.phases_json;
		return;
	}
	if (entry.kind === "image") return;
	if (patch.text !== undefined) entry.text = clampText(patch.text, AGENT_ENTRY_CAPS.text);
	if (patch.appendText !== undefined)
		entry.text = clampText(entry.text + patch.appendText, AGENT_ENTRY_CAPS.text);
	if (patch.done !== undefined) entry.done = patch.done;
}

export function str(v: unknown): string {
	return typeof v === "string" ? v : "";
}

export interface SplitContent {
	text: string;
	images: { media_type: string; data_b64: string }[];
}

/** Split user-visible text and image blocks without replacing images with a
 * lossy marker. Callers decide whether the images belong in the transcript. */
export function splitContent(content: unknown): SplitContent {
	if (!Array.isArray(content)) return { text: "", images: [] };
	let text = "";
	const images: SplitContent["images"] = [];
	for (const block of content) {
		if (!isRpcRecord(block)) continue;
		if (block.type === "text") {
			text += str(block.text);
			continue;
		}
		if (block.type === "image" && str(block.data)) {
			images.push({
				media_type: str(block.mimeType) || str(block.media_type) || "image/png",
				data_b64: str(block.data),
			});
		}
	}
	return { text, images };
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

function hasErrorFlag(message: Record<string, unknown>, flag: number): boolean {
	return typeof message.errorId === "number" && (message.errorId & flag) !== 0;
}

function isSilentAbort(message: Record<string, unknown>): boolean {
	return (
		hasErrorFlag(message, ERROR_FLAG_SILENT_ABORT) ||
		str(message.errorMessage) === "__omp.silent_abort__"
	);
}

function isUserInterrupt(message: Record<string, unknown>): boolean {
	return (
		hasErrorFlag(message, ERROR_FLAG_USER_INTERRUPT) ||
		str(message.errorMessage) === "Interrupted by user"
	);
}

function finalizeOpenAssistantEntries(state: ProjectionState): ProjectionOp[] {
	const seqs: number[] = [];
	if (state.openThinkingSeq !== null) seqs.push(state.openThinkingSeq);
	if (state.openTextSeq !== null && state.openTextSeq !== state.openThinkingSeq) {
		seqs.push(state.openTextSeq);
	}
	state.openThinkingSeq = null;
	state.openTextSeq = null;
	seqs.sort((a, b) => a - b);
	return seqs.map((seq) => ({ op: "update", seq, patch: { done: true } }));
}

/** Close any unterminated streamed blocks and project the terminal assistant
 * status using omp's stop-reason and structured error-flag semantics. */
export function projectAssistantTermination(
	message: Record<string, unknown>,
	state: ProjectionState,
	now: number,
	fallbackStopReason = "",
	fallbackError = "",
): ProjectionOp[] {
	const ops = finalizeOpenAssistantEntries(state);
	const stopReason = str(message.stopReason) || fallbackStopReason;
	if (stopReason === "error") {
		if (isSilentAbort(message)) return ops;
		const error = str(message.errorMessage) || fallbackError;
		if (error) ops.push(notice(state, now, "error", error));
		return ops;
	}
	if (stopReason !== "aborted" || isSilentAbort(message) || isUserInterrupt(message)) {
		return ops;
	}
	const error = str(message.errorMessage);
	const generic =
		hasErrorFlag(message, ERROR_FLAG_ABORT) ||
		!error ||
		error === "Request was aborted";
	ops.push(notice(state, now, "info", generic ? "Operation aborted" : error));
	return ops;
}

function customMessageSignature(message: Record<string, unknown>): string {
	return `${String(message.role)}:${String(message.customType)}:${String(message.timestamp)}`;
}

function asyncToolState(details: unknown): string {
	if (!isRpcRecord(details) || !isRpcRecord(details.async)) return "";
	return str(details.async.state);
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
			const callId = str(frame.toolCallId);
			const seq = state.toolSeqByCallId.get(callId);
			if (seq === undefined) return [];
			const partial = frame.partialResult;
			if (!isRpcRecord(partial)) return [];
			const patch: EntryPatch = {
				text: splitContent(partial.content).text,
				details_json: jsonOf(partial.details, AGENT_ENTRY_CAPS.toolDetails),
			};
			if (state.backgroundToolCallIds.has(callId)) {
				const asyncState = asyncToolState(partial.details);
				if (asyncState === "running") patch.status = "running";
				if (asyncState === "completed") patch.status = "ok";
				if (asyncState === "failed") patch.status = "error";
				if (asyncState === "completed" || asyncState === "failed") {
					state.backgroundToolCallIds.delete(callId);
					state.toolSeqByCallId.delete(callId);
				}
			}
			return [{ op: "update", seq, patch }];
		}
		case "tool_execution_end": {
			const callId = str(frame.toolCallId);
			const seq = state.toolSeqByCallId.get(callId);
			if (seq === undefined) return [];
			const result = isRpcRecord(frame.result) ? frame.result : undefined;
			const asyncState = asyncToolState(result?.details);
			const isBackground = asyncState === "running";
			if (isBackground) {
				state.backgroundToolCallIds.add(callId);
			} else {
				state.backgroundToolCallIds.delete(callId);
				state.toolSeqByCallId.delete(callId);
			}
			const content = splitContent(result?.content);
			const ops: ProjectionOp[] = [
				{
					op: "update",
					seq,
					patch: {
						status: isBackground
							? "running"
							: frame.isError === true || asyncState === "failed"
								? "error"
								: "ok",
						text: content.text,
						details_json: jsonOf(result?.details, AGENT_ENTRY_CAPS.toolDetails),
					},
				},
			];
			const toolName = str(frame.toolName) || "tool";
			for (const image of content.images) {
				if (image.data_b64.length > AGENT_ENTRY_CAPS.imageBytes) continue;
				ops.push({
					op: "append",
					entry: {
						kind: "image",
						seq: state.nextSeq++,
						ts: now,
						media_type: image.media_type,
						data_b64: image.data_b64,
						alt: `${toolName} result image`,
					},
				});
			}
			return ops;
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
		case "notice": {
			const message = str(frame.message);
			if (!message) return [];
			const source = str(frame.source);
			const level =
				frame.level === "error"
					? "error"
					: frame.level === "warning"
						? "warn"
						: "info";
			return [notice(state, now, level, source ? `${source}: ${message}` : message)];
		}
		case "message_end": {
			const message = isRpcRecord(frame.message) ? frame.message : undefined;
			if (!message) return [];
			if (message.role === "assistant") {
				return projectAssistantTermination(message, state, now);
			}
			if (
				(message.role === "custom" || message.role === "hookMessage") &&
				message.display === true
			) {
				const signature = customMessageSignature(message);
				if (state.renderedCustomMessageSignatures.has(signature)) return [];
				state.renderedCustomMessageSignatures.add(signature);
				const content =
					typeof message.content === "string" ? message.content : splitContent(message.content).text;
				if (!content) return [];
				return [
					notice(
						state,
						now,
						"info",
						`${str(message.customType) || "custom"}: ${content}`,
						message.details,
					),
				];
			}
			const visibleInput = projectLiveInputMessage(message, state, now);
			if (visibleInput.length > 0) return visibleInput;
			return [];
		}
		case "auto_retry_start": {
			const attempt = typeof frame.attempt === "number" ? frame.attempt : undefined;
			const maxAttempts = typeof frame.maxAttempts === "number" ? frame.maxAttempts : undefined;
			const progress =
				attempt !== undefined && maxAttempts !== undefined
					? ` (attempt ${attempt}/${maxAttempts})`
					: attempt !== undefined
						? ` (attempt ${attempt})`
						: maxAttempts !== undefined
							? ` (max ${maxAttempts})`
							: "";
			const error = str(frame.errorMessage);
			return [notice(state, now, "warn", `retrying after error${progress}${error ? ` — ${error}` : ""}`)];
		}
		case "auto_retry_end":
			return frame.success === false
				? [notice(
						state,
						now,
						"error",
						`Retry failed after ${typeof frame.attempt === "number" ? frame.attempt : "?"} attempts: ${str(frame.finalError) || "Unknown error"}`,
					)]
				: [notice(state, now, "info", "Retry succeeded")];
		case "retry_fallback_applied":
			return [notice(state, now, "warn", `Fallback: ${str(frame.from)} -> ${str(frame.to)}`)];
		case "retry_fallback_succeeded":
			return [notice(state, now, "info", `Fallback succeeded on ${str(frame.model)}`)];
		case "auto_compaction_start":
			return [
				notice(
					state,
					now,
					"info",
					`${maintenanceReasonPrefix(frame.reason)}${maintenanceLabel(frame.action)}…`,
				),
			];
		case "auto_compaction_end": {
			const label = maintenanceLabel(frame.action);
			if (frame.aborted === true) return [notice(state, now, "info", `${label} cancelled`)];
			const error = str(frame.errorMessage);
			if (error) return [notice(state, now, "warn", error)];
			if (frame.skipped === true) return [];
			return [notice(state, now, "info", `${label} completed`)];
		}
		case "ttsr_triggered": {
			const rules = Array.isArray(frame.rules) ? frame.rules.filter(isRpcRecord) : [];
			if (rules.length === 0) {
				return [notice(state, now, "info", "tool-time system reminder injected")];
			}
			const details = rules.map((rule) => {
				const name = str(rule.name) || "unnamed rule";
				const description = str(rule.description) || str(rule.content);
				return description ? `${name}: ${description}` : name;
			});
			const heading = `Injecting ${details.length === 1 ? "rule" : `${details.length} rules`}`;
			return [notice(state, now, "warn", `${heading}\n${details.join("\n")}`)];
		}
		case "irc_message": {
			const message = isRpcRecord(frame.message) ? frame.message : undefined;
			if (!message || message.display !== true) return [];
			const signature = customMessageSignature(message);
			if (state.renderedCustomMessageSignatures.has(signature)) return [];
			state.renderedCustomMessageSignatures.add(signature);
			const content =
				typeof message.content === "string" ? message.content : splitContent(message.content).text;
			return content
				? [
						notice(
							state,
							now,
							"info",
							`${str(message.customType) || "irc"}: ${content}`,
							message.details,
						),
					]
				: [];
		}
		case "todo_reminder": {
			if (!Array.isArray(frame.todos)) return [];
			const attempt = typeof frame.attempt === "number" ? frame.attempt : undefined;
			const maxAttempts = typeof frame.maxAttempts === "number" ? frame.maxAttempts : undefined;
			const progress =
				attempt !== undefined && maxAttempts !== undefined ? ` ${attempt}/${maxAttempts}` : "";
			return [
				{
					op: "append",
					entry: {
						kind: "todo",
						seq: state.nextSeq++,
						ts: now,
						phases_json: JSON.stringify([
							{ name: `Todo reminder${progress}`, tasks: frame.todos },
						]),
					},
				},
			];
		}
		case "todo_auto_clear":
			return [];
		case "subagent_lifecycle":
		case "subagent_progress":
		case "subagent_event":
			return projectSubagentFrame(frame, state, now);
		// Deliberately dropped repaint/lifecycle chatter: agent_start, agent_end,
		// turn_start, turn_end, message_start, prompt_result,
		// available_commands_update, session_info_update, config_update,
		// response and ready. Chrome setStatus/setWidget/setTitle/
		// set_editor_text methods are filtered by chromeNotice.
		// Unknown types land here too: omp adds frames across versions, and an
		// unknown frame is never an error.
		default:
			return [];
	}
}
function projectLiveInputMessage(
	message: Record<string, unknown>,
	state: ProjectionState,
	now: number,
): ProjectionOp[] {
	if (
		message.role === "developer" ||
		(message.role === "user" && message.synthetic === true)
	) {
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
					ts: now,
					text: clampText(content.text, AGENT_ENTRY_CAPS.text),
					done: true,
				},
			});
		}
		appendImages(ops, content.images, state, now, "user image");
		return ops;
	}
	if (message.role === "bashExecution" || message.role === "pythonExecution") {
		const bash = message.role === "bashExecution";
		const input = bash ? str(message.command) : str(message.code);
		const output = str(message.output);
		const suffix =
			message.cancelled === true
				? "\n(cancelled)"
				: typeof message.exitCode === "number" && message.exitCode !== 0
					? `\n(exit ${message.exitCode})`
					: "";
		const label = bash ? "Bash execution" : "Python execution";
		return [notice(state, now, "info", `${label}: ${input}${output ? `\n${output}` : ""}${suffix}`)];
	}
	if (message.role === "branchSummary" || message.role === "compactionSummary") {
		const label = message.role === "branchSummary" ? "Branch summary" : "Compaction summary";
		const warning = str(message.warning);
		const ops = [
			notice(
				state,
				now,
				warning ? "warn" : "info",
				`${label}: ${str(message.summary)}${warning ? `\n${warning}` : ""}`,
			),
		];
		if (message.role === "compactionSummary") {
			const content = splitContent([
				...(Array.isArray(message.blocks) ? message.blocks : []),
				...(Array.isArray(message.images) ? message.images : []),
			]);
			appendImages(ops, content.images, state, now, "compaction image");
		}
		return ops;
	}
	if (message.role !== "fileMention" || !Array.isArray(message.files)) return [];
	const paths: string[] = [];
	const images: SplitContent["images"] = [];
	for (const file of message.files) {
		if (!isRpcRecord(file)) continue;
		const path = str(file.path);
		if (path) paths.push(path);
		const image = isRpcRecord(file.image) ? file.image : undefined;
		const data_b64 = str(image?.data);
		if (!data_b64) continue;
		images.push({
			media_type: str(image?.mimeType) || "image/png",
			data_b64,
		});
	}
	const ops: ProjectionOp[] =
		paths.length > 0 ? [notice(state, now, "info", `Files: ${paths.join(", ")}`)] : [];
	appendImages(ops, images, state, now, "file image");
	return ops;
}

function appendImages(
	ops: ProjectionOp[],
	images: SplitContent["images"],
	state: ProjectionState,
	now: number,
	alt: string,
): void {
	for (const image of images) {
		if (image.data_b64.length > AGENT_ENTRY_CAPS.imageBytes) continue;
		ops.push({
			op: "append",
			entry: {
				kind: "image",
				seq: state.nextSeq++,
				ts: now,
				media_type: image.media_type,
				data_b64: image.data_b64,
				alt,
			},
		});
	}
}

function projectSubagentFrame(
	frame: RpcFrame,
	state: ProjectionState,
	now: number,
): ProjectionOp[] {
	if (frame.type === "subagent_event") return [];
	const payload = isRpcRecord(frame.payload) ? frame.payload : undefined;
	if (!payload) return [];
	const progress =
		frame.type === "subagent_progress" && isRpcRecord(payload.progress)
			? payload.progress
			: undefined;
	const id = str(frame.type === "subagent_progress" ? progress?.id : payload.id);
	if (!id) return [];

	const status = str(frame.type === "subagent_progress" ? progress?.status : payload.status);
	const terminalState =
		status === "completed"
			? "done"
			: status === "aborted"
				? "aborted"
				: status === "failed"
					? "failed"
					: "running";
	const recentOutput = Array.isArray(progress?.recentOutput)
		? progress.recentOutput
				.filter((line): line is string => typeof line === "string")
				.reverse()
				.join("\n")
		: "";
	const retryText = subagentRetryText(progress, status);
	const output = recentOutput && retryText ? `${recentOutput}\n${retryText}` : recentOutput || retryText;
	const seq = state.subagentSeqById.get(id);
	if (seq !== undefined) {
		const patch: EntryPatch = { subagent_state: terminalState };
		if (output) patch.text = output;
		return [{ op: "update", seq, patch }];
	}

	const nextSeq = state.nextSeq++;
	state.subagentSeqById.set(id, nextSeq);
	return [
		{
			op: "append",
			entry: {
				kind: "subagent",
				seq: nextSeq,
				ts: now,
				subagent_id: id,
				name: str(payload.description) || str(payload.agent) || str(progress?.agent) || "subagent",
				state: terminalState,
				text: clampText(output, AGENT_ENTRY_CAPS.text),
			},
		},
	];
}
function subagentRetryText(progress: Record<string, unknown> | undefined, status: string): string {
	if (!progress) return "";
	const retryState = isRpcRecord(progress.retryState) ? progress.retryState : undefined;
	if (retryState && status === "running") {
		const attempt = typeof retryState.attempt === "number" ? retryState.attempt : "?";
		const maxAttempts = typeof retryState.maxAttempts === "number" ? retryState.maxAttempts : "?";
		const error = str(retryState.errorMessage);
		return `retrying ${attempt}/${maxAttempts}${error ? `: ${error}` : ""}`;
	}
	const retryFailure = isRpcRecord(progress.retryFailure) ? progress.retryFailure : undefined;
	if (!retryFailure || status === "running") return "";
	const attempt = typeof retryFailure.attempt === "number" ? retryFailure.attempt : 0;
	const error = str(retryFailure.errorMessage);
	return `auto-retry gave up after ${attempt} attempt${attempt === 1 ? "" : "s"}${
		error ? `: ${error}` : ""
	}`;
}

function maintenanceReasonPrefix(reason: unknown): string {
	switch (reason) {
		case "overflow":
			return "Context overflow detected, ";
		case "incomplete":
			return "Response incomplete, ";
		case "idle":
			return "Idle ";
		default:
			return "";
	}
}

function maintenanceLabel(action: unknown): string {
	switch (action) {
		case "handoff":
			return "Auto-handoff";
		case "shake":
			return "Auto-shake";
		case "snapcompact":
			return "Auto-snapcompact";
		default:
			return "Auto context-full maintenance";
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
		case "error": {
			const errorMessage = isRpcRecord(ev.error)
				? ev.error
				: { errorMessage: str(ev.error) || str(ev.message) };
			return projectAssistantTermination(
				errorMessage,
				state,
				now,
				str(ev.reason) || "error",
				"assistant stream error",
			);
		}
		case "image_end": {
			const content = isRpcRecord(ev.content) ? ev.content : undefined;
			const data = str(content?.data);
			if (!data) return [];
			if (data.length > AGENT_ENTRY_CAPS.imageBytes) {
				return [notice(state, now, "info", `[image dropped: ${data.length} bytes]`)];
			}
			return [
				{
					op: "append",
					entry: {
						kind: "image",
						seq: state.nextSeq++,
						ts: now,
						media_type: str(content?.mimeType) || "image/png",
						data_b64: data,
						alt: "assistant image",
					},
				},
			];
		}
		// toolcall_* deltas are ignored here: tool state comes from the
		// authoritative tool_execution_* frames, which carry parsed args.
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
		const level = frame.notifyType === "error" ? "error" : frame.notifyType === "warning" ? "warn" : "info";
		return [notice(state, now, level, text)];
	}
	if (method === "open_url") {
		const url = str(frame.launchUrl) || str(frame.url);
		if (!url) return [];
		const instructions = str(frame.instructions);
		return [notice(state, now, "info", instructions ? `${url}\n${instructions}` : url)];
	}
	return [];
}

function notice(
	state: ProjectionState,
	now: number,
	level: "info" | "warn" | "error",
	text: string,
	details?: unknown,
): ProjectionOp {
	return {
		op: "append",
		entry: {
			kind: "notice",
			seq: state.nextSeq++,
			ts: now,
			level,
			text: clampText(text, AGENT_ENTRY_CAPS.text),
			details_json: jsonOf(details, AGENT_ENTRY_CAPS.toolDetails),
		},
	};
}
