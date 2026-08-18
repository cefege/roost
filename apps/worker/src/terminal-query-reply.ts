// Terminal capability-probe tokenizer and the single ordered reply lane.
//
// One pty chunk can carry several probes and the application reads every reply
// off the same stdin, so the bytes must reach the pty in the order the probes
// appeared in the stream. Two sources feed that one lane:
//
//   native     probes @wterm/core answers itself (DSR cursor report, ESC[6n).
//              0.3.4 queues them and getResponse() pops ONE, so the lane drains
//              until null — after every internal <=8192-byte write chunk via the
//              `afterChunk` hook, and again after the write returns. Reading
//              once leaves a reply queued to surface against a LATER chunk's
//              probes, which is how a CPR ends up answering a DA.
//   synthetic  probes the core stays silent on. Verified empirically against
//              @wterm/core + the roost-patched wasm: Primary DA (ESC[c, ESC[0c)
//              and XTVERSION (ESC[>0q) get no reply at all, and a full-screen
//              TUI may gate its whole render path on one of them.
//
// Ordering falls out of SEGMENTING the write: the chunk is fed to the core in
// pieces cut at each synthetic probe's end, the natives the preceding bytes
// produced are drained first, and only then is the synthesized reply appended.
// Concatenating every native ahead of every synthesized reply answers `ESC[c`
// then `ESC[6n` backwards.
//
// Probes are tokenized as complete CSI sequences over a bounded per-session
// carry, so a probe split across pty chunk boundaries at ANY offset is
// recognised exactly once, and a CSI that never terminates cannot pin an
// unbounded prefix.

import type { TerminalCore } from "@wterm/core";

/** The only core surface the reply lane touches. Narrow on purpose: this is a
 *  byte-ordering concern, not a grid one, and every `TerminalCore` satisfies it. */
export type QueryCore = Pick<TerminalCore, "writeRaw" | "getResponse">;

const ESC = 0x1b;
const CSI_OPEN = 0x5b; // [
// CSI body: parameter bytes 0x30-0x3f followed by intermediate bytes 0x20-0x2f.
// Their order is not enforced here — classification needs the span and the
// final byte, and a malformed order is not a probe either way.
const BODY_MIN = 0x20;
const BODY_MAX = 0x3f;
const FINAL_MIN = 0x40;
const FINAL_MAX = 0x7e;
// A private marker (`<` `=` `>` `?`) occupies the first parameter position.
const PRIV_MIN = 0x3c;
const PRIV_MAX = 0x3f;
const PRIV_GT = 0x3e; // >
const ZERO = 0x30;
const SEMICOLON = 0x3b;
const FINAL_C = 0x63;
const FINAL_Q = 0x71;

/** Primary DA reply: VT100 + Advanced Video Option — the universal "I am a
 *  terminal" handshake, enough to unblock any DA-gated init, and what xterm
 *  reports as its baseline class. */
export const PRIMARY_DA_REPLY = "\x1b[?1;2c";
/** XTVERSION reply: DCS > | <name> ST. Report the emulator we actually run so a
 *  client that version-gates behaviour sees a real name instead of silence. */
export const XTVERSION_REPLY = "\x1bP>|wterm(roost)\x1b\\";

/** The longest probe Roost answers is 5 bytes and the longest CSI it must
 *  tokenize past (DECRQM, `ESC [ ? 2 0 2 6 $ p`) is 9, so no legitimate partial
 *  needs more. Past the cap the unterminated CSI is abandoned: a stream that
 *  opens a CSI and never closes it cannot pin worker memory, and abandoning is
 *  safe because the discarded bytes contain no further ESC to re-anchor on. */
export const QUERY_CARRY_MAX = 32;

/** Per-session tokenizer carry, held on SessionRecord beside mode_carry and
 *  osc7_carry. EVERY byte of the session's pty stream advances it, the capture
 *  lane's included, so a partial probe left by one chunk can never be glued
 *  onto a chunk that did not actually follow it. */
export interface QueryCarry {
	query_carry: Uint8Array;
}

export interface QueryReply {
	/** What the pty is owed: natives and synthesized replies interleaved in the
	 *  order their probes appeared. Written back as one batch. */
	bytes: string;
	/** The subset the core produced, in drain order. */
	native: string;
	/** The subset Roost synthesized, in probe order. */
	synth: string;
	/** Bytes of an unterminated CSI abandoned at the carry cap. */
	droppedCarry: number;
}

/** Shared, so the vast majority of chunks — the ones carrying no probe and
 *  leaving no queued reply — allocate nothing at all. */
const NO_REPLY: QueryReply = Object.freeze({ bytes: "", native: "", synth: "", droppedCarry: 0 });
const EMPTY_CARRY = new Uint8Array(0);

