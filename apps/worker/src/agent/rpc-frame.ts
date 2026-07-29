// Leaf types for the omp RPC wire. Imports nothing so the frame shape can be
// shared by the transport (rpc-process / rpc-chunks) and the pure projectors
// (entry-projection / history-projection) without a cycle between them.

/** One parsed JSONL frame from the omp child. Deliberately opaque: the wire is
 *  untyped and omp adds frame types across versions, so every consumer narrows
 *  only the fields it actually reads. */
export type RpcFrame = Record<string, unknown>;

/** The single canonical record guard for this package — never re-declare one at
 *  a call site. It proves "object", nothing about the fields. */
export function isRpcRecord(v: unknown): v is RpcFrame {
	return typeof v === "object" && v !== null && !Array.isArray(v);
}
