// Omp transcript watcher — resolve the per-session JSONL path and tail it.
//
// RESOLUTION (the hard part under Roost's mux-keeper):
//   omp under the keeper runs on a Bun-allocated PTY whose slave isn't a named
//   /dev/ttysNNN omp recognizes, so omp does NOT write ~/.omp/agent/terminal-
//   sessions/<tty>. Bun's PTY spawn also detaches omp from the keeper's ppid
//   tree, so descendant walks from childPid can't reach it. The tty file AND
//   the process tree are therefore dead ends.
//   Primary resolver: a single global `lsof -c bun` for the .jsonl a live omp
//   holds OPEN (omp keeps its transcript open for the whole session; idle/
//   exited sessions' files are closed), filtered by the session cwd — exact and
//   process-model-independent. Fallback: cwd + active-growth (mtime recency),
//   single-match only (refuse ambiguous — never show the wrong conversation).
//
// TAILING: byte-offset tail, split on \n, parseOmpLine each line, monotonic seq.
// fs.watch + 1s poll fallback (macOS drops). Emits onAppend(messages, seq).
// All failure paths degrade silently — never throws into the session path.

import { watch, type FSWatcher } from "node:fs";
import { open, readdir, stat } from "node:fs/promises";
import type { Dirent } from "node:fs";
import { join } from "node:path";
import { diag } from "@roost/shared";
import { parseOmpLine } from "./parse.ts";
import type { ChatMessage } from "@roost/shared/chat/wire";

const HOME = process.env.HOME ?? "";
const SESSIONS_DIR = `${HOME}/.omp/agent/sessions`;
const TOOL_PATH = `/opt/homebrew/bin:/usr/local/bin:/usr/sbin:/usr/bin:/bin:${process.env.PATH ?? ""}`;
const POLL_FALLBACK_MS = 1000;

/** Named handle returned by startTranscriptWatcher. */
export interface WatcherHandle {
	dispose: () => void;
	path: string | null;
}

/** Result of resolving the transcript path. */
export interface ResolveResult {
	path: string;
	via: "lsof" | "fallback";
}

async function runPs(cmd: string[]): Promise<string | null> {
	try {
		const proc = Bun.spawn(cmd, { stdout: "pipe", stderr: "ignore", env: { ...process.env, PATH: TOOL_PATH } });
		const out = await new Response(proc.stdout).text();
		await proc.exited;
		return proc.exitCode === 0 ? out.trim() : null;
	} catch { return null; }
}

function sleep(ms: number): Promise<void> {
	const { promise, resolve } = Promise.withResolvers<void>();
	setTimeout(resolve, ms);
	return promise;
}

/** Read a transcript's session cwd. omp transcripts start with a `title` line,
 *  not the `session` line, so we scan the head for `type:"session"` (carries cwd). */
async function readSessionCwd(path: string): Promise<string | null> {
	try {
		const fh = await open(path, "r");
		const buf = Buffer.alloc(4096);
		const { bytesRead } = await fh.read(buf, 0, 4096, 0);
		await fh.close();
		const head = buf.subarray(0, bytesRead).toString("utf8");
		for (const line of head.split("\n")) {
			if (line.length === 0) continue;
			const s: unknown = JSON.parse(line);
			if (s !== null && typeof s === "object" && "type" in s && s.type === "session" && "cwd" in s && typeof s.cwd === "string") {
				return s.cwd;
			}
		}
	} catch { /* ill-formed — skip */ }
	return null;
}

/** PRIMARY resolver: find the omp transcript a live process holds OPEN whose
 *  session.cwd matches. omp keeps its transcript open for the whole session, so
 *  OPEN = live conversation; idle/exited sessions' files are closed. One global
 *  lsof, cwd-filtered, unambiguous-only. Returns null on 0 or >1 matches. */
async function resolveByOpenTranscript(expectedCwd: string): Promise<string | null> {
	// omp runs under `bun`; one lsof lists every bun process's open files.
	const out = await runPs(["lsof", "-c", "bun", "-Fn"]);
	if (!out) return null;
	const open: Set<string> = new Set();
	for (const line of out.split("\n")) {
		if (!line.startsWith("n")) continue;
		const p = line.slice(1);
		if (p.includes("/.omp/agent/sessions/") && p.endsWith(".jsonl") && !p.endsWith("__advisor.jsonl")) {
			open.add(p);
		}
	}
	if (open.size === 0) return null;
	const matches: string[] = [];
	for (const p of open) {
		const cwd = await readSessionCwd(p);
		if (cwd === expectedCwd) matches.push(p);
	}
	if (matches.length !== 1) return null;   // 0 = none for this cwd; >1 = ambiguous
	return matches[0];
}

/** FALLBACK resolver: scan the sessions dir for transcripts whose session.cwd
 *  matches AND were modified within `activeWindowMs` (actively growing = the
 *  live session). Returns the single match, or null on 0 or >1 (ambiguous). */
