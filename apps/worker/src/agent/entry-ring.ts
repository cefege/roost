// The worker-side transcript: a drop-oldest ring of AgentEntry plus the
// coalescing upstream flush. Split from agent-controller.ts to stay under the
// 400-line cap.
//
// Upsert-by-seq is the whole contract. An entry is re-emitted under the SAME
// seq every time it grows, so the client can apply frames in any order, twice,
// or after a reconnect, and converge on the same transcript.

import { create, toBinary } from "@bufbuild/protobuf";
import {
	AgentEntriesFrameSchema,
	type AgentEntriesFrame as PbAgentEntriesFrame,
} from "@roost/shared/proto/sync_pb";
import {
	AgentEntrySchema,
	type AgentEntry as PbAgentEntry,
} from "@roost/shared/proto/wire_pb";
import { agentEntryToProto } from "@roost/shared/wire/agent-proto";
import { AGENT_ENTRY_CAPS, type AgentEntry } from "@roost/shared/wire/agent-entry";
import type { SessionId } from "@roost/shared";
import { applyEntryPatch, type EntryPatch } from "./entry-projection.ts";

// A streaming turn emits a text_delta per token; 50 ms of coalescing turns that
// into ~20 whole-entry upserts a second.
const FLUSH_COALESCE_MS = 50;

function varintSize(value: number): number {
	let size = 1;
	while (value >= 128) {
		value = Math.floor(value / 128);
		size++;
	}
	return size;
}

/** Exact contribution of one entry to AgentEntriesFrame.entries: one field
 * tag, the length varint, then the encoded AgentEntry message. */
function entryWireSize(entry: PbAgentEntry): number {
	const bytes = toBinary(AgentEntrySchema, entry).byteLength;
	return 1 + varintSize(bytes) + bytes;
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


	#flush(): void {
		if (this.#dirty.size === 0) return;
		const seqs = [...this.#dirty].sort((a, b) => a - b);
		this.#dirty.clear();
		let batch: PbAgentEntry[] = [];
		const baseBytes = toBinary(
			AgentEntriesFrameSchema,
			create(AgentEntriesFrameSchema, { sessionId: this.#sessionId, entries: [] }),
		).byteLength;
		let bytes = baseBytes;
		for (const seq of seqs) {
			const entry = this.#bySeq.get(seq);
			if (!entry) continue;
			let proto = agentEntryToProto(entry);
			let size = entryWireSize(proto);
			if (baseBytes + size > AGENT_ENTRY_CAPS.framePayload) {
				const notice: AgentEntry = {
					kind: "notice",
					seq: entry.seq,
					ts: entry.ts,
					level: "error",
					text: `${entry.kind} entry omitted: encoded payload exceeded ${AGENT_ENTRY_CAPS.framePayload} bytes`,
					details_json: "",
				};
				const index = this.#entries.indexOf(entry);
				if (index >= 0) this.#entries[index] = notice;
				this.#bySeq.set(seq, notice);
				proto = agentEntryToProto(notice);
				size = entryWireSize(proto);
			}
			if (batch.length > 0 && bytes + size > AGENT_ENTRY_CAPS.framePayload) {
				this.#emit(batch);
				batch = [];
				bytes = baseBytes;
			}
			batch.push(proto);
			bytes += size;
		}
		if (batch.length > 0) this.#emit(batch);
	}

	#emit(entries: PbAgentEntry[]): void {
		this.#send(
			create(AgentEntriesFrameSchema, {
				sessionId: this.#sessionId,
				entries,
			}),
		);
	}
}
