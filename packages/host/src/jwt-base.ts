// Base64url codec for the node side of JWT minting/verification. Coord's
// jwt.ts and the worker's jwt.ts carried byte-identical copies; this is the
// single source. The web SPA keeps its WebCrypto/btoa variant because it
// has no Buffer — do not import this module from browser code.

export function b64urlEncode(input: Uint8Array | string): string {
  const buf = typeof input === "string" ? Buffer.from(input, "utf8") : Buffer.from(input);
  return buf.toString("base64url");
}

export function b64urlDecode(input: string): Uint8Array {
  return new Uint8Array(Buffer.from(input, "base64url"));
}

export function b64urlDecodeToUtf8(input: string): string {
  return Buffer.from(input, "base64url").toString("utf8");
}
