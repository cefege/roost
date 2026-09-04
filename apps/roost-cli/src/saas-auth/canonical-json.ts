/**
 * Canonicalizes bounded JSON for signatures and strict private-provider parsing.
 * Authentication and IPC code use these routines before trusting structured input.
 * Deterministic encoding and depth limits prevent ambiguous signatures and resource abuse.
 */

import { Buffer } from "node:buffer";

export type CanonicalJsonValue = null | boolean | string | number | CanonicalJsonValue[] | { [key: string]: CanonicalJsonValue };
export interface CanonicalJsonLimits { maxDepth: number; maxValues: number; maxStringBytes: number; maxTotalStringBytes: number; maxOutputBytes: number; maxInputBytes: number }
export const PRIVATE_IPC_CANONICAL_JSON_LIMITS: Readonly<CanonicalJsonLimits> = Object.freeze({ maxDepth: 16, maxValues: 1_024, maxStringBytes: 8_192, maxTotalStringBytes: 16_384, maxOutputBytes: 16_384, maxInputBytes: 16_384 });
const IDENTIFIER_RE = /^[A-Za-z_][A-Za-z0-9_]*$/;
export class CanonicalJsonError extends Error { constructor(message: string) { super(message); this.name = "CanonicalJsonError"; } }

function limitsWith(value?: Partial<CanonicalJsonLimits>): CanonicalJsonLimits {
  const limits = { ...PRIVATE_IPC_CANONICAL_JSON_LIMITS, ...value };
  for (const [name, limit] of Object.entries(limits)) if (!Number.isSafeInteger(limit) || limit <= 0) throw new CanonicalJsonError(`${name} must be a positive safe integer`);
  return limits;
}
function bytes(value: string): number { return Buffer.byteLength(value, "utf8"); }
function keyOkay(key: string, allowNonIdentifierKeys: boolean): void {
  if (!allowNonIdentifierKeys && !IDENTIFIER_RE.test(key)) {
    throw new CanonicalJsonError("object key is not an ASCII identifier");
  }
}

function encodeCanonicalJson(
  value: unknown,
  overrides: Partial<CanonicalJsonLimits> | undefined,
  allowNonIdentifierKeys: boolean,
): string {
  const limits = limitsWith(overrides);
  let count = 0, stringBytes = 0, outputBytes = 0;
  const active = new Set<object>();
  const out: string[] = [];
  const add = (text: string) => { outputBytes += bytes(text); if (outputBytes > limits.maxOutputBytes) throw new CanonicalJsonError("encoded size limit exceeded"); out.push(text); };
  const string = (text: string) => { const size = bytes(text); if (size > limits.maxStringBytes) throw new CanonicalJsonError("string size limit exceeded"); stringBytes += size; if (stringBytes > limits.maxTotalStringBytes) throw new CanonicalJsonError("total string size limit exceeded"); };
  const visit = (item: unknown, depth: number): void => {
    if (depth > limits.maxDepth) throw new CanonicalJsonError("depth limit exceeded");
    if (++count > limits.maxValues) throw new CanonicalJsonError("value count limit exceeded");
    if (item === null) return add("null");
    if (typeof item === "boolean") return add(item ? "true" : "false");
    if (typeof item === "string") { string(item); return add(JSON.stringify(item)); }
    if (typeof item === "number") { if (!Number.isSafeInteger(item)) throw new CanonicalJsonError("number is not a safe integer"); return add(String(item)); }
    if (typeof item !== "object") throw new CanonicalJsonError("unsupported value");
    if (active.has(item)) throw new CanonicalJsonError("cycle rejected");
    if (Array.isArray(item)) {
      if (Object.getPrototypeOf(item) !== Array.prototype || Reflect.ownKeys(item).length !== item.length + 1 || item.some((_, index) => !(index in item))) throw new CanonicalJsonError("non-plain array rejected");
      active.add(item); add("["); item.forEach((entry, index) => { if (index) add(","); visit(entry, depth + 1); }); add("]"); active.delete(item); return;
    }
    const prototype = Object.getPrototypeOf(item);
    if (prototype !== Object.prototype && prototype !== null) throw new CanonicalJsonError("non-plain object rejected");
    const ownKeys = Reflect.ownKeys(item);
    if (ownKeys.some((key) => typeof key !== "string")) throw new CanonicalJsonError("symbol key rejected");
    const entries = (ownKeys as string[]).map((key) => {
      keyOkay(key, allowNonIdentifierKeys); string(key);
      const descriptor = Object.getOwnPropertyDescriptor(item, key);
      if (!descriptor?.enumerable || !("value" in descriptor)) throw new CanonicalJsonError("non-data property rejected");
      return [key, descriptor.value] as const;
    }).sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0);
    active.add(item); add("{"); entries.forEach(([key, entry], index) => { if (index) add(","); add(JSON.stringify(key)); add(":"); visit(entry, depth + 1); }); add("}"); active.delete(item);
  };
  visit(value, 0);
  return out.join("");
}

export function canonicalJson(value: unknown, overrides?: Partial<CanonicalJsonLimits>): string {
  return encodeCanonicalJson(value, overrides, false);
}

