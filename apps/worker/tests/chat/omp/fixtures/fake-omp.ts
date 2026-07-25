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
//   prompt "__withdraw" → ack, a `select` request, then a `cancel` naming it.
//   prompt "__mystery"  → ack, then an extension_ui_request with a method the
//                         worker cannot render. omp awaits it, so a silent drop
//                         would wedge the turn forever.
//   extension_ui_response → agent_end (turn closes)
//   prompt "__die"      → ack, then exit(0) (child-death / respawn path)
//   get_state           → sessionFile + isStreaming
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

out({ type: "ready", protocolVersion: 1, supportedProtocolVersions: [1], maxFrameBytes: 1048576 });

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
			out({ type: "agent_end", messages: [] });
			continue;
		}

		if (type === "prompt") {
			out({ id, type: "response", command: "prompt", success: true, data: { agentInvoked: true } });
			if (cmd.message === "__die") { await Bun.sleep(5); process.exit(0); }
			if (cmd.message === "__decide") { out({ type: "agent_start" }); askSelect("ui-sel"); continue; }
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
					model: { provider: "anthropic", id: "claude-opus-5" },
					contextUsage: { tokens: 18004, contextWindow: 1000000, percent: 1.8004 },
				},
			});
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
