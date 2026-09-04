import { describe, expect, test } from "bun:test";
import { safeAttachmentInsertion } from "../src/lib/attachmentInsertion.ts";

const POSIX_CASES = [
  ["/tmp/a b.txt", "'/tmp/a b.txt'"],
  ["/tmp/it's.txt", "'/tmp/it'\"'\"'s.txt'"],
  ["/tmp/$(printf exploited)", "'/tmp/$(printf exploited)'"],
  ["/tmp/`printf exploited`", "'/tmp/`printf exploited`'"],
  ["/tmp/你好 🐓.txt", "'/tmp/你好 🐓.txt'"],
] as const;

describe("safeAttachmentInsertion", () => {
  for (const [absPath, expected] of POSIX_CASES) {
    test(`quotes ${JSON.stringify(absPath)} as one inert POSIX argument`, () => {
      expect(safeAttachmentInsertion("linux", absPath)).toBe(expected);
      expect(safeAttachmentInsertion("darwin", absPath)).toBe(expected);
    });
  }

  test("rejects every C0, DEL, and C1 control character", () => {
    const controls = [
      ...Array.from({ length: 0x20 }, (_, codePoint) => codePoint),
      ...Array.from({ length: 0x21 }, (_, offset) => 0x7f + offset),
    ];

    for (const codePoint of controls) {
      const absPath = `/tmp/before${String.fromCodePoint(codePoint)}after`;
      expect(safeAttachmentInsertion("linux", absPath)).toBeNull();
      expect(safeAttachmentInsertion("darwin", absPath)).toBeNull();
    }
  });

  test("returns null on Windows without rewriting the path", () => {
    expect(safeAttachmentInsertion("win32", String.raw`C:\Users\alice\report.txt`)).toBeNull();
    expect(safeAttachmentInsertion("win32", "/posix-looking/path.txt")).toBeNull();
  });
});
