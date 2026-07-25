#!/usr/bin/env bun
// Scripted stand-in for `omp --mode rpc`. Speaks the real JSONL grammar over
// stdio so rpc-driver/rpc-chat are exercised end to end without an omp install
// or model credentials. Pointed at by ROOST_OMP_BIN.
//
// Every inbound frame is appended to $FAKE_OMP_LOG (one JSON object per line)
// so a test can assert what actually reached the agent — e.g. that the worker
// injected streamingBehavior on a mid-turn prompt.
//
// Scripted behavior:
//   prompt              → ack, then agent_start, 3 growing message_updates,
//                         message_end, tool_execution_start/end, and a confirm
//                         extension_ui_request. The turn STAYS open (streaming)
//                         until the UI request is answered — that is the window
//                         a test uses to send a mid-turn prompt.
//   prompt "__decide"   → ack, then a `select` request carrying N options,
//                         mirroring what a real `omp --mode rpc-ui` ask tool
//                         emits. Answering it emits the `editor` follow-up the
//                         "Other (type your own)" branch produces.
//   prompt "__ask"      → ack, then a batched two-question `ask`: the rich
//                         args on tool_execution_start, then Q1 (single) and Q2
//                         (multi, re-prompted per tick) as bare select frames.
//                         Answering Q2 with "Next →" ends the tool call.
//   prompt "__withdraw" → ack, a `select` request, then a `cancel` naming it.
//   prompt "__mystery"  → ack, then an extension_ui_request with a method the
//                         worker cannot render. omp awaits it, so a silent drop
//                         would wedge the turn forever.
//   extension_ui_response → agent_end (turn closes)
//   prompt "__die"      → ack, then exit(0) (child-death / respawn path)
//   get_state           → sessionFile + isStreaming + model + thinkingLevel
//   get_available_models→ the catalog, with the friendly `name` the chip shows
//   set_model           → mutate + ack, and NOTHING else: omp 17.1.2 pushes no
//                         event for a model change (verified against a live
//                         child), so the response is the client's only signal
//   set_thinking_level  → mutate + ack, then the `thinking_level_changed` event
//                         omp really does push
//   switch_session      → ack
//   get_messages_page   → one message, no nextCursor

import { appendFileSync, writeFileSync } from "node:fs";

const SESSION_FILE = process.env.FAKE_OMP_SESSION_FILE ?? "/tmp/fake-omp-session.jsonl";
const LOG = process.env.FAKE_OMP_LOG ?? "";

// Real omp creates its session JSONL on start. The durable session store
// prunes mappings whose file has vanished, so a fixture that only *names* a
// path would look like a cache bug on the next load.
writeFileSync(SESSION_FILE, `${JSON.stringify({ type: "session", version: 3 })}\n`);
// Record how we were launched. The `ask` tool only exists when omp runs with a
// UI (`hasUI = isInteractive || mode === "rpc-ui"`), so the spawn argv is the
// difference between a chat that can ask the user a question and one that
// structurally cannot — a regression there is invisible in every other assert.
if (LOG) appendFileSync(LOG, `${JSON.stringify({ type: "__argv", argv: process.argv.slice(2) })}\n`);

function out(frame: Record<string, unknown>): void {
	process.stdout.write(`${JSON.stringify(frame)}\n`);
}

/** N-option decision, the shape a real rpc-ui `ask` tool emits. */
function askSelect(id: string): void {
	out({ type: "extension_ui_request", id, method: "select", title: "Pick a colour", options: ["Red", "Green", "Blue", "Other (type your own)"] });
}

/** A batched two-question `ask`, as omp emits it over RPC: the rich question
 *  data rides ONLY on tool_execution_start.args, while the select frames that
 *  follow carry bare labels with omp's own title decorations — ` (i/total)` for
 *  the batch position, `(N selected) ` on a multi-select re-prompt — plus the
 *  " (Recommended)" suffix that is the only way recommendation crosses. */
const ASK_ARGS = {
	questions: [
		{
			id: "auth",
			question: "Which auth method?",
			header: "Auth",
			options: [
				{ label: "JWT", description: "Bearer tokens for stateless API clients." },
				{ label: "OAuth2", description: "Delegated authorization via an external IdP." },
				{ label: "Session cookies", description: "Browser-first, server-side sessions." },
			],
			recommended: 1,
		},
		{
			id: "features",
			question: "Which features ship in v1?",
			header: "Scope",
			multi: true,
			options: [
				{ label: "Streaming", description: "Token-by-token responses." },
				{ label: "Attachments", description: "File uploads on a prompt." },
				{ label: "Search", description: "" },
			],
		},
	],
};

const ASK_Q2_OPTIONS = ["Streaming", "Attachments", "Search", "Other (type your own)", "← Back", "Next →"];

