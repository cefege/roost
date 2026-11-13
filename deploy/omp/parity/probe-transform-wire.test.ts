import { describe, expect, test } from "bun:test";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { ParityProbe } from "./probe";
import { instrumentSource } from "./probe-transform";

describe("encodeTspJson wire probe", () => {
  test("records a relay-encoded reply as an inbound frame with relay origin, and the decoded copy too", () => {
    const dir = mkdtempSync(join(tmpdir(), "probe-origin-"));
    const tracePath = join(dir, "trace.jsonl");
    writeFileSync(tracePath, "");
    const local = new ParityProbe({ artifactRoot: dir, tracePath, effectsPath: join(dir, "effects.jsonl"), adapter: "browser" });
    const origin = { source: "packages/tui/src/native/encode.ts", line: 1, endLine: 1, component: "c", method: "m" };
    const body = { r: "blobs", have: [] };
    local.wire("out", { verb: "r", body }, origin);
    local.wire("in", { verb: "r", body }, origin);
    local.wire("in", { verb: "r", body: { r: "hello" } }, origin);
    local.wire("in", { verb: "r", body }, origin);
    const rows = readFileSync(tracePath, "utf8").trim().split("\n").map(line => JSON.parse(line));
    expect(rows.map(row => [row.direction, row.origin])).toEqual([["in", "relay"], ["in", "relay"], ["in", undefined], ["in", undefined]]);
  });
});

describe("delegated helper access probe", () => {
  const helper = (line: number, expression: string) => ({ source: "packages/tui/src/x.ts", line, expression });
  const entries = [{ category: "component", details: { delegatedHelpers: [helper(1, "f"), helper(2, "g"), helper(3, "o.p"), helper(4, "padding")] } }] as never;
  const text = "export const a = (x: number) => f(x) ?? 1;\nexport const b = (x: number) => g(x) > 2;\nexport const c = (o: { p: number }) => { o.p = 1; };\nexport const d = (n: number, ctx: { gutter: string; text: string }) => padding(n) + ctx.gutter + ctx.text;\n";

  test("observes a helper call on the left of a binary operator but not an assignment target", () => {
    const out = instrumentSource(text, "packages/tui/src/x.ts", "file:///probe.ts", entries);
    expect(out.contents).toContain("observedValue(f(x),");
    expect(out.contents).toContain("observedValue(g(x),");
    expect(out.contents).toContain("observedValue(padding(n),");
    expect(out.contents).not.toContain("observedValue(o.p");
  });
});
