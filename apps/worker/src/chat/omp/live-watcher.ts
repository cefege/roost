// Live-bridge sidecar tailer — the streaming half of the omp chat pane.
//
// omp's SessionManager persists a message only once it is COMPLETE, so the
// transcript can never stream. The bridge extension (chat/omp/bridge-extension.js,
// installed into ~/.omp/agent/extensions) writes omp's own live event bus to
// `${OMP_LIVE_DIR}/<sessionId>.ndjson`, one JSON object per line:
//   {"t":"hello","v":1,"sid":…,"sessionFile":…,"cwd":…,"pid":…,"ts":…}
//   {"t":"ev","seq":N,"live":"live-3"?,"e":{…omp event…}}
//   {"t":"bye"}
// This module tails that file with the SAME byte-offset loop as
// transcript-watcher.ts (fs.watch + 1 s poll fallback, carry for partial lines,
// shrink ⇒ reseed) — deliberately one tailing style, not two — and projects
// every event through the shared projector so the streamed rows are worded and
// id'd exactly like the RPC engine's.
//
// The join back to the transcript is omp's OWN persistence key, computed here
// from the streamed message itself (parse.ts::assistantPersistenceKey) and
// again by the tailer from the transcript entry. The sidecar's `entryId` is
// deliberately IGNORED: omp's `message_end` fires BEFORE the entry is appended,
// so `getLeafId()` names the previous leaf (verified against a live omp 17.1.3:
// a title_change, a toolResult, a developer reminder). Keying on it would miss
// the streamed row and rewrite an innocent one. The caller records key → live-N
// and the tailer rewrites the transcript copy's id, so the canonical row
// REPLACES the streamed one — one row, no delete verb.
//
// Every failure path degrades silently: a missing/garbage sidecar means the pane
// falls back to transcript-only behaviour, exactly as before the bridge existed.

import { watch, type FSWatcher } from "node:fs";
import { open, stat } from "node:fs/promises";
import { diag } from "@roost/shared";
import type { ChatMessage } from "@roost/shared/chat/wire";
import { newProjectState, projectEvent, resetProjectState } from "./event-project.ts";
import type { WatcherHandle } from "./transcript-watcher.ts";
import { assistantPersistenceKey } from "./parse.ts";

const POLL_FALLBACK_MS = 1000;

/** A turn whose bridge has written NOTHING for this long is gone (omp killed,
 *  extension threw, machine slept). Longer than any plausible inter-token gap —
 *  a model can stall for seconds mid-stream, and tool calls are silent for as
 *  long as the tool runs — short enough that the frozen partial does not
 *  outlive the transcript's canonical copy of the same turn. */
const STALL_MS = 30_000;

/** Everything one sidecar line can tell the session layer. One callback, one
 *  discriminated event: the caller has to react to all six and a callbacks
 *  object would let it silently forget one. */
export type LiveEvent =
	/** The bridge attached (omp booted / switched session in this pane). */
	| { kind: "hello"; sessionFile: string; pid: number }
	/** Streamed row — upsert BY ID, it will be re-emitted as the turn grows. */
	| { kind: "message"; msg: ChatMessage }
	/** Authoritative turn state (agent_start/agent_end), beating the OSC title. */
	| { kind: "streaming"; value: boolean }
	/** The message with omp persistence key `key` is already on screen as `liveId`. */
	| { kind: "join"; key: string; liveId: string }
	/** A row that rendered mid-turn ended as something the TUI paints as
	 *  NOTHING (a silent abort). Remove it — the terminal shows silence. */
	| { kind: "retract"; liveId: string }
	/** The bridge stopped. `liveId` = a row it left mid-stream (never reached
	 *  message_end) that must be dropped, or null when the turn was complete. */
	| { kind: "abort"; liveId: string | null };

/** Tail a bridge sidecar, projecting each event to the caller. The file need
 *  not exist yet — omp may not have started — the poll loop picks it up when it
 *  appears, and a truncation (a second omp in the same pane) reseeds from 0. */
