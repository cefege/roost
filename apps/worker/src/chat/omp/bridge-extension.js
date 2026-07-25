// Roost live chat bridge — an omp extension that mirrors the agent event bus
// into a per-session NDJSON sidecar so Roost's web chat pane can stream.
//
// SOURCE OF TRUTH IS THIS REPO FILE: apps/worker/src/chat/omp/bridge-extension.js.
// `bridge-install.ts` copies it VERBATIM (byte-for-byte, header comment included)
// to `~/.omp/agent/extensions/roost-chat-bridge.js` on worker boot. Never edit the
// installed copy — it is overwritten. Written against @oh-my-pi/pi-coding-agent@17.1.3
// (`src/extensibility/extensions/types.ts` for the `ExtensionAPI.on` overload list
// and the event field shapes; `src/modes/warp-events.ts` for the supported shape).
//
// Why a sidecar at all: omp's `SessionManager.appendMessage` persists a message to
// the JSONL transcript only once it is COMPLETE, so the transcript can never stream.
// The event bus is the only live source, and the extension API exposes it in full.
//
// Runs INSIDE omp's runtime, not Roost's: zero imports beyond node:fs / node:path.
// No-ops with a single `process.env` read unless ROOST_OMP_LIVE_DIR and
// ROOST_SESSION_ID are both set and stdout is a TTY, so a hand-started omp on this
// machine pays nothing.

import * as fs from "node:fs";
import * as path from "node:path";

/** Trailing-flush window, newest-wins per key. Mirrors STREAM_FLUSH_MS in
 *  apps/worker/src/chat/omp/rpc-chat.ts: a token stream becomes ~16 writes/s
 *  instead of one write per token. */
const FLUSH_MS = 60;

/** Tool `partialResult` text is capped here before it hits the sidecar, matching
 *  PARTIAL_CAP in rpc-chat.ts. Everything else is written whole — the worker
 *  applies TRUNC_CAP when it maps to a ContentBlock. */
const PARTIAL_CAP = 2000;

/** Last 2000 chars of a tool partial result, in a shape the worker's
 *  `partialResultText` still understands (string, or `{ content: [{type,text}] }`). */
function capPartial(raw) {
	if (typeof raw === "string") return raw.length > PARTIAL_CAP ? raw.slice(-PARTIAL_CAP) : raw;
	if (raw === null || typeof raw !== "object") return "";
	const content = raw.content;
	if (!Array.isArray(content)) return "";
	let text = "";
	for (const block of content) {
		if (block !== null && typeof block === "object" && block.type === "text" && typeof block.text === "string") {
			text += block.text;
		}
	}
	if (text.length > PARTIAL_CAP) text = text.slice(-PARTIAL_CAP);
	return { content: [{ type: "text", text }] };
}

