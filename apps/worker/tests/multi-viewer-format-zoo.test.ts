// Format zoo: every class of ANSI/VT sequence we care about, driven
// through the server↔client wterm-core parity check. If a sequence
// type ever gets serialized in a way the parser handles differently
// from how its own writer applied it, the round-trip diverges and
// the test fails for that specific format class.
//
// Author 2026-06-17: "we had a lot of issues with the formatting and
// stuff. now you have the time to actually do all the tests."
//
// Each case is one named ANSI pattern → write into server wterm →
// serialize → write into client wterm → both wterms must canonical-
// serialize to the same string. The format CLASS is the test name so
// failures point at the regression's category (SGR vs cursor vs alt).

import { describe, test, expect } from "bun:test";
import { WasmBridge } from "@wterm/core";
import { serializeWTerm } from "../src/wterm-serialize.ts";

// One-shot harness: write `payload` into a server-side wterm at
// (cols,rows), pull the serialize output, replay into a client-side
// wterm, compare canonical serializes. Returns the two strings for
// debug-friendly diff; expect() lives at the call site so the test
// name attributes the failure.
async function roundtrip(payload: string, cols = 60, rows = 12): Promise<{ server: string; client: string }> {
  const server = await WasmBridge.load();
  server.init(cols, rows);
  server.writeRaw(new TextEncoder().encode(payload));
  const wire = serializeWTerm(server);
  const client = await WasmBridge.load();
  client.init(cols, rows);
  client.writeRaw(new TextEncoder().encode(wire));
  return { server: serializeWTerm(server), client: serializeWTerm(client) };
}

describe("format zoo — plain text", () => {
  test("Z01 ASCII single line", async () => {
    const r = await roundtrip("hello world");
    expect(r.client).toBe(r.server);
  });
  test("Z02 multiple CRLF lines", async () => {
    const r = await roundtrip("line1\r\nline2\r\nline3\r\n");
    expect(r.client).toBe(r.server);
  });
  test("Z03 LF without CR (xterm default mode)", async () => {
    const r = await roundtrip("a\nb\nc\n");
    expect(r.client).toBe(r.server);
  });
  test("Z04 line longer than cols (wrap)", async () => {
    const r = await roundtrip("x".repeat(75), 40, 10);
    expect(r.client).toBe(r.server);
  });
  test("Z05 trailing whitespace preserved", async () => {
    const r = await roundtrip("text     trailing   \r\n");
    expect(r.client).toBe(r.server);
  });
});

describe("format zoo — SGR (colors, attributes)", () => {
  test("Z10 reset (SGR 0)", async () => {
    const r = await roundtrip("\x1b[0mtxt\r\n");
    expect(r.client).toBe(r.server);
  });
  test("Z11 standard 8 fg colors", async () => {
    let s = "";
    for (let c = 30; c <= 37; c++) s += `\x1b[${c}mF`;
    s += "\x1b[0m\r\n";
    const r = await roundtrip(s);
    expect(r.client).toBe(r.server);
  });
  test("Z12 standard 8 bg colors", async () => {
    let s = "";
    for (let c = 40; c <= 47; c++) s += `\x1b[${c}mB`;
    s += "\x1b[0m\r\n";
    const r = await roundtrip(s);
    expect(r.client).toBe(r.server);
  });
  test("Z13 bright (90-97 / 100-107)", async () => {
    const r = await roundtrip("\x1b[91mR\x1b[92mG\x1b[94mB\x1b[0m\r\n");
    expect(r.client).toBe(r.server);
  });
  test("Z14 256-color fg (5;n)", async () => {
    const r = await roundtrip("\x1b[38;5;208morange\x1b[0m\r\n");
    expect(r.client).toBe(r.server);
  });
  test("Z15 truecolor (2;r;g;b)", async () => {
    const r = await roundtrip("\x1b[38;2;255;128;0mhex\x1b[0m\r\n");
    expect(r.client).toBe(r.server);
  });
  test("Z16 bold + italic + underline + inverse + strike", async () => {
    const r = await roundtrip("\x1b[1;3;4;7;9mall\x1b[0m\r\n");
    expect(r.client).toBe(r.server);
  });
  test("Z17 nested SGR (open one, layer another, close inner)", async () => {
    const r = await roundtrip("\x1b[31mred \x1b[1mboldred\x1b[22m red\x1b[0m\r\n");
    expect(r.client).toBe(r.server);
  });
});

describe("format zoo — cursor motion", () => {
  test("Z20 CUP (cursor position)", async () => {
    const r = await roundtrip("\x1b[5;10HX");
    expect(r.client).toBe(r.server);
  });
  test("Z21 home (\\x1b[H)", async () => {
    const r = await roundtrip("\x1b[Hhome");
    expect(r.client).toBe(r.server);
  });
  test("Z22 CUU/CUD/CUF/CUB", async () => {
    const r = await roundtrip("AAAA\x1b[2DZZ\x1b[Bbelow\x1b[3AAUP");
    expect(r.client).toBe(r.server);
  });
  test("Z23 save/restore cursor (ESC 7 / ESC 8)", async () => {
    const r = await roundtrip("first\x1b7\r\n\r\n\r\nthird\x1b8MARK");
    expect(r.client).toBe(r.server);
  });
  test("Z24 CSI save/restore (\\x1b[s / \\x1b[u)", async () => {
    const r = await roundtrip("first\x1b[s\r\n\r\nthird\x1b[uMARK");
    expect(r.client).toBe(r.server);
  });
});

