// Browser-command handlers: omp chat history backfill (get-chat-history /
// get-chat-block) + the terminal-vs-web parity oracle (get-chat-parity).
// Mirrors browser-command-terminal.ts::handleGetScrollbackCells.
// Serves the in-memory transcript cache (history) + a file re-read (full block).

import { readFile } from "node:fs/promises";
import { join } from "node:path";
import type { ClientControlFrame } from "@roost/shared/wire";
import type { CoordLink } from "./transport/CoordLink.ts";
import type { SessionManager } from "./session-manager.ts";
import { getChatHistory, getChatBlockText } from "./session-chat.ts";
import { rpcChatCommand, rpcChatFullBlock, republishRpcChatState } from "./chat/omp/rpc-chat.ts";
import { tuiRows, roostRows, type TuiRow } from "./chat/omp/tui-rows.ts";
import { OMP_LIVE_DIR } from "./session-constants.ts";

/** Cap on rows serialized into either diff array. One rpc-ok frame is bounded
 *  at 1 MiB, and a diff longer than this is a systemic break, not a detail. */
const PARITY_DIFF_CAP = 200;

/** Serve a slice of cached chat history. rpc-ok data:
 *  { messages, next_seq, truncated }. Empty messages when no watcher is running
 *  (non-omp session / not yet resolved). */
export async function handleGetChatHistory(
	frame: Extract<ClientControlFrame, { kind: "get-chat-history" }>,
	request_id: string,
	deps: { coordLink: CoordLink; sessionMgr: SessionManager },
): Promise<void> {
	const { coordLink, sessionMgr } = deps;
	const rec = sessionMgr.getBySessionId(frame.session_id);
	if (!rec) {
		coordLink.send({ kind: "rpc-error", request_id, message: "session not found" });
		return;
	}
	const afterSeq = frame.after_seq ?? rec.chat_seq;
	const page = getChatHistory(rec, afterSeq, frame.max_messages);
	coordLink.send({
		kind: "rpc-ok",
		request_id,
		data: {
			messages: page.messages,
			next_seq: page.nextSeq,
			truncated: page.truncated,
		},
	});
	// Session status (model, effort, context) is PUSHED on change, never part of
	// this page — so a pane that mounts after the child booted would show no
	// model chip until the next turn ended. Re-publish it on every reseed: the
	// mount is exactly the moment the client needs it.
	republishRpcChatState(sessionMgr, frame.session_id);
}

/** Full untruncated text of one ContentBlock. rpc-ok data: { text }.
 *  Native RPC chats keep the full text in memory (their synthetic message ids
 *  match no transcript entry); mirror chats re-read the transcript file. */
export async function handleGetChatBlock(
	frame: Extract<ClientControlFrame, { kind: "get-chat-block" }>,
	request_id: string,
	deps: { coordLink: CoordLink; sessionMgr: SessionManager },
): Promise<void> {
	const { coordLink, sessionMgr } = deps;
	const rec = sessionMgr.getBySessionId(frame.session_id);
	if (!rec) {
		coordLink.send({ kind: "rpc-error", request_id, message: "session not found" });
		return;
	}
	const text = rpcChatFullBlock(frame.session_id, frame.message_id, frame.block_index)
		?? await getChatBlockText(rec, frame.message_id, frame.block_index);
	if (text === null) {
		coordLink.send({ kind: "rpc-error", request_id, message: "block not found" });
		return;
	}
	coordLink.send({ kind: "rpc-ok", request_id, data: { text } });
}

/** Native omp chat: tunnel one RpcCommand to the session's RPC child.
 *  rpc-ok data: { response_json }. Lazy-starts the child on first command. */
export async function handleChatCommand(
	frame: Extract<ClientControlFrame, { kind: "chat-command" }>,
	request_id: string,
	deps: { coordLink: CoordLink; sessionMgr: SessionManager },
): Promise<void> {
	const { coordLink, sessionMgr } = deps;
	const rec = sessionMgr.getBySessionId(frame.session_id);
	if (!rec) {
		coordLink.send({ kind: "rpc-error", request_id, message: "session not found" });
		return;
	}
	const result = await rpcChatCommand(sessionMgr, rec, frame.command_json);
	if (!result.ok) {
		coordLink.send({ kind: "rpc-error", request_id, message: result.error });
		return;
	}
	coordLink.send({ kind: "rpc-ok", request_id, data: { response_json: JSON.stringify(result.response) } });
}

/** Terminal-vs-web parity oracle. Projects the raw transcript onto the rows
 *  omp's TUI would paint, projects rec.chatMessages onto the same row type, and
 *  diffs them POSITIONALLY: walk both together, and on a mismatch record the
 *  tuiRows entry as missing and advance that side (so one inserted row does not
 *  report every later row as different). rpc-ok data mirrors
 *  SessionsGetChatParityResponse field-for-field. */
export async function handleGetChatParity(
	frame: Extract<ClientControlFrame, { kind: "get-chat-parity" }>,
	request_id: string,
	deps: { coordLink: CoordLink; sessionMgr: SessionManager },
): Promise<void> {
	const { coordLink, sessionMgr } = deps;
	const rec = sessionMgr.getBySessionId(frame.session_id);
	if (!rec) {
		coordLink.send({ kind: "rpc-error", request_id, message: "session not found" });
		return;
	}
	const transcriptPath = rec.chatTranscriptPath ?? "";
	let lines: string[] = [];
	if (transcriptPath) {
		try { lines = (await readFile(transcriptPath, "utf8")).split("\n"); }
		catch { /* unreadable → empty projection, reported as a row-count gap */ }
	}
	const livePath = join(OMP_LIVE_DIR, `${rec.sessionId}.ndjson`);

	const tui = tuiRows(lines);
	const roost = roostRows(rec.chatMessages ?? []);
	const missing: TuiRow[] = [];
	const extra: TuiRow[] = [];
	const same = (a: TuiRow, b: TuiRow): boolean => JSON.stringify(a) === JSON.stringify(b);
	let i = 0, j = 0;
	while (i < tui.length && j < roost.length) {
		if (same(tui[i]!, roost[j]!)) { i++; j++; continue; }
		// Prefer resyncing: if the TUI row shows up later on the Roost side the
		// gap is an EXTRA Roost row, otherwise the TUI row is missing.
		if (roost.slice(j + 1, j + 4).some((r) => same(tui[i]!, r))) extra.push(roost[j++]!);
		else missing.push(tui[i++]!);
	}
	while (i < tui.length) missing.push(tui[i++]!);
	while (j < roost.length) extra.push(roost[j++]!);

	coordLink.send({
		kind: "rpc-ok",
		request_id,
		data: {
			transcript_path: transcriptPath,
			// Both gate on the sidecar's `hello`, not on the watcher existing: the
			// watcher polls a path that may never appear (omp never started in this
			// pane), and chatLiveStreaming only flips on agent_start/agent_end, so
			// an attached-but-idle bridge would read as detached.
			live_path: rec.chatLiveAttached === true ? livePath : "",
			live_attached: rec.chatLiveAttached === true,
			tui_rows: tui.length,
			roost_rows: roost.length,
			missing_json: JSON.stringify(missing.slice(0, PARITY_DIFF_CAP)),
			extra_json: JSON.stringify(extra.slice(0, PARITY_DIFF_CAP)),
		},
	});
}