export function startLiveWatcher(path: string, onEmit: (ev: LiveEvent) => void): WatcherHandle {
	let disposed = false;
	let watcher: FSWatcher | null = null;
	let pollTimer: ReturnType<typeof setInterval> | null = null;
	let offset = 0;
	let carry = "";              // partial line across reads
	let streaming = false;       // last value published to the caller
	let lastLineMs = Date.now(); // staleness clock for the mid-turn death check
	// Ids stay monotonic across resets: a second omp must not re-mint ids whose
	// rows the first one's turns still occupy in the record.
	const proj = newProjectState("live");

	/** The turn the bridge left unfinished, if any. */
	const abort = (): void => {
		const liveId = proj.curMsgId;
		resetProjectState(proj);
		streaming = false;
		onEmit({ kind: "abort", liveId });
	};

	const handleLine = (line: string): void => {
		let raw: unknown;
		try { raw = JSON.parse(line); }
		catch { diag("chat.live_skip", { reason: "json", len: line.length }); return; }
		if (raw === null || typeof raw !== "object" || !("t" in raw)) {
			diag("chat.live_skip", { reason: "shape", len: line.length });
			return;
		}
		lastLineMs = Date.now();
		if (raw.t === "hello") {
			// A hello mid-turn means the previous omp died without a bye; its
			// half-streamed row can never complete.
			if (proj.curMsgId !== null) abort();
			resetProjectState(proj);
			onEmit({
				kind: "hello",
				sessionFile: "sessionFile" in raw && typeof raw.sessionFile === "string" ? raw.sessionFile : "",
				pid: "pid" in raw && typeof raw.pid === "number" ? raw.pid : 0,
			});
			return;
		}
		if (raw.t === "bye") { abort(); return; }
		if (raw.t !== "ev" || !("e" in raw)) {
			diag("chat.live_skip", { reason: "kind" });
			return;
		}
		const e = raw.e;
		if (e === null || typeof e !== "object" || Array.isArray(e)) {
			diag("chat.live_skip", { reason: "event" });
			return;
		}
		// Sidecar events are opaque omp objects — the projector reads every field
		// through its own typeof guards, so an index-signature view IS the contract.
		const ompEvent = e as Record<string, unknown>;
		const out = projectEvent(proj, ompEvent);
		if (!out) return;
		if (out.kind === "streaming") {
			streaming = out.value;
			onEmit(out);
			return;
		}
		if (out.kind === "drop") {
			// The turn rendered mid-flight but ends as nothing the TUI would
			// paint (a silent abort: `message_start` carries "Request was
			// aborted", `message_end` the silent marker). Retract the row rather
			// than leave a red line where the terminal shows silence.
			onEmit({ kind: "retract", liveId: out.id });
			return;
		}
		// message / tool / narrate all carry one upsertable row. The bridge
		// already coalesced its writes (60 ms trailing, newest-wins), so
		// `coalesce` needs no second timer here — emitting straight through is
		// what makes the pane grow in step with the terminal.
		onEmit({ kind: "message", msg: out.msg });
		// Only a completed assistant turn can be joined to a transcript entry.
		// Computed from the message, NOT from the sidecar's `entryId` — see the
		// header: that field names the wrong entry.
		if (ompEvent.type === "message_end") {
			const key = assistantPersistenceKey(ompEvent.message);
			if (key) onEmit({ kind: "join", key, liveId: out.msg.id });
		}
	};

	const parseSlice = (chunk: string): void => {
		carry += chunk;
		const lines = carry.split("\n");
		carry = lines.pop() ?? "";   // last element is the partial (no trailing \n)
		for (const line of lines) {
			if (line.length === 0) continue;
			handleLine(line);
		}
	};

	const readFrom = async (): Promise<void> => {
		if (disposed) return;
		let st;
		try { st = await stat(path); } catch { return; }   // no sidecar yet — poll covers it
		if (st.size < offset) {
			// The bridge reopens with "w" on every session_start, so a shrink is a
			// NEW omp in this pane: drop the partial line and re-read from 0.
			offset = 0; carry = "";
		}
		if (st.size === offset) return;                     // nothing new
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

	const tick = async (): Promise<void> => {
		await readFrom();
		// Bridge death mid-turn: nothing written for STALL_MS while a row is
		// still growing. Without this the transcript's later copy of the turn
		// shows up as a SECOND row beside the frozen partial.
		if (!disposed && streaming && proj.curMsgId !== null && Date.now() - lastLineMs > STALL_MS) {
			diag("chat.live_stall", { path });
			abort();
		}
	};

	const init = async (): Promise<void> => {
		await readFrom();
		// fs.watch + 1s poll fallback (macOS reports EINVAL / drops on some FSes,
		// and the file usually does not exist when the session starts).
		try {
			watcher = watch(path, () => { void readFrom(); });
			watcher.on("error", () => { /* poll covers it */ });
		} catch {
			watcher = null;
		}
		pollTimer = setInterval(() => { void tick(); }, POLL_FALLBACK_MS);
	};

	void init();

	return {
		path,
		dispose: () => {
			disposed = true;
			try { watcher?.close(); } catch { /* best-effort */ }
			if (pollTimer !== null) { clearInterval(pollTimer); pollTimer = null; }
			watcher = null;
		},
	};
}
