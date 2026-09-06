// Owns browser UI-report identity, text, cardinality, and admission bounds.
// Coordinator retention and browser/CLI ingress share these deterministic limits.
// UTF-8 measurement is allocation-free and matches TextEncoder replacement semantics.

export const UI_TAB_ID_MAX_UTF8_BYTES = 256;
export const UI_ACTIVE_PATH_MAX_UTF8_BYTES = 8_192;
export const UI_FOLDER_KEY_MAX_UTF8_BYTES = 8_192;
export const UI_STATE_MAX_TABS_PER_FINGERPRINT = 32;
export const UI_STATE_MAX_TABS_PER_DASHBOARD = 256;
export const UI_STATE_NEW_IDENTITIES_PER_WINDOW = 16;
export const UI_STATE_IDENTITY_WINDOW_MS = 60_000;

/** Return the UTF-8 byte length without allocating an encoded copy. */
export function utf8ByteLength(value: string): number {
  return utf8ByteLengthUpTo(value, Number.MAX_SAFE_INTEGER);
}

/** Check a UTF-8 bound while inspecting at most `maxBytes + 1` code units. */
export function hasAtMostUtf8Bytes(value: string, maxBytes: number): boolean {
  if (!Number.isSafeInteger(maxBytes) || maxBytes < 0) {
    throw new RangeError("maxBytes must be a nonnegative safe integer");
  }
  if (value.length > maxBytes) return false;
  return utf8ByteLengthUpTo(value, maxBytes) <= maxBytes;
}

function utf8ByteLengthUpTo(value: string, maxBytes: number): number {
  let bytes = 0;
  let offset = 0;
  while (offset < value.length) {
    const codeUnit = value.charCodeAt(offset++);
    if (codeUnit <= 0x7f) {
      bytes += 1;
    } else if (codeUnit <= 0x7ff) {
      bytes += 2;
    } else if (
      codeUnit >= 0xd800
      && codeUnit <= 0xdbff
      && offset < value.length
      && value.charCodeAt(offset) >= 0xdc00
      && value.charCodeAt(offset) <= 0xdfff
    ) {
      bytes += 4;
      offset++;
    } else {
      bytes += 3;
    }
    if (bytes > maxBytes) return maxBytes + 1;
  }
  return bytes;
}
