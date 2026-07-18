// Covers synthQueryReplies: the DA/XTVERSION probe scanner. Real claude sends
// Primary DA (ESC[c) x2 + XTVERSION (ESC[>0q) at startup (captured 2026-07-05);
// wterm-core answers neither. These pin the byte-exact replies + no false match.

import { test, expect } from "bun:test";
import { synthQueryReplies } from "../src/terminal-query-reply.ts";

const enc = (s: string) => new TextEncoder().encode(s);
const DA = "\x1b[?1;2c";
const XTV = "\x1bP>|wterm(roost)\x1b\\";

test("Primary DA (ESC[c) → DA reply", () => {
  expect(synthQueryReplies(enc("\x1b[c"))).toBe(DA);
});

test("Primary DA with explicit 0 (ESC[0c) → DA reply", () => {
  expect(synthQueryReplies(enc("\x1b[0c"))).toBe(DA);
});

test("XTVERSION (ESC[>0q) → version reply", () => {
  expect(synthQueryReplies(enc("\x1b[>0q"))).toBe(XTV);
});

test("real claude startup burst → DA x2 + XTVERSION, in order", () => {
  // Shape mirrors the captured startup: DA, mode sets, DA again, XTVERSION.
  const burst = "\x1b[c\x1b[?1049h\x1b[2J\x1b[c\x1b[>0q\x1b[?2004h";
  expect(synthQueryReplies(enc(burst))).toBe(DA + DA + XTV);
});

test("no false match on secondary DA / cursor moves / SGR", () => {
  // ESC[>c (secondary DA — we don't answer), ESC[12G (cursor col), ESC[38;2;..m
  expect(synthQueryReplies(enc("\x1b[>c\x1b[12G\x1b[38;2;1;2;3m"))).toBe("");
});

test("empty / no-escape chunk → no reply", () => {
  expect(synthQueryReplies(enc("hello world\n"))).toBe("");
  expect(synthQueryReplies(new Uint8Array(0))).toBe("");
});