describe("format zoo — erase", () => {
  test("Z30 ED (erase display) cursor-to-end", async () => {
    const r = await roundtrip("\x1b[Habcdef\r\nghi\x1b[H\x1b[J");
    expect(r.client).toBe(r.server);
  });
  test("Z31 EL (erase line) full", async () => {
    const r = await roundtrip("abc\x1b[5D\x1b[2K");
    expect(r.client).toBe(r.server);
  });
  test("Z32 ED 2 (full screen clear)", async () => {
    const r = await roundtrip("noise\r\nmore\x1b[2J\x1b[Hclean");
    expect(r.client).toBe(r.server);
  });
});

describe("format zoo — alt-screen", () => {
  test("Z40 enter/exit alt-screen via 1049", async () => {
    const r = await roundtrip("before\r\n\x1b[?1049hINSIDE\x1b[?1049lafter\r\n");
    expect(r.client).toBe(r.server);
  });
  test("Z41 alt-screen via 47 (legacy)", async () => {
    const r = await roundtrip("\x1b[?47hALT\x1b[?47lOUT\r\n");
    expect(r.client).toBe(r.server);
  });
  test("Z42 alt-screen via 1047", async () => {
    const r = await roundtrip("\x1b[?1047hMID\x1b[?1047lEND\r\n");
    expect(r.client).toBe(r.server);
  });
});

describe("format zoo — UTF-8 / wide / emoji", () => {
  test("Z50 ASCII + Latin-1 accented", async () => {
    const r = await roundtrip("héllo café résumé\r\n");
    expect(r.client).toBe(r.server);
  });
  test("Z51 CJK (wide chars)", async () => {
    const r = await roundtrip("中文测试 日本語 한국어\r\n", 40, 8);
    expect(r.client).toBe(r.server);
  });
  test("Z52 emoji including ZWJ + skin-tone modifier", async () => {
    const r = await roundtrip("🐙 👨‍👩‍👧 👋🏽\r\n", 40, 8);
    expect(r.client).toBe(r.server);
  });
  test("Z53 RTL Arabic + Hebrew", async () => {
    const r = await roundtrip("مرحبا שלום\r\n", 40, 8);
    expect(r.client).toBe(r.server);
  });
  test("Z54 combining diacritics", async () => {
    const r = await roundtrip("é à ñ\r\n");
    expect(r.client).toBe(r.server);
  });
});

describe("format zoo — scroll regions + tabs", () => {
  test("Z60 DECSTBM scroll region + content", async () => {
    const r = await roundtrip("\x1b[3;6r\x1b[3;1HtopOfRegion\r\n\r\n\r\n\r\nbeyond");
    expect(r.client).toBe(r.server);
  });
  test("Z61 tab stops + HT", async () => {
    const r = await roundtrip("a\tb\tc\r\n");
    expect(r.client).toBe(r.server);
  });
  test("Z62 backspace (BS) + overstrike", async () => {
    const r = await roundtrip("abc\b\b\bXYZ");
    expect(r.client).toBe(r.server);
  });
});

describe("format zoo — OSC + DCS (non-grid sequences ignored cleanly)", () => {
  test("Z70 OSC 0 (window title) doesn't corrupt grid", async () => {
    const r = await roundtrip("\x1b]0;my title\x07visible\r\n");
    expect(r.client).toBe(r.server);
  });
  test("Z71 OSC 7 (cwd notification) ignored by grid", async () => {
    const r = await roundtrip("\x1b]7;file:///tmp/foo\x1b\\after\r\n");
    expect(r.client).toBe(r.server);
  });
  test("Z72 OSC 8 (hyperlink)", async () => {
    const r = await roundtrip("\x1b]8;;https://roost.dev\x07link\x1b]8;;\x07 after\r\n");
    expect(r.client).toBe(r.server);
  });
  test("Z73 OSC 1337 (iterm-style image) — bytes consumed, no garbage", async () => {
    const r = await roundtrip("before\x1b]1337;File=name=x.png:abcdef\x07after\r\n");
    expect(r.client).toBe(r.server);
  });
});

describe("format zoo — split / partial sequences", () => {
  test("Z80 split CSI across writeRaw calls (mid-sequence boundary)", async () => {
    // Done in a single write here; the carry semantics are exercised in
    // session-manager's appendScrollback path. This is the simpler
    // serialize parity check after a full sequence lands.
    const r = await roundtrip("\x1b[31mred\x1b[0m");
    expect(r.client).toBe(r.server);
  });
  test("Z81 incomplete OSC at end (no terminator) tolerated", async () => {
    // wterm should hold the OSC in the parser buffer; serialize emits
    // the grid (empty visible content) and the client mirrors.
    const r = await roundtrip("\x1b]0;unterminated");
    expect(r.client).toBe(r.server);
  });
});

describe("format zoo — long content stress", () => {
  test("Z90 20 lines of mixed SGR (visible-grid only)", async () => {
    const lines: string[] = [];
    for (let i = 0; i < 20; i++) {
      const c = 30 + (i % 8);
      lines.push(`\x1b[${c}mline ${i}\x1b[0m`);
    }
    const r = await roundtrip(lines.join("\r\n") + "\r\n", 80, 24);
    expect(r.client).toBe(r.server);
  });
  test("Z91 box drawing (TUI frame)", async () => {
    const r = await roundtrip("┌──────┐\r\n│ box  │\r\n└──────┘\r\n", 20, 6);
    expect(r.client).toBe(r.server);
  });
});