async function findActiveTranscriptByCwd(expectedCwd: string, activeWindowMs: number): Promise<string | null> {
	let dirs: Dirent[];
	try { dirs = await readdir(SESSIONS_DIR, { withFileTypes: true }); }
	catch { return null; }
	const now = Date.now();
	const candidates: { path: string; mtime: number }[] = [];
	for (const d of dirs) {
		if (!d.isDirectory()) continue;
		const dirPath = join(SESSIONS_DIR, d.name);
		let entries: string[];
		try { entries = await readdir(dirPath); } catch { continue; }
		for (const name of entries) {
			if (!name.endsWith(".jsonl")) continue;
			const p = join(dirPath, name);
			try {
				const st = await stat(p);
				if (now - st.mtimeMs > activeWindowMs) continue;   // not actively growing
				const cwd = await readSessionCwd(p);
				if (cwd === expectedCwd) candidates.push({ path: p, mtime: st.mtimeMs });
			} catch { /* ill-formed — skip */ }
		}
	}
	if (candidates.length !== 1) return null;   // 0 = not yet active; >1 = ambiguous
	candidates.sort((a, b) => b.mtime - a.mtime);
	return candidates[0].path;
}

/** Resolve the omp transcript path for a session.
 *  Primary: global lsof by cwd (exact). Fallback: cwd + active-growth. Both
 *  refuse ambiguous matches (never show the wrong conversation). Polled so a
 *  freshly-spawned omp is caught once it opens its transcript. */
export async function resolveTranscriptPath(
	_childPid: number | null | undefined,
	cwd: string,
	opts: { pollMs?: number; timeoutMs?: number; activeWindowMs?: number } = {},
): Promise<ResolveResult | null> {
	const pollMs = opts.pollMs ?? 500;
	const timeoutMs = opts.timeoutMs ?? 8_000;
	const activeWindowMs = opts.activeWindowMs ?? 60_000;
	const deadline = Date.now() + timeoutMs;

	// Primary: global lsof by cwd.
	while (Date.now() < deadline) {
		const p = await resolveByOpenTranscript(cwd);
		if (p) return { path: p, via: "lsof" };
		await sleep(pollMs);
	}

	// Fallback: cwd + active-growth (unambiguous single match only).
	const fbDeadline = Date.now() + Math.min(timeoutMs, 4_000);
	while (Date.now() < fbDeadline) {
		const p = await findActiveTranscriptByCwd(cwd, activeWindowMs);
		if (p) return { path: p, via: "fallback" };
		await sleep(pollMs);
	}
	return null;
}

/** Tail a transcript file, invoking onAppend for each newly-parsed ChatMessage.
 *  `seq` is the 1-based line index of the LAST message in the batch (monotonic).
 *  Reads from offset 0 on first open (full reparse), then byte-offset tails.
 *  Handles rotation/truncation by re-reading from 0 if size shrinks. */
export function startTranscriptWatcher(
	path: string,
	onAppend: (messages: ChatMessage[], seq: number) => void,
): WatcherHandle {
	let disposed = false;
	let watcher: FSWatcher | null = null;
	let pollTimer: ReturnType<typeof setInterval> | null = null;
	let offset = 0;
	let lineCount = 0;       // monotonic seq = total parsed lines so far
	let carry = "";          // partial line across reads

	const parseSlice = (chunk: string): void => {
		carry += chunk;
		const lines = carry.split("\n");
		carry = lines.pop() ?? "";   // last element is the partial (no trailing \n)
		const batch: ChatMessage[] = [];
		for (const line of lines) {
			if (line.length === 0) continue;
			lineCount++;
			const msg = parseOmpLine(line);
			if (msg) batch.push(msg);
		}
		if (batch.length > 0) {
			diag("chat.frame_emit", { count: batch.length, seq: lineCount });
			onAppend(batch, lineCount);
		}
	};

	const readFrom = async (): Promise<void> => {
		if (disposed) return;
		let st;
		try { st = await stat(path); } catch { return; }   // file gone — poll covers recreate
		if (st.size < offset) {
			// truncated / rotated — reseed from 0.
			offset = 0; lineCount = 0; carry = "";
		}
		if (st.size === offset) return;                   // nothing new
		try {
			const fh = await open(path, "r");
			const buf = Buffer.alloc(st.size - offset);
			await fh.read(buf, 0, buf.length, offset);
			await fh.close();
			offset = st.size;
			parseSlice(buf.toString("utf8"));
		} catch {
			// file vanished mid-read — next poll retries.
		}
	};

	const init = async (): Promise<void> => {
		await readFrom();
		// fs.watch + 1s poll fallback (macOS reports EINVAL / drops on some FSes).
		try {
			watcher = watch(path, () => { void readFrom(); });
			watcher.on("error", () => { /* poll covers it */ });
		} catch {
			watcher = null;
		}
		pollTimer = setInterval(() => { void readFrom(); }, POLL_FALLBACK_MS);
	};

	void init();

	return {
		path,
		dispose: () => {
			disposed = true;
			try { watcher?.close(); } catch { /* best-effort */ }
			if (pollTimer) clearInterval(pollTimer);
			pollTimer = null;
			watcher = null;
		},
	};
}
