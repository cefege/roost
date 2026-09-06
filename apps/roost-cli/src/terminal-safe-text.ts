// Formats untrusted CLI text for one bounded terminal cell.
// Human renderers call this before interpolation; lossless JSON paths bypass it.
// Controls become visible escapes and oversized rendered values carry a marker.

import { utf8ByteLength } from "@roost/shared/ui-state";

export const TERMINAL_SAFE_TEXT_MAX_CODE_POINTS = 256;
export const TERMINAL_SAFE_TEXT_MAX_UTF8_BYTES = 512;
export const TERMINAL_SAFE_TEXT_TRUNCATION_MARKER = "…[truncated]";

const UNSAFE_UNICODE_PATTERN = /[\p{Cf}\p{Zl}\p{Zp}\p{Cs}]/u;
const TRUNCATION_MARKER_CODE_POINTS = [...TERMINAL_SAFE_TEXT_TRUNCATION_MARKER].length;
const TRUNCATION_MARKER_UTF8_BYTES = utf8ByteLength(TERMINAL_SAFE_TEXT_TRUNCATION_MARKER);

type RenderedPiece = {
  text: string;
  codePoints: number;
  utf8Bytes: number;
};

/** Make one untrusted value visible, single-line, and terminal-safe. */
export function formatTerminalSafeText(value: string): string {
  const pieces: RenderedPiece[] = [];
  let renderedCodePoints = 0;
  let renderedUtf8Bytes = 0;
  let offset = 0;

  while (offset < value.length) {
    const codePoint = value.codePointAt(offset)!;
    const character = String.fromCodePoint(codePoint);
    offset += character.length;
    const text = escapedCharacter(character, codePoint);
    const unchanged = text === character;
    let pieceUtf8Bytes = text.length;
    if (unchanged) {
      if (codePoint <= 0x7f) pieceUtf8Bytes = 1;
      else if (codePoint <= 0x7ff) pieceUtf8Bytes = 2;
      else if (codePoint <= 0xffff) pieceUtf8Bytes = 3;
      else pieceUtf8Bytes = 4;
    }
    const piece = {
      text,
      codePoints: unchanged ? 1 : text.length,
      utf8Bytes: pieceUtf8Bytes,
    };
    if (
      renderedCodePoints + piece.codePoints > TERMINAL_SAFE_TEXT_MAX_CODE_POINTS
      || renderedUtf8Bytes + piece.utf8Bytes > TERMINAL_SAFE_TEXT_MAX_UTF8_BYTES
    ) {
      return appendTruncationMarker(pieces, renderedCodePoints, renderedUtf8Bytes);
    }
    pieces.push(piece);
    renderedCodePoints += piece.codePoints;
    renderedUtf8Bytes += piece.utf8Bytes;
  }

  return pieces.map((piece) => piece.text).join("");
}

function escapedCharacter(character: string, codePoint: number): string {
  if (character === "\\") return "\\\\";
  if (character === "\t") return "\\t";
  if (character === "\r") return "\\r";
  if (character === "\n") return "\\n";
  if (codePoint <= 0x1f || (codePoint >= 0x7f && codePoint <= 0x9f)) {
    return `\\x${codePoint.toString(16).padStart(2, "0")}`;
  }
  if (UNSAFE_UNICODE_PATTERN.test(character)) return `\\u{${codePoint.toString(16)}}`;
  return character;
}

function appendTruncationMarker(
  pieces: RenderedPiece[],
  renderedCodePoints: number,
  renderedUtf8Bytes: number,
): string {
  while (
    renderedCodePoints + TRUNCATION_MARKER_CODE_POINTS > TERMINAL_SAFE_TEXT_MAX_CODE_POINTS
    || renderedUtf8Bytes + TRUNCATION_MARKER_UTF8_BYTES > TERMINAL_SAFE_TEXT_MAX_UTF8_BYTES
  ) {
    const removed = pieces.pop();
    if (removed === undefined) break;
    renderedCodePoints -= removed.codePoints;
    renderedUtf8Bytes -= removed.utf8Bytes;
  }
  return pieces.map((piece) => piece.text).join("") + TERMINAL_SAFE_TEXT_TRUNCATION_MARKER;
}

