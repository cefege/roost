// T2.1 — multiplexed keeper protocol. A single keeper process hosts N
// PTYs; each frame carries a channel_id so the worker side can demux.
//
// Frame format:
//   [4-byte BE uint32 total_length] (covers everything after these 4 bytes)
//   [1-byte type tag]
//   [2-byte BE uint16 channel_id]   (0 = control / global)
//   [payload bytes]
//
// Stable, never renumber the type tags.
//
// Split by message family; this module is the entry point and re-exports all
// three:
//   protocol-envelope.ts — frame envelope, spawn frames, shared scalar codecs
//   protocol-io.ts       — hello handshake + PtyIn typed input
//   protocol-terminal.ts — resize control, terminal state, history records
export * from "./protocol-envelope.ts";
export * from "./protocol-io.ts";
export * from "./protocol-terminal.ts";

/** Wire-protocol version. BUMP whenever:
 *  - any existing frame's payload JSON shape changes
 *  - a frame's tag number is reassigned (never do this; add a new tag)
 *  - the encoding of any frame changes (e.g. switching JSON → binary)
 *
 *  A bump makes a running keeper incompatible. Authentication still succeeds
 *  so administrative tooling can report or explicitly shut it down, but the
 *  worker must not dispatch application commands across the mismatch.
 *  Backwards-compatible additive tags may instead be feature-negotiated.
 *
 *  Bump log:
 *    1 — initial Hello/HelloResp handshake (2026-06-18)
 *    2 — authenticated Hello, ShellSpec Spawn, ordered history and typed IO */
export const KEEPER_PROTOCOL_VERSION = 2;