/** Q2 re-prompt, carrying omp's `(N selected) ` title prefix. */
function askQ2(id: string, selected: number): void {
	const prefix = selected > 0 ? `(${selected} selected) ` : "";
	out({
		type: "extension_ui_request", id, method: "select",
		title: `${prefix}Which features ship in v1? (2/2)`,
		options: ASK_Q2_OPTIONS,
	});
}


async function runTurn(): Promise<void> {
	out({ type: "agent_start" });
	const parts = ["Hel", "Hello, ", "Hello, world"];
	for (const text of parts) {
		await Bun.sleep(5);
		out({
			type: "message_update",
			message: { role: "assistant", content: [{ type: "text", text }] },
			assistantMessageEvent: { type: "text_delta", delta: text },
		});
	}
	await Bun.sleep(5);
	out({ type: "message_end", message: { role: "assistant", content: [{ type: "text", text: "Hello, world" }] } });
	out({ type: "tool_execution_start", toolCallId: "call_1", toolName: "read", intent: "Reading a file" });
	// Live partial output, faster than STREAM_FLUSH_MS so the coalescing path
	// is what the test actually exercises. Newest must win.
	for (const chunk of ["line 1\n", "line 1\nline 2\n", "line 1\nline 2\nline 3\n"]) {
		out({
			type: "tool_execution_update", toolCallId: "call_1", toolName: "read",
			partialResult: { content: [{ type: "text", text: chunk }] },
		});
	}
	await Bun.sleep(120);
	out({ type: "tool_execution_end", toolCallId: "call_1", toolName: "read", result: { ok: true } });
	out({ type: "extension_ui_request", id: "ui-1", method: "confirm", title: "Confirm", message: "Continue?" });
}

// Live session config the model/effort commands mutate. get_state reports it
// and thinking_level_changed announces an effort change.
let modelProvider = "anthropic";
let modelId = "claude-opus-5";
let thinkingLevel = "medium";

/** The catalog get_available_models returns. `cost` is deliberately present and
 *  fat: the real payload is ~1.1 MB of provider metadata, and the worker is
 *  expected to project it down to the five fields the picker uses. */
const CATALOG = [
	{ provider: "anthropic", id: "claude-opus-5", name: "Claude Opus 5", reasoning: true, thinking: { efforts: ["low", "medium", "high"] }, cost: { input: 5, output: 25 }, contextWindow: 1000000 },
	{ provider: "anthropic", id: "claude-sonnet-5", name: "Claude Sonnet 5", reasoning: true, thinking: { efforts: ["low", "medium", "high"] }, cost: { input: 3, output: 15 }, contextWindow: 1000000 },
	{ provider: "openai", id: "gpt-5", name: "GPT-5", reasoning: false, cost: { input: 1, output: 8 }, contextWindow: 400000 },
];

/** 1 until the client negotiates up. At 2 an oversized response is delivered as
 *  base64 rpc_chunk frames — the only way the real catalog fits at all. */
let protocolVersion = 1;

/** get_state's model object carries the friendly name on the real agent, which
 *  is where the composer chip reads it from. */
const modelName = () => CATALOG.find((m) => m.provider === modelProvider && m.id === modelId)?.name ?? modelId;

/** Emit a frame the way protocol v2 delivers a large one: the JSON is split
 *  into byte slices and EACH slice is base64'd on its own (padding included) —
 *  matching omp 17.1.2 exactly. A reader that concatenates the base64 text and
 *  decodes once gets only the first slice, which is the bug this shape pins. */
function outChunked(frame: Record<string, unknown>): void {
	const bytes = Buffer.from(JSON.stringify(frame), "utf8");
	const count = 3;
	const size = Math.ceil(bytes.length / count);
	const chunkId = `chunk-${String(frame.id ?? "x")}`;
	for (let i = 0; i < count; i++) {
		const slice = bytes.subarray(i * size, Math.min((i + 1) * size, bytes.length));
		out({ type: "rpc_chunk", chunkId, index: i, count, byteLength: bytes.length, data: slice.toString("base64") });
	}
}

out({ type: "ready", protocolVersion: 1, supportedProtocolVersions: [1, 2], maxFrameBytes: 1048576, maxReassembledFrameBytes: 67108864 });

