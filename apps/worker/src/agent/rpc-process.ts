// One `omp --mode rpc-ui` child process per agent session, spoken to as
// newline-delimited JSON over stdio.
//
// This module imports NOTHING from any omp package, on purpose. The pinned
// `omp` binary already on each Mac's PATH is the whole contract, so bumping omp
// is not a Roost release; and one-process-per-session is what makes omp's
// process-global singletons (async job manager, agent registry, MCP manager,
// process.chdir) per-session and correct. A crash inside an agent kills one
// child, not the worker and every PTY with it.
//
// The RpcClient omp ships is deliberately unused: its extension-UI listener set
// is private and only populated inside login(), so extension_ui_request frames
// are dropped — and an unanswered approval hangs the agent FOREVER (the approval
// call site passes neither timeout nor signal). We speak raw JSONL instead and
// copy only the frame shapes. Those shapes were captured from a live v17.1.7
// child; see local://omp-rpc-contract.md.

import { log, diag } from "@roost/shared";
import { isRpcRecord, type RpcFrame } from "./rpc-frame.ts";
import { ChunkReassembler, MAX_FRAME_BYTES, MAX_REASSEMBLED_BYTES } from "./rpc-chunks.ts";

const READY_TIMEOUT_MS = 30_000;
const REQUEST_TIMEOUT_MS = 30_000;
// Only diagnostic we get when the child dies unexpectedly, so keep enough to
// hold a stack trace but not enough to matter against the ring.
const STDERR_TAIL_CHARS = 32 * 1024;
// Frames the child emits between spawn and the controller's on() call — the
// resume path in particular chatters before startOmpRpc has even resolved.
// Bounded so a listener that never arrives can't grow without limit.
const PRELISTEN_BUFFER_CAP = 512;

/** A `{success:false}` response. `code` is omp's machine-readable reason
 *  ("session_busy", "stale_cursor", …) — the controller branches on it. */
export class OmpRpcError extends Error {
	readonly code: string | undefined;
	readonly command: string | undefined;
	constructor(message: string, code?: string, command?: string) {
		super(message);
		this.name = "OmpRpcError";
		this.code = code;
		this.command = command;
	}
}

export interface OmpRpcHandle {
	/** Fire-and-forget. Appends the newline; no reply is awaited. */
	send(cmd: RpcFrame): void;
	/** Correlated round-trip. Rejects on `success:false`, timeout, or child exit.
	 *  `T` is the caller's claim about `response.data`; load-bearing fields are
	 *  re-narrowed at the use site rather than trusted from this signature. */
	request<T>(cmd: RpcFrame, timeoutMs?: number): Promise<T>;
	/** Resolves once the child reported `ready` and protocol negotiation
	 *  finished; rejects with the stderr tail when it never got there. Commands
	 *  sent before this settles are queued, not dropped. */
	readonly ready: Promise<void>;
	/** Register an event-frame listener (everything that is not a `response`).
	 *  Frames that arrived before the first registration are replayed in order. */
	on(listener: (frame: RpcFrame) => void): void;
	readonly pid: number;
	kill(): void;
	readonly exited: Promise<number>;
	/** Last STDERR_TAIL_CHARS of the child's stderr. */
	stderrTail(): string;
}


// ─── spawn ───────────────────────────────────────────────────────────────

interface Pending {
	resolve: (value: unknown) => void;
	reject: (err: Error) => void;
	command: string;
	timer: ReturnType<typeof setTimeout>;
}

export interface StartOmpRpcOptions {
	cwd: string;
	/** Absolute omp session .jsonl to `--resume`. */
	resumeFile?: string;
	env?: Record<string, string>;
}