class Parser {
  #at = 0; #count = 0; #stringBytes = 0;
  constructor(
    readonly source: string,
    readonly limits: CanonicalJsonLimits,
    readonly allowNonIdentifierKeys: boolean,
  ) {}
  parse(): CanonicalJsonValue { this.ws(); const value = this.value(0); this.ws(); if (this.#at !== this.source.length) throw new CanonicalJsonError("trailing JSON data"); return value; }
  ws(): void { while (/[\x20\t\r\n]/.test(this.source[this.#at] ?? "!")) this.#at++; }
  bump(depth: number): void { if (depth > this.limits.maxDepth) throw new CanonicalJsonError("depth limit exceeded"); if (++this.#count > this.limits.maxValues) throw new CanonicalJsonError("value count limit exceeded"); }
  value(depth: number): CanonicalJsonValue {
    this.bump(depth); const ch = this.source[this.#at];
    if (ch === '"') return this.string(); if (ch === "{") return this.object(depth); if (ch === "[") return this.array(depth);
    for (const [word, result] of [["true", true], ["false", false], ["null", null]] as const) if (this.source.startsWith(word, this.#at)) { this.#at += word.length; return result; }
    const match = /^-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?/.exec(this.source.slice(this.#at));
    if (!match) throw new CanonicalJsonError("invalid JSON token"); this.#at += match[0].length; const number = Number(match[0]); if (!Number.isSafeInteger(number)) throw new CanonicalJsonError("number is not a safe integer"); return number;
  }
  string(): string {
    const start = this.#at++;
    while (this.#at < this.source.length) {
      const code = this.source.charCodeAt(this.#at);
      if (code === 34) { const raw = this.source.slice(start, ++this.#at); let value: string; try { value = JSON.parse(raw); } catch { throw new CanonicalJsonError("invalid JSON string"); } const size = bytes(value); if (size > this.limits.maxStringBytes || (this.#stringBytes += size) > this.limits.maxTotalStringBytes) throw new CanonicalJsonError("string size limit exceeded"); return value; }
      if (code < 32) throw new CanonicalJsonError("control character in string");
      if (code === 92) { const escape = this.source[++this.#at]; if (escape === "u") { if (!/^[0-9a-f]{4}$/i.test(this.source.slice(this.#at + 1, this.#at + 5))) throw new CanonicalJsonError("invalid Unicode escape"); this.#at += 4; } else if (!escape || !'"\\/bfnrt'.includes(escape)) throw new CanonicalJsonError("invalid escape"); }
      this.#at++;
    }
    throw new CanonicalJsonError("unterminated string");
  }
  object(depth: number): { [key: string]: CanonicalJsonValue } {
    this.#at++; this.ws(); const object: { [key: string]: CanonicalJsonValue } = Object.create(null); const seen = new Set<string>(); if (this.source[this.#at] === "}") { this.#at++; return object; }
    while (true) { if (this.source[this.#at] !== '"') throw new CanonicalJsonError("object key must be a string"); const key = this.string(); keyOkay(key, this.allowNonIdentifierKeys); if (seen.has(key)) throw new CanonicalJsonError("duplicate object key"); seen.add(key); this.ws(); if (this.source[this.#at++] !== ":") throw new CanonicalJsonError("missing colon"); this.ws(); Object.defineProperty(object, key, { value: this.value(depth + 1), enumerable: true, writable: true, configurable: true }); this.ws(); const separator = this.source[this.#at++]; if (separator === "}") return object; if (separator !== ",") throw new CanonicalJsonError("missing object separator"); this.ws(); }
  }
  array(depth: number): CanonicalJsonValue[] {
    this.#at++; this.ws(); const array: CanonicalJsonValue[] = []; if (this.source[this.#at] === "]") { this.#at++; return array; }
    while (true) { array.push(this.value(depth + 1)); this.ws(); const separator = this.source[this.#at++]; if (separator === "]") return array; if (separator !== ",") throw new CanonicalJsonError("missing array separator"); this.ws(); }
  }
}

function parseJson(
  input: string | Uint8Array,
  overrides: Partial<CanonicalJsonLimits> | undefined,
  allowNonIdentifierKeys: boolean,
): CanonicalJsonValue {
  const limits = limitsWith(overrides);
  let source: string;
  try { source = typeof input === "string" ? input : new TextDecoder("utf-8", { fatal: true }).decode(input); } catch { throw new CanonicalJsonError("invalid UTF-8"); }
  if (bytes(source) > limits.maxInputBytes) throw new CanonicalJsonError("input size limit exceeded");
  const value = new Parser(source, limits, allowNonIdentifierKeys).parse();
  encodeCanonicalJson(value, limits, allowNonIdentifierKeys);
  return value;
}

export function parseStrictJson(input: string | Uint8Array, overrides?: Partial<CanonicalJsonLimits>): CanonicalJsonValue {
  return parseJson(input, overrides, false);
}

export function parseStrictProviderJson(input: string | Uint8Array, overrides?: Partial<CanonicalJsonLimits>): CanonicalJsonValue {
  return parseJson(input, overrides, true);
}