export default function roostChatBridge(pi) {
	const dir = process.env.ROOST_OMP_LIVE_DIR;
	const sid = process.env.ROOST_SESSION_ID;
	if (!dir || !sid) return; // not a Roost pane -> no-op, zero cost
	if (!process.stdout.isTTY) return; // subagents / piped invocations never bridge

	const file = path.join(dir, `${sid}.ndjson`);

	let fd = null;
	let seq = 0;
	let live = 0;
	let curLive = null;

	// Coalescing state: key -> frame. "" is the assistant message stream, a
	// toolCallId is that tool's output stream. Newest frame per key wins; every
	// pending frame is stamped with a fresh `seq` at emit time so the worker sees
	// the same monotonic order the writes land in.
	const pending = new Map();
	let timer = null;
	let ctxRef = null;

	const write = (obj) => {
		if (fd === null) return;
		try {
			fs.writeSync(fd, `${JSON.stringify(obj)}\n`);
		} catch {
			// A dead sidecar must never surface in the user's agent.
		}
	};

	const clearTimer = () => {
		if (timer === null) return;
		try {
			if (ctxRef && typeof ctxRef.clearTimer === "function") ctxRef.clearTimer(timer);
			else clearTimeout(timer);
		} catch {}
		timer = null;
	};

	const flush = () => {
		clearTimer();
		if (pending.size === 0) return;
		const frames = [...pending.values()];
		pending.clear();
		for (const frame of frames) {
			frame.seq = ++seq;
			write(frame);
		}
	};

	// `ctx.setTimeout` contains throws and is auto-cleared on session_shutdown
	// (types.ts:456-463); raw setTimeout is only the fallback for a host that
	// predates it.
	const schedule = (frame, key, ctx) => {
		if (fd === null) return;
		if (ctx) ctxRef = ctx;
		pending.set(key, frame);
		if (timer !== null) return;
		try {
			if (ctxRef && typeof ctxRef.setTimeout === "function") timer = ctxRef.setTimeout(flush, FLUSH_MS);
			else timer = setTimeout(flush, FLUSH_MS);
		} catch {
			timer = null;
			flush(); // could not schedule -> do not silently swallow the frame
		}
	};

	const emit = (event) => {
		write({ t: "ev", seq: ++seq, e: event });
	};

	/** Every handler is individually contained: a bridge that throws must never
	 *  break the user's agent loop. */
	const safe = (fn) => (event, ctx) => {
		try {
			if (ctx) ctxRef = ctx;
			fn(event, ctx);
		} catch {}
	};

	const openSidecar = (_event, ctx) => {
		pending.clear();
		clearTimer();
		try {
			fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
		} catch {}
		try {
			if (fd !== null) fs.closeSync(fd);
		} catch {}
		fd = null;
		// "w" TRUNCATES. The worker tailer treats size < offset as a reseed, so a
		// second omp started in the same pane restarts the stream cleanly.
		try {
			fd = fs.openSync(file, "w", 0o600);
		} catch {
			return;
		}
		seq = 0;
		live = 0;
		curLive = null;
		let sessionFile;
		let cwd;
		try {
			sessionFile = ctx.sessionManager.getSessionFile();
			cwd = ctx.sessionManager.getCwd();
		} catch {}
		write({
			t: "hello",
			v: 1,
			sid,
			sessionFile: sessionFile ?? "",
			cwd: cwd ?? "",
			pid: process.pid,
			ts: new Date().toISOString(),
		});
	};

	pi.on("session_start", safe(openSidecar));
	pi.on("session_switch", safe(openSidecar));
	pi.on(
		"session_shutdown",
		safe(() => {
			flush();
			write({ t: "bye" });
			try {
				if (fd !== null) fs.closeSync(fd);
			} catch {}
			fd = null;
			curLive = null;
		}),
	);

	pi.on(
		"agent_start",
		safe(() => {
			emit({ type: "agent_start" });
		}),
	);
	// `AgentEndEvent` also carries the whole `messages` array; the worker only
	// needs the transition, so it is dropped rather than written every turn.
	pi.on(
		"agent_end",
		safe(() => {
			flush();
			curLive = null;
			emit({ type: "agent_end" });
		}),
	);

	pi.on(
		"message_start",
		safe((event) => {
			if (!event.message || event.message.role !== "assistant") return;
			flush();
			curLive = `live-${++live}`;
			write({ t: "ev", seq: ++seq, live: curLive, e: { type: "message_start", message: event.message } });
		}),
	);
	// `e.message` is the FULL message so far, not a delta — same contract
	// rpc-chat.ts::onEvent already assumes.
	pi.on(
		"message_update",
		safe((event, ctx) => {
			if (!event.message || event.message.role !== "assistant" || !curLive) return;
			schedule({ t: "ev", live: curLive, e: { type: "message_update", message: event.message } }, "", ctx);
		}),
	);
	pi.on(
		"message_end",
		safe(event => {
			flush();
			if (!event.message || event.message.role !== "assistant") return;
			// No entry id is written on purpose. `message_end` fires BEFORE omp
			// appends the entry, so `ctx.sessionManager.getLeafId()` here returns
			// the PREVIOUS leaf — measured against omp 17.1.3: a `title_change`, a
			// `toolResult`, a `developer` reminder. The worker joins the streamed
			// row to its durable copy on omp's own persistence key instead, which
			// it derives from `message` on both sides
			// (parse.ts::assistantPersistenceKey). Everything the join needs is
			// already in the payload below.
			write({
				t: "ev",
				seq: ++seq,
				live: curLive,
				e: { type: "message_end", message: event.message },
			});
			curLive = null;
		}),
	);

	pi.on(
		"tool_execution_start",
		safe((event) => {
			flush();
			emit({
				type: "tool_execution_start",
				toolCallId: event.toolCallId,
				toolName: event.toolName,
				args: event.args,
				intent: event.intent,
			});
		}),
	);
	pi.on(
		"tool_execution_update",
		safe((event, ctx) => {
			schedule(
				{
					t: "ev",
					e: {
						type: "tool_execution_update",
						toolCallId: event.toolCallId,
						toolName: event.toolName,
						partialResult: capPartial(event.partialResult),
					},
				},
				`tool:${event.toolCallId}`,
				ctx,
			);
		}),
	);
	pi.on(
		"tool_execution_end",
		safe((event) => {
			flush();
			emit({
				type: "tool_execution_end",
				toolCallId: event.toolCallId,
				toolName: event.toolName,
				result: event.result,
				isError: event.isError,
			});
		}),
	);

	// Narration rows the TUI paints from live-only events, forwarded verbatim; the
	// worker's projector turns each into a `notice` block.
	//
	// The TUI additionally paints `notice`, `irc_message`, `retry_fallback_applied`,
	// `retry_fallback_succeeded`, `todo_auto_clear` and `thinking_level_changed`,
	// none of which exist in `ExtensionAPI.on`'s overload list, and `pi.events`
	// carries only `lsp:startup` / `mcp:connection-status` / `task:subagent:event`
	// — so they are unreachable from an extension in 17.1.3. `irc_message` and
	// `thinking_level_changed` still arrive durably (as a `custom_message` entry and
	// a `thinking_level_change` entry); the rest stay absent, exactly as today.
	for (const type of [
		"auto_compaction_start",
		"auto_compaction_end",
		"auto_retry_start",
		"auto_retry_end",
		"ttsr_triggered",
		"todo_reminder",
	]) {
		pi.on(
			type,
			safe((event) => {
				flush();
				emit({ ...event, type });
			}),
		);
	}
}