/** Pop every queued core reply, oldest first. */
export function drainCoreReplies(core: QueryCore): string {
	const first = core.getResponse();
	if (first === null || first.length === 0) return "";
	let out = first;
	for (;;) {
		const next = core.getResponse();
		if (next === null || next.length === 0) return out;
		out += next;
	}
}

/** Feed `chunk` to `core` and return the replies the pty is owed for it, in
 *  probe order. The core is written in segments cut at each synthesized probe,
 *  so this is also the session's only live `writeRaw` site.
 *
 *  A null `core` is the capture lane: a geometry-uncertain transaction freezes
 *  the core, so those bytes are never parsed and their probes are answered
 *  once, FIFO, by the post-boundary tail replay. The stream still moved, so the
 *  carry moves with it and only `droppedCarry` is meaningful. */
export function answerQueries(
	state: QueryCarry,
	core: QueryCore | null,
	chunk: Uint8Array,
): QueryReply {
	const carry = state.query_carry;
	const shift = carry.length;
	// Zero-copy on the overwhelmingly common carry-free chunk.
	let buf = chunk;
	if (shift > 0) {
		buf = new Uint8Array(shift + chunk.length);
		buf.set(carry, 0);
		buf.set(chunk, shift);
	}
	let bytes = "";
	let native = "";
	let synth = "";
	const drain = (): void => {
		if (core === null) return;
		const got = drainCoreReplies(core);
		if (got.length === 0) return;
		native += got;
		bytes += got;
	};
	// Chunk bytes already handed to the core.
	let cursor = 0;
	let dropped = 0;
	let carryFrom = buf.length;
	let at = 0;
	while (at < buf.length) {
		const esc = buf.indexOf(ESC, at);
		if (esc < 0) break;
		if (esc + 1 === buf.length) { carryFrom = esc; break; }
		if (buf[esc + 1] !== CSI_OPEN) { at = esc + 1; continue; }
		let end = esc + 2;
		while (end < buf.length && buf[end]! >= BODY_MIN && buf[end]! <= BODY_MAX) end++;
		if (end === buf.length) {
			if (buf.length - esc > QUERY_CARRY_MAX) dropped = buf.length - esc;
			else carryFrom = esc;
			break;
		}
		const final = buf[end]!;
		// A byte outside the final range (C0, or the ESC of the next sequence)
		// aborts this CSI; rescan from just after its ESC.
		if (final < FINAL_MIN || final > FINAL_MAX) { at = esc + 1; continue; }
		at = end + 1;
		const reply = synthReplyFor(buf, esc + 2, end, final);
		if (reply.length === 0 || core === null) continue;
		// The carry only ever holds an UNTERMINATED CSI, so a complete probe's
		// final byte is always inside this chunk: `at - shift` is a real offset.
		const probeEnd = at - shift;
		core.writeRaw(chunk.subarray(cursor, probeEnd), drain);
		cursor = probeEnd;
		drain();
		bytes += reply;
		synth += reply;
	}
	if (core !== null) {
		if (cursor === 0) core.writeRaw(chunk, drain);
		else if (cursor < chunk.length) core.writeRaw(chunk.subarray(cursor), drain);
		drain();
	}
	// Always a copy: `buf` may be a view onto the pty's reusable read buffer.
	state.query_carry = carryFrom < buf.length ? buf.slice(carryFrom) : EMPTY_CARRY;
	if (bytes.length === 0 && dropped === 0) return NO_REPLY;
	return { bytes, native, synth, droppedCarry: dropped };
}

/** The reply Roost owes for one complete CSI whose body spans [from, end) and
 *  whose final byte is `final`; "" when the core answers it or it is not a
 *  probe at all. */
function synthReplyFor(buf: Uint8Array, from: number, end: number, final: number): string {
	const first = from < end ? buf[from]! : 0;
	const priv = first >= PRIV_MIN && first <= PRIV_MAX ? first : 0;
	const params = priv === 0 ? from : from + 1;
	if (final === FINAL_C && priv === 0 && zeroParams(buf, params, end)) return PRIMARY_DA_REPLY;
	if (final === FINAL_Q && priv === PRIV_GT && zeroParams(buf, params, end)) return XTVERSION_REPLY;
	return "";
}

/** Primary DA and XTVERSION both take Ps=0, defaulting to 0 when omitted; any
 *  other parameter makes it a different request. `ESC[>c` is DA2, `ESC[?...$p`
 *  is DECRQM and `ESC[6n` is the DSR the core answers — all three are tokenized
 *  here (so they can never be mis-answered or split the stream wrongly) and
 *  none of them is answered here. */
function zeroParams(buf: Uint8Array, from: number, end: number): boolean {
	for (let i = from; i < end; i++) {
		const b = buf[i]!;
		if (b !== ZERO && b !== SEMICOLON) return false;
	}
	return true;
}
