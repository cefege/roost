// The worker-side transcript: a drop-oldest ring of AgentEntry plus the
// coalescing upstream flush. Split from agent-controller.ts to stay under the
// 400-line cap.
//
// Upsert-by-seq is the whole contract. An entry is re-emitted under the SAME
// seq every time it grows, so the client can apply frames in any order, twice,
// or after a reconnect, and converge on the same transcript.

import { create } from "@bufbuild/protobuf";
import {
	AgentEntriesFrameSchema,
	type AgentEntriesFrame as PbAgentEntriesFrame,
} from "@roost/shared/proto/sync_pb";
import { agentEntryToProto } from "@roost/shared/wire/agent-proto";
import { AGENT_ENTRY_CAPS, type AgentEntry } from "@roost/shared/wire/agent-entry";
import type { SessionId } from "@roost/shared";
import { applyEntryPatch, type EntryPatch } from "./entry-projection.ts";

// A streaming turn emits a text_delta per token; 50 ms of coalescing turns that
// into ~20 whole-entry upserts a second.
const FLUSH_COALESCE_MS = 50;
const ENTRIES_PAGE_LIMIT = 128;

export interface AgentEntriesPage {
	entries: AgentEntry[];
	first_seq: number;
	more: boolean;
}

/** Rough wire cost of one entry, used only to decide where to split a batch.
 *  Over-counting is free; under-counting would push a frame past the cap. */
function entrySize(entry: AgentEntry): number {
	switch (entry.kind) {
		case "tool":
			return (
				128 + entry.tool_call_id.length + entry.name.length + entry.intent.length +
				entry.args_json.length + entry.text.length + entry.details_json.length
			);
		case "prompt": {
			let n = 128 + entry.prompt_id.length + entry.title.length + entry.answer.length;
			for (const option of entry.options) n += option.length + 4;
			return n;
		}
		default:
			return 64 + entry.text.length;
	}
}

export class AgentEntryRing {
	readonly #sessionId: SessionId;
	readonly #send: (frame: PbAgentEntriesFrame) => void;
	// Ascending by seq. #bySeq holds the SAME objects, so an in-place patch is
	// visible through both.
	#entries: AgentEntry[] = [];
	#bySeq = new Map<number, AgentEntry>();
	#dirty = new Set<number>();
	#timer: ReturnType<typeof setTimeout> | null = null;

	constructor(sessionId: SessionId, send: (frame: PbAgentEntriesFrame) => void) {
		this.#sessionId = sessionId;
		this.#send = send;
	}

	get(seq: number): AgentEntry | undefined {
		return this.#bySeq.get(seq);
	}

	append(entry: AgentEntry): void {
		this.#entries.push(entry);
		this.#bySeq.set(entry.seq, entry);
		while (this.#entries.length > AGENT_ENTRY_CAPS.ringEntries) {
			const dropped = this.#entries.shift();
			if (!dropped) break;
			this.#bySeq.delete(dropped.seq);
			this.#dirty.delete(dropped.seq);
		}
		this.markDirty(entry.seq);
	}

	/** Fold a patch into a live entry. Returns the patched entry, or undefined
	 *  when the ring already evicted it (a very long turn can outlive its own
	 *  opening entry). */
	patch(seq: number, patch: EntryPatch): AgentEntry | undefined {
		const entry = this.#bySeq.get(seq);
		if (!entry) return undefined;
		applyEntryPatch(entry, patch);
		this.markDirty(seq);
		return entry;
	}

	markDirty(seq: number): void {
		this.#dirty.add(seq);
		if (this.#timer !== null) return;
		this.#timer = setTimeout(() => {
			this.#timer = null;
			this.#flush();
		}, FLUSH_COALESCE_MS);
	}

	/** Flush immediately, skipping the coalesce window — on teardown the next
	 *  timer tick may never come. */
	flushNow(): void {
		if (this.#timer !== null) {
			clearTimeout(this.#timer);
			this.#timer = null;
		}
		this.#flush();
	}

	/** One page of history, newest-last. `beforeSeq === 0` means the newest
	 *  page; otherwise the page ending just before that seq. */
	page(beforeSeq: number): AgentEntriesPage {
		const all = this.#entries;
		if (all.length === 0) return { entries: [], first_seq: 0, more: false };
		let end = all.length;
		if (beforeSeq > 0) {
			end = all.findIndex((e) => e.seq >= beforeSeq);
			if (end < 0) end = all.length;
		}
		const start = Math.max(0, end - ENTRIES_PAGE_LIMIT);
		const entries = all.slice(start, end);
		return { entries, first_seq: entries[0]?.seq ?? 0, more: start > 0 };
	}

	#flush(): void {
		if (this.#dirty.size === 0) return;
		const seqs = [...this.#dirty].sort((a, b) => a - b);
		this.#dirty.clear();
		let batch: AgentEntry[] = [];
		let bytes = 0;
		for (const seq of seqs) {
			const entry = this.#bySeq.get(seq);
			if (!entry) continue;
			const size = entrySize(entry);
			if (batch.length > 0 && bytes + size > AGENT_ENTRY_CAPS.framePayload) {
				this.#emit(batch);
				batch = [];
				bytes = 0;
			}
			batch.push(entry);
			bytes += size;
		}
		if (batch.length > 0) this.#emit(batch);
	}

	#emit(entries: AgentEntry[]): void {
		this.#send(
			create(AgentEntriesFrameSchema, {
				sessionId: this.#sessionId,
				entries: entries.map(agentEntryToProto),
			}),
		);
	}
}
