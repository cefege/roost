// Discover omp transcripts on this machine so a chat can be RESUMED by path.
//
// This is what replaces the mirror engine: the reason to attach Roost to a
// terminal's omp was "I started in a terminal, continue on my phone". Reading
// the transcript once at spawn buys the same thing with none of the coupling —
// no sidecar, no keystroke synthesis, no live shared state. The chosen path is
// handed to a fresh `omp --mode rpc-ui` as `--session FILE`.
//
// Resolution order is ported from Paseo's providers/omp/session-descriptor.ts,
// which is the tested version of this:
//   1. OMP_SESSION_DIR, else `<cwd>/.omp/settings.json`.sessionDir (cwd wins),
//      else `<agentDir>/settings.json`.sessionDir, else `<agentDir>/sessions`.
//   2. agentDir = OMP_AGENT_DIR, else ~/.omp/agent.
//
// Reads are DELIBERATELY partial: transcripts run to hundreds of megabytes, and
// a full read of the newest 50 would stall the worker for seconds. The header
// lives in the first bytes and the newest prompt in the last, so that is all we
// read.

import { existsSync, readFileSync, statSync, readdirSync, openSync, readSync, closeSync } from "node:fs";
import type { Dirent } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { log } from "@roost/shared";

/** Head slice: the `{"type":"session"}` header is the first line. */
const HEAD_BYTES = 64 * 1024;
/** Tail slice: enough for the newest few turns, nowhere near the whole file. */
const TAIL_BYTES = 256 * 1024;
/** Title fallback cap — one sidebar line, not a paragraph. */
const TITLE_CAP = 160;
/** Depth cap on the recursive walk. omp nests one level (per-cwd dirs); three
 *  is slack, and it stops a symlink loop from becoming a hang. */
const MAX_DEPTH = 3;
/** Files considered at all, before header validation. */
const MAX_CANDIDATES = 200;

/** A transcript whose mtime is younger than this is probably being written by a
 *  live omp. Two processes writing one session file corrupt it, and Roost
 *  cannot know whether a foreign omp holds it — so resuming one is refused.
 *  Far above omp's inter-write gap, far below a genuinely abandoned session. */
export const ACTIVE_WINDOW_MS = 60_000;

export interface OmpSessionEntry {
	path: string;
	cwd: string;
	title: string;
	updatedAt: number;
	lastPrompt: string;
	active: boolean;
}

/** `<agentDir>` — where omp keeps settings and, by default, sessions. */
function agentDir(): string {
	return process.env.OMP_AGENT_DIR || join(homedir(), ".omp", "agent");
}

function settingsSessionDir(dir: string): string | null {
	const file = join(dir, "settings.json");
	if (!existsSync(file)) return null;
	try {
		const parsed: unknown = JSON.parse(readFileSync(file, "utf8"));
		if (typeof parsed === "object" && parsed !== null && "sessionDir" in parsed) {
			const v = parsed.sessionDir;
			if (typeof v === "string" && v) return v;
		}
	} catch { /* malformed settings must not break discovery */ }
	return null;
}

/** Where omp writes session JSONL. `cwd` lets a project-local `.omp/settings.json`
 *  win, exactly as omp itself resolves it. */
export function resolveSessionDir(cwd?: string): string {
	const env = process.env.OMP_SESSION_DIR;
	if (env) return env;
	const local = cwd ? settingsSessionDir(join(cwd, ".omp")) : null;
	if (local) return local;
	const global = settingsSessionDir(agentDir());
	if (global) return global;
	return join(agentDir(), "sessions");
}

/** Every *.jsonl under `dir`, newest mtime first, capped. */
function candidates(dir: string): { path: string; mtimeMs: number }[] {
	const out: { path: string; mtimeMs: number }[] = [];
	const walk = (d: string, depth: number): void => {
		if (depth > MAX_DEPTH || out.length >= MAX_CANDIDATES * 4) return;
		let entries: Dirent[];
		try { entries = readdirSync(d, { withFileTypes: true }); }
		catch { return; }
		for (const e of entries) {
			const p = join(d, e.name);
			// Not followed as directories: a symlinked tree would double-count and
			// can cycle. isDirectory() is false for a symlink with withFileTypes.
			if (e.isDirectory()) { walk(p, depth + 1); continue; }
			if (!e.isFile() || !e.name.endsWith(".jsonl")) continue;
			try { out.push({ path: p, mtimeMs: statSync(p).mtimeMs }); }
			catch { /* raced with a delete */ }
		}
	};
	walk(dir, 0);
	out.sort((a, b) => b.mtimeMs - a.mtimeMs);
	return out.slice(0, MAX_CANDIDATES);
}

/** Read at most `n` bytes at `offset`. Avoids pulling a 300 MB transcript into
 *  memory to answer "what is this conversation called". */
