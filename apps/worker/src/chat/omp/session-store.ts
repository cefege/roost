// Durable roost-sessionId → omp-sessionFile map for native RPC chats.
//
// Why this exists: `entries` in rpc-chat.ts is process memory. A dead CHILD is
// recoverable from it (the entry survives, carrying sessionFile), but a worker
// RESTART loses the map entirely — and nothing else on SessionRecord is
// persisted either (chatTranscriptPath is written at runtime, never stored).
// Without this file the restarted worker opens a FRESH omp session under the
// same chat pane and the conversation is silently gone.
//
// The key is stable across restarts: boot-reconcile.ts resumes each session
// with coord's durable `r.id`, so the same pane keeps the same sessionId.
//
// Storage mirrors transport/client-seq.ts: one small file in the worker data
// dir, atomic write-temp + rename, tolerant reads (a corrupt file degrades to
// "no history", never a boot crash).

import { existsSync, readFileSync, writeFileSync, renameSync, mkdirSync } from "node:fs";
import { join, dirname } from "node:path";
import { homedir } from "node:os";
import { log } from "@roost/shared";

/** Bound the file. Chats are long-lived but finite; oldest mappings lose. */
const MAX_ENTRIES = 256;

function storePath(): string {
	const dataDir = process.env.ROOST_WORKER_DATA_DIR
		?? join(homedir(), "Library", "Application Support", "RoostWorkerV2");
	return join(dataDir, "omp-chat-sessions.json");
}

/** Insertion-ordered sessionId → sessionFile. null until the first access. */
let cache: Map<string, string> | null = null;

function load(): Map<string, string> {
	if (cache) return cache;
	const map = new Map<string, string>();
	const path = storePath();
	try {
		if (existsSync(path)) {
			const raw: unknown = JSON.parse(readFileSync(path, "utf8"));
			if (raw !== null && typeof raw === "object" && !Array.isArray(raw)) {
				for (const [sid, file] of Object.entries(raw as Record<string, unknown>)) {
					// Prune mappings whose omp transcript is gone (session deleted
					// out from under us) — switch_session would just fail later.
					if (typeof file === "string" && file.length > 0 && existsSync(file)) map.set(sid, file);
				}
			}
		}
	} catch (err) {
		log.warn("omp-rpc", "session_store_load_failed", { error: String(err) });
	}
	cache = map;
	return map;
}

function persist(map: Map<string, string>): void {
	// Oldest-first eviction: Map preserves insertion order and every save
	// re-inserts the touched key last (see saveOmpSessionFile).
	while (map.size > MAX_ENTRIES) {
		const oldest = map.keys().next();
		if (oldest.done) break;
		map.delete(oldest.value);
	}
	const path = storePath();
	try {
		mkdirSync(dirname(path), { recursive: true });
		const tmp = `${path}.tmp`;
		writeFileSync(tmp, JSON.stringify(Object.fromEntries(map)));
		renameSync(tmp, path);
	} catch (err) {
		log.warn("omp-rpc", "session_store_save_failed", { error: String(err) });
	}
}

/** The omp session file this chat pane last used, or null if unknown. */
export function loadOmpSessionFile(sessionId: string): string | null {
	return load().get(sessionId) ?? null;
}

/** Remember the omp session file backing this chat pane. No-op when unchanged
 *  — the common case is re-resolving the same path on every child start. */
export function saveOmpSessionFile(sessionId: string, sessionFile: string): void {
	const map = load();
	if (map.get(sessionId) === sessionFile) return;
	map.delete(sessionId);        // re-insert last → newest, for the eviction order
	map.set(sessionId, sessionFile);
	persist(map);
}

/** Drop a pane's mapping. Called when the session closes for good: the
 *  conversation has no pane to resume into. */
export function forgetOmpSession(sessionId: string): void {
	const map = load();
	if (!map.delete(sessionId)) return;
	persist(map);
}

/** Test seam: drop the in-memory cache so the next access re-reads the file. */
export function _resetOmpSessionStoreCache(): void {
	cache = null;
}