const dec = new TextDecoder();
let buf = "";
for await (const chunk of Bun.stdin.stream()) {
	buf += dec.decode(chunk as Uint8Array, { stream: true });
	let nl: number;
	while ((nl = buf.indexOf("\n")) >= 0) {
		const line = buf.slice(0, nl).trim();
		buf = buf.slice(nl + 1);
		if (!line) continue;
		let cmd: Record<string, unknown>;
		try { cmd = JSON.parse(line) as Record<string, unknown>; } catch { continue; }
		if (LOG) appendFileSync(LOG, `${line}\n`);
		const id = typeof cmd.id === "string" ? cmd.id : undefined;
		const type = String(cmd.type);

		if (type === "extension_ui_response") {
			// "Other (type your own)" is not an answer — the real ask tool follows
			// it with a free-text `editor` request, which omp AWAITS.
			if (cmd.id === "ui-sel" && cmd.value === "Other (type your own)") {
				out({ type: "extension_ui_request", id: "ui-ed", method: "editor", title: "Pick a colour\n\nEnter your response:", prefill: "Red", promptStyle: true });
				continue;
			}
			// The batched ask advances question by question, exactly as ask.ts's
			// loop does: Q1 answered → Q2; a Q2 tick → Q2 again with the count in
			// the title; `Next →` → the batch is done.
			if (cmd.id === "ui-a1") { askQ2("ui-a2", 0); continue; }
			if (cmd.id === "ui-a2") { askQ2("ui-a3", 1); continue; }
			if (cmd.id === "ui-a3") {
				out({ type: "tool_execution_end", toolCallId: "call_ask", toolName: "ask", result: { ok: true } });
				out({ type: "agent_end", messages: [] });
				continue;
			}
			out({ type: "agent_end", messages: [] });
			continue;
		}

		if (type === "prompt") {
			out({ id, type: "response", command: "prompt", success: true, data: { agentInvoked: true } });
			if (cmd.message === "__die") { await Bun.sleep(5); process.exit(0); }
			if (cmd.message === "__decide") { out({ type: "agent_start" }); askSelect("ui-sel"); continue; }
			if (cmd.message === "__ask") {
				out({ type: "agent_start" });
				// Q1's select frame FIRST, tool_execution_start after — the real
				// ordering: omp writes extension_ui_request straight to stdout while
				// the tool event rides the session event bus a tick behind. A host
				// that only correlates forward loses Q1's descriptions entirely.
				out({
					type: "extension_ui_request", id: "ui-a1", method: "select",
					title: "Which auth method? (1/2)",
					options: ["JWT", "OAuth2 (Recommended)", "Session cookies", "Other (type your own)", "Next →"],
				});
				out({ type: "tool_execution_start", toolCallId: "call_ask", toolName: "ask", args: ASK_ARGS });
				continue;
			}
			if (cmd.message === "__withdraw") {
				out({ type: "agent_start" });
				askSelect("ui-w1");
				await Bun.sleep(20);
				out({ type: "extension_ui_request", id: "ui-w2", method: "cancel", targetId: "ui-w1" });
				continue;
			}
			if (cmd.message === "__mystery") {
				out({ type: "agent_start" });
				out({ type: "extension_ui_request", id: "ui-m1", method: "someFutureDialog", title: "?" });
				continue;
			}
			void runTurn();
			continue;
		}
		if (type === "get_state") {
			out({
				id, type: "response", command: "get_state", success: true,
				data: {
					sessionFile: SESSION_FILE, sessionId: "fake", isStreaming: false, messageCount: 0,
					// Shapes copied from a live omp 17.1.2 get_state.
					model: { provider: modelProvider, id: modelId, name: modelName() },
					thinkingLevel,
					contextUsage: { tokens: 18004, contextWindow: 1000000, percent: 1.8004 },
				},
			});
			continue;
		}
		if (type === "negotiate_protocol") {
			protocolVersion = Number(cmd.protocolVersion ?? 1);
			out({ id, type: "response", command: "negotiate_protocol", success: true, data: { protocolVersion } });
			continue;
		}
		if (type === "get_available_models") {
			const frame = { id, type: "response", command: "get_available_models", success: true, data: { models: CATALOG } };
			// A v1 client cannot receive this at all on a real omp (the reply is
			// over the frame cap) — mirror that split here so the chunk path is
			// what the worker actually exercises.
			if (protocolVersion >= 2) outChunked(frame);
			else out({ id, type: "response", command: "get_available_models", success: false, error: "RPC response exceeded the transport limit" });
			continue;
		}
		if (type === "set_model") {
			modelProvider = String(cmd.provider ?? modelProvider);
			modelId = String(cmd.modelId ?? modelId);
			out({ id, type: "response", command: type, success: true });
			continue;
		}
		if (type === "set_thinking_level") {
			thinkingLevel = String(cmd.level ?? thinkingLevel);
			out({ id, type: "response", command: type, success: true });
			// Unsolicited push, exactly as real omp does — no agent turn runs, so
			// this event is the ONLY notification the client gets.
			out({ type: "thinking_level_changed", thinkingLevel });
			continue;
		}
		if (type === "switch_session") {
			out({ id, type: "response", command: "switch_session", success: true, data: { cancelled: false } });
			continue;
		}
		if (type === "get_messages_page") {
			out({
				id, type: "response", command: "get_messages_page", success: true,
				data: {
					messages: [{ role: "user", content: [{ type: "text", text: "resumed history" }] }],
					totalMessages: 1,
				},
			});
			continue;
		}
		out({ id, type: "response", command: type, success: true });
	}
}
