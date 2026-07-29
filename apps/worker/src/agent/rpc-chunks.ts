// Protocol-v2 frame reassembly for the omp RPC child.
//
// A logical frame larger than maxFrameBytes is split into `rpc_chunk` frames.
// The encoder emits them strictly sequentially, so "interleaved" is simply an
// index/chunkId mismatch. Mirrors omp's own decoder (modes/rpc/rpc-frame.ts):
// same bounds, same fatal-on-violation posture. Concatenation happens on BYTES
// and the UTF-8 decode runs once at the end — chunks split at arbitrary byte
// offsets, so decoding per chunk would corrupt any multi-byte character
// straddling a boundary.

import { isRpcRecord, type RpcFrame } from "./rpc-frame.ts";

// The child announces maxFrameBytes / maxReassembledFrameBytes in its `ready`
// frame; these are the v17.1.7 values. They are compile-time constants on
// purpose — the decoder must mirror the encoder exactly — so the handshake
// exports them to refuse protocol v2 unless the child advertises these same
// numbers. A child chunking at different bounds is never negotiated with.
export const MAX_FRAME_BYTES = 1024 * 1024;
export const MAX_REASSEMBLED_BYTES = 64 * 1024 * 1024;
const CHUNK_PAYLOAD_BYTES = 256 * 1024;

const BASE64_RE = /^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/;

interface PendingChunks {
	chunkId: string;
	count: number;
	byteLength: number;
	nextIndex: number;
	parts: Buffer[];
	received: number;
}

export class ChunkReassembler {
	#pending: PendingChunks | undefined;

	/** Returns the logical frame, or undefined while a sequence is still
	 *  filling. Throws on any violation, having already dropped the sequence. */
	push(frame: RpcFrame): RpcFrame | undefined {
		if (frame.type !== "rpc_chunk") {
			// A non-chunk line arriving mid-sequence means the stream desynced.
			if (this.#pending) {
				this.#pending = undefined;
				throw new Error("rpc chunk sequence interrupted");
			}
			return frame;
		}
		const { chunkId, index, count, byteLength } = frame;
		if (
			typeof chunkId !== "string" ||
			chunkId.length === 0 ||
			chunkId.length > 128 ||
			typeof index !== "number" ||
			typeof count !== "number" ||
			typeof byteLength !== "number" ||
			!Number.isSafeInteger(index) ||
			!Number.isSafeInteger(count) ||
			!Number.isSafeInteger(byteLength) ||
			index < 0 ||
			count < 2 ||
			count > Math.ceil(MAX_REASSEMBLED_BYTES / CHUNK_PAYLOAD_BYTES) ||
			index >= count ||
			// byteLength is the TOTAL utf8 length of the logical frame: anything
			// that fits in one physical frame is never chunked.
			byteLength < MAX_FRAME_BYTES ||
			byteLength > MAX_REASSEMBLED_BYTES
		) {
			this.#pending = undefined;
			throw new Error("invalid rpc chunk metadata");
		}
		const bytes = decodeChunkBase64(frame.data);
		if (bytes.byteLength > CHUNK_PAYLOAD_BYTES) {
			this.#pending = undefined;
			throw new Error("rpc chunk payload exceeds the transport limit");
		}
		if (!this.#pending) {
			if (index !== 0) throw new Error("rpc chunk sequence must start at index 0");
			this.#pending = { chunkId, count, byteLength, nextIndex: 0, parts: [], received: 0 };
		}
		const pending = this.#pending;
		if (
			pending.chunkId !== chunkId ||
			pending.count !== count ||
			pending.byteLength !== byteLength ||
			pending.nextIndex !== index
		) {
			this.#pending = undefined;
			throw new Error("rpc chunk sequence mismatch");
		}
		pending.parts.push(bytes);
		pending.received += bytes.byteLength;
		pending.nextIndex++;
		if (pending.received > pending.byteLength) {
			this.#pending = undefined;
			throw new Error("rpc chunk sequence exceeds declared length");
		}
		if (pending.nextIndex < pending.count) return undefined;
		this.#pending = undefined;
		if (pending.received !== pending.byteLength) throw new Error("rpc chunk sequence length mismatch");
		const text = new TextDecoder("utf-8", { fatal: true }).decode(Buffer.concat(pending.parts));
		const decoded: unknown = JSON.parse(text);
		if (!isRpcRecord(decoded)) throw new Error("rpc frame must be an object");
		return decoded;
	}

	reset(): void {
		this.#pending = undefined;
	}
}

function decodeChunkBase64(data: unknown): Buffer {
	if (typeof data !== "string" || data.length === 0 || !BASE64_RE.test(data))
		throw new Error("invalid rpc chunk data");
	const bytes = Buffer.from(data, "base64");
	// Round-trip guard: Buffer.from is lenient and silently drops garbage.
	if (bytes.toString("base64") !== data) throw new Error("invalid rpc chunk data");
	return bytes;
}
