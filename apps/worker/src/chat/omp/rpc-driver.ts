// OmpRpcDriver — native driver for `omp --mode rpc` (headless JSONL over stdio).
//
// The proper omp↔Roost integration: no PTY, no TUI, no OSC titles, no transcript
// tailing. omp's RPC mode speaks newline-delimited JSON on stdin/stdout
// (rpc-frame.ts in the omp repo caps frames at 1MB): commands in
// ({type:"prompt"|"steer"|"abort"|"get_state"|...}), responses + the full live
// AgentSessionEvent stream out (message_start/update/end, tool_execution_*,
// extension_ui_request, ...). This driver owns exactly that transport: spawn,
// frame parsing, id-correlated request/response, event fanout, teardown.
//
// Protocol shapes are structural (plain JSON, tolerant guards) — we deliberately
// do NOT import omp's types: the wire is the contract, same stance as parse.ts.

import type { Subprocess } from "bun";
import { log } from "@roost/shared";

export interface RpcFrame {
	type: string;
	[k: string]: unknown;
}

export interface OmpRpcDriverOpts {
	cwd: string;
	/** Extra env on top of the worker's (worker env is clean — no CI/test vars). */
	env?: Record<string, string>;
	onEvent: (frame: RpcFrame) => void;
	onExit: (code: number | null) => void;
}

const RESPONSE_TIMEOUT_MS = 30_000;

/** Reassembly cap for a chunked (protocol v2) frame. omp advertises 64 MiB;
 *  the only response that gets anywhere near a megabyte is the model catalog,
 *  so this is a runaway guard, not a working limit. */
const MAX_REASSEMBLED_BYTES = 8 * 1024 * 1024;

/** Resolve the omp binary: explicit override first (tests + non-standard
 *  installs), then PATH, then the standard bun global bin. */
export function resolveOmpBin(): string | null {
	const override = process.env.ROOST_OMP_BIN;
	if (override) return override;
	const fromPath = Bun.which("omp");
	if (fromPath) return fromPath;
	const bunBin = `${process.env.HOME ?? ""}/.bun/bin/omp`;
	try { if (Bun.file(bunBin).size > 0) return bunBin; } catch { /* absent */ }
	return null;
}

export class OmpRpcDriver {
	#proc: Subprocess | null = null;
	#opts: OmpRpcDriverOpts;
	#nextId = 1;
	#pending = new Map<string, { resolve: (f: RpcFrame) => void; timer: ReturnType<typeof setTimeout> }>();
	#disposed = false;
	/** chunkId → base64 pieces of an in-flight protocol-v2 frame. */
	#chunks = new Map<string, string[]>();

	constructor(opts: OmpRpcDriverOpts) {
		this.#opts = opts;
	}

	get alive(): boolean {
		return this.#proc !== null && !this.#disposed;
	}

