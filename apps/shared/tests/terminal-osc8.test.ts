import { describe, expect, test } from "bun:test";
import { Osc8Tracker } from "../src/terminal-osc8.ts";

const encoder = new TextEncoder();
const bytes = (text: string): Uint8Array => encoder.encode(text);
const belLink = (uri: string, text: string): string =>
  `\x1b]8;;${uri}\x07${text}\x1b]8;;\x07`;
const stLink = (uri: string, text: string): string =>
  `\x1b]8;;${uri}\x1b\\${text}\x1b]8;;\x1b\\`;

describe("shared OSC 8 parser", () => {
  test("parses BEL and ST records including split sequences", () => {
    const seen: Array<[string, string]> = [];
    const tracker = new Osc8Tracker((text, uri) => seen.push([text, uri]));

    tracker.process(bytes(belLink("https://example.test/bel", "bel-label")));
    const split = stLink("https://example.test/st", "split-label");
    tracker.process(bytes(split.slice(0, 11)));
    tracker.process(bytes(split.slice(11, 29)));
    tracker.process(bytes(split.slice(29)));

    expect(seen).toEqual([
      ["bel-label", "https://example.test/bel"],
      ["split-label", "https://example.test/st"],
    ]);
    expect(tracker.lookup("split-label")).toBe("https://example.test/st");
  });

  test("sanitizes before storage and invokes callbacks after storage", () => {
    const observedLookups: Array<string | undefined> = [];
    let tracker!: Osc8Tracker;
    tracker = new Osc8Tracker((text) => observedLookups.push(tracker.lookup(text)));

    tracker.process(bytes(belLink(
      "https://example.test/styled",
      "\x1b[31mStyled.txt\x1b[0m\r\n",
    )));
    tracker.record("\x1b[32mDirect.txt\x1b[0m\t", "https://example.test/direct");

    expect(Array.from(tracker.entries())).toEqual([
      ["Styled.txt", "https://example.test/styled"],
      ["Direct.txt", "https://example.test/direct"],
    ]);
    expect(observedLookups).toEqual([
      "https://example.test/styled",
      "https://example.test/direct",
    ]);
  });

  test("ordinary output and incomplete links remain silent", () => {
    const seen: Array<[string, string]> = [];
    const tracker = new Osc8Tracker((text, uri) => seen.push([text, uri]));

    tracker.process(bytes("ordinary output\n"));
    tracker.process(bytes("\x1b]8;;https://example.test/incomplete\x07unfinished"));

    expect(seen).toEqual([]);
    expect(tracker.lookup("unfinished")).toBeUndefined();
  });

  test("accepts 8 KiB text and discards 8193-byte streamed records whole", () => {
    const exactText = "x".repeat(8 * 1024);
    const exactSeen: Array<[string, string]> = [];
    const exact = new Osc8Tracker((text, uri) => exactSeen.push([text, uri]));
    exact.process(bytes(belLink("https://example.test/exact", exactText)));
    const splitSeen: Array<[string, string]> = [];
    const split = new Osc8Tracker((text, uri) => splitSeen.push([text, uri]));
    split.process(bytes("\x1b]8;;https://example.test/split-large\x07"));
    split.process(bytes("a".repeat(8 * 1024)));
    split.process(bytes("z\x1b]8;;\x07"));
    const multibyteSeen: Array<[string, string]> = [];
    const multibyte = new Osc8Tracker((text, uri) => multibyteSeen.push([text, uri]));
    multibyte.process(bytes(belLink(
      "https://example.test/same-chunk-multibyte",
      "\u00e9".repeat(4 * 1024) + "b",
    )));
    expect(exactSeen).toEqual([[exactText, "https://example.test/exact"]]);
    expect(exact.lookup(exactText)).toBe("https://example.test/exact");
    expect(splitSeen).toEqual([]);
    expect(Array.from(split.entries())).toEqual([]);
    expect(multibyteSeen).toEqual([]);
    expect(Array.from(multibyte.entries())).toEqual([]);
  });

  test("consumes oversized same-buffer URI payloads without activating them", () => {
    const seen: Array<[string, string]> = [];
    const tracker = new Osc8Tracker((text, uri) => seen.push([text, uri]));
    const oversizedUri = "\u00e9".repeat(4 * 1024 + 1);

    tracker.process(bytes(
      belLink(oversizedUri, "discarded")
      + belLink("https://example.test/recovered", "recovered"),
    ));

    expect(seen).toEqual([["recovered", "https://example.test/recovered"]]);
    expect(tracker.lookup("discarded")).toBeUndefined();
  });

  test("consumes oversized split-buffer URI payloads through their terminator", () => {
    const seen: Array<[string, string]> = [];
    const tracker = new Osc8Tracker((text, uri) => seen.push([text, uri]));
    const oversizedLink = bytes(stLink("u".repeat(8 * 1024 + 1), "discarded"));
    const splitAt = bytes("\x1b]8;;").length + 4 * 1024;

    tracker.process(oversizedLink.subarray(0, splitAt));
    tracker.process(oversizedLink.subarray(splitAt));
    tracker.process(bytes(stLink("https://example.test/recovered", "recovered")));

    expect(seen).toEqual([["recovered", "https://example.test/recovered"]]);
    expect(tracker.lookup("discarded")).toBeUndefined();
  });

  test("accepts direct 8 KiB text and rejects 8193-byte text or URI", () => {
    const seen: Array<[string, string]> = [];
    const tracker = new Osc8Tracker((text, uri) => seen.push([text, uri]));
    const exactText = "d".repeat(8 * 1024);
    const oversizedMultibyteText = "\u00e9".repeat(4 * 1024) + "m";
    tracker.record(exactText, "https://example.test/exact-direct");
    tracker.record("a".repeat(8 * 1024 + 1), "https://example.test/ascii-text");
    tracker.record(oversizedMultibyteText, "https://example.test/multibyte-text");
    tracker.record("oversized-ascii-uri", "u".repeat(8 * 1024 + 1));
    tracker.record("oversized-multibyte-uri", "\u00e9".repeat(4 * 1024) + "u");
    expect(seen).toEqual([[exactText, "https://example.test/exact-direct"]]);
    expect(Array.from(tracker.entries())).toEqual(seen);
    expect(tracker.lookup("a".repeat(8 * 1024 + 1))).toBeUndefined();
    expect(tracker.lookup(oversizedMultibyteText)).toBeUndefined();
  });

  test("unterminated OSC carry is bounded and never published", () => {
    const seen: Array<[string, string]> = [];
    const tracker = new Osc8Tracker((text, uri) => seen.push([text, uri]));

    tracker.process(bytes(`\x1b]8;;${"x".repeat(64 * 1024 + 1)}`));
    tracker.process(bytes(belLink("https://example.test/recovered", "recovered")));

    expect(seen).toEqual([["recovered", "https://example.test/recovered"]]);
  });

  test("evicts stored mappings in 1024-entry FIFO order", () => {
    const tracker = new Osc8Tracker();
    for (let i = 0; i < 1025; i += 1) tracker.record(`text-${i}`, `https://example.test/${i}`);

    expect(tracker.lookup("text-0")).toBeUndefined();
    expect(tracker.lookup("text-1")).toBe("https://example.test/1");
    expect(tracker.lookup("text-1024")).toBe("https://example.test/1024");
  });
});
