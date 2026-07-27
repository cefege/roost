// OSC 8 hyperlink tracker. Taps the PTY byte stream before it reaches
// wterm, parses ESC]8;params;URI ST text ESC]8;;ST sequences, records
// {visibleText → uri} into a per-Terminal Map. The linkifier
// (terminal-links.ts) queries this map after each render and wraps
// matching text spans in <a href={uri}>.
//
// Spec: https://gist.github.com/egmontkob/eb114294efbcd5adb1944c9f3cb5feda
// Format: ESC ] 8 ; params ; URI ST text ESC ] 8 ; ; ST
//   ST = ESC \  (preferred)  OR  BEL (0x07, tolerated)
//   params is `key=value:key=value`, optional `id=` for grouping
//   empty URI after ; ; means "end of hyperlink"
//
// Limitation (acknowledged): we map visible text → URI. If the same
// text appears twice with different URIs in one render window, the
// later URI wins. Most shell, git, or ls --hyperlink output uses
// unique paths so this is rare in practice. A positional tracker using
// wterm.bridge.getCursor() remains a possible future upgrade.
//
// Performance: the parser runs on EVERY byte chunk before wterm.write,
// so it must be fast. Fast-bail when the chunk has no ESC]8 prefix.

const ESC = 0x1b;
const RBRACKET = 0x5d;  // ]
const SEMI = 0x3b;      // ;
const ASCII_8 = 0x38;
const BEL = 0x07;
const BACKSLASH = 0x5c; // \  (for ST = ESC \)

const decoder = new TextDecoder("utf-8", { fatal: false });

/** Per-Terminal OSC 8 state machine + visible-text-to-URI map.
 *  Construct one per Terminal mount; call `process(chunk)` BEFORE
 *  `wterm.write(chunk)`; query via `lookup(text)`. */
export class Osc8Tracker {
  // Bytes that crossed the last chunk boundary mid-parse. Hard-capped
  // at MAX_CARRY_BYTES — pathological producers that emit `ESC]8;` and
  // never send a terminator would otherwise grow _carry unboundedly
  // across every chunk (the next chunk concats + rescans, finds no
  // terminator, sets carry = current).
  private _carry: Uint8Array = new Uint8Array(0);
  private static MAX_CARRY_BYTES = 64 * 1024;
  // Visible-text buffer while inside an active link. Capped so a link
  // that never closes can't grow the buffer to OOM. When over the cap,
  // we record what we have so far and reset.
  private static MAX_VISIBLE_BUF = 16 * 1024;
  // Currently-active hyperlink URI (null = not inside one).
  private _activeUri: string | null = null;
  // Pending visible-text buffer while inside a hyperlink. Flushed to
  // _textToUri on link-close.
  private _visibleBuf: number[] = [];
  // The lookup table the linkifier consumes.
  private _textToUri: Map<string, string> = new Map();
  // Cap on stored mappings — older entries evicted FIFO.
  private static MAX_ENTRIES = 1024;
  // Insertion-order keys for eviction.
  private _order: string[] = [];

  /** Process a chunk of raw PTY bytes. The chunk is fed through
   *  unchanged for OSC 8 (wterm ignores OSC 8 as an unhandled OSC,
   *  so pass-through is safe — we're just observing). */
  process(chunk: Uint8Array): void {
    // Fast bail: no ESC anywhere → cannot contain OSC 8.
    if (chunk.indexOf(ESC) === -1 && this._carry.length === 0) {
      // If we're mid-link, the visible text continues here.
      if (this._activeUri !== null) this._pushVisible(chunk);
      return;
    }
    // Combine with carry.
    const buf = this._carry.length === 0 ? chunk : _concat(this._carry, chunk);
    this._carry = new Uint8Array(0);
    this._scan(buf);
    // Hard-cap carry. If a producer never terminates an OSC 8 sequence
    // we'd otherwise grow it without bound. Drop the active link state
    // and reset — the malformed sequence is unrecoverable anyway.
    if (this._carry.length > Osc8Tracker.MAX_CARRY_BYTES) {
      this._carry = new Uint8Array(0);
      this._activeUri = null;
      this._visibleBuf = [];
    }
  }

  /** Append a chunk's bytes to the visible-text buffer in bulk, with
   *  the OOM cap enforced. On cap hit, flush current text → uri before
   *  resetting so we don't lose what we have. */
  private _pushVisible(chunk: Uint8Array): void {
    if (this._visibleBuf.length + chunk.length > Osc8Tracker.MAX_VISIBLE_BUF) {
      if (this._activeUri !== null && this._visibleBuf.length > 0) {
        this._record(decoder.decode(new Uint8Array(this._visibleBuf)), this._activeUri);
      }
      this._visibleBuf = [];
      return;
    }
    // Bulk append via spread is O(n) but avoids the per-byte function-call
    // overhead of a manual loop. Still O(n) per chunk.
    for (let i = 0; i < chunk.length; i++) this._visibleBuf.push(chunk[i]);
  }

  /** Return the URI for a visible text fragment, or undefined.
   *  Linkifier calls this for every URL-shaped match found via the
   *  regex AND for every plain-text span as a fallback lookup. */
  lookup(text: string): string | undefined {
    return this._textToUri.get(text);
  }

