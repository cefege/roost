// omp model catalog reader — context window + friendly name for a model selector.
//
// The transcript records WHICH model a session runs (`anthropic/claude-opus-5`)
// but not how big its context window is, so a raw promptTokens count can't be
// turned into the percentage omp shows. omp keeps that in its OWN catalog at
// ~/.omp/agent/models.db, so Roost reads that rather than maintaining a second
// model table that would rot the day a provider ships a new window.
//
// Schema (verified): model_cache(provider_id TEXT PK, …, models TEXT) where
// `models` is a JSON array of { id, name, contextWindow, … }.
// NOTE: ~/.omp/models.db (no `agent/`) is a 0-byte decoy — not this file.
//
// Every failure path degrades to null: a missing, locked, or reshaped DB must
// cost the caller a percentage, never an exception on the session path.

import { Database } from "bun:sqlite";

const DB_PATH = `${process.env.HOME ?? ""}/.omp/agent/models.db`;

export interface OmpModelInfo {
	name: string;
	contextWindow: number;
}

/** How long a MISS is trusted before re-querying. Non-zero because both the DB
 *  and the row can appear later: the worker is a login-time LaunchAgent, omp
 *  creates and refreshes models.db lazily, and a worker that started first
 *  would otherwise serve "no window, no name" for its entire uptime. Hits are
 *  cached forever — a model's context window does not change under a fixed id. */
const MISS_TTL_MS = 60_000;

// Lazily opened, held for the process: a session emits a frame per transcript
// append, and reopening SQLite per frame would be absurd. A failed open leaves
// this null so the NEXT lookup retries — retries are rate-limited to one per
// MISS_TTL_MS per selector by the miss cache, so a permanently absent DB still
// costs almost nothing.
let db: Database | null = null;
const cache = new Map<string, { info: OmpModelInfo | null; at: number }>();

/** contextWindow + display name for an omp "provider/id" selector, from omp's
 *  own catalog. Null when the model, the provider, or the DB is unknown. */
export function lookupOmpModel(selector: string): OmpModelInfo | null {
	const hit = cache.get(selector);
	if (hit && (hit.info !== null || Date.now() - hit.at < MISS_TTL_MS)) return hit.info;
	const info = query(selector);
	cache.set(selector, { info, at: Date.now() });
	return info;
}

function query(selector: string): OmpModelInfo | null {
	const slash = selector.indexOf("/");
	if (slash <= 0) return null;
	const provider = selector.slice(0, slash);
	const id = selector.slice(slash + 1);
	if (!id) return null;
	try {
		db ??= new Database(DB_PATH, { readonly: true });
		const row = db.query<{ models: string }, [string]>(
			"select models from model_cache where provider_id = ?",
		).get(provider);
		if (!row) return null;
		const models: unknown = JSON.parse(row.models);
		if (!Array.isArray(models)) return null;
		for (const m of models) {
			if (typeof m !== "object" || m === null) continue;
			const rec = m as Record<string, unknown>;
			if (rec.id !== id) continue;
			const w = rec.contextWindow;
			if (typeof w !== "number" || !Number.isFinite(w) || w <= 0) return null;
			return { name: typeof rec.name === "string" ? rec.name : "", contextWindow: w };
		}
		return null;
	} catch {
		// Drop the handle: a DB that appeared, moved, or was rebuilt since the
		// open must be reachable on the next attempt. The miss TTL is what keeps
		// this from re-opening on every frame.
		db = null;
		return null;
	}
}
