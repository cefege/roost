// Bounded OSC 8 hyperlink parser shared by coordinator producers and browser
// registries. It records sanitized visible-text -> URI mappings without retaining
// terminal output. Completed records notify only after the mapping is stored.

const ESC = 0x1b;
const RBRACKET = 0x5d;
const SEMI = 0x3b;
const ASCII_8 = 0x38;
const BEL = 0x07;
const BACKSLASH = 0x5c;

const decoder = new TextDecoder("utf-8", { fatal: false });
const encoder = new TextEncoder();

export type Osc8RecordCallback = (text: string, uri: string) => void;

/** Per-stream OSC 8 state machine and bounded visible-text-to-URI map. */
export class Osc8Tracker {
  private _carry: Uint8Array = new Uint8Array(0);
  private static readonly MAX_CARRY_BYTES = 64 * 1024;
  private static readonly MAX_TEXT_BYTES = 8 * 1024;
  private static readonly MAX_URI_BYTES = 8 * 1024;
  private _activeUri: string | null = null;
  private _visibleBuf: number[] = [];
  // Once the visible-text cap is exceeded, discard the entire active record
  // until OSC close/re-open. Prefixes and tails are not valid producer mappings.
  private _activeRecordDiscarded = false;
  private _textToUri = new Map<string, string>();
  private static readonly MAX_ENTRIES = 1024;
  private _order: string[] = [];

  constructor(private readonly _onCompletedRecord?: Osc8RecordCallback) {}

  /** Observe raw PTY bytes. Only a terminated OSC 8 record notifies consumers. */
  process(chunk: Uint8Array): void {
    if (chunk.indexOf(ESC) === -1 && this._carry.length === 0) {
      if (this._activeUri !== null) this._pushVisible(chunk);
      return;
    }
    const buf = this._carry.length === 0 ? chunk : concat(this._carry, chunk);
    this._carry = new Uint8Array(0);
    this._scan(buf);
    if (this._carry.length > Osc8Tracker.MAX_CARRY_BYTES) {
      this._carry = new Uint8Array(0);
      this._activeUri = null;
      this._visibleBuf = [];
      this._activeRecordDiscarded = false;
    }
  }

  /** Store an already-completed mapping through the parser's sanitize path. */
  record(text: string, uri: string): void {
    this._store(text, uri);
  }

  lookup(text: string): string | undefined {
    return this._textToUri.get(text);
  }

  entries(): IterableIterator<[string, string]> {
    return this._textToUri.entries();
  }

  private _pushVisible(chunk: Uint8Array): void {
    if (this._activeRecordDiscarded) return;
    if (this._visibleBuf.length + chunk.length > Osc8Tracker.MAX_TEXT_BYTES) {
      this._activeRecordDiscarded = true;
      this._visibleBuf = [];
      return;
    }
    for (let i = 0; i < chunk.length; i++) this._visibleBuf.push(chunk[i]!);
  }

  private _scan(buf: Uint8Array): void {
    let i = 0;
    while (i < buf.length) {
      const b = buf[i];
      if (b === ESC) {
        if (i + 2 >= buf.length) {
          this._carry = buf.subarray(i);
          return;
        }
        if (buf[i + 1] === RBRACKET && buf[i + 2] === ASCII_8) {
          const term = findOscTerm(buf, i + 3);
          if (term === -1) {
            this._carry = buf.subarray(i);
            return;
          }
          this._handleOsc8(buf, i + 3, term.end);
          i = term.next;
          continue;
        }
        if (this._activeUri !== null) this._pushVisibleByte(b);
        i += 1;
        continue;
      }
      if (this._activeUri !== null) this._pushVisibleByte(b!);
      i += 1;
    }
  }

  private _pushVisibleByte(byte: number): void {
    if (this._activeRecordDiscarded) return;
    if (this._visibleBuf.length >= Osc8Tracker.MAX_TEXT_BYTES) {
      this._activeRecordDiscarded = true;
      this._visibleBuf = [];
      return;
    }
    this._visibleBuf.push(byte);
  }

  private _handleOsc8(buf: Uint8Array, start: number, end: number): void {
    let p = start;
    if (p < end && buf[p] === SEMI) p += 1;
    let paramsEnd = p;
    while (paramsEnd < end && buf[paramsEnd] !== SEMI) paramsEnd += 1;
    const uriStart = paramsEnd < end ? paramsEnd + 1 : end;
    const uriBytes = end - uriStart;
    if (uriBytes === 0) {
      this._completeActiveRecord();
      this._activeUri = null;
      this._visibleBuf = [];
      this._activeRecordDiscarded = false;
      return;
    }
    if (uriBytes > Osc8Tracker.MAX_URI_BYTES) {
      this._completeActiveRecord();
      this._activeUri = null;
      this._visibleBuf = [];
      this._activeRecordDiscarded = false;
      return;
    }
    const uri = decoder.decode(buf.subarray(uriStart, end));
    this._completeActiveRecord();
    this._activeUri = uri;
    this._activeRecordDiscarded = false;
    this._visibleBuf = [];
  }

  private _completeActiveRecord(): void {
    if (
      this._activeUri === null
      || this._activeRecordDiscarded
      || this._visibleBuf.length === 0
    ) return;
    this._store(decoder.decode(new Uint8Array(this._visibleBuf)), this._activeUri);
  }

  private _store(text: string, uri: string): void {
    if (
      text.length > Osc8Tracker.MAX_TEXT_BYTES
      || uri.length > Osc8Tracker.MAX_URI_BYTES
      || encoder.encode(text).byteLength > Osc8Tracker.MAX_TEXT_BYTES
      || encoder.encode(uri).byteLength > Osc8Tracker.MAX_URI_BYTES
    ) return;
    const noCsi = text
      .replace(/\x1b\[[0-9;:?]*[ -/]*[@-~]/g, "")
      .replace(/\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)/g, "");
    const clean = noCsi.replace(/[\x00-\x08\x0b-\x1f\x7f]+/g, "").trim();
    if (clean.length === 0) return;
    if (!this._textToUri.has(clean)) this._order.push(clean);
    this._textToUri.set(clean, uri);
    if (this._order.length > Osc8Tracker.MAX_ENTRIES) {
      const evict = this._order.shift()!;
      this._textToUri.delete(evict);
    }
    this._onCompletedRecord?.(clean, uri);
  }
}

function concat(a: Uint8Array, b: Uint8Array): Uint8Array {
  const out = new Uint8Array(a.length + b.length);
  out.set(a, 0);
  out.set(b, a.length);
  return out;
}

function findOscTerm(buf: Uint8Array, from: number): { end: number; next: number } | -1 {
  for (let i = from; i < buf.length; i++) {
    if (buf[i] === BEL) return { end: i, next: i + 1 };
    if (buf[i] === ESC && i + 1 < buf.length && buf[i + 1] === BACKSLASH) {
      return { end: i, next: i + 2 };
    }
    if (buf[i] === ESC && i + 1 >= buf.length) return -1;
  }
  return -1;
}