  /** Iterate all (text, uri) pairs. Linkifier uses this to wrap
   *  OSC-8-only link texts that the regex would never match
   *  (e.g. bare "Foo.txt"). */
  entries(): IterableIterator<[string, string]> {
    return this._textToUri.entries();
  }

  private _scan(buf: Uint8Array): void {
    let i = 0;
    while (i < buf.length) {
      const b = buf[i];
      if (b === ESC) {
        // Try to match ESC ] 8 ; …
        if (i + 2 >= buf.length) { this._carry = buf.subarray(i); return; }
        if (buf[i + 1] === RBRACKET && buf[i + 2] === ASCII_8) {
          // Find the terminator ST = (ESC \) or BEL.
          const term = _findOscTerm(buf, i + 3);
          if (term === -1) { this._carry = buf.subarray(i); return; }
          this._handleOsc8(buf, i + 3, term.end);
          i = term.next;
          continue;
        }
        // Other ESC sequence — consume the ESC byte; the next pass
        // treats subsequent bytes as plain. (Visible text inside a
        // link can include CSI/SGR — those count as visible payload
        // for our text→uri map, which is what we want.)
        if (this._activeUri !== null) this._visibleBuf.push(b);
        i++;
        continue;
      }
      if (this._activeUri !== null) this._visibleBuf.push(b);
      i++;
    }
  }

  private _handleOsc8(buf: Uint8Array, start: number, end: number): void {
    // Payload between `8` and ST: ;params;URI  (or ;;  for close)
    // Skip leading `;`.
    let p = start;
    if (p < end && buf[p] === SEMI) p++;
    // params end at next `;`
    let pe = p;
    while (pe < end && buf[pe] !== SEMI) pe++;
    // URI starts after that `;` (if present), runs to ST.
    const uriStart = pe < end ? pe + 1 : end;
    const uriBytes = buf.subarray(uriStart, end);
    const uri = decoder.decode(uriBytes);
    if (uri.length === 0) {
      // Close: flush visible buf into the map.
      if (this._activeUri !== null && this._visibleBuf.length > 0) {
        const text = decoder.decode(new Uint8Array(this._visibleBuf));
        this._record(text, this._activeUri);
      }
      this._activeUri = null;
      this._visibleBuf = [];
    } else {
      // Open: switch active URI (allow re-open without explicit close).
      if (this._activeUri !== null && this._visibleBuf.length > 0) {
        const text = decoder.decode(new Uint8Array(this._visibleBuf));
        this._record(text, this._activeUri);
      }
      this._activeUri = uri;
      this._visibleBuf = [];
    }
  }

  private _record(text: string, uri: string): void {
    // Strip CSI sequences (ESC [ params final) FIRST — terminal apps and git emit
    // OSC 8 link text with SGR styling like `ESC[31mFoo.txt\x1b[0m`,
    // which would otherwise leave `[31mFoo.txt[0m` in the lookup map
    // and never match wterm's textContent (which is just `Foo.txt`).
    // Also strip OSC payload (ESC ] ... ST) defensively — shouldn't
    // appear inside a link's visible text but cheap to handle.
    const noCsi = text
      .replace(/\x1b\[[0-9;:?]*[ -/]*[@-~]/g, "")
      .replace(/\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)/g, "");
    // Then strip remaining lone control chars.
    const clean = noCsi.replace(/[\x00-\x08\x0b-\x1f\x7f]+/g, "").trim();
    if (clean.length === 0) return;
    if (!this._textToUri.has(clean)) this._order.push(clean);
    this._textToUri.set(clean, uri);
    if (this._order.length > Osc8Tracker.MAX_ENTRIES) {
      const evict = this._order.shift()!;
      this._textToUri.delete(evict);
    }
  }
}

const trackersBySession = new Map<string, Osc8Tracker>();

export function osc8TrackerFor(sessionId: string): Osc8Tracker {
  let tracker = trackersBySession.get(sessionId);
  if (!tracker) {
    tracker = new Osc8Tracker();
    trackersBySession.set(sessionId, tracker);
  }
  return tracker;
}

export function processOsc8Chunk(sessionId: string, chunk: Uint8Array): void {
  osc8TrackerFor(sessionId).process(chunk);
}

export function pruneOsc8Tracker(sessionId: string): void {
  trackersBySession.delete(sessionId);
}

function _concat(a: Uint8Array, b: Uint8Array): Uint8Array {
  const out = new Uint8Array(a.length + b.length);
  out.set(a, 0); out.set(b, a.length);
  return out;
}

// Return the END (exclusive) of the OSC payload and the index AFTER
// the terminator, or -1 if the terminator isn't in `buf` yet (caller
// carries over).
function _findOscTerm(buf: Uint8Array, from: number): { end: number; next: number } | -1 {
  for (let i = from; i < buf.length; i++) {
    if (buf[i] === BEL) return { end: i, next: i + 1 };
    if (buf[i] === ESC && i + 1 < buf.length && buf[i + 1] === BACKSLASH) {
      return { end: i, next: i + 2 };
    }
    if (buf[i] === ESC && i + 1 >= buf.length) return -1; // carry — ST may be split
  }
  return -1;
}