export async function startOmpRpc(opts: StartOmpRpcOptions): Promise<OmpRpcHandle> {
	const bin = Bun.which("omp");
	// Surfaced verbatim to the SPA via rpc-error. Never silently fall back to a
	// shell session — the user asked for an agent.
	if (!bin) throw new Error("omp not found on PATH");

	// `--approval-mode write` is load-bearing: the schema default is `yolo`, so
	// without it NOTHING prompts and the whole approval pipeline is dead code.
	// The resume token must be ONE `=`-joined argument WITH a value — a bare
	// `--resume` opens omp's interactive session picker, which wedges a headless
	// child at startup.
	const cmd = [bin, "--mode", "rpc-ui", "--approval-mode", "write"];
	if (opts.resumeFile) cmd.push(`--resume=${opts.resumeFile}`);

	const proc = Bun.spawn({
		cmd,
		cwd: opts.cwd,
		stdin: "pipe",
		stdout: "pipe",
		stderr: "pipe",
		env: { ...process.env, ...opts.env },
	});

	const listeners: ((frame: RpcFrame) => void)[] = [];
	// The child chatters (notices, session_info_update, and on --resume a whole
	// history replay) from the instant it is spawned, which is before the
	// controller can call on(). Hold those frames instead of dropping them.
	let preListen: RpcFrame[] | null = [];
	const pending = new Map<string, Pending>();
	const reassembler = new ChunkReassembler();
	let nextId = 1;
	let stderrTail = "";
	let dead = false;
	// Resolved by the child's first `ready` frame; consumed by the `ready`
	// handshake promise at the bottom of this function.
	const readyFrame = Promise.withResolvers<RpcFrame>();
	// The rejection is consumed there; without this the exit handler's reject on
	// an already-settled deferred would surface as unhandled.
	readyFrame.promise.catch(() => {});
	// Commands issued before the handshake completes, replayed in order once it
	// does. Keeps negotiate_protocol provably first on the wire.
	let handshakeDone = false;
	const preHandshake: RpcFrame[] = [];

	function failAllPending(err: Error): void {
		for (const p of pending.values()) {
			clearTimeout(p.timer);
			p.reject(err);
		}
		pending.clear();
	}

	function settleResponse(frame: RpcFrame): void {
		const command = typeof frame.command === "string" ? frame.command : undefined;
		let key = typeof frame.id === "string" ? frame.id : undefined;
		if (key === undefined) {
			// An UNKNOWN command comes back with no id at all. Only then may we
			// fall back to a command-name match, oldest first (Map iteration is
			// insertion-ordered). Matching on a known-but-unpending id would let a
			// late reply steal an unrelated in-flight request of the same command,
			// which get_state — sent after every agent_end and prompt change — hits
			// constantly.
			for (const [k, p] of pending) {
				if (p.command === command) {
					key = k;
					break;
				}
			}
		}
		const p = key === undefined ? undefined : pending.get(key);
		// Late reply to an already-timed-out request: nothing to settle.
		if (!p || key === undefined) return;
		pending.delete(key);
		clearTimeout(p.timer);
		if (frame.success === true) {
			p.resolve(frame.data);
			return;
		}
		p.reject(
			new OmpRpcError(
				typeof frame.error === "string" ? frame.error : `omp ${command ?? "command"} failed`,
				typeof frame.code === "string" ? frame.code : undefined,
				command,
			),
		);
	}

	function dispatch(frame: RpcFrame): void {
		if (frame.type === "response") {
			settleResponse(frame);
			return;
		}
		if (frame.type === "ready") {
			readyFrame.resolve(frame);
			return;
		}
		if (preListen) {
			if (preListen.length < PRELISTEN_BUFFER_CAP) preListen.push(frame);
			return;
		}
		for (const l of listeners) {
			try {
				l(frame);
			} catch (err) {
				log.warn("agent-rpc", "listener_threw", {
					pid: proc.pid,
					type: String(frame.type),
					error: err instanceof Error ? err.message : String(err),
				});
			}
		}
	}

	function handleLine(line: string): void {
		let parsed: unknown;
		try {
			parsed = JSON.parse(line);
		} catch {
			// omp never writes non-JSON to stdout, so this is a stray print from a
			// dependency. Drop the line rather than desync the frame stream.
			log.warn("agent-rpc", "stdout_not_json", { pid: proc.pid, len: line.length });
			return;
		}
		if (!isRpcRecord(parsed)) return;
		let frame: RpcFrame | undefined;
		try {
			frame = reassembler.push(parsed);
		} catch (err) {
			// One logical frame is lost; the reassembler already dropped the
			// sequence so the next complete frame parses normally.
			log.warn("agent-rpc", "chunk_reassembly_failed", {
				pid: proc.pid,
				error: err instanceof Error ? err.message : String(err),
			});
			reassembler.reset();
			return;
		}
		if (frame) dispatch(frame);
	}

	void (async () => {
		const decoder = new TextDecoder();
		let buf = "";
		try {
			for await (const chunk of proc.stdout) {
				buf += decoder.decode(chunk, { stream: true });
				let nl = buf.indexOf("\n");
				while (nl >= 0) {
					const line = buf.slice(0, nl);
					buf = buf.slice(nl + 1);
					if (line.length > 0) handleLine(line);
					nl = buf.indexOf("\n");
				}
			}
		} catch (err) {
			log.warn("agent-rpc", "stdout_read_failed", {
				pid: proc.pid,
				error: err instanceof Error ? err.message : String(err),
			});
		}
	})();

	void (async () => {
		const decoder = new TextDecoder();
		try {
			for await (const chunk of proc.stderr) {
				stderrTail += decoder.decode(chunk, { stream: true });
				if (stderrTail.length > STDERR_TAIL_CHARS)
					stderrTail = stderrTail.slice(stderrTail.length - STDERR_TAIL_CHARS);
			}
		} catch {
			// stderr closing early is not itself an error; the exit code is.
		}
	})();

	void proc.exited.then((code) => {
		dead = true;
		readyFrame.reject(new Error(`omp exited before ready (code ${code})`));
		failAllPending(new Error(`omp exited (code ${code})`));
	});

	function write(command: RpcFrame): void {
		try {
			proc.stdin.write(`${JSON.stringify(command)}\n`);
			proc.stdin.flush();
		} catch (err) {
			log.warn("agent-rpc", "stdin_write_failed", {
				pid: proc.pid,
				type: String(command.type),
				error: err instanceof Error ? err.message : String(err),
			});
		}
	}

	function send(command: RpcFrame): void {
		if (dead) return;
		// Hold everything behind the handshake so negotiate_protocol is provably
		// the first command on the wire — a chunk frame arriving un-negotiated is
		// a protocol violation, and the SPA can compose into a session whose child
		// is still booting.
		if (!handshakeDone && command.type !== "negotiate_protocol") {
			preHandshake.push(command);
			return;
		}
		write(command);
	}

	function request<T>(command: RpcFrame, timeoutMs = REQUEST_TIMEOUT_MS): Promise<T> {
		if (dead) return Promise.reject(new Error("omp child is gone"));
		const id = `r${nextId++}`;
		const name = typeof command.type === "string" ? command.type : "unknown";
		const d = Promise.withResolvers<unknown>();
		const timer = setTimeout(() => {
			pending.delete(id);
			d.reject(new Error(`omp ${name} timed out after ${timeoutMs}ms`));
		}, timeoutMs);
		pending.set(id, { resolve: d.resolve, reject: d.reject, command: name, timer });
		send({ ...command, id });
		// Unchecked downcast of the JSON payload: the wire is untyped and only
		// the caller knows which command it issued. Callers re-narrow the fields
		// they actually depend on (see agent-controller's get_state read).
		return d.promise as Promise<T>;
	}

	// The handshake runs OFF the spawn path. Coord's sessionsSpawn deadline is
	// 15s and a cold `omp` takes ~14s to say `ready` (warm: ~3-5s), so blocking
	// the rpc-ok on it would hand the SPA a DeadlineExceeded while the worker
	// still holds a live, invisible child. Callers await `ready` instead; a
	// handshake failure surfaces as a transcript notice, not a failed spawn.
	const ready = (async (): Promise<void> => {
		const readyTimer = setTimeout(
			() => readyFrame.reject(new Error(`omp did not report ready within ${READY_TIMEOUT_MS}ms`)),
			READY_TIMEOUT_MS,
		);
		let frame: RpcFrame;
		try {
			frame = await readyFrame.promise;
		} catch (err) {
			proc.kill();
			const tail = stderrTail.trim();
			throw new Error(
				`${err instanceof Error ? err.message : String(err)}${tail ? `: ${tail.slice(-2000)}` : ""}`,
			);
		} finally {
			clearTimeout(readyTimer);
		}
		// v2 = chunked frames, i.e. a tool result larger than 1 MiB survives
		// instead of being elided by the encoder's shrink passes.
		//
		// The limit equality is load-bearing, and is the same check omp's own
		// client makes before negotiating (modes/rpc/rpc-client.ts
		// supportsRpcProtocolV2). Our reassembler mirrors the encoder's chunking
		// bounds as compile-time constants — deliberately, so the decoder cannot
		// drift from the encoder — which means a child chunking at DIFFERENT
		// bounds would produce sequences we fatally reject. Refusing v2 in that
		// case costs only large-frame fidelity; accepting it would break the
		// session outright the first time a big tool result arrived.
		const versions = frame.supportedProtocolVersions;
		const limitsMatch =
			frame.maxFrameBytes === MAX_FRAME_BYTES &&
			frame.maxReassembledFrameBytes === MAX_REASSEMBLED_BYTES;
		if (Array.isArray(versions) && versions.includes(2) && limitsMatch) {
			try {
				await request({ type: "negotiate_protocol", protocolVersion: 2 }, 10_000);
			} catch (err) {
				// v1 still works, it just caps a logical frame at 1 MiB.
				log.warn("agent-rpc", "negotiate_v2_failed", {
					pid: proc.pid,
					error: err instanceof Error ? err.message : String(err),
				});
			}
		} else if (Array.isArray(versions) && versions.includes(2)) {
			log.warn("agent-rpc", "negotiate_v2_declined_limit_mismatch", {
				pid: proc.pid,
				max_frame_bytes: frame.maxFrameBytes,
				max_reassembled_frame_bytes: frame.maxReassembledFrameBytes,
			});
		}
		handshakeDone = true;
		for (const queued of preHandshake) write(queued);
		preHandshake.length = 0;
		diag("agent.rpc_started", {
			pid: proc.pid,
			cwd: opts.cwd,
			resumed: opts.resumeFile !== undefined,
		});
	})();
	// Owned by the caller via handle.ready; this keeps a rejection from being
	// reported as unhandled before the controller gets to await it.
	ready.catch(() => {});

	return {
		ready,
		send,
		request,
		on: (listener) => {
			listeners.push(listener);
			const buffered = preListen;
			if (buffered) {
				preListen = null;
				for (const f of buffered) dispatch(f);
			}
		},
		pid: proc.pid,
		kill: () => {
			dead = true;
			proc.kill();
		},
		exited: proc.exited,
		stderrTail: () => stderrTail,
	};
}
