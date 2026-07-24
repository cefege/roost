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

function out(frame: Record<string, unknown>): void {
	process.stdout.write(`${JSON.stringify(frame)}\n`);
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
	await Bun.sleep(5);
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

		if (type === "extension_ui_response") { out({ type: "agent_end", messages: [] }); continue; }

		if (type === "prompt") {
			out({ id, type: "response", command: "prompt", success: true, data: { agentInvoked: true } });
			if (cmd.message === "__die") { await Bun.sleep(5); process.exit(0); }
			void runTurn();
			continue;
		}
		if (type === "get_state") {
			out({
				id, type: "response", command: "get_state", success: true,
				data: { sessionFile: SESSION_FILE, sessionId: "fake", isStreaming: false, messageCount: 0 },
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