	start(): void {
		if (this.#proc) return;
		const bin = resolveOmpBin();
		if (!bin) throw new Error("omp binary not found (PATH or ~/.bun/bin/omp)");
		// omp's global install is a `#!/usr/bin/env bun` shim; the worker's
		// LaunchAgent PATH has no bun → exit 127. The worker itself runs under
		// bun, so its own binary dir is the authoritative bun location.
		const bunDir = process.execPath.slice(0, process.execPath.lastIndexOf("/"));
		const path = `${bunDir}:${process.env.PATH ?? ""}`;
		// `rpc-ui`, NOT `rpc`: main.ts sets `hasUI = isInteractive || mode ===
		// "rpc-ui"`, and AskTool.createIf returns null without it — so a plain
		// `--mode rpc` child cannot ask the user ANYTHING. Every multi-option
		// decision (the `ask` tool) is simply absent from its toolset, which is
		// why the chat never showed one. rpc-ui is otherwise a strict superset:
		// same applyRpcDefaultSettingOverrides, plus PI_NO_PTY=1.
		const proc = Bun.spawn([bin, "--mode", "rpc-ui"], {
			cwd: this.#opts.cwd,
			stdin: "pipe",
			stdout: "pipe",
			stderr: "ignore",
			env: { ...process.env, PATH: path, ...this.#opts.env },
		});
		this.#proc = proc;
		void this.#readLoop(proc);
		void proc.exited.then((code) => {
			if (this.#disposed) return;
			log.info("omp-rpc", "child_exit", { code });
			this.#failAllPending("omp rpc child exited");
			// A half-delivered chunk series can never complete now.
			this.#chunks.clear();
			this.#proc = null;
			this.#opts.onExit(code);
		});
	}

	/** Fire-and-correlate: send a command, resolve on its id-matched response. */
	send(command: Record<string, unknown>): Promise<RpcFrame> {
		if (!this.#proc || this.#disposed) return Promise.reject(new Error("omp rpc child not running"));
		const id = String(this.#nextId++);
		const { promise, resolve, reject } = Promise.withResolvers<RpcFrame>();
		const timer = setTimeout(() => {
			this.#pending.delete(id);
			reject(new Error(`omp rpc timeout for ${String(command.type)}`));
		}, RESPONSE_TIMEOUT_MS);
		this.#pending.set(id, { resolve, timer });
		try {
			this.#write({ ...command, id });
		} catch (err) {
			clearTimeout(timer);
			this.#pending.delete(id);
			reject(err instanceof Error ? err : new Error(String(err)));
		}
		return promise;
	}

	/** Fire-and-forget: write one frame with NO id correlation. Extension UI
	 *  responses (and other sub-protocol replies) get no `response` frame back,
	 *  so routing them through send() would just leak a 30 s pending entry. */
	post(frame: Record<string, unknown>): void {
		if (!this.#proc || this.#disposed) throw new Error("omp rpc child not running");
		this.#write(frame);
	}

	#write(frame: Record<string, unknown>): void {
		const stdin = this.#proc?.stdin as { write(s: string): unknown; flush?(): unknown } | undefined;
		if (!stdin) throw new Error("omp rpc child not running");
		stdin.write(`${JSON.stringify(frame)}\n`);
		void stdin.flush?.();
	}

	dispose(): void {
		if (this.#disposed) return;
		this.#disposed = true;
		this.#failAllPending("driver disposed");
		this.#chunks.clear();
		try { this.#proc?.kill(); } catch { /* already dead */ }
		this.#proc = null;
	}

	#failAllPending(reason: string): void {
		for (const [, p] of this.#pending) {
			clearTimeout(p.timer);
			// resolve with a synthetic error response — callers treat success:false uniformly
			p.resolve({ type: "response", success: false, error: reason });
		}
		this.#pending.clear();
	}

	async #readLoop(proc: Subprocess): Promise<void> {
		const dec = new TextDecoder();
		let buf = "";
		try {
			for await (const chunk of proc.stdout as AsyncIterable<Uint8Array>) {
				buf += dec.decode(chunk, { stream: true });
				let nl: number;
				while ((nl = buf.indexOf("\n")) >= 0) {
					const line = buf.slice(0, nl).trim();
					buf = buf.slice(nl + 1);
					if (!line) continue;
					let frame: RpcFrame;
					try { frame = JSON.parse(line) as RpcFrame; }
					catch { log.warn("omp-rpc", "bad_frame", { head: line.slice(0, 80) }); continue; }
					this.#dispatch(frame);
				}
			}
		} catch (err) {
			if (!this.#disposed) log.warn("omp-rpc", "read_loop_error", { error: String(err) });
		}
	}

	#dispatch(frame: RpcFrame): void {
		// Protocol v2: omp splits any response over its 1 MiB frame cap into
		// rpc_chunk frames. Without negotiating + reassembling, the ONLY reply
		// that exceeds the cap — get_available_models, ~1.1 MB — comes back as
		// `success:false, error:"RPC response exceeded the transport limit"`,
		// i.e. the model picker has no catalog at all.
		if (frame.type === "ready") {
			const versions = frame.supportedProtocolVersions;
			if (Array.isArray(versions) && versions.includes(2)) {
				void this.send({ type: "negotiate_protocol", protocolVersion: 2 })
					.catch(() => { /* v1 still works for everything but the catalog */ });
			}
			this.#opts.onEvent(frame);
			return;
		}
		if (frame.type === "rpc_chunk") {
			const whole = this.#collectChunk(frame);
			if (whole) this.#dispatch(whole);
			return;
		}
		const id = typeof frame.id === "string" ? frame.id : null;
		if (frame.type === "response" && id) {
			const p = this.#pending.get(id);
			if (p) {
				this.#pending.delete(id);
				clearTimeout(p.timer);
				p.resolve(frame);
				return;
			}
		}
		this.#opts.onEvent(frame);
	}

	/** Accumulate one rpc_chunk; returns the decoded frame once the last piece
	 *  lands, else null. A malformed or oversized series is dropped whole —
	 *  the caller's 30 s timeout then reports it as a failed command. */
	#collectChunk(frame: RpcFrame): RpcFrame | null {
		const chunkId = typeof frame.chunkId === "string" ? frame.chunkId : "";
		const index = typeof frame.index === "number" ? frame.index : -1;
		const count = typeof frame.count === "number" ? frame.count : 0;
		const data = typeof frame.data === "string" ? frame.data : "";
		if (!chunkId || index < 0 || count <= 0 || index >= count) return null;
		const parts = this.#chunks.get(chunkId) ?? new Array<string>(count).fill("");
		parts[index] = data;
		if (parts.reduce((n, p) => n + p.length, 0) > MAX_REASSEMBLED_BYTES) {
			log.warn("omp-rpc", "chunk_overflow", { chunkId, count });
			this.#chunks.delete(chunkId);
			return null;
		}
		if (parts.some((p) => p === "")) {
			this.#chunks.set(chunkId, parts);
			return null;
		}
		this.#chunks.delete(chunkId);
		try {
			// Each piece is base64'd INDEPENDENTLY, padding and all — concatenating
			// the base64 text and decoding once stops at the first chunk's `=`
			// and silently yields only the first 256 KiB. Decode per piece, then
			// join the bytes.
			const bytes = Buffer.concat(parts.map((p) => Buffer.from(p, "base64")));
			return JSON.parse(bytes.toString("utf8")) as RpcFrame;
		} catch {
			log.warn("omp-rpc", "chunk_decode_failed", { chunkId, count });
			return null;
		}
	}
}