function readSlice(path: string, offset: number, n: number): string {
	if (n <= 0) return "";
	const fd = openSync(path, "r");
	try {
		const buf = Buffer.allocUnsafe(n);
		const read = readSync(fd, buf, 0, n, offset);
		return buf.subarray(0, read).toString("utf8");
	} finally {
		closeSync(fd);
	}
}

/** A file-stem that is a timestamp or a hex id is not a title. */
function isJunkTitle(s: string): boolean {
	return /^\d{4}-\d{2}-\d{2}T\d{2}[-:]\d{2}[-:]\d{2}/.test(s) || /^[0-9a-f]{8,}$/.test(s);
}

function clean(s: string): string {
	return s.replace(/\s+/g, " ").trim().slice(0, TITLE_CAP);
}

/** First user-message text in a slice of JSONL, or "". */
function firstUserText(lines: string[]): string {
	for (const line of lines) {
		if (!line.includes('"user"')) continue;
		let raw: unknown;
		try { raw = JSON.parse(line); } catch { continue; }
		if (typeof raw !== "object" || raw === null) continue;
		const rec = raw as Record<string, unknown>;
		if (rec.role !== "user" && rec.type !== "user") continue;
		const content = rec.content ?? rec.text;
		if (typeof content === "string" && content.trim()) return clean(content);
		if (Array.isArray(content)) {
			for (const part of content) {
				if (typeof part === "object" && part !== null && "text" in part && typeof part.text === "string" && part.text.trim()) {
					return clean(part.text);
				}
			}
		}
	}
	return "";
}

/** Parse one transcript's summary, or null when it is not an omp session file.
 *  A missing `{"type":"session"}` header is the reject: it is the only positive
 *  proof that this JSONL is omp's and not some other tool's. */
export function describeSession(path: string, mtimeMs: number, now = Date.now()): OmpSessionEntry | null {
	let head: string;
	let size: number;
	try {
		size = statSync(path).size;
		head = readSlice(path, 0, Math.min(HEAD_BYTES, size));
	} catch { return null; }
	const headLines = head.split("\n").filter((l) => l.length > 0);

	let cwd = "";
	let title = "";
	let seenHeader = false;
	for (const line of headLines) {
		let raw: unknown;
		try { raw = JSON.parse(line); } catch { continue; }
		if (typeof raw !== "object" || raw === null) continue;
		const rec = raw as Record<string, unknown>;
		if (rec.type === "session") {
			seenHeader = true;
			if (typeof rec.cwd === "string") cwd = rec.cwd;
		}
		if (rec.type === "session_info" && typeof rec.name === "string" && rec.name.trim()) title = clean(rec.name);
		if (rec.type === "title" && typeof rec.title === "string" && rec.title.trim()) title = clean(rec.title);
	}
	if (!seenHeader) return null;

	// Tail read only when the file is bigger than the head we already have —
	// otherwise headLines IS the whole file.
	const tailOffset = Math.max(HEAD_BYTES, size - TAIL_BYTES);
	const tailLines = size > HEAD_BYTES
		// The first line of a mid-file slice is a fragment; drop it.
		? readSlice(path, tailOffset, size - tailOffset).split("\n").slice(1).filter((l) => l.length > 0)
		: headLines;

	if (!title || isJunkTitle(title)) title = firstUserText(headLines);
	// Newest prompt: scan the tail backwards by reusing the same extractor on
	// the reversed lines.
	const lastPrompt = firstUserText([...tailLines].reverse());
	if (!title) title = lastPrompt;

	return {
		path,
		cwd,
		title,
		updatedAt: Math.round(mtimeMs),
		lastPrompt,
		active: now - mtimeMs < ACTIVE_WINDOW_MS,
	};
}

/** Resumable omp transcripts, newest first. Never throws: a missing session dir
 *  is an empty list, not an error — plenty of machines have never run omp. */
export function listOmpSessions(limit = 50, cwd?: string): OmpSessionEntry[] {
	const dir = resolveSessionDir(cwd);
	if (!existsSync(dir)) return [];
	const now = Date.now();
	const out: OmpSessionEntry[] = [];
	for (const c of candidates(dir)) {
		if (out.length >= limit) break;
		try {
			const entry = describeSession(c.path, c.mtimeMs, now);
			if (entry) out.push(entry);
		} catch (err) {
			log.warn("omp-sessions", "describe_failed", { path: c.path, error: String(err) });
		}
	}
	return out;
}

/** Refuse a resume that would put a SECOND writer on a live transcript. Two omp
 *  processes appending to one session file corrupt it, and there is no lock to
 *  check — recent mtime is the only evidence available. Returns the refusal
 *  message, or null when the path is safe to hand to `--session`. */
export function resumeBlockedReason(path: string, now = Date.now()): string | null {
	let mtimeMs: number;
	try { mtimeMs = statSync(path).mtimeMs; }
	catch { return null; }   // missing file degrades to a fresh conversation, not a refusal
	if (now - mtimeMs >= ACTIVE_WINDOW_MS) return null;
	return "that omp session looks active — quit it in the terminal first";
}
